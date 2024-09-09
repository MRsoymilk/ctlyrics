import curses
import subprocess
import re

from Logger import Logger
from LyricsCache import LyricsCache
from Player import Player


def get_cmus_info():
    try:
        output = subprocess.check_output(['cmus-remote', '-Q']).decode('utf-8')
        # get position
        position_match = re.search(r'position (\d+)', output)
        position = int(position_match.group(1)) if position_match else 0
        # get duration
        duration_match = re.search(r'duration (\d+)', output)
        duration = int(duration_match.group(1)) if duration_match else 0
        # get status
        status_match = re.search(r'status (\w+)', output)
        status = status_match.group(1) if status_match else 'stopped'
        # get title and artist
        song_match = re.search(r'/([^/]+?)(?:\s*-\s*([^/.]+))?\.', output)
        title = song_match.group(1).strip() if song_match.group(1) else "unknown"
        artist = song_match.group(2).strip() if song_match.group(2) else "unknown"
        return {
            "position": position,
            "duration": duration,
            "status": status,
            "title": title,
            "artist": artist
        }
    except subprocess.CalledProcessError:
        return {
            "position": 0,
            "duration": 0,
            "status": "unknown",
            "title": "unknown",
            "artist": "unknown"
        }

def main(stdscr):
    log = Logger(log_file='ctlyrics.log')
    log.info("ctlyrics started")

    player = Player(stdscr, log)

    lyrics_cache = LyricsCache()

    while True:
        input_key = stdscr.getch()
        status = player.handle_input(input_key)
        if status == 1:
            break
        info = get_cmus_info()
        lyrics = lyrics_cache.load_lyrics(info['title'], info['artist'])
        player.get_termianl_size()
        player.display_song_info(info['title'], info['artist'], info['status'])
        player.display_progress_bar(info['position'], info['duration'])
        player.display_lyrics(lyrics, info['position'])
        stdscr.refresh()

curses.wrapper(main)
