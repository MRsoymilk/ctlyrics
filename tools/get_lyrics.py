#!/usr/bin/env python3
"""Download LRC files for a generated song list from sq0527.cn."""

from __future__ import annotations

import argparse
import html.parser
import os
import re
import sys
import time
import unicodedata
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from difflib import SequenceMatcher
from pathlib import Path
from typing import TextIO


DEFAULT_BASE_URL = "https://www.sq0527.cn"
USER_AGENT = "ctlyrics-tools/1.0 (+https://github.com/)"
TIMESTAMP_PATTERN = re.compile(r"\[\d{1,3}:\d{2}(?:[.:]\d{1,3})?\]")


def default_lyrics_dir() -> Path:
    data_home = os.environ.get("XDG_DATA_HOME")
    base = Path(data_home).expanduser() if data_home else Path.home() / ".local" / "share"
    return base / "ctlyrics" / "lyrics"


def write_line(message: str = "", stream: TextIO = sys.stdout) -> None:
    """Write a line correctly even when the parent process disabled ONLCR."""
    output = stream
    newline = "\r\n" if output.isatty() else "\n"
    output.write(message + newline)
    output.flush()


@dataclass(frozen=True)
class Song:
    title: str
    artist: str
    original: str


@dataclass(frozen=True)
class SearchResult:
    text: str
    url: str
    score: float = 0.0


class SearchPageParser(html.parser.HTMLParser):
    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.results: list[tuple[str, str]] = []
        self._href: str | None = None
        self._text: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        if tag != "a":
            return
        href = dict(attrs).get("href")
        if href and re.fullmatch(r"(?:https?://[^/]+)?/music/\d+\.html", href):
            self._href = href
            self._text = []

    def handle_data(self, data: str) -> None:
        if self._href is not None:
            self._text.append(data)

    def handle_endtag(self, tag: str) -> None:
        if tag == "a" and self._href is not None:
            text = " ".join("".join(self._text).split())
            if text:
                self.results.append((text, self._href))
            self._href = None
            self._text = []


class LyricsPageParser(html.parser.HTMLParser):
    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.lyrics: str | None = None
        self._capturing = False
        self._text: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        classes = (dict(attrs).get("class") or "").split()
        if tag == "textarea" and "layui-textarea" in classes and self.lyrics is None:
            self._capturing = True

    def handle_data(self, data: str) -> None:
        if self._capturing:
            self._text.append(data)

    def handle_endtag(self, tag: str) -> None:
        if tag == "textarea" and self._capturing:
            self.lyrics = "".join(self._text).strip()
            self._capturing = False


class LyricsClient:
    def __init__(self, base_url: str, timeout: float, retries: int) -> None:
        self.base_url = base_url.rstrip("/") + "/"
        self.timeout = timeout
        self.retries = retries

    def get_text(self, url: str) -> str:
        request = urllib.request.Request(
            url,
            headers={"User-Agent": USER_AGENT, "Accept": "text/html,application/xhtml+xml"},
        )
        last_error: Exception | None = None
        for attempt in range(self.retries + 1):
            try:
                with urllib.request.urlopen(request, timeout=self.timeout) as response:
                    charset = response.headers.get_content_charset() or "utf-8"
                    return response.read().decode(charset, errors="replace")
            except urllib.error.HTTPError as error:
                last_error = error
                if error.code not in {429, 500, 502, 503, 504}:
                    break
            except (urllib.error.URLError, TimeoutError) as error:
                last_error = error

            if attempt < self.retries:
                time.sleep(min(2**attempt, 4))

        raise RuntimeError(f"request failed for {url}: {last_error}")

    def search(self, song: Song, limit: int) -> list[SearchResult]:
        query = urllib.parse.urlencode({"ac": song.title})
        page = self.get_text(urllib.parse.urljoin(self.base_url, f"search?{query}"))
        parser = SearchPageParser()
        parser.feed(page)

        seen: set[str] = set()
        results: list[SearchResult] = []
        base_host = urllib.parse.urlparse(self.base_url).netloc
        for text, href in parser.results:
            url = urllib.parse.urljoin(self.base_url, href)
            if urllib.parse.urlparse(url).netloc != base_host or url in seen:
                continue
            seen.add(url)
            results.append(SearchResult(text, url, result_score(song, text)))

        results.sort(key=lambda result: result.score, reverse=True)
        return results[:limit]

    def fetch_lyrics(self, url: str) -> str:
        parser = LyricsPageParser()
        parser.feed(self.get_text(url))
        lyrics = parser.lyrics or ""
        if not lyrics or not TIMESTAMP_PATTERN.search(lyrics):
            raise RuntimeError("page does not contain timestamped lyrics")
        return lyrics.rstrip() + "\n"


def normalize(value: str) -> str:
    value = unicodedata.normalize("NFKC", value).casefold()
    return "".join(character for character in value if character.isalnum())


def result_score(song: Song, result_text: str) -> float:
    candidate_artist, separator, candidate_title = result_text.partition(" - ")
    if not separator:
        candidate_title = result_text
        candidate_artist = ""

    title = normalize(song.title)
    artist = normalize(song.artist)
    candidate_title = normalize(candidate_title)
    candidate_artist = normalize(candidate_artist)
    title_score = SequenceMatcher(None, title, candidate_title).ratio()
    artist_score = SequenceMatcher(None, artist, candidate_artist).ratio() if artist else 0.0
    exact_title_bonus = 0.35 if title == candidate_title else 0.0
    exact_artist_bonus = 0.25 if artist and artist == candidate_artist else 0.0
    return title_score + artist_score * 0.45 + exact_title_bonus + exact_artist_bonus


def load_songs(path: Path) -> list[Song]:
    songs: list[Song] = []
    for line in path.read_text(encoding="utf-8-sig").splitlines():
        original = line.strip()
        if not original or original.startswith("#"):
            continue
        title, separator, artist = original.partition(" - ")
        title = title.strip()
        if title:
            songs.append(Song(title, artist.strip() if separator else "", original))
    return songs


def safe_filename(value: str) -> str:
    value = re.sub(r"[\x00-\x1f/\\:*?\"<>|]", "_", value).strip(" .")
    return (value[:180].rstrip(" .") or "untitled") + ".lrc"


def choose_result(results: list[SearchResult], interactive: bool) -> SearchResult | None:
    if not results:
        return None
    if not interactive:
        return results[0]

    for index, result in enumerate(results, 1):
        write_line(f"  {index}. {result.text} [{result.score:.2f}]")
    while True:
        choice = input("Choose a result (Enter to skip): ").strip()
        if not choice:
            return None
        if choice.isdigit() and 1 <= int(choice) <= len(results):
            return results[int(choice) - 1]
        write_line("Invalid choice.")


def write_text_atomic(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp")
    temporary.write_text(content, encoding="utf-8")
    temporary.replace(path)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Download LRC files for a generated song list.")
    parser.add_argument(
        "songs_file", nargs="?", type=Path, default=Path("songs_list.txt")
    )
    parser.add_argument("-o", "--output-dir", type=Path, default=default_lyrics_dir())
    parser.add_argument("--base-url", default=DEFAULT_BASE_URL)
    parser.add_argument("--limit", type=int, default=15, help="maximum search candidates")
    parser.add_argument("--interactive", action="store_true", help="choose each match manually")
    parser.add_argument("--overwrite", action="store_true")
    parser.add_argument("--delay", type=float, default=0.5, help="seconds between songs")
    parser.add_argument("--timeout", type=float, default=15.0)
    parser.add_argument("--retries", type=int, default=2)
    parser.add_argument("--error-log", type=Path, default=Path("error.txt"))
    parser.add_argument("--dry-run", action="store_true", help="search without downloading")
    args = parser.parse_args()
    if args.limit < 1 or args.timeout <= 0 or args.retries < 0 or args.delay < 0:
        parser.error("limit and timeout must be positive; retries and delay cannot be negative")
    return args


def main() -> int:
    args = parse_args()
    try:
        songs = load_songs(args.songs_file.expanduser())
    except OSError as error:
        write_line(f"Error: cannot read song list: {error}", sys.stderr)
        return 2

    client = LyricsClient(args.base_url, args.timeout, args.retries)
    output_dir = args.output_dir.expanduser()
    error_log = args.error_log.expanduser()
    failures: list[str] = []
    downloaded = skipped = 0

    try:
        for index, song in enumerate(songs, 1):
            destination = output_dir / safe_filename(song.original)
            write_line(f"[{index}/{len(songs)}] {song.original}")
            if destination.exists() and not args.overwrite and not args.dry_run:
                write_line(f"  Exists, skipped: {destination}")
                skipped += 1
                continue

            try:
                result = choose_result(client.search(song, args.limit), args.interactive)
                if result is None:
                    raise RuntimeError("no search result selected")
                write_line(f"  Match: {result.text} [{result.score:.2f}]")
                if not args.dry_run:
                    write_text_atomic(destination, client.fetch_lyrics(result.url))
                    write_line(f"  Saved: {destination}")
                    downloaded += 1
            except (RuntimeError, OSError) as error:
                write_line(f"  Failed: {error}", sys.stderr)
                failures.append(f"{song.original}|{error}")

            if index < len(songs) and args.delay:
                time.sleep(args.delay)
    except KeyboardInterrupt:
        write_line(stream=sys.stderr)
        write_line("Interrupted.", sys.stderr)
        return 130

    if failures:
        write_text_atomic(error_log, "\n".join(failures) + "\n")
    elif error_log.exists():
        error_log.unlink()

    write_line(
        f"Finished: {downloaded} downloaded, {skipped} skipped, "
        f"{len(failures)} failed."
    )
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
