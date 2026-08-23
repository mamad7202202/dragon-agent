#!/bin/sh
# Dragon Agent installer - Linux / macOS
#   curl -fsSL https://raw.githubusercontent.com/mamad7202202/dragon-agent/main/install.sh | sh

set -eu

REPO="mamad7202202/dragon-agent"
TAG=""

os=$(uname -s)
arch=$(uname -m)

case "$os" in
    Linux)  os="linux" ;;
    Darwin) os="darwin" ;;
    *) echo "unsupported OS: $os" >&2; exit 1 ;;
esac

case "$arch" in
    x86_64|amd64)      arch="amd64" ;;
    arm64|aarch64)     arch="arm64" ;;
    *) echo "unsupported architecture: $arch" >&2; exit 1 ;;
esac

file="dragon-$os-$arch"
url="https://github.com/$REPO/releases/latest/download/$file"
dest="${DRAGON_INSTALL_DIR:-$HOME/.local/bin}"

echo "dragon installer"
echo "  from : $url"
echo "  to   : $dest/dragon"

mkdir -p "$dest"
curl -fsSL "$url" -o "$dest/dragon"
chmod +x "$dest/dragon"

case ":$PATH:" in
    *":$dest:"*) : ;;
    *) echo ""
       echo "NOTE: $dest is not on your PATH. Add this to ~/.zshrc or ~/.bashrc:"
       echo "  export PATH=\"$dest:\$PATH\"" ;;
esac

echo ""
echo "installed -> $dest/dragon"
echo "run: dragon"
