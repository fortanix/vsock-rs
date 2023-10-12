#!/bin/bash -ex
docker build . -t vsock_container
docker run --privileged -ti vsock_container
