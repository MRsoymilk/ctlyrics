use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ctlyrics::cmus::CmusInfo;
use ctlyrics::i18n::Locale;
use ctlyrics::player::Player;
use ratatui::{Terminal, backend::TestBackend};
use std::thread;
use std::time::Duration;

fn press(player: &mut Player, code: KeyCode) -> bool {
    player.handle_input(KeyEvent::new(code, KeyModifiers::NONE))
}

fn type_command(player: &mut Player, command: &str) {
    press(player, KeyCode::Char(':'));
    for character in command.chars() {
        press(player, KeyCode::Char(character));
    }
    press(player, KeyCode::Enter);
}

fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn colon_and_escape_leave_command_mode() {
    for cancel in [KeyCode::Char(':'), KeyCode::Esc] {
        let mut player = Player::new(Locale::En);
        press(&mut player, KeyCode::Char(':'));
        for character in "web".chars() {
            press(&mut player, KeyCode::Char(character));
        }
        press(&mut player, cancel);
        assert!(!player.command_mode);
        assert!(player.command_buffer.is_empty());
    }
}

#[test]
fn control_c_requests_a_clean_exit_in_any_mode() {
    for command_mode in [false, true] {
        let mut player = Player::new(Locale::En);
        player.command_mode = command_mode;
        assert!(player.handle_input(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL,)));
    }
}

#[test]
fn help_tree_opens_from_keys_and_command() {
    for key in [KeyCode::Char('h'), KeyCode::Char('?')] {
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        let mut player = Player::new(Locale::En);
        press(&mut player, key);
        terminal
            .draw(|frame| player.draw(frame, &CmusInfo::default(), &[]))
            .unwrap();
        assert!(buffer_text(&terminal).contains("ctlyrics Help"));

        press(&mut player, KeyCode::Esc);
        terminal
            .draw(|frame| player.draw(frame, &CmusInfo::default(), &[]))
            .unwrap();
        assert!(!buffer_text(&terminal).contains("ctlyrics Help"));
    }

    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    let mut player = Player::new(Locale::En);
    type_command(&mut player, "help");
    terminal
        .draw(|frame| player.draw(frame, &CmusInfo::default(), &[]))
        .unwrap();
    assert!(buffer_text(&terminal).contains("ctlyrics Help"));
    assert!(!player.command_mode);
}

#[test]
fn help_tree_is_localized_and_scrollable() {
    let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
    let mut player = Player::new(Locale::ZhCn);
    press(&mut player, KeyCode::Char('h'));

    terminal
        .draw(|frame| player.draw(frame, &CmusInfo::default(), &[]))
        .unwrap();
    let top = buffer_text(&terminal);
    assert!(top.contains("ctlyrics"));
    assert!(top.contains('帮'));
    assert!(top.contains('全'));

    press(&mut player, KeyCode::End);
    terminal
        .draw(|frame| player.draw(frame, &CmusInfo::default(), &[]))
        .unwrap();
    let bottom = buffer_text(&terminal);
    assert!(bottom.contains('鼠'));
    assert!(bottom.contains('播'));
}

#[test]
fn player_controls_are_rendered_on_the_bottom_row() {
    let mut terminal = Terminal::new(TestBackend::new(100, 10)).unwrap();
    let mut player = Player::new(Locale::En);
    let info = CmusInfo {
        title: "Song".to_string(),
        artist: "Artist".to_string(),
        duration: 180,
        position: 60,
        status: "playing".to_string(),
        ..CmusInfo::default()
    };

    terminal
        .draw(|frame| player.draw(frame, &info, &[]))
        .unwrap();

    let bottom_row = terminal.backend().buffer().content()[900..1000]
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(bottom_row.contains("Song - Artist"));
    assert!(bottom_row.contains("01:00 / 03:00"));
    assert!(bottom_row.contains("⏮︎"));
    assert!(bottom_row.contains("⏸︎"));
    assert!(bottom_row.contains("⏭︎"));
}

#[test]
fn progress_bar_reflects_the_playback_position() {
    let mut terminal = Terminal::new(TestBackend::new(100, 10)).unwrap();
    let mut player = Player::new(Locale::En);
    let info = CmusInfo {
        duration: 200,
        position: 100,
        ..CmusInfo::default()
    };

    terminal
        .draw(|frame| player.draw(frame, &info, &[]))
        .unwrap();

    let progress = &terminal.backend().buffer().content()[934..968];
    assert_eq!(
        progress.iter().filter(|cell| cell.symbol() == "━").count(),
        17
    );
    assert_eq!(
        progress.iter().filter(|cell| cell.symbol() == "─").count(),
        17
    );
}

#[test]
fn arrow_keys_adjust_the_lyric_offset() {
    let mut player = Player::new(Locale::En);
    press(&mut player, KeyCode::Left);
    assert!((player.offset + 0.1).abs() < f64::EPSILON);
    press(&mut player, KeyCode::Right);
    assert!(player.offset.abs() < f64::EPSILON);
    press(&mut player, KeyCode::Up);
    assert!((player.offset + 0.5).abs() < f64::EPSILON);
    press(&mut player, KeyCode::Down);
    assert!(player.offset.abs() < f64::EPSILON);
}

#[test]
fn command_message_expires_back_to_player_bar() {
    let mut terminal = Terminal::new(TestBackend::new(100, 10)).unwrap();
    let mut player = Player::new(Locale::En);
    type_command(&mut player, "love");
    assert!(!player.message.is_empty());

    thread::sleep(Duration::from_millis(1100));
    terminal
        .draw(|frame| player.draw(frame, &CmusInfo::default(), &[]))
        .unwrap();
    assert!(player.message.is_empty());
    assert!(buffer_text(&terminal).contains("00:00 / 00:00"));
}
