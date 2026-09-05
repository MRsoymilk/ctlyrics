#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)

if [ -x "$HOME/.cargo/bin/rustup" ]; then
    PATH="$HOME/.cargo/bin:$PATH"
    export PATH
fi

case "$(uname -m)" in
    x86_64)
        ARCH=x86_64
        RUST_TARGET=x86_64-unknown-linux-musl
        ;;
    aarch64|arm64)
        ARCH=aarch64
        RUST_TARGET=aarch64-unknown-linux-musl
        ;;
    *)
        printf 'Unsupported architecture: %s\n' "$(uname -m)" >&2
        exit 1
        ;;
esac

"$SCRIPT_DIR/install-dependencies.sh"

cd "$PROJECT_ROOT"
if command -v rustup >/dev/null 2>&1; then
    cargo +stable build --release --locked --target "$RUST_TARGET"
    BINARY="$PROJECT_ROOT/target/$RUST_TARGET/release/ctlyrics"
else
    printf 'Building for the native target because rustup is unavailable.\n'
    cargo build --release --locked
    BINARY="$PROJECT_ROOT/target/release/ctlyrics"
fi
VERSION_OUTPUT=$($BINARY --version)
VERSION=${VERSION_OUTPUT##* }
APPDIR="$SCRIPT_DIR/build/AppDir"
OUTPUT_DIR="$SCRIPT_DIR/dist"
OUTPUT="$OUTPUT_DIR/ctlyrics-$VERSION-$ARCH.AppImage"
APPIMAGETOOL="$SCRIPT_DIR/tools/appimagetool-$ARCH.AppImage"

rm -rf "$APPDIR"
mkdir -p \
    "$APPDIR/usr/bin" \
    "$APPDIR/usr/lib/ctlyrics/tools" \
    "$APPDIR/usr/share/applications" \
    "$APPDIR/usr/share/icons/hicolor/512x512/apps" \
    "$OUTPUT_DIR"

install -m755 "$BINARY" "$APPDIR/usr/bin/ctlyrics"
install -m755 "$SCRIPT_DIR/AppRun" "$APPDIR/AppRun"
install -m644 "$SCRIPT_DIR/ctlyrics.desktop" "$APPDIR/ctlyrics.desktop"
install -m644 "$SCRIPT_DIR/ctlyrics.desktop" "$APPDIR/usr/share/applications/ctlyrics.desktop"
install -m644 "$PROJECT_ROOT/res/logo_icon.png" "$APPDIR/ctlyrics.png"
install -m644 "$PROJECT_ROOT/res/logo_icon.png" "$APPDIR/usr/share/icons/hicolor/512x512/apps/ctlyrics.png"
install -m755 \
    "$PROJECT_ROOT/tools/get_songs_from_directory.py" \
    "$PROJECT_ROOT/tools/get_lyrics.py" \
    "$PROJECT_ROOT/tools/auto_map.py" \
    "$APPDIR/usr/lib/ctlyrics/tools/"

rm -f "$OUTPUT"
ARCH="$ARCH" "$APPIMAGETOOL" --appimage-extract-and-run "$APPDIR" "$OUTPUT"
chmod +x "$OUTPUT"

"$OUTPUT" --appimage-extract-and-run --version
"$OUTPUT" --appimage-extract-and-run tools get-songs --help >/dev/null
"$OUTPUT" --appimage-extract-and-run tools get-lyrics --help >/dev/null
"$OUTPUT" --appimage-extract-and-run tools auto-map --help >/dev/null

printf 'Created AppImage: %s\n' "$OUTPUT"
