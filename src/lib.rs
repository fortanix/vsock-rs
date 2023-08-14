/*
 * Copyright 2019 fsyncd, Berlin, Germany.
 * Additional material Copyright the Rust project and it's contributors.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */
//! Virtio socket support for Rust.

#![cfg_attr(not(feature = "std"), no_std)]
#![deny(warnings)]

use core::mem;
use core::fmt::{Display, Error as FmtError, Formatter};
use core::convert::TryFrom;
use core::marker::PhantomData;
use core::time::Duration;
#[cfg(feature="random_port")]
pub use getrandom::Error as GetRandomError;
use libc::*;
#[cfg(not(feature="std"))]
use libc::c_int as RawFd;
pub use libc::{VMADDR_CID_ANY, VMADDR_CID_HOST, VMADDR_CID_HYPERVISOR, VMADDR_CID_LOCAL};
#[cfg(feature="std")]
use nix::ioctl_read_bad;
#[cfg(feature="std")]
use nix::sys::socket::{SockAddr as NixSockAddr, VsockAddr as NixVsockAddr};
#[cfg(feature="std")]
use std::error::Error as StdError;
#[cfg(feature="std")]
use std::fs::File;
#[cfg(feature="std")]
use std::io::{self, ErrorKind, Read, Write};
#[cfg(feature="std")]
pub use std::net::Shutdown;
#[cfg(feature="std")]
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, RawFd};

#[cfg(feature="random_port")]
const MAX_PRIVILEGED_PORT: u32 = 1023;
#[cfg(feature="random_port")]
const BIND_RETRIES: u32 = 10;
#[cfg(not(feature="random_port"))]
const BIND_RETRIES: u32 = 0;

#[cfg(not(feature="std"))]
pub enum Shutdown {
    Read,
    Write,
    Both,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    EntropyError,
    SystemError(i32),
    WrongAddressType,
    ZeroDurationTimeout,
    ReservedPort,
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), FmtError> {
        match self {
            Error::EntropyError        => write!(f, "failed to retrieve enough entropy"),
            Error::SystemError(errno)  => write!(f, "system error {}", errno),
            Error::WrongAddressType    => write!(f, "wrong address type provided"),
            Error::ZeroDurationTimeout => write!(f, "cannot set a zero duration timeout"),
            Error::ReservedPort        => write!(f, "reserved port"),
        }
    }
}

#[cfg(feature="std")]
impl StdError for Error {}

#[cfg(feature="std")]
impl From<Error> for io::Error {
    fn from(err: Error) -> io::Error {
        match err {
            Error::EntropyError        => io::Error::new(ErrorKind::Other, err),
            Error::ReservedPort        => io::Error::new(ErrorKind::InvalidInput, err),
            Error::SystemError(errno)  => io::Error::from_raw_os_error(errno),
            Error::WrongAddressType    => io::Error::new(ErrorKind::InvalidInput, err),
            Error::ZeroDurationTimeout => io::Error::new(ErrorKind::InvalidInput, err),
        }
    }
}

pub trait Platform {
    fn last_os_error() -> Error;
}

#[cfg(feature="std")]
#[derive(Debug, Clone)]
pub struct Std;

#[cfg(feature="std")]
impl Platform for Std {
    fn last_os_error() -> Error {
        let errno = io::Error::last_os_error().raw_os_error().expect("Error from OS");
        Error::SystemError(errno)
    }
}

#[derive(Clone)]
#[cfg_attr(feature = "std", derive(Debug, Eq, Hash, PartialEq))]
pub struct SockAddr(libc::sockaddr_vm);

impl SockAddr {
    pub fn from_raw_fd<P: Platform>(fd: RawFd) -> Result<SockAddr, Error> {
        socket_addr::<P>(fd)
    }

    pub fn peer_from_raw_fd<P: Platform>(fd: RawFd) -> Result<SockAddr, Error> {
        peer_addr::<P>(fd)
    }

    pub fn new(cid: u32, port: u32) -> Self {
        SockAddr(new_socket_addr(cid, port))
    }

    pub fn port(&self) -> u32 {
        self.0.svm_port
    }

    pub fn cid(&self) -> u32 {
        self.0.svm_cid
    }
}

#[cfg(feature="std")]
impl From<SockAddr> for NixSockAddr {
    fn from(addr: SockAddr) -> NixSockAddr {
        NixSockAddr::Vsock(NixVsockAddr(addr.0))
    }
}

impl TryFrom<libc::sockaddr_vm> for SockAddr {
    type Error = ();

    fn try_from(addr: libc::sockaddr_vm) -> Result<SockAddr, ()> {
        if addr.svm_family == libc::AF_VSOCK as _ {
            Ok(SockAddr(addr))
        } else {
            Err(())
        }
    }
}

#[cfg(feature="std")]
impl TryFrom<NixSockAddr> for SockAddr {
    type Error = ();

    fn try_from(addr: NixSockAddr) -> Result<SockAddr, ()> {
        if let NixSockAddr::Vsock(addr) = addr {
            Ok(SockAddr(addr.0))
        } else {
            Err(())
        }
    }
}

fn new_socket() -> libc::c_int {
    unsafe { socket(AF_VSOCK, SOCK_STREAM | SOCK_CLOEXEC, 0) }
}

fn new_socket_addr(cid: u32, port: u32) -> libc::sockaddr_vm {
    let mut vsock_addr: libc::sockaddr_vm = unsafe { mem::zeroed() };
    vsock_addr.svm_family = libc::AF_VSOCK as _;
    vsock_addr.svm_cid = cid;
    vsock_addr.svm_port = port;
    vsock_addr
}

/// An iterator that infinitely accepts connections on a VsockListener.
#[cfg(feature="std")]
#[derive(Debug)]
pub struct Incoming<'a, P: Platform = Std> {
    listener: &'a VsockListener<P>,
}

#[cfg(not(feature="std"))]
#[derive(Debug)]
pub struct Incoming<'a, P: Platform> {
    listener: &'a VsockListener<P>,
}

impl<'a, P: Platform> Iterator for Incoming<'a, P> {
    type Item = Result<VsockStream<P>, Error>;

    fn next(&mut self) -> Option<Result<VsockStream<P>, Error>> {
        Some(self.listener.accept().map(|p| p.0))
    }
}

/// A virtio socket server, listening for connections.
#[derive(Debug, Clone)]
#[cfg(feature="std")]
pub struct VsockListener<P: Platform = Std> {
    socket: RawFd,
    phantom: PhantomData<P>,
}

/// A virtio socket server, listening for connections.
#[derive(Debug, Clone)]
#[cfg(not(feature="std"))]
pub struct VsockListener<P: Platform> {
    socket: RawFd,
    phantom: PhantomData<P>,
}

#[cfg(feature="std")]
impl VsockListener<Std> {
    /// Create a new VsockListener which is bound and listening on the socket address.
    pub fn bind(addr: &NixSockAddr) -> Result<VsockListener<Std>, Error> {
        if let NixSockAddr::Vsock(addr) = addr {
            Self::bind_with_cid_port(addr.cid(), addr.port())
        } else {
            Err(Error::WrongAddressType)
        }
    }
}

impl<P: Platform> VsockListener<P> {
    /// Create a new VsockListener with specified cid and port.
    pub fn bind_with_cid_port(cid: u32, port: u32) -> Result<VsockListener<P>, Error> {
        if port == 0 {
            #[cfg(feature="random_port")]
            return Self::bind_with_cid(cid);
            #[cfg(not(feature="random_port"))]
            return Err(Error::ReservedPort);
        }
        let socket = new_socket();
        if socket < 0 {
            return Err(P::last_os_error());
        }

        let mut vsock_addr = new_socket_addr(cid, port);

        let res = unsafe {
            bind(
                socket,
                &mut vsock_addr as *mut _ as *mut sockaddr,
                mem::size_of::<sockaddr_vm>() as socklen_t,
            )
        };
        if res < 0 {
            return Err(P::last_os_error());
        }

        // rust stdlib uses a 128 connection backlog
        let res = unsafe { listen(socket, 128) };
        if res < 0 {
            return Err(P::last_os_error());
        }

        Ok(Self { socket, phantom: PhantomData::<P>::default()})
    }

    /// Create a new VsockListener with specified cid and random port.
    #[cfg(feature="random_port")]
    pub fn bind_with_cid(cid: u32) -> Result<VsockListener<P>, Error> {
        fn bind_with_cid_ex<P: Platform>(cid: u32, retries: u32) -> Result<VsockListener<P>, Error> {
            let listener = Vsock::gen_rand_port()
                .and_then(|port| VsockListener::<P>::bind_with_cid_port(cid, port));
            match listener {
                Ok(listener)           => Ok(listener),
                Err(e) if retries == 0 => Err(e),
                Err(_e)                => bind_with_cid_ex(cid, retries - 1),
            }
        }
        bind_with_cid_ex(cid, BIND_RETRIES)
    }

    /// The local socket address of the listener.
    pub fn local_addr(&self) -> Result<SockAddr, Error> {
        socket_addr::<P>(self.socket)
    }

    /// Create a new independently owned handle to the underlying socket.
    pub fn try_clone(&self) -> Result<VsockListener<P>, Error> {
        Ok(VsockListener {
            socket: self.socket.clone(),
            phantom: PhantomData::default(),
        })
    }

    /// Accept a new incoming connection from this listener.
    pub fn accept(&self) -> Result<(VsockStream<P>, SockAddr), Error> {
        let mut vsock_addr = sockaddr_vm {
            svm_family: AF_VSOCK as sa_family_t,
            svm_reserved1: 0,
            svm_port: 0,
            svm_cid: 0,
            svm_zero: [0u8; 4],
        };
        let mut vsock_addr_len = mem::size_of::<sockaddr_vm>() as socklen_t;
        let socket = unsafe {
            accept4(
                self.socket,
                &mut vsock_addr as *mut _ as *mut sockaddr,
                &mut vsock_addr_len,
                SOCK_CLOEXEC,
            )
        };
        if socket < 0 {
            Err(<P as Platform>::last_os_error())
        } else {
            Ok((
                unsafe { VsockStream::from_raw_fd(socket as RawFd) },
                SockAddr(vsock_addr)
            ))
        }
    }

    /// An iterator over the connections being received on this listener.
    pub fn incoming(&self) -> Incoming<P> {
        Incoming::<P> { listener: self }
    }

    /// Retrieve the latest error associated with the underlying socket.
    pub fn take_error(&self) -> Result<Option<Error>, Error> {
        let mut error: i32 = 0;
        let mut error_len: socklen_t = 0;
        if unsafe {
            getsockopt(
                self.socket,
                SOL_SOCKET,
                SO_ERROR,
                &mut error as *mut _ as *mut c_void,
                &mut error_len,
            )
        } < 0
        {
            Err(P::last_os_error())
        } else {
            Ok(if error == 0 {
                None
            } else {
                Some(Error::SystemError(error))
            })
        }
    }

    /// Move this stream in and out of nonblocking mode.
    pub fn set_nonblocking(&self, nonblocking: bool) -> Result<(), Error> {
        let mut nonblocking: i32 = if nonblocking { 1 } else { 0 };
        if unsafe { ioctl(self.socket, FIONBIO, &mut nonblocking) } < 0 {
            Err(<P as Platform>::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn as_raw_fd(&self) -> RawFd {
        self.socket
    }

    pub fn into_raw_fd(self) -> RawFd {
        let fd = self.socket;
        mem::forget(self);
        fd
    }
}
#[cfg(feature="std")]
impl AsRawFd for VsockListener {
    fn as_raw_fd(&self) -> RawFd {
        VsockListener::as_raw_fd(self)
    }
}

#[cfg(feature="std")]
impl FromRawFd for VsockListener {
    unsafe fn from_raw_fd(socket: RawFd) -> Self {
        Self { socket, phantom: PhantomData::default() }
    }
}

#[cfg(feature="std")]
impl IntoRawFd for VsockListener {
    fn into_raw_fd(self) -> RawFd {
        VsockListener::into_raw_fd(self)
    }
}

impl<P: Platform> Drop for VsockListener<P> {
    fn drop(&mut self) {
        unsafe { close(self.socket) };
    }
}

fn socket_addr<P: Platform>(socket: RawFd) -> Result <SockAddr, Error> {
    let mut vsock_addr = sockaddr_vm {
        svm_family: AF_VSOCK as sa_family_t,
        svm_reserved1: 0,
        svm_port: 0,
        svm_cid: 0,
        svm_zero: [0u8; 4],
    };
    let mut vsock_addr_len = mem::size_of::<sockaddr_vm>() as socklen_t;
    if unsafe {
        getsockname(
            socket,
            &mut vsock_addr as *mut _ as *mut sockaddr,
            &mut vsock_addr_len,
        )
    } < 0
    {
        Err(P::last_os_error())
    } else {
        Ok(SockAddr(vsock_addr))
    }
}

fn peer_addr<P: Platform>(socket: RawFd) -> Result <SockAddr, Error> {
    let mut vsock_addr = sockaddr_vm {
        svm_family: AF_VSOCK as sa_family_t,
        svm_reserved1: 0,
        svm_port: 0,
        svm_cid: 0,
        svm_zero: [0u8; 4],
    };
    let mut vsock_addr_len = mem::size_of::<sockaddr_vm>() as socklen_t;
    if unsafe {
        getpeername(
            socket,
            &mut vsock_addr as *mut _ as *mut sockaddr,
            &mut vsock_addr_len,
        )
    } < 0
    {
        Err(P::last_os_error())
    } else {
        Ok(SockAddr(vsock_addr))
    }
}

pub struct Vsock {
    socket: RawFd,
}

impl Vsock {
    #[cfg(feature="random_port")]
    fn gen_rand_port() -> Result<u32, Error> {
        let mut buf = [0u8; 4];
        getrandom::getrandom(&mut buf).map_err(|_e| Error::EntropyError)?;
        let port = u32::from_le_bytes(buf);
        if port <= MAX_PRIVILEGED_PORT {
            Self::gen_rand_port()
        } else {
            Ok(port)
        }
    }

    pub fn new<P: Platform>() -> Result<Self, Error> {
        let socket = new_socket();
        let mut socket = if 0 <= socket {
            Ok(Vsock{ socket })
        } else {
            Err(<P as Platform>::last_os_error())
        }?;
        socket.bind::<P>(VMADDR_CID_ANY, 0)?;
        Ok(socket)
    }

    fn bind<P: Platform>(&mut self, cid: u32, port: u32) -> Result<(), Error> {
        fn do_bind<P: Platform>(vsock: &mut Vsock, cid: u32, port: u32) -> Result<(), Error> {
            #[cfg(feature="random_port")]
            let port = if port == 0 {
                Vsock::gen_rand_port()?
            } else {
                port
            };

            let mut addr = sockaddr_vm {
                svm_family: AF_VSOCK as sa_family_t,
                svm_reserved1: 0,
                svm_port: port,
                svm_cid: cid,
                svm_zero: [0u8; 4],
            };
            let res = unsafe {
                bind(
                    vsock.socket,
                    &mut addr as *mut _ as *mut sockaddr,
                    mem::size_of::<sockaddr_vm>() as socklen_t,
                )
            };
            if res < 0 {
                Err(P::last_os_error())
            } else {
                Ok(())
            }
        }
        fn bind_ex<P: Platform>(vsock: &mut Vsock, cid: u32, port: u32, retries: u32) -> Result<(), Error> {
            match do_bind::<P>(vsock, cid, port) {
                Ok(())                 => Ok(()),
                Err(e) if retries == 0 => Err(e),
                Err(_e)                => bind_ex::<P>(vsock, cid, port, retries - 1)
            }
        }
        #[cfg(not(feature="random_port"))]
        if port == 0 {
            return Err(Error::ReservedPort);
        }
        bind_ex::<P>(self, cid, port, BIND_RETRIES)
    }

    pub fn addr<P: Platform>(&self) -> Result<SockAddr, Error> {
        socket_addr::<P>(self.socket)
    }

    pub fn connect_with_cid_port<P: Platform>(self, cid: u32, port: u32) -> Result<VsockStream<P>, Error> {
        let addr = new_socket_addr(cid, port);
        Vsock::connect_with_socket_addr(self, &addr)
    }

    pub fn connect_with_socket_addr<P: Platform>(self, vsock_addr: &libc::sockaddr_vm) -> Result<VsockStream<P>, Error> {
        VsockStream::<P>::connect_vsock_to_socket_addr(self, vsock_addr)
    }
}

/// A virtio stream between a local and a remote socket.
#[derive(Debug, Clone)]
#[cfg(feature="std")]
pub struct VsockStream<P: Platform = Std> {
    socket: RawFd,
    phantom: PhantomData<P>,
}

/// A virtio stream between a local and a remote socket.
#[derive(Debug, Clone)]
#[cfg(not(feature="std"))]
pub struct VsockStream<P: Platform> {
    socket: RawFd,
    phantom: PhantomData<P>,
}

impl<P: Platform> VsockStream<P> {
    /// The `FromRawFd` trait isn't available in a `no_std` environment. We mimic
    /// its existance here and make it `unsafe` to avoid compiler warnings
    pub unsafe fn from_raw_fd(socket: RawFd) -> Self {
        Self { socket, phantom: PhantomData::default() }
    }

    pub fn into_raw_fd(self) -> RawFd {
        let fd = self.socket;
        mem::forget(self);
        fd
    }

    /// Open a connection to a remote host.
    pub fn connect(addr: &SockAddr) -> Result<Self, Error> {
        Self::connect_with_socket_addr(&addr.0)
    }
    
    #[cfg(feature="std")]
    pub fn connect_with_vsock_addr(vsock_addr: &NixVsockAddr) -> Result<Self, Error> {
        Self::connect_with_socket_addr(&vsock_addr.0)
    }

    /// Open a connection to a remote host.
    pub fn connect_with_socket_addr(vsock_addr: &libc::sockaddr_vm) -> Result<Self, Error> {
        let vsock = Vsock::new::<P>()?;
        Self::connect_vsock_to_socket_addr(vsock, vsock_addr)
    }

    fn connect_vsock_to_socket_addr(local: Vsock, remote: &libc::sockaddr_vm) -> Result<Self, Error> {
        let Vsock { socket: local } = local;
        if unsafe {
            connect(
                local,
                remote as *const _ as *const sockaddr,
                mem::size_of::<sockaddr_vm>() as socklen_t,
            )
        } < 0
        {
            Err(<P as Platform>::last_os_error())
        } else {
            Ok(unsafe { VsockStream::from_raw_fd(local) })
        }
    }

    /// Open a connection to a remote host with specified cid and port.
    pub fn connect_with_cid_port(cid: u32, port: u32) -> Result<Self, Error> {
        let vsock_addr = new_socket_addr(cid, port);
        Self::connect_with_socket_addr(&vsock_addr)
    }

    /// Virtio socket address of the remote peer associated with this connection.
    pub fn peer_addr(&self) -> Result<SockAddr, Error> {
        let mut vsock_addr = sockaddr_vm {
            svm_family: AF_VSOCK as sa_family_t,
            svm_reserved1: 0,
            svm_port: 0,
            svm_cid: 0,
            svm_zero: [0u8; 4],
        };
        let mut vsock_addr_len = mem::size_of::<sockaddr_vm>() as socklen_t;
        if unsafe {
            getpeername(
                self.socket,
                &mut vsock_addr as *mut _ as *mut sockaddr,
                &mut vsock_addr_len,
            )
        } < 0
        {
            Err(<P as Platform>::last_os_error())
        } else {
            Ok(SockAddr(vsock_addr))
        }
    }

    /// Virtio socket address of the local address associated with this connection.
    pub fn local_addr(&self) -> Result<SockAddr, Error> {
        let mut vsock_addr = sockaddr_vm {
            svm_family: AF_VSOCK as sa_family_t,
            svm_reserved1: 0,
            svm_port: 0,
            svm_cid: 0,
            svm_zero: [0u8; 4],
        };
        let mut vsock_addr_len = mem::size_of::<sockaddr_vm>() as socklen_t;
        if unsafe {
            getsockname(
                self.socket,
                &mut vsock_addr as *mut _ as *mut sockaddr,
                &mut vsock_addr_len,
            )
        } < 0
        {
            Err(<P as Platform>::last_os_error())
        } else {
            Ok(SockAddr(vsock_addr))
        }
    }

    /// Shutdown the read, write, or both halves of this connection.
    pub fn shutdown(&self, how: Shutdown) -> Result<(), Error> {
        let how = match how {
            Shutdown::Write => SHUT_WR,
            Shutdown::Read => SHUT_RD,
            Shutdown::Both => SHUT_RDWR,
        };
        if unsafe { shutdown(self.socket, how) } < 0 {
            Err(<P as Platform>::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Create a new independently owned handle to the underlying socket.
    pub fn try_clone(&self) -> Result<Self, Error> {
        Ok(VsockStream {
            socket: self.socket.clone(),
            phantom: PhantomData::default() 
        })
    }

    /// Set the timeout on read operations.
    pub fn set_read_timeout(&self, dur: Option<Duration>) -> Result<(), Error> {
        let timeout = Self::timeval_from_duration(dur)?;
        if unsafe {
            setsockopt(
                self.socket,
                SOL_SOCKET,
                SO_SNDTIMEO,
                &timeout as *const _ as *const c_void,
                mem::size_of::<timeval>() as socklen_t,
            )
        } < 0
        {
            Err(<P as Platform>::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Set the timeout on write operations.
    pub fn set_write_timeout(&self, dur: Option<Duration>) -> Result<(), Error> {
        let timeout = Self::timeval_from_duration(dur)?;
        if unsafe {
            setsockopt(
                self.socket,
                SOL_SOCKET,
                SO_RCVTIMEO,
                &timeout as *const _ as *const c_void,
                mem::size_of::<timeval>() as socklen_t,
            )
        } < 0
        {
            Err(<P as Platform>::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Retrieve the latest error associated with the underlying socket.
    pub fn take_error(&self) -> Result<Option<Error>, Error> {
        let mut error: i32 = 0;
        let mut error_len: socklen_t = 0;
        if unsafe {
            getsockopt(
                self.socket,
                SOL_SOCKET,
                SO_ERROR,
                &mut error as *mut _ as *mut c_void,
                &mut error_len,
            )
        } < 0
        {
            Err(<P as Platform>::last_os_error())
        } else {
            Ok(if error == 0 {
                None
            } else {
                Some(Error::SystemError(error))
            })
        }
    }

    /// Move this stream in and out of nonblocking mode.
    pub fn set_nonblocking(&self, nonblocking: bool) -> Result<(), Error> {
        let mut nonblocking: i32 = if nonblocking { 1 } else { 0 };
        if unsafe { ioctl(self.socket, FIONBIO, &mut nonblocking) } < 0 {
            Err(<P as Platform>::last_os_error())
        } else {
            Ok(())
        }
    }

    fn timeval_from_duration(dur: Option<Duration>) -> Result<timeval, Error> {
        match dur {
            Some(dur) => {
                if dur.as_secs() == 0 && dur.subsec_nanos() == 0 {
                    return Err(Error::ZeroDurationTimeout);
                }

                // https://github.com/rust-lang/libc/issues/1848
                #[cfg_attr(target_env = "musl", allow(deprecated))]
                let secs = if dur.as_secs() > time_t::max_value() as u64 {
                    time_t::max_value()
                } else {
                    dur.as_secs() as time_t
                };
                let mut timeout = timeval {
                    tv_sec: secs,
                    tv_usec: i64::from(dur.subsec_micros()) as suseconds_t,
                };
                if timeout.tv_sec == 0 && timeout.tv_usec == 0 {
                    timeout.tv_usec = 1;
                }
                Ok(timeout)
            }
            None => Ok(timeval {
                tv_sec: 0,
                tv_usec: 0,
            }),
        }
    }

    pub fn read(&self, buf: &mut [u8]) -> Result<usize, Error> {
        let ret = unsafe { recv(self.socket, buf.as_mut_ptr() as *mut c_void, buf.len(), 0) };
        if ret < 0 {
            Err(<P as Platform>::last_os_error())
        } else {
            Ok(ret as usize)
        }
    }

    pub fn write(&self, buf: &[u8]) -> Result<usize, Error> {
        let ret = unsafe {
            send(
                self.socket,
                buf.as_ptr() as *const c_void,
                buf.len(),
                MSG_NOSIGNAL,
            )
        };
        if ret < 0 {
            Err(<P as Platform>::last_os_error())
        } else {
            Ok(ret as usize)
        }
    }

    #[cfg(not(feature="std"))]
    pub fn write_all(&self, buf: &[u8]) -> Result<usize, Error> {
        if buf.len() == 0 {
            return Ok(0);
        }

        let n = self.write(buf)?;
        Ok(n + self.write_all(&buf[n..])?)
    }

    #[cfg(not(feature="std"))]
    pub fn read_all(&self, buf: &mut [u8]) -> Result<usize, Error> {
        if buf.len() == 0 {
            return Ok(0);
        }

        let n = self.read(buf)?;
        Ok(n + self.read_all(&mut buf[n..])?)
    }

    pub fn flush() -> Result<(), Error> {
        Ok(())
    }
}

#[cfg(feature="std")]
impl Read for VsockStream<Std> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
        <&Self>::read(&mut &*self, buf).into()
    }
}

#[cfg(feature="std")]
impl Write for VsockStream<Std> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, io::Error> {
        <&Self>::write(&mut &*self, buf).into()
    }

    fn flush(&mut self) -> Result<(), io::Error> {
        Ok(())
    }
}

#[cfg(feature="std")]
impl Read for &VsockStream<Std> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
        VsockStream::<Std>::read(self, buf)
            .map_err(|e| e.into())
    }
}

#[cfg(feature="std")]
impl Write for &VsockStream<Std> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, io::Error> {
        VsockStream::<Std>::write(self, buf)
            .map_err(|e| e.into())
    }

    fn flush(&mut self) -> Result<(), io::Error> {
        VsockStream::<Std>::flush()
            .map_err(|e| e.into())
    }
}

#[cfg(feature="std")]
impl AsRawFd for VsockStream {
    fn as_raw_fd(&self) -> RawFd {
        self.socket
    }
}

#[cfg(feature="std")]
impl FromRawFd for VsockStream {
    unsafe fn from_raw_fd(socket: RawFd) -> Self {
        VsockStream::from_raw_fd(socket)
    }
}

#[cfg(feature="std")]
impl IntoRawFd for VsockStream {
    fn into_raw_fd(self) -> RawFd {
        VsockStream::into_raw_fd(self)
    }
}

impl<P: Platform> Drop for VsockStream<P> {
    fn drop(&mut self) {
        unsafe { close(self.socket) };
    }
}


#[cfg(feature="std")]
const IOCTL_VM_SOCKETS_GET_LOCAL_CID: usize = 0x7b9;
#[cfg(feature="std")]
ioctl_read_bad!(
    vm_sockets_get_local_cid,
    IOCTL_VM_SOCKETS_GET_LOCAL_CID,
    u32
);

/// Gets the CID of the local machine.
///
/// Note that when calling [`VsockListener::bind`], you should generally use [`VMADDR_CID_ANY`]
/// instead, and for making a loopback connection you should use [`VMADDR_CID_LOCAL`].
#[cfg(feature="std")]
pub fn get_local_cid() -> Result<u32, io::Error> {
    let f = File::open("/dev/vsock")?;
    let mut cid = 0;
    // SAFETY: the kernel only modifies the given u32 integer.
    unsafe { vm_sockets_get_local_cid(f.as_raw_fd(), &mut cid) }?;
    Ok(cid)
}

#[test]
#[cfg(all(features="std", feature="random_port"))]
fn rand_ports() {
    let mut ports = Vec::new();
    for _i in 0..1000 {
        let port = Vsock::gen_rand_port().unwrap();
        assert!(MAX_PRIVILEGED_PORT < port);
        assert!(ports.iter().all(|p| *p != port));
        ports.push(port);
    }
}
