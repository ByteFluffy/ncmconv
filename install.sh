#!/bin/sh
# Installs ncmconv from GitHub releases.
#
#   curl -fsSL https://raw.githubusercontent.com/ByteFluffy/ncmconv/main/install.sh | sh
#
# Environment:
#   NCMCONV_VERSION      release tag to install (default: latest)
#   NCMCONV_INSTALL_DIR  destination directory (default: ~/.local/bin)
#
# The archive is verified against the release's checksums.txt before anything
# is installed, and nothing outside the destination directory is touched.
set -eu

REPO="ByteFluffy/ncmconv"
BIN="ncmconv"

info() { printf '%s\n' "$*"; }
fail() { printf 'error: %s\n' "$*" >&2; exit 1; }

detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Darwin) os_part="apple-darwin" ;;
        Linux) os_part="unknown-linux-gnu" ;;
        *) fail "unsupported operating system: $os (use the Windows zip from the releases page)" ;;
    esac
    case "$arch" in
        arm64|aarch64) cpu="aarch64" ;;
        x86_64|amd64) cpu="x86_64" ;;
        *) fail "unsupported architecture: $arch" ;;
    esac
    if [ "$os_part" = "unknown-linux-gnu" ] && [ "$cpu" = "aarch64" ]; then
        fail "no Linux arm64 build is published; build from source with: cargo install --git https://github.com/$REPO"
    fi
    printf '%s-%s' "$cpu" "$os_part"
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        fail "need sha256sum or shasum to verify the download"
    fi
}

target=$(detect_target)
version=${NCMCONV_VERSION:-latest}
if [ "$version" = "latest" ]; then
    base="https://github.com/$REPO/releases/latest/download"
    name="${BIN}-latest-${target}.tar.gz"
    # The latest download URL keeps the versioned asset name, so ask the API for
    # it rather than guessing.
    version=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" |
        sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
    [ -n "$version" ] || fail "could not resolve the latest release (set NCMCONV_VERSION)"
    base="https://github.com/$REPO/releases/download/$version"
    name="${BIN}-${version}-${target}.tar.gz"
else
    base="https://github.com/$REPO/releases/download/$version"
    name="${BIN}-${version}-${target}.tar.gz"
fi

install_dir=${NCMCONV_INSTALL_DIR:-$HOME/.local/bin}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

info "downloading $name"
curl -fsSL "$base/$name" -o "$tmp/$name" || fail "download failed: $base/$name"
curl -fsSL "$base/checksums.txt" -o "$tmp/checksums.txt" || fail "cannot fetch checksums.txt"

expected=$(grep " $name\$" "$tmp/checksums.txt" | cut -d' ' -f1 || true)
[ -n "$expected" ] || fail "no checksum listed for $name"
actual=$(sha256_of "$tmp/$name")
[ "$expected" = "$actual" ] || fail "checksum mismatch for $name (expected $expected, got $actual)"
info "checksum ok"

tar -xzf "$tmp/$name" -C "$tmp" || fail "cannot extract $name"
[ -f "$tmp/$BIN" ] || fail "archive does not contain $BIN"

mkdir -p "$install_dir"
install -m 0755 "$tmp/$BIN" "$install_dir/$BIN" 2>/dev/null || {
    cp "$tmp/$BIN" "$install_dir/$BIN"
    chmod 0755 "$install_dir/$BIN"
}

info "installed $("$install_dir/$BIN" -version) to $install_dir/$BIN"
case ":$PATH:" in
    *":$install_dir:"*) ;;
    *) info "note: $install_dir is not in PATH; add it before running ncmconv" ;;
esac
if [ "$(uname -s)" = "Darwin" ]; then
    info "note: if macOS blocks it, run: xattr -d com.apple.quarantine $install_dir/$BIN"
fi
