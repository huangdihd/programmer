#!/bin/sh
#
# install.sh — install programmer from a GitHub Release into a user directory.
#
#   Latest:      curl -fsSL https://raw.githubusercontent.com/huangdihd/programmer/main/scripts/install.sh | sh
#   Pinned:      sh install.sh --version v0.2.0
#   Custom dir:  sh install.sh --bin-dir "$HOME/bin"
#
set -eu

repo_owner="huangdihd"
repo_name="programmer"
version="latest"
bin_dir=""

usage() {
  cat <<'EOF'
Usage: install.sh [--version TAG] [--bin-dir DIR]

Installs the programmer binary from GitHub Releases (macOS / Linux).

Options:
  --version TAG   Install a specific release tag, e.g. v0.2.0 (default: latest)
  --bin-dir DIR   Install into DIR instead of an on-PATH user bin directory
  -h, --help      Show this help

Examples:
  curl -fsSL https://raw.githubusercontent.com/huangdihd/programmer/main/scripts/install.sh | sh
  sh install.sh --version v0.2.0
EOF
}

err() {
  echo "install.sh: error: $*" >&2
  exit 1
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      [ "$#" -ge 2 ] || err "--version requires an argument"
      version="$2"
      shift 2
      ;;
    --bin-dir)
      [ "$#" -ge 2 ] || err "--bin-dir requires an argument"
      bin_dir="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      err "unknown option: $1 (see --help)"
      ;;
  esac
done

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Darwin)
    case "$arch" in
      arm64|aarch64) target="aarch64-apple-darwin" ;;
      x86_64|amd64)  target="x86_64-apple-darwin" ;;
      *) err "unsupported macOS architecture: $arch" ;;
    esac
    ;;
  Linux)
    case "$arch" in
      x86_64|amd64)  target="x86_64-unknown-linux-gnu" ;;
      aarch64|arm64) target="aarch64-unknown-linux-gnu" ;;
      armv7l|armv7)  target="armv7-unknown-linux-gnueabihf" ;;
      riscv64)       target="riscv64gc-unknown-linux-gnu" ;;
      i686)          target="i686-unknown-linux-gnu" ;;
      *) err "unsupported Linux architecture: $arch" ;;
    esac
    ;;
  *)
    err "unsupported OS: $os (prebuilt binaries exist for macOS and Linux only)"
    ;;
esac

if command -v curl >/dev/null 2>&1; then
  download() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  download() { wget -qO "$2" "$1"; }
else
  err "neither curl nor wget is available"
fi

base_url="https://github.com/$repo_owner/$repo_name/releases"
if [ "$version" = "latest" ]; then
  url="$base_url/latest/download/programmer-$target.tar.gz"
else
  url="$base_url/download/$version/programmer-$target.tar.gz"
fi

if [ -z "$bin_dir" ]; then
  bin_dir="$HOME/.local/bin"
  case ":$PATH:" in
    *":$HOME/bin:"*)        bin_dir="$HOME/bin" ;;
    *":$HOME/.cargo/bin:"*) bin_dir="$HOME/.cargo/bin" ;;
  esac
fi

if [ ! -d "$bin_dir" ] && ! mkdir -p "$bin_dir" 2>/dev/null; then
  err "cannot create install directory: $bin_dir"
fi
if [ ! -w "$bin_dir" ]; then
  err "install directory is not writable: $bin_dir (use --bin-dir with a user-writable path)"
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/programmer-install.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT INT TERM

echo "Downloading $url"
download "$url" "$tmp_dir/programmer.tar.gz"
tar -xzf "$tmp_dir/programmer.tar.gz" -C "$tmp_dir"
[ -f "$tmp_dir/programmer" ] || err "archive did not contain a 'programmer' binary"

dest="$bin_dir/programmer"
install -m 0755 "$tmp_dir/programmer" "$dest"

echo "Installed $dest"
if ! "$dest" --version; then
  rm -f "$dest"
  err "installed binary failed its version check"
fi

case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) echo "Note: add $bin_dir to your PATH to run 'programmer'." ;;
esac
