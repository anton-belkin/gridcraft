#!/usr/bin/env bash
# Build GridCraft for the web (wasm + wasm-bindgen glue, in target/agent-web) and, given a
# workbook, export it with its agent layer as one HTML page.
# Usage: agent-layer/export-web.sh [book.xlsx out.html]
# Needs: rustup target add wasm32-unknown-unknown; cargo install wasm-bindgen-cli --version <the
# wasm-bindgen version in Cargo.lock>.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
target="${CARGO_TARGET_DIR:-target}"
want=$(awk '/^name = "wasm-bindgen"$/ {getline; gsub(/[^0-9.]/, ""); print; exit}' Cargo.lock)
have=$(wasm-bindgen --version 2>/dev/null | awk '{print $2}' || true)
if [ "$have" != "$want" ]; then
  echo "wasm-bindgen $want needed (found: ${have:-none}): cargo install wasm-bindgen-cli --version $want --locked" >&2
  exit 1
fi
# Some toolchains ship rust-lld without finding their own libLLVM: point it there.
DYLD_FALLBACK_LIBRARY_PATH="$(rustc --print sysroot)/lib${DYLD_FALLBACK_LIBRARY_PATH:+:$DYLD_FALLBACK_LIBRARY_PATH}" \
  cargo build --release --target wasm32-unknown-unknown -p gridcraft-web
wasm-bindgen --target web --weak-refs --no-typescript --out-dir "$target/agent-web" "$target/wasm32-unknown-unknown/release/gridcraft-web.wasm"
echo "Web build: $target/agent-web"
if [ $# -ge 2 ]; then
  cargo run --release -q -p gridcraft-agent --example export_html -- "$1" --web "$target/agent-web" "$2"
fi
