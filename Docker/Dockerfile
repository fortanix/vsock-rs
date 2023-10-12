From ubuntu:latest
# make the tzdata prompt non-interactive.
ENV DEBIAN_FRONTEND=noninteractive

COPY Scripts/rustup.sh .

RUN apt-get update
RUN apt-get install -y clang cmake curl git libssl-dev make ninja-build pkg-config protobuf-compiler python3 wget

RUN ./rustup.sh -y
ENV PATH="$HOME/.cargo/bin:$PATH"
RUN "$HOME/.cargo/bin/rustup" target add x86_64-unknown-linux-musl

RUN git clone https://github.com/fortanix/vsock-rs.git -b raoul/test_vsock_in_container

WORKDIR /vsock-rs
ENTRYPOINT bash
