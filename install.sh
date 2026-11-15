#!/bin/sh
# Install craftctl from the latest GitHub release (macOS or Linux), then hook it into topgrade
# if present:
#   curl -fsSL https://raw.githubusercontent.com/badriram/craftctl/main/install.sh | sh
set -eu
REPO="badriram/craftctl"
BIN_DIR="${CRAFTCTL_BIN_DIR:-$HOME/.local/bin}"

case "$(uname -s)/$(uname -m)" in
  Darwin/*) ASSET="craftctl-macos-universal.tar.gz" ;;
  Linux/x86_64 | Linux/amd64) ASSET="craftctl-linux-x86_64.tar.gz" ;;
  Linux/aarch64 | Linux/arm64) ASSET="craftctl-linux-aarch64.tar.gz" ;;
  *) echo "craftctl supports macOS and Linux (x86_64, aarch64); this is $(uname -sm)" >&2; exit 1 ;;
esac
if command -v sha256sum >/dev/null 2>&1; then SHA="sha256sum"; else SHA="shasum -a 256"; fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
base="https://github.com/$REPO/releases/latest/download"
curl -fsSL -o "$tmp/$ASSET" "$base/$ASSET"
curl -fsSL -o "$tmp/SHA256SUMS.txt" "$base/SHA256SUMS.txt"
(cd "$tmp" && grep " $ASSET\$" SHA256SUMS.txt | $SHA -c - >/dev/null) \
  || { echo "checksum mismatch for $ASSET" >&2; exit 1; }
tar -xzf "$tmp/$ASSET" -C "$tmp"
mkdir -p "$BIN_DIR"
install -m 755 "$tmp/craftctl" "$BIN_DIR/craftctl"
echo "installed $("$BIN_DIR/craftctl" --version) to $BIN_DIR"

if command -v topgrade >/dev/null 2>&1; then
  mkdir -p "$HOME/.config/topgrade.d"
  cat > "$HOME/.config/topgrade.d/craftctl.toml" <<TOML
# ArtCraft apps + MCP servers, managed by craftctl (https://github.com/$REPO)
[commands]
"ArtCraft apps + MCP" = "$BIN_DIR/craftctl sync"
TOML
  echo "topgrade will now run: craftctl sync"
fi

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "note: add $BIN_DIR to your PATH" ;;
esac
echo "next: craftctl -n sync   (dry run), then: craftctl sync"
