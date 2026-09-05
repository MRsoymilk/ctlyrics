import io
import tempfile
import unittest
from pathlib import Path

from get_lyrics import (
    LyricsPageParser,
    SearchPageParser,
    Song,
    result_score,
    safe_filename,
    write_line,
)
from get_songs_from_directory import scan_music_directory


class SongListTests(unittest.TestCase):
    def test_scans_recursively_and_sorts_supported_audio(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            nested = root / "album"
            nested.mkdir()
            (root / "Second - Artist B.FLAC").touch()
            (nested / "First - Artist A.mp3").touch()
            (nested / "cover.jpg").touch()

            self.assertEqual(
                [song.line() for song in scan_music_directory(root)],
                ["First - Artist A", "Second - Artist B"],
            )


class LyricsTests(unittest.TestCase):
    def test_uses_explicit_carriage_return_in_terminal(self) -> None:
        class TerminalBuffer(io.StringIO):
            def isatty(self) -> bool:
                return True

        output = TerminalBuffer()
        write_line("status", output)
        self.assertEqual(output.getvalue(), "status\r\n")

    def test_parses_nested_search_result_text(self) -> None:
        parser = SearchPageParser()
        parser.feed('<a href="/music/1.html">Artist - <font>Title</font></a>')
        self.assertEqual(parser.results, [("Artist - Title", "/music/1.html")])

    def test_parses_timestamped_lyrics(self) -> None:
        parser = LyricsPageParser()
        parser.feed('<textarea class="layui-textarea">[00:01.00]Line &amp; more</textarea>')
        self.assertEqual(parser.lyrics, "[00:01.00]Line & more")

    def test_matching_artist_improves_result_score(self) -> None:
        song = Song("Title", "Right Artist", "Title - Right Artist")
        right = result_score(song, "Right Artist - Title")
        wrong = result_score(song, "Wrong Artist - Title")
        self.assertGreater(right, wrong)

    def test_sanitizes_lyric_filename(self) -> None:
        self.assertEqual(safe_filename('Title / Artist: "Live"'), "Title _ Artist_ _Live_.lrc")


if __name__ == "__main__":
    unittest.main()
