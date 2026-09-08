#!/bin/sh
# Installs duw from the prebuilt GitHub releases.
#
#   curl -fsSL https://raw.githubusercontent.com/co3moz/duw/master/install.sh | sh
#
# Environment:
#   DUW_VERSION      tag to install, e.g. v0.1.0 (default: the latest release)
#   DUW_INSTALL_DIR  where to put the binary (default: $HOME/.local/bin)

set -eu

REPO="co3moz/duw"
INSTALL_DIR="${DUW_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# Maps uname output onto the Rust target triples the release workflow builds.
detect_target() {
    arch="$(uname -m)"
    case "$arch" in
        x86_64 | amd64) arch="x86_64" ;;
        aarch64 | arm64) arch="aarch64" ;;
        *) die "no prebuilt binary for architecture $arch, try: cargo install duw" ;;
    esac

    case "$(uname -s)" in
        Linux) printf '%s-unknown-linux-gnu\n' "$arch" ;;
        Darwin) printf '%s-apple-darwin\n' "$arch" ;;
        MINGW* | MSYS* | CYGWIN* | Windows_NT)
            die "on Windows, download the .zip from https://github.com/$REPO/releases/latest"
            ;;
        *) die "no prebuilt binary for $(uname -s), try: cargo install duw" ;;
    esac
}

fetch() { # fetch <url> <destination>
    if have curl; then
        curl -fsSL "$1" -o "$2"
    elif have wget; then
        wget -qO "$2" "$1"
    else
        die "need curl or wget"
    fi
}

verify() { # verify <directory> <archive name>
    if ! [ -f "$1/SHA256SUMS" ]; then
        say "note: no SHA256SUMS in this release, skipping verification"
        return 0
    fi

    if have sha256sum; then
        check="sha256sum -c -"
    elif have shasum; then
        check="shasum -a 256 -c -"
    else
        say "note: no sha256 tool available, skipping verification"
        return 0
    fi

    # Pull out our own line: the file lists every platform's build. Some
    # sha256sum builds prefix the name with '*' to mean "binary mode", so match
    # on the name itself rather than on the surrounding whitespace.
    entry="$(awk -v want="$2" '{ name = $NF; sub(/^\*/, "", name); if (name == want) print }' "$1/SHA256SUMS")"
    [ -n "$entry" ] || die "$2 is not listed in SHA256SUMS, refusing to install"

    if ! printf '%s\n' "$entry" | (cd "$1" && $check) >/dev/null 2>&1; then
        die "checksum mismatch for $2, refusing to install"
    fi
    say "checksum ok"
}

target="$(detect_target)"
archive="duw-$target.tar.gz"

if [ -n "${DUW_VERSION:-}" ]; then
    base="https://github.com/$REPO/releases/download/$DUW_VERSION"
else
    base="https://github.com/$REPO/releases/latest/download"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $archive"
fetch "$base/$archive" "$tmp/$archive" ||
    die "could not download $base/$archive"

fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" 2>/dev/null || true
verify "$tmp" "$archive"

tar -xzf "$tmp/$archive" -C "$tmp"
binary="$tmp/duw-$target/duw"
[ -f "$binary" ] || die "the archive did not contain the expected binary"

mkdir -p "$INSTALL_DIR" || die "cannot create $INSTALL_DIR"
chmod +x "$binary"
mv "$binary" "$INSTALL_DIR/duw" || die "cannot write to $INSTALL_DIR"

# curl does not set the quarantine flag, but be defensive about how the file
# arrived on macOS.
if [ "$(uname -s)" = "Darwin" ]; then
    xattr -d com.apple.quarantine "$INSTALL_DIR/duw" 2>/dev/null || true
fi

version="$("$INSTALL_DIR/duw" --version 2>/dev/null || echo duw)"
say "installed $version to $INSTALL_DIR/duw"

case ":$PATH:" in
    *":$INSTALL_DIR:"*) say "run: duw" ;;
    *)
        say ""
        say "$INSTALL_DIR is not on your PATH. Add it with:"
        say "  export PATH=\"$INSTALL_DIR:\$PATH\""
        ;;
esac
