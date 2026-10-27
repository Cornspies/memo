#!/bin/sh
# Builds memo-core for the browser into src/wasm/. Needs the wasm32-unknown-unknown target and the
# wasm-bindgen CLI at the exact version of the wasm-bindgen crate (see README.md).
set -eu
cd "$(dirname "$0")/.."
# Where `cargo install` puts the CLI, in case it is not on PATH.
PATH="$PATH:${CARGO_HOME:-$HOME/.cargo}/bin"
want=$(sed -n 's/^wasm-bindgen = "=\(.*\)"$/\1/p' crates/wasm/Cargo.toml)
have=$(wasm-bindgen --version 2>/dev/null | cut -d' ' -f2 || true)
if [ "$have" != "$want" ]; then
    echo "need wasm-bindgen $want (found: ${have:-none}): cargo install wasm-bindgen-cli --version $want --locked" >&2
    exit 1
fi
cargo build -p memo-wasm --target wasm32-unknown-unknown --profile wasm
wasm-bindgen --target web --out-dir web/src/wasm --out-name memo \
    target/wasm32-unknown-unknown/wasm/memo_wasm.wasm
