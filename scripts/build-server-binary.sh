#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SERVER_DIR="$ROOT_DIR/server"

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Darwin) os_id="darwin" ;;
  Linux) os_id="linux" ;;
  *)
    echo "Unsupported OS: $os" >&2
    exit 1
    ;;
esac

case "$arch" in
  arm64|aarch64) arch_id="aarch64" ;;
  x86_64) arch_id="x86_64" ;;
  *)
    echo "Unsupported architecture: $arch" >&2
    exit 1
    ;;
esac

binary_name="shanhai-translate-lsp-server"
if [[ "$os_id" == "windows" ]]; then
  binary_name="${binary_name}.exe"
fi

target_dir="$ROOT_DIR/bin/${os_id}-${arch_id}"

cargo build --manifest-path "$SERVER_DIR/Cargo.toml" --release
mkdir -p "$target_dir"
cp "$SERVER_DIR/target/release/$binary_name" "$target_dir/$binary_name"
chmod +x "$target_dir/$binary_name"

echo "Packaged binary: $target_dir/$binary_name"
