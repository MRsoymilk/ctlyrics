#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
OUTPUT_DIR="$SCRIPT_DIR/dist"
VERSION=$(awk -F '"' '/^version = / { print $2; exit }' "$PROJECT_ROOT/Cargo.toml")
PACKAGE_FILE="ctlyrics_${VERSION}-1_amd64.deb"
IMAGE="ctlyrics-deb-build:${VERSION}"

case "$(uname -m)" in
    x86_64)
        ;;
    *)
        printf 'The Ubuntu 26.04 package currently supports amd64 hosts only.\n' >&2
        exit 1
        ;;
esac

if ! command -v docker >/dev/null 2>&1; then
    printf 'Missing build dependency: docker\n' >&2
    exit 1
fi

mkdir -p "$OUTPUT_DIR"
docker build \
    --file "$SCRIPT_DIR/deb/Dockerfile" \
    --target build \
    --tag "$IMAGE" \
    "$PROJECT_ROOT"

CONTAINER=$(docker create "$IMAGE")
trap 'docker rm "$CONTAINER" >/dev/null' EXIT HUP INT TERM
docker cp "$CONTAINER:/out/$PACKAGE_FILE" "$OUTPUT_DIR/$PACKAGE_FILE"
docker rm "$CONTAINER" >/dev/null
trap - EXIT HUP INT TERM

printf 'Created Ubuntu package: %s/%s\n' "$OUTPUT_DIR" "$PACKAGE_FILE"
