#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SERVER_DIR="$ROOT_DIR/server"
DIST_DIR="$ROOT_DIR/dist"
BINARY_NAME="shanhai-translate-lsp-server"

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Darwin) os_id="darwin" ;;
  Linux) os_id="linux" ;;
  MINGW*|MSYS*|CYGWIN*|Windows_NT) os_id="windows" ;;
  *)
    echo "Unsupported OS: $os" >&2
    exit 1
    ;;
esac

case "$arch" in
  arm64|aarch64) arch_id="aarch64" ;;
  x86_64|amd64) arch_id="x86_64" ;;
  *)
    echo "Unsupported architecture: $arch" >&2
    exit 1
    ;;
esac

binary_filename="$BINARY_NAME"
archive_suffix="tar.gz"
if [[ "$os_id" == "windows" ]]; then
  binary_filename="${binary_filename}.exe"
  archive_suffix="zip"
fi

cargo build --manifest-path "$SERVER_DIR/Cargo.toml" --release

mkdir -p "$DIST_DIR"
work_dir="$(mktemp -d)"
cleanup() {
  rm -rf "$work_dir"
}
trap cleanup EXIT

cp "$SERVER_DIR/target/release/$binary_filename" "$work_dir/$binary_filename"

asset_name="${BINARY_NAME}-${os_id}-${arch_id}.${archive_suffix}"

if [[ "$os_id" == "windows" ]]; then
  powershell -NoProfile -Command "Compress-Archive -Path '$work_dir\\$binary_filename' -DestinationPath '$DIST_DIR\\$asset_name' -Force" >/dev/null
else
  chmod +x "$work_dir/$binary_filename"
  tar -czf "$DIST_DIR/$asset_name" -C "$work_dir" "$binary_filename"
fi

echo "Created release asset: $DIST_DIR/$asset_name"
