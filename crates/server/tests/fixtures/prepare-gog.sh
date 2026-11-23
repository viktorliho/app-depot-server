#!/bin/sh
# Synthetic Inno Setup packages from innospect's test matrix, pinned by commit and SHA-256.
set -eu
target=${1:?provide an isolated fixture directory}
mkdir -p "$target/single" "$target/multi" "$target/launch"
base=https://raw.githubusercontent.com/ATRAPSLLC/innospect/2e286a43d30cc4a350e02cf45713552b771b56d4/tests/samples
fetch() {
    curl --fail --silent --show-error --location --max-time 30 --retry 3 "$base/$1" -o "$target/$2"
    printf '%s  %s\n' "$3" "$target/$2" | sha256sum --check
}
fetch plain/plain-tool5_5_7.exe single/setup.exe f30fede0cd4ec70d7c9e2eed45a8d601c553749bb52a91bf62098f7b07a5ade4
fetch spanning/spanning-tool5_5_7.exe multi/spanning.exe 687504d828b9d4df54677d86b29ddc508ea010ea4a8fb433666cf2c4baaf0bda
fetch spanning/spanning-tool5_5_7.slices/spanning-1a.bin multi/spanning-1a.bin fbb87971147462ecb1749db5748414b41adbe3228a74a00adabd197263724644
fetch spanning/spanning-tool5_5_7.slices/spanning-1b.bin multi/spanning-1b.bin c5442ff680ad4e52623eb40ed4cd2655d67e718235f2b6d007eda8ffaaecbd0f
fetch full/full-tool6_1_0.exe launch/setup.exe 43653b8a902d483dc53131dd42d463f087d637cbf87529f1d63b616feeb01ee0
