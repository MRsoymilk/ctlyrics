import curses
import os
import time
import re
import subprocess

exit_flag = False

def parse_lrc(file_path):
    lyrics = []
    with open(file_path, 'r', encoding='utf-8') as f:
        for line in f:
            matches = re.findall(r'\[(\d+):(\d+\.\d+)\](.*)', line)
            for match in matches:
                minutes = int(match[0])
                seconds = float(match[1])
                timestamp = minutes * 60 + seconds
                lyrics.append((timestamp, match[2]))
    lyrics.sort()
    return lyrics

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

def find_lrc_file(dir, title, artist=None):
    if not title:
        return None
    title_pattern = re.compile(re.escape(title), re.IGNORECASE)
    files = os.listdir(dir)
    matching_files = [file for file in files if title_pattern.search(file) and file.endswith('.lrc')]
    if len(matching_files) == 0:
        return None
    if len(matching_files) == 1:
        return os.path.join(dir, matching_files[0])
    if artist:
        artist_pattern = re.compile(re.escape(artist), re.IGNORECASE)
        for file in matching_files:
            if artist_pattern.search(file):
                return os.path.join(dir, file)
    return os.path.join(dir, matching_files[0]) if matching_files else None

def handle_input(stdscr):
    key = stdscr.getch()
    if key == ord('q'):
        return 'quit'
    elif key == curses.KEY_LEFT:
        return 'left'
    elif key == curses.KEY_RIGHT:
        return 'right'
    elif key == curses.KEY_UP:
        return 'up'
    elif key == curses.KEY_DOWN:
        return 'down'
    return None

class LyricsCache:
    def __init__(self):
        self.last_title = None
        self.last_lyrics = []
        self.last_total_lines = 0

    def load_lyrics_and_info(self, info):
        current_title = info.get('title')
        artist = info.get('artist')
        if current_title == self.last_title:
            return self.last_lyrics, self.last_total_lines
        lyrics_path = find_lrc_file('lyrics', current_title, artist)
        if lyrics_path:
            self.last_lyrics = parse_lrc(lyrics_path)
            self.last_total_lines = len(self.last_lyrics)
        else:
            self.last_lyrics = []
            self.last_total_lines = 0
        self.last_title = current_title
        return self.last_lyrics, self.last_total_lines

def display_lyrics(stdscr, info, lyrics, total_lines, offset):
    current_line = 0
    adjusted_position = info['position'] + offset
    for i, (timestamp, _) in enumerate(lyrics):
        if adjusted_position >= timestamp:
            current_line = i
        else:
            break
    height, width = stdscr.getmaxyx()
    if width <= 0 or height <= 0:
        return
    start_line = max(0, current_line - height // 2)
    end_line = min(total_lines, start_line + height)
    pad = curses.newpad(total_lines + 1, width)
    song_info = f"{info['title']} - {info['artist']} [{info['status']}] (Offset: {offset:.1f}s)"
    pad.addstr(0, 0, song_info, curses.A_BOLD)
    for i in range(end_line - start_line):
        display_line = start_line + i
        if display_line >= total_lines:
            break
        text = lyrics[display_line][1]
        try:
            if display_line == current_line:
                pad.addstr(i + 1, 0, text, curses.color_pair(1) | curses.A_BOLD)
            else:
                pad.addstr(i + 1, 0, text)
        except curses.error:
            print(f"Failed to display line {i + 1}: {text}")
    pad.refresh(0, 0, 0, 0, height - 1, width - 1)

def main(stdscr):
    curses.curs_set(0)
    stdscr.nodelay(True)
    stdscr.timeout(100)
    curses.start_color()
    curses.init_pair(1, curses.COLOR_GREEN, curses.COLOR_BLACK)
    lyrics_cache = LyricsCache()
    offset = 0.0
    while True:
        user_input = handle_input(stdscr)
        if user_input == 'quit':
            break
        elif user_input == 'left':
            offset -= 0.1
        elif user_input == 'right':
            offset += 0.1
        elif user_input == 'up':
            offset -= 0.5
        elif user_input == 'down':
            offset += 0.5
        info = get_cmus_info()
        lyrics, total_lines = lyrics_cache.load_lyrics_and_info(info)
        display_lyrics(stdscr, info, lyrics, total_lines, offset)
        time.sleep(0.5)

if __name__ == "__main__":
    curses.wrapper(main)
