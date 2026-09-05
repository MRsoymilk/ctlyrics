#!/usr/bin/env python3
"""Automatically map local music files to LRC files in mappings.json."""

from __future__ import annotations

import argparse
import json
import os
import secrets
import shutil
import sys
import unicodedata
from dataclasses import dataclass
from difflib import SequenceMatcher
from pathlib import Path
from typing import TextIO


AUDIO_EXTENSIONS = {".mp3", ".flac", ".wav", ".m4a", ".ogg", ".ape"}


def write_line(message: str = "", stream: TextIO = sys.stdout) -> None:
    newline = "\r\n" if stream.isatty() else "\n"
    stream.write(message + newline)
    stream.flush()


@dataclass(frozen=True)
class NamedFile:
    path: Path
    title: str
    artist: str


def split_name(stem: str) -> tuple[str, str]:
    title, separator, artist = stem.partition("-")
    return title.strip(), artist.strip() if separator else ""


def normalize(value: str) -> str:
    value = unicodedata.normalize("NFKC", value).casefold()
    return "".join(character for character in value if character.isalnum())


def scan_music(directory: Path) -> list[NamedFile]:
    music: list[NamedFile] = []
    for root, _, filenames in os.walk(directory):
        for filename in filenames:
            path = Path(root) / filename
            if path.suffix.casefold() not in AUDIO_EXTENSIONS:
                continue
            title, artist = split_name(path.stem)
            if title:
                music.append(NamedFile(path, title, artist))
    music.sort(key=lambda item: (item.title.casefold(), item.artist.casefold()))
    return music


def scan_lyrics(directory: Path) -> list[NamedFile]:
    lyrics: list[NamedFile] = []
    for path in directory.iterdir():
        if not path.is_file() or path.suffix.casefold() != ".lrc":
            continue
        title, artist = split_name(path.stem)
        if title:
            lyrics.append(NamedFile(path, title, artist))
    lyrics.sort(key=lambda item: item.path.name.casefold())
    return lyrics


def match_score(music: NamedFile, lyric: NamedFile) -> float:
    music_stem = normalize(music.path.stem)
    lyric_stem = normalize(lyric.path.stem)
    if music_stem and music_stem == lyric_stem:
        return 1.0

    title_score = SequenceMatcher(None, normalize(music.title), normalize(lyric.title)).ratio()
    if not music.artist:
        return title_score
    artist_score = SequenceMatcher(
        None, normalize(music.artist), normalize(lyric.artist)
    ).ratio()
    return title_score * 0.75 + artist_score * 0.25


def find_match(
    music: NamedFile,
    lyrics: list[NamedFile],
    minimum_score: float,
    minimum_gap: float,
) -> tuple[NamedFile | None, float]:
    best: NamedFile | None = None
    best_score = second_score = -1.0
    for lyric in lyrics:
        score = match_score(music, lyric)
        if score > best_score:
            second_score = best_score
            best_score = score
            best = lyric
        elif score > second_score:
            second_score = score

    if best is None or best_score < minimum_score:
        return None, max(best_score, 0.0)
    if second_score >= 0 and best_score - second_score < minimum_gap:
        return None, best_score
    return best, best_score


def load_config(path: Path) -> dict[str, object]:
    if not path.exists():
        return {"mappings": {}, "music_dir": None}
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or not isinstance(data.get("mappings", {}), dict):
        raise ValueError("configuration must contain a mappings object")
    data.setdefault("mappings", {})
    data.setdefault("music_dir", None)
    return data


def save_config(path: Path, config: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        shutil.copy2(path, path.with_suffix(path.suffix + ".bak"))
    temporary = path.with_name(f".{path.name}.tmp")
    temporary.write_text(
        json.dumps(config, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    temporary.replace(path)


def copy_lyric(source: Path, destination: Path, overwrite: bool, dry_run: bool) -> str:
    if source.resolve() == destination.resolve():
        return "same"
    if destination.exists() and not overwrite:
        return "exists"
    if dry_run:
        return "preview"

    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_name(f".{destination.name}.tmp")
    shutil.copy2(source, temporary)
    temporary.replace(destination)
    return "copied"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Automatically map music files to LRC files for ctlyrics."
    )
    parser.add_argument("--lyrics-dir", type=Path, required=True, help="source LRC directory")
    parser.add_argument(
        "--config-dir",
        type=Path,
        required=True,
        help="runtime config directory; LRC files are copied beside it into lyrics/",
    )
    parser.add_argument(
        "--music-dir",
        type=Path,
        help="override music_dir from mappings.json and save the new value",
    )
    parser.add_argument("--overwrite", action="store_true", help="replace existing mappings")
    parser.add_argument("--dry-run", action="store_true", help="show changes without writing")
    parser.add_argument("--min-score", type=float, default=0.88)
    parser.add_argument("--min-gap", type=float, default=0.08)
    args = parser.parse_args()
    if not 0 <= args.min_score <= 1 or not 0 <= args.min_gap <= 1:
        parser.error("min-score and min-gap must be between 0 and 1")
    return args


def main() -> int:
    args = parse_args()
    lyrics_dir = args.lyrics_dir.expanduser().resolve()
    config_dir = args.config_dir.expanduser().resolve()
    config_path = config_dir / "mappings.json"
    target_lyrics_dir = config_dir.parent / "lyrics"

    if not lyrics_dir.is_dir():
        write_line(f"Error: lyrics directory does not exist: {lyrics_dir}", sys.stderr)
        return 2

    try:
        config = load_config(config_path)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        write_line(f"Error: cannot read {config_path}: {error}", sys.stderr)
        return 2

    configured_music_dir = args.music_dir or config.get("music_dir")
    if not isinstance(configured_music_dir, (str, os.PathLike)) or not configured_music_dir:
        write_line(
            "Error: music_dir is not configured; provide --music-dir.", sys.stderr
        )
        return 2
    music_dir = Path(configured_music_dir).expanduser()
    if not music_dir.is_dir():
        write_line(f"Error: music directory does not exist: {music_dir}", sys.stderr)
        return 2

    if args.music_dir:
        config["music_dir"] = str(music_dir)
    removed_lyrics_dir = config.pop("lyrics_dir", None) is not None

    try:
        music_files = scan_music(music_dir)
        lyric_files = scan_lyrics(lyrics_dir)
    except OSError as error:
        write_line(f"Error: cannot scan files: {error}", sys.stderr)
        return 1

    mappings = config["mappings"]
    existing_by_path = {
        value.get("music_path"): (mapping_id, value)
        for mapping_id, value in mappings.items()
        if isinstance(value, dict)
    }
    added = updated = preserved = unmatched = copied = copy_skipped = 0

    for music in music_files:
        music_path = str(music.path)
        existing = existing_by_path.get(music_path)
        if existing and not args.overwrite:
            preserved += 1
            existing_filename = existing[1].get("lrc_filename")
            if isinstance(existing_filename, str):
                source = lyrics_dir / Path(existing_filename).name
                destination = target_lyrics_dir / Path(existing_filename).name
                if destination.exists():
                    copy_skipped += 1
                elif source.is_file():
                    try:
                        result = copy_lyric(source, destination, False, args.dry_run)
                    except OSError as error:
                        unmatched += 1
                        write_line(f"Copy failed: {source} -> {destination}: {error}")
                    else:
                        if result in {"copied", "preview"}:
                            copied += 1
                        write_line(f"Copy: {source.name} -> {destination}")
            continue

        lyric, score = find_match(music, lyric_files, args.min_score, args.min_gap)
        if lyric is None:
            unmatched += 1
            write_line(f"No confident match [{score:.2f}]: {music.path.name}")
            continue

        destination = target_lyrics_dir / lyric.path.name
        try:
            copy_result = copy_lyric(lyric.path, destination, args.overwrite, args.dry_run)
        except OSError as error:
            unmatched += 1
            write_line(f"Copy failed: {lyric.path} -> {destination}: {error}")
            continue
        if copy_result in {"copied", "preview"}:
            copied += 1
        if copy_result in {"exists", "same"}:
            copy_skipped += 1

        mapping_id = existing[0] if existing else secrets.token_hex(16)
        mappings[mapping_id] = {
            "id": mapping_id,
            "title": music.title,
            "artist": music.artist,
            "music_path": music_path,
            "lrc_filename": lyric.path.name,
        }
        if existing:
            updated += 1
            action = "Update"
        else:
            added += 1
            action = "Add"
        write_line(f"{action} [{score:.2f}]: {music.path.name} -> {lyric.path.name}")

    config_changed = bool(added or updated or args.music_dir or removed_lyrics_dir)
    if not args.dry_run and config_changed:
        try:
            save_config(config_path, config)
        except OSError as error:
            write_line(f"Error: cannot save {config_path}: {error}", sys.stderr)
            return 1

    mode = "Dry run" if args.dry_run else "Finished"
    write_line(
        f"{mode}: {added} added, {updated} updated, {preserved} preserved, "
        f"{unmatched} unmatched, {copied} lyrics copied, {copy_skipped} already present."
    )
    if not args.dry_run and config_changed:
        write_line(f"Config: {config_path}")
        if config_path.with_suffix(config_path.suffix + ".bak").exists():
            write_line(f"Backup: {config_path}.bak")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
