#!/bin/sh
# Builds the example plugins and lays each out as an installable folder in
# ./dist/<id>/ (plugin.toml + plugin.wasm). Copy a folder into
# <config dir>/plugins/ to install it.
set -eu
cd "$(dirname "$0")"
cargo build --release --target wasm32-unknown-unknown
for p in text-replacer word-highlighter quote-message; do
    mkdir -p "dist/$p"
    cp "$p/plugin.toml" "dist/$p/plugin.toml"
    cp "target/wasm32-unknown-unknown/release/$(echo "$p" | tr - _).wasm" "dist/$p/plugin.wasm"
done
ls -l dist/*/plugin.wasm
