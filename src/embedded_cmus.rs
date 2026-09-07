use nix::libc;
use nix::pty::{Winsize, openpty};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use ratatui::{
    Frame,
    style::{Color, Modifier, Style},
};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub struct EmbeddedCmus {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Option<File>,
    child: Option<Child>,
    reader: Option<JoinHandle<()>>,
    reader_stop: Arc<AtomicBool>,
}

impl EmbeddedCmus {
    pub fn spawn(cols: u16, rows: u16) -> io::Result<Self> {
        Self::spawn_command("cmus", &[], cols, rows)
    }

    fn spawn_command(program: &str, args: &[&str], cols: u16, rows: u16) -> io::Result<Self> {
        let cols = cols.max(2);
        let rows = rows.max(2);
        let winsize = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = openpty(Some(&winsize), None).map_err(io::Error::from)?;
        let master = File::from(pty.master);
        set_nonblocking(&master)?;
        let mut reader_file = master.try_clone()?;
        let slave = File::from(pty.slave);
        let stdin = slave.try_clone()?;
        let stdout = slave.try_clone()?;

        let mut command = Command::new(program);
        command
            .args(args)
            .env("TERM", "xterm-256color")
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(slave));

        // The child must own a session and controlling terminal for ncurses.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let child = command.spawn()?;
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let reader_parser = Arc::clone(&parser);
        let reader_stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&reader_stop);
        let reader = thread::spawn(move || {
            let mut bytes = [0_u8; 8192];
            while !thread_stop.load(Ordering::Relaxed) {
                match reader_file.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(count) => {
                        if let Ok(mut parser) = reader_parser.lock() {
                            parser.process(&bytes[..count]);
                        } else {
                            break;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            parser,
            writer: Some(master),
            child: Some(child),
            reader: Some(reader),
            reader_stop,
        })
    }

    pub fn draw(&self, frame: &mut Frame) {
        let Ok(parser) = self.parser.lock() else {
            return;
        };
        let screen = parser.screen();
        let area = frame.area();
        let rows = area.height.min(screen.size().0);
        let cols = area.width.min(screen.size().1);

        for row in 0..rows {
            for col in 0..cols {
                let Some(source) = screen.cell(row, col) else {
                    continue;
                };
                let Some(target) = frame
                    .buffer_mut()
                    .cell_mut((area.x.saturating_add(col), area.y.saturating_add(row)))
                else {
                    continue;
                };
                target.reset();
                if source.has_contents() {
                    target.set_symbol(&source.contents());
                }
                target.set_style(cell_style(source));
            }
        }

        if !screen.hide_cursor() {
            let (row, col) = screen.cursor_position();
            if row < area.height && col < area.width {
                frame.set_cursor_position((area.x + col, area.y + row));
            }
        }
    }

    pub fn send_key(&mut self, key: KeyEvent) -> io::Result<()> {
        let application_cursor = self
            .parser
            .lock()
            .map(|parser| parser.screen().application_cursor())
            .unwrap_or(false);
        let bytes = encode_key(key, application_cursor);
        if !bytes.is_empty() {
            let writer = self
                .writer
                .as_mut()
                .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "cmus PTY is closed"))?;
            write_nonblocking(writer, &bytes)?;
        }
        Ok(())
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        let cols = cols.max(2);
        let rows = rows.max(2);
        if let Ok(mut parser) = self.parser.lock() {
            parser.set_size(rows, cols);
        }
        let Some(writer) = self.writer.as_ref() else {
            return Ok(());
        };
        let winsize = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let result = unsafe { libc::ioctl(writer.as_raw_fd(), libc::TIOCSWINSZ, &winsize) };
        if result == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn is_running(&mut self) -> io::Result<bool> {
        match self.child.as_mut() {
            Some(child) => Ok(child.try_wait()?.is_none()),
            None => Ok(false),
        }
    }

    pub fn shutdown(&mut self) {
        if self.child.is_none() {
            return;
        }

        if let Some(writer) = self.writer.as_mut() {
            let _ = write_nonblocking(writer, b"\x1b:quit\r");
        }

        if let Some(mut child) = self.child.take() {
            let process_group = Pid::from_raw(child.id() as i32);
            let exited = wait_for_exit(&mut child, Duration::from_secs(2));
            let _ = killpg(process_group, Signal::SIGTERM);
            if !exited {
                let _ = wait_for_exit(&mut child, Duration::from_secs(1));
            }
            wait_for_process_group_exit(process_group, Duration::from_secs(1));
            let _ = killpg(process_group, Signal::SIGKILL);
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        self.reader_stop.store(true, Ordering::Relaxed);
        self.writer.take();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn set_nonblocking(file: &File) -> io::Result<()> {
    let descriptor = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn write_nonblocking(writer: &mut File, bytes: &[u8]) -> io::Result<()> {
    loop {
        match writer.write(bytes) {
            Ok(count) if count == bytes.len() => return writer.flush(),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "partial write to cmus PTY",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    false
}

fn wait_for_process_group_exit(process_group: Pid, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let result = unsafe { libc::kill(-process_group.as_raw(), 0) };
        if result == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

impl Drop for EmbeddedCmus {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn cell_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default()
        .fg(color(cell.fgcolor()))
        .bg(color(cell.bgcolor()));
    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

fn color(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(index) => Color::Indexed(index),
        vt100::Color::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
    }
}

pub fn encode_key(key: KeyEvent, application_cursor: bool) -> Vec<u8> {
    let mut bytes = Vec::new();

    match key.code {
        KeyCode::Char(character) if key.modifiers.contains(KeyModifiers::CONTROL) => {
            prepend_alt(&mut bytes, key.modifiers);
            if let Some(control) = control_byte(character) {
                bytes.push(control);
            }
        }
        KeyCode::Char(character) => {
            prepend_alt(&mut bytes, key.modifiers);
            let mut encoded = [0_u8; 4];
            bytes.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
        }
        KeyCode::Enter => {
            prepend_alt(&mut bytes, key.modifiers);
            bytes.push(b'\r');
        }
        KeyCode::Tab => {
            prepend_alt(&mut bytes, key.modifiers);
            bytes.push(b'\t');
        }
        KeyCode::BackTab => bytes.extend_from_slice(b"\x1b[Z"),
        KeyCode::Backspace => {
            prepend_alt(&mut bytes, key.modifiers);
            bytes.push(0x7f);
        }
        KeyCode::Esc => {
            prepend_alt(&mut bytes, key.modifiers);
            bytes.push(0x1b);
        }
        KeyCode::Left => bytes.extend(cursor_sequence(application_cursor, 'D', key.modifiers)),
        KeyCode::Right => bytes.extend(cursor_sequence(application_cursor, 'C', key.modifiers)),
        KeyCode::Up => bytes.extend(cursor_sequence(application_cursor, 'A', key.modifiers)),
        KeyCode::Down => bytes.extend(cursor_sequence(application_cursor, 'B', key.modifiers)),
        KeyCode::Home => bytes.extend(cursor_sequence(application_cursor, 'H', key.modifiers)),
        KeyCode::End => bytes.extend(cursor_sequence(application_cursor, 'F', key.modifiers)),
        KeyCode::PageUp => bytes.extend(tilde_sequence(5, key.modifiers)),
        KeyCode::PageDown => bytes.extend(tilde_sequence(6, key.modifiers)),
        KeyCode::Insert => bytes.extend(tilde_sequence(2, key.modifiers)),
        KeyCode::Delete => bytes.extend(tilde_sequence(3, key.modifiers)),
        KeyCode::F(number) => bytes.extend(function_key(number, key.modifiers)),
        KeyCode::Null => bytes.push(0),
        _ => {}
    }
    bytes
}

fn prepend_alt(bytes: &mut Vec<u8>, modifiers: KeyModifiers) {
    if modifiers.contains(KeyModifiers::ALT) {
        bytes.push(0x1b);
    }
}

fn control_byte(character: char) -> Option<u8> {
    match character {
        'a'..='z' => Some(character as u8 - b'a' + 1),
        'A'..='Z' => Some(character as u8 - b'A' + 1),
        ' ' | '@' => Some(0),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    }
}

fn modifier_parameter(modifiers: KeyModifiers) -> u8 {
    1 + u8::from(modifiers.contains(KeyModifiers::SHIFT))
        + 2 * u8::from(modifiers.contains(KeyModifiers::ALT))
        + 4 * u8::from(modifiers.contains(KeyModifiers::CONTROL))
}

fn cursor_sequence(application_cursor: bool, direction: char, modifiers: KeyModifiers) -> Vec<u8> {
    let modifier = modifier_parameter(modifiers);
    if modifier > 1 {
        format!("\x1b[1;{modifier}{direction}").into_bytes()
    } else if application_cursor {
        format!("\x1bO{direction}").into_bytes()
    } else {
        format!("\x1b[{direction}").into_bytes()
    }
}

fn tilde_sequence(number: u8, modifiers: KeyModifiers) -> Vec<u8> {
    let modifier = modifier_parameter(modifiers);
    if modifier > 1 {
        format!("\x1b[{number};{modifier}~").into_bytes()
    } else {
        format!("\x1b[{number}~").into_bytes()
    }
}

fn function_key(number: u8, modifiers: KeyModifiers) -> Vec<u8> {
    let modifier = modifier_parameter(modifiers);
    if number <= 4 {
        let suffix = match number {
            1 => 'P',
            2 => 'Q',
            3 => 'R',
            4 => 'S',
            _ => return Vec::new(),
        };
        return if modifier > 1 {
            format!("\x1b[1;{modifier}{suffix}").into_bytes()
        } else {
            format!("\x1bO{suffix}").into_bytes()
        };
    }
    let code = match number {
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        _ => return Vec::new(),
    };
    if modifier > 1 {
        format!("\x1b[{code};{modifier}~").into_bytes()
    } else {
        format!("\x1b[{code}~").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use std::time::Duration;

    #[test]
    fn encodes_text_control_and_cursor_keys() {
        assert_eq!(
            encode_key(
                KeyEvent::new(KeyCode::Char('中'), KeyModifiers::NONE),
                false
            ),
            "中".as_bytes()
        );
        assert_eq!(
            encode_key(
                KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
                false
            ),
            [0x17]
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), true),
            b"\x1bOA"
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL), true),
            b"\x1b[1;5A"
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::F(5), KeyModifiers::SHIFT), false),
            b"\x1b[15;2~"
        );
    }

    #[test]
    fn renders_utf8_wide_cells() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(2, 8, 0)));
        parser.lock().unwrap().process("中文 ok".as_bytes());
        let session = EmbeddedCmus {
            parser,
            writer: None,
            child: None,
            reader: None,
            reader_stop: Arc::new(AtomicBool::new(false)),
        };
        let mut terminal = Terminal::new(TestBackend::new(8, 2)).unwrap();
        terminal.draw(|frame| session.draw(frame)).unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.cell((0, 0)).unwrap().symbol(), "中");
        assert_eq!(buffer.cell((2, 0)).unwrap().symbol(), "文");
        assert_eq!(buffer.cell((4, 0)).unwrap().symbol(), " ");
        assert_eq!(buffer.cell((5, 0)).unwrap().symbol(), "o");
        assert_eq!(buffer.cell((6, 0)).unwrap().symbol(), "k");
    }

    #[test]
    fn pty_child_output_is_parsed_and_child_can_exit() {
        let mut session = EmbeddedCmus::spawn_command(
            "/bin/sh",
            &["-c", r"printf '\033[31m中文\033[0m'; read answer"],
            20,
            2,
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            let contents = session.parser.lock().unwrap().screen().contents();
            if contents.contains("中文") {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let screen = session.parser.lock().unwrap().screen().clone();
        assert!(screen.contents().contains("中文"));
        assert_eq!(screen.cell(0, 0).unwrap().fgcolor(), vt100::Color::Idx(1));
        session
            .send_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        while session.is_running().unwrap() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!session.is_running().unwrap());
    }

    #[test]
    fn shutdown_is_bounded_when_a_descendant_keeps_the_pty_open() {
        let mut session =
            EmbeddedCmus::spawn_command("/bin/sh", &["-c", "sleep 5 & exit 0"], 20, 2).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while session.is_running().unwrap() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }

        let started = Instant::now();
        session.shutdown();
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn pty_size_is_clamped_for_wide_character_rendering() {
        let mut session =
            EmbeddedCmus::spawn_command("/bin/sh", &["-c", "printf '中文中文'; sleep 0.1"], 0, 0)
                .unwrap();
        thread::sleep(Duration::from_millis(50));

        assert_eq!(session.parser.lock().unwrap().screen().size(), (2, 2));
        session.shutdown();
    }
}
