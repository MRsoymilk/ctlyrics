#!/usr/bin/env python3
"""Generate a ctlyrics-compatible song list from a local music directory."""

from __future__ import annotations

import argparse
import os
import sys
from dataclasses import dataclass
from pathlib import Path


AUDIO_EXTENSIONS = {".mp3", ".flac", ".wav", ".m4a", ".ogg", ".ape"}


@dataclass(frozen=True)
class Song:
    title: str
    artist: str = ""

    def line(self) -> str:
        return f"{self.title} - {self.artist}" if self.artist else self.title


def parse_song(path: Path) -> Song | None:
    if path.suffix.casefold() not in AUDIO_EXTENSIONS:
        return None

    title, separator, artist = path.stem.partition("-")
    title = title.strip()
    artist = artist.strip() if separator else ""
    return Song(title, artist) if title else None


def scan_music_directory(directory: Path, recursive: bool = True) -> list[Song]:
    songs: list[Song] = []

    def report_error(error: OSError) -> None:
        print(f"Warning: cannot access {error.filename}: {error.strerror}", file=sys.stderr)

    if recursive:
        paths = (
            Path(root) / filename
            for root, _, filenames in os.walk(directory, onerror=report_error)
            for filename in filenames
        )
    else:
        try:
            paths = [entry for entry in directory.iterdir() if entry.is_file()]
        except OSError as error:
            raise RuntimeError(f"cannot read {directory}: {error}") from error

    for path in paths:
        song = parse_song(path)
        if song is not None:
            songs.append(song)

    songs.sort(key=lambda song: (song.title.casefold(), song.artist.casefold()))
    return songs


def write_song_list(songs: list[Song], output: Path) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_name(f".{output.name}.tmp")
    content = "".join(f"{song.line()}\n" for song in songs)
    temporary.write_text(content, encoding="utf-8")
    temporary.replace(output)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Scan a music directory and generate a title/artist list."
    )
    parser.add_argument("music_directory", type=Path, help="directory containing music files")
    parser.add_argument(
        "-o",
        "--output",
        type=Path,
        default=Path("songs_list.txt"),
        help="output file (default: songs_list.txt)",
    )
    parser.add_argument(
        "--no-recursive", action="store_true", help="scan only the specified directory"
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    directory = args.music_directory.expanduser()
    if not directory.is_dir():
        print(f"Error: music directory does not exist: {directory}", file=sys.stderr)
        return 2

    try:
        songs = scan_music_directory(directory, not args.no_recursive)
        write_song_list(songs, args.output.expanduser())
    except (OSError, RuntimeError) as error:
        print(f"Error: {error}", file=sys.stderr)
        return 1

    print(f"Wrote {len(songs)} songs to {args.output.expanduser()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
