#! /bin/bash

set -eux -o pipefail

cargo zigbuild --target x86_64-unknown-linux-gnu.2.17 --release
cargo zigbuild --target aarch64-unknown-linux-gnu.2.17 --release
