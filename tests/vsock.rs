/*
 * Copyright 2019 fsyncd, Berlin, Germany.
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

#[cfg(feature="std")]
use {
    nix::sys::socket::{SockAddr as NixSockAddr},
    std::time::Duration,
    std::thread::{self, JoinHandle},
    std::io::Write,
    std::os::unix::io::AsRawFd,
    vsock::{Error, Std, SockAddr, VsockListener, VsockStream, VMADDR_CID_LOCAL},
};

#[cfg(feature="test_vm")]
use {
    nix::sys::socket::VsockAddr,
    rand::RngCore,
    sha2::{Digest, Sha256},
    vsock::{get_local_cid, VMADDR_CID_HOST},
};

use serde::{Serialize, Deserialize};

#[cfg(feature="test_vm")]
const TEST_BLOB_SIZE: usize = 1_000_000;
#[cfg(feature="test_vm")]
const TEST_BLOCK_SIZE: usize = 5_000;

/// A simple test for the vsock implementation.
/// Generate a large random blob of binary data, and transfer it in chunks over the VsockStream
/// interface. The vm enpoint is running a simple echo server, so for each chunk we will read
/// it's reply and compute a checksum. Comparing the data sent and received confirms that the
/// vsock implementation does not introduce corruption and properly implements the interface
/// semantics.
#[test]
#[cfg(all(feature = "test_vm", feature="std"))]
fn test_vsock() {
    let mut rng = rand::thread_rng();
    let mut blob: Vec<u8> = vec![];
    let mut rx_blob = vec![];
    let mut tx_pos = 0;

    blob.resize(TEST_BLOB_SIZE, 0);
    rx_blob.resize(TEST_BLOB_SIZE, 0);
    rng.fill_bytes(&mut blob);

    let stream =
        VsockStream::<Std>::connect_with_vsock_addr(&VsockAddr::new(3, 8000)).expect("connection failed");

    while tx_pos < TEST_BLOB_SIZE {
        let written_bytes = stream
            .write(&blob[tx_pos..tx_pos + TEST_BLOCK_SIZE])
            .expect("write failed");
        if written_bytes == 0 {
            panic!("stream unexpectedly closed");
        }

        let mut rx_pos = tx_pos;
        while rx_pos < (tx_pos + written_bytes) {
            let read_bytes = stream.read(&mut rx_blob[rx_pos..]).expect("read failed");
            if read_bytes == 0 {
                panic!("stream unexpectedly closed");
            }
            rx_pos += read_bytes;
        }

        tx_pos += written_bytes;
    }

    let expected = Sha256::digest(&blob);
    let actual = Sha256::digest(&rx_blob);

    assert_eq!(expected, actual);
}

#[test]
#[cfg(all(feature = "test_vm", feature="std"))]
fn test_get_local_cid() {
    assert_eq!(get_local_cid().unwrap(), VMADDR_CID_HOST);
}

#[derive(Debug, Serialize, Deserialize)]
struct Request {
    message: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Response {
    message: String,
}

#[cfg(feature="std")]
fn handle_connection(stream: &mut VsockStream) {
    let mut buff = [0u8; 100];
    let n = stream.read(&mut buff).unwrap();
    let req: Request = serde_cbor::from_slice(&buff[0..n]).unwrap();
    let resp = Response {
        message: req.message.clone().to_uppercase(),
    };
    stream.write_all(&serde_cbor::ser::to_vec(&resp).expect("serialization error")).unwrap();
}

#[cfg(feature="std")]
fn start_server(cid: u32, port: u32) -> (JoinHandle<()>, u32) {
    let listener = VsockListener::bind_with_cid_port(cid, port).unwrap();
    assert_eq!(SockAddr::from_raw_fd::<Std>(listener.as_raw_fd()).unwrap(), listener.local_addr().unwrap());
    assert_eq!(SockAddr::peer_from_raw_fd::<Std>(listener.as_raw_fd()).unwrap_err(), Error::SystemError(107));
    let port = if let NixSockAddr::Vsock(vsock) = listener.local_addr().unwrap().into() {
        vsock.port()
    } else {
        panic!("Not vsock port");
    };

    let handle = thread::Builder::new().spawn(move || {
        let listener = listener;
        loop {
            let (stream, peer_addr) = listener.accept().unwrap();
            assert_eq!(SockAddr::peer_from_raw_fd::<Std>(stream.as_raw_fd()).unwrap(), peer_addr);
            let _ = thread::Builder::new()
                .spawn(move || {
                    let mut stream = stream;
                    handle_connection(&mut stream);
                });
        }
    }).unwrap();
    (handle, port)
}

#[cfg(feature="std")]
fn connect(cid: u32, port: u32) -> Result<(Request, Response), Error> {
    let mut client = VsockStream::<Std>::connect_with_cid_port(cid, port)?;
    let req = Request {
        message: "Hello world!".to_string(),
    };
    client.write_all(&serde_cbor::ser::to_vec(&req).unwrap()).unwrap();
    let mut buff = [0u8; 100];
    let n = client.read(&mut buff).unwrap();
    let resp: Response = serde_cbor::from_slice(&buff[0..n]).unwrap();
    Ok((req, resp))
}

#[cfg(feature="std")]
fn test_connection(cid: u32, port: u32) {
    let (req, resp) = connect(cid, port).unwrap();
    println!("send: {:?}", req);
    println!("received: {:?}", resp);
    assert_eq!(resp, Response { message: req.message.to_uppercase() });
}

#[test]
#[cfg(feature="std")]
fn test_loopback() {
    let (_server_thread, port) = start_server(vsock::VMADDR_CID_ANY, 3001);
    // Wait until server started
    std::thread::sleep(Duration::from_millis(500));
    test_connection(VMADDR_CID_LOCAL, port);
    assert!(connect(vsock::VMADDR_CID_HOST, port).is_err());
    connect(vsock::VMADDR_CID_HOST, port).or_else(|_e| connect(vsock::VMADDR_CID_LOCAL, port)).unwrap();
    connect(vsock::VMADDR_CID_HOST, port).or_else(|_e| connect(vsock::VMADDR_CID_LOCAL, port)).unwrap();
    connect(vsock::VMADDR_CID_HOST, port).or_else(|_e| connect(vsock::VMADDR_CID_LOCAL, port)).unwrap();
    test_connection(VMADDR_CID_LOCAL, port);
    test_connection(VMADDR_CID_LOCAL, port);
}

#[test]
#[cfg(feature="std")]
fn test_loopback_rand_port() {
    let (_server_thread, port0) = start_server(VMADDR_CID_LOCAL, 0);
    let (_server_thread, port1) = start_server(VMADDR_CID_LOCAL, 0);
    let (_server_thread, port2) = start_server(VMADDR_CID_LOCAL, 0);
    // Wait until server started
    std::thread::sleep(Duration::from_millis(500));
    test_connection(VMADDR_CID_LOCAL, port0);
    test_connection(VMADDR_CID_LOCAL, port1);
    test_connection(VMADDR_CID_LOCAL, port2);
}
