#! /bin/bash

set -eux -o pipefail

cargo zigbuild --target x86_64-unknown-linux-gnu.2.17 --release
cargo zigbuild --target aarch64-unknown-linux-gnu.2.17 --release

mkdir -p target/github_release
tar cvfz target/github_release/zb-x86_64-unknown-linux-gnu.tar.gz -C target/x86_64-unknown-linux-gnu/release zb
tar cvfz target/github_release/zb-aarch64-unknown-linux-gnu.tar.gz -C target/aarch64-unknown-linux-gnu/release zb
