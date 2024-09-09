import os
import re


class LyricsCache:
    def __init__(self):
        self.last_title = None
        self.last_lyrics = []

    def load_lyrics(self, title, artist):
        if not title:
            return [], 0

        if title == self.last_title:
            return self.last_lyrics

        lyrics_path = find_lrc_file('lyrics', title, artist)
        if lyrics_path:
            self.last_lyrics = parse_lrc(lyrics_path)
        else:
            self.last_lyrics = []

        self.last_title = title
        return self.last_lyrics


def find_lrc_file(directory, title, artist=None):
    if not title:
        return None

    title_pattern = re.compile(re.escape(title), re.IGNORECASE)

    try:
        files = os.listdir(directory)
    except FileNotFoundError:
        return None

    matching_files = [file for file in files if title_pattern.search(file) and file.endswith('.lrc')]

    if len(matching_files) == 0:
        return None
    if len(matching_files) == 1:
        return os.path.join(directory, matching_files[0])

    if artist:
        artist_pattern = re.compile(re.escape(artist), re.IGNORECASE)
        for file in matching_files:
            if artist_pattern.search(file):
                return os.path.join(directory, file)

    return os.path.join(directory, matching_files[0]) if matching_files else None


def parse_lrc(file_path):
    lyrics = []
    try:
        with open(file_path, 'r', encoding='utf-8') as f:
            for line in f:
                matches = re.findall(r'\[(\d+):(\d+\.\d+)\](.*)', line)
                for match in matches:
                    minutes = int(match[0])
                    seconds = float(match[1])
                    timestamp = minutes * 60 + seconds
                    lyrics.append((timestamp, match[2]))
        lyrics.sort()
    except FileNotFoundError:
        print(f"File not found: {file_path}")
    except Exception as e:
        print(f"Error reading file {file_path}: {e}")

    return lyrics
