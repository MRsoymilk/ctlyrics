#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
    printf 'Usage: %s BINARY OUTPUT_DIR\n' "$0" >&2
    exit 2
fi

BINARY=$1
OUTPUT_DIR=$2
PROJECT_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
VERSION=$(awk -F '"' '/^version = / { print $2; exit }' "$PROJECT_ROOT/Cargo.toml")
PACKAGE_VERSION="${VERSION}-1"
PACKAGE_ROOT=$(mktemp -d)
trap 'rm -rf "$PACKAGE_ROOT"' EXIT HUP INT TERM

install -d \
    "$PACKAGE_ROOT/DEBIAN" \
    "$PACKAGE_ROOT/usr/bin" \
    "$PACKAGE_ROOT/usr/share/applications" \
    "$PACKAGE_ROOT/usr/share/doc/ctlyrics" \
    "$PACKAGE_ROOT/usr/share/icons/hicolor/512x512/apps" \
    "$PACKAGE_ROOT/usr/share/man/man1" \
    "$OUTPUT_DIR"

install -m755 "$BINARY" "$PACKAGE_ROOT/usr/bin/ctlyrics"
install -m755 "$PROJECT_ROOT/tools/get_songs_from_directory.py" \
    "$PACKAGE_ROOT/usr/bin/ctlyrics-get-songs"
install -m755 "$PROJECT_ROOT/tools/get_lyrics.py" \
    "$PACKAGE_ROOT/usr/bin/ctlyrics-get-lyrics"
install -m755 "$PROJECT_ROOT/tools/auto_map.py" \
    "$PACKAGE_ROOT/usr/bin/ctlyrics-auto-map"
install -m644 "$PROJECT_ROOT/package/ctlyrics.desktop" \
    "$PACKAGE_ROOT/usr/share/applications/ctlyrics.desktop"
install -m644 "$PROJECT_ROOT/res/logo_icon.png" \
    "$PACKAGE_ROOT/usr/share/icons/hicolor/512x512/apps/ctlyrics.png"
install -m644 "$PROJECT_ROOT/LICENSE" "$PACKAGE_ROOT/usr/share/doc/ctlyrics/copyright"
gzip -9n -c "$PROJECT_ROOT/package/deb/ctlyrics.1" \
    >"$PACKAGE_ROOT/usr/share/man/man1/ctlyrics.1.gz"

INSTALLED_SIZE=$(du -sk "$PACKAGE_ROOT/usr" | cut -f1)
sed \
    -e "s/@VERSION@/$PACKAGE_VERSION/g" \
    -e "s/@INSTALLED_SIZE@/$INSTALLED_SIZE/g" \
    "$PROJECT_ROOT/package/deb/control" >"$PACKAGE_ROOT/DEBIAN/control"

OUTPUT="$OUTPUT_DIR/ctlyrics_${PACKAGE_VERSION}_amd64.deb"
dpkg-deb --root-owner-group --build "$PACKAGE_ROOT" "$OUTPUT"
printf 'Created %s\n' "$OUTPUT"
