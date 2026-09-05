#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
TOOLS_DIR="$SCRIPT_DIR/tools"

if [ -x "$HOME/.cargo/bin/rustup" ]; then
    PATH="$HOME/.cargo/bin:$PATH"
    export PATH
fi

case "$(uname -m)" in
    x86_64)
        ARCH=x86_64
        RUST_TARGET=x86_64-unknown-linux-musl
        SHA256=a6d71e2b6cd66f8e8d16c37ad164658985e0cf5fcaa950c90a482890cb9d13e0
        ;;
    aarch64|arm64)
        ARCH=aarch64
        RUST_TARGET=aarch64-unknown-linux-musl
        SHA256=1b00524ba8c6b678dc15ef88a5c25ec24def36cdfc7e3abb32ddcd068e8007fe
        ;;
    *)
        printf 'Unsupported architecture: %s\n' "$(uname -m)" >&2
        exit 1
        ;;
esac

for dependency in cargo curl sha256sum; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        printf 'Missing build dependency: %s\n' "$dependency" >&2
        exit 1
    fi
done

if command -v rustup >/dev/null 2>&1; then
    rustup toolchain install stable --profile minimal
    rustup target add --toolchain stable "$RUST_TARGET"
else
    printf 'rustup is not available; the build will use the native Rust target.\n'
fi

mkdir -p "$TOOLS_DIR"
APPIMAGETOOL="$TOOLS_DIR/appimagetool-$ARCH.AppImage"
DOWNLOAD_URL="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage"

if [ -f "$APPIMAGETOOL" ] && printf '%s  %s\n' "$SHA256" "$APPIMAGETOOL" | sha256sum -c - >/dev/null 2>&1; then
    chmod +x "$APPIMAGETOOL"
    printf 'Dependency is ready: %s\n' "$APPIMAGETOOL"
    exit 0
fi

TEMPORARY="$APPIMAGETOOL.download"
trap 'rm -f "$TEMPORARY"' EXIT HUP INT TERM
printf 'Downloading %s\n' "$DOWNLOAD_URL"
curl --fail --location --retry 3 --output "$TEMPORARY" "$DOWNLOAD_URL"
printf '%s  %s\n' "$SHA256" "$TEMPORARY" | sha256sum -c -
mv "$TEMPORARY" "$APPIMAGETOOL"
chmod +x "$APPIMAGETOOL"
trap - EXIT HUP INT TERM

printf 'Installed dependency: %s\n' "$APPIMAGETOOL"
