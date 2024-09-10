import curses

class Player:
    def __init__(self, stdscr, log):
        self.log = log
        self.cmd = ''
        self.command_mode = False
        self.msg = ''
        self.offset = 0
        self.stdscr = stdscr
        self.stdscr.nodelay(True)
        self.height, self.width = stdscr.getmaxyx()
        curses.curs_set(False)
        curses.start_color()
        curses.init_pair(1, curses.COLOR_GREEN, curses.COLOR_BLACK)

    def handle_input(self, input_key):
        if input_key > -1:
            curses.napms(10)
            if self.command_mode:
                if input_key in [10, 13]:
                    self.command_mode = False
                elif input_key in [127, curses.KEY_BACKSPACE]:
                    if len(self.cmd) > 0:
                        self.cmd = self.cmd[:-1]
                else:
                    self.cmd += chr(input_key)
            else:
                if input_key == ord(':'):
                    self.command_mode = True
                    self.log.info("Enter command mode")
                elif input_key == ord('q'):
                    self.log.info("Quit application")
                    return 1
                elif input_key == curses.KEY_LEFT:
                    self.offset -= 0.1
                elif input_key == curses.KEY_RIGHT:
                    self.offset += 0.1
                elif input_key == curses.KEY_UP:
                    self.offset -= 0.5
                elif input_key == curses.KEY_DOWN:
                    self.offset += 0.5
        else:
            curses.napms(100)

        if not self.command_mode and len(self.cmd) > 0:
            if self.cmd == 'love':
                self.log.info("to my beloved Can'.")
            else:
                self.log.info(f"cmd: {self.cmd}")
                self.msg = 'exec: ' + self.cmd
            self.cmd = ''
        self.stdscr.move(self.height - 1, 0)
        self.stdscr.clrtoeol()
        if self.command_mode:
            self.stdscr.addstr(f":{self.cmd}")
        else:
            self.stdscr.addstr(self.height - 1, 0, self.msg)
        return 0

    def get_termianl_size(self):
        if curses.is_term_resized(self.height, self.width):
            self.height, self.width = self.stdscr.getmaxyx()
            curses.resizeterm(self.height, self.width)
            self.stdscr.clear()
            self.stdscr.refresh()

    def format_time(self, seconds):
        minutes = seconds // 60
        seconds = seconds % 60
        return f"{minutes:02}:{seconds:02}"

    def display_progress_bar(self, position, duration):
        if duration == 0:
            return
        time_str = f"{self.format_time(position)} / {self.format_time(duration)}"
        available_width = self.width - len(time_str) - 1  # 预留时间字符串空间
        progress = int((position / duration) * available_width)
        bar = '=' * progress + '-' * (available_width - progress)
        self.stdscr.move(1, 0)
        self.stdscr.addstr(f"{bar} {time_str}")

    def display_lyrics(self, lyrics, position):
        current_line = 0
        adjusted_position = position + self.offset
        for i, (timestamp, _) in enumerate(lyrics):
            if adjusted_position >= timestamp:
                current_line = i
            else:
                break
        if self.width <= 0 or self.height <= 0:
            return
        start_line = max(0, current_line - self.height // 2)
        total_lines = len(lyrics)
        end_line = min(total_lines, start_line + self.height)
        pad = curses.newpad(total_lines + 1, self.width)
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
                self.log.info(f"Failed to display line {i + 1}: {text}")
        pad.refresh(0, 0, 2, 0, self.height - 2, self.width - 1)

    def display_song_info(self, title, artist, status):
        self.stdscr.move(0, 0)
        song_info = f"{title} - {artist} [{status}] (Offset: {self.offset:.1f}s)"
        self.stdscr.clrtoeol()
        self.stdscr.addstr(song_info)
