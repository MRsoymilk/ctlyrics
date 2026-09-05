use ctlyrics::{
    compatible::tray::{ExitSignal, marquee_text, playback_line},
    i18n::Locale,
};

#[test]
fn exit_signal_is_shared_between_tray_and_tui() {
    let tui_signal = ExitSignal::new();
    let tray_signal = tui_signal.clone();

    assert!(!tui_signal.is_requested());
    tray_signal.request();
    assert!(tui_signal.is_requested());
}

#[test]
fn marquee_scrolls_long_unicode_text_in_one_line() {
    assert_eq!(marquee_text("short", 10, 3), "short");
    assert_eq!(marquee_text("歌曲名称ABC", 8, 0), "歌曲名称");
    assert_eq!(marquee_text("歌曲名称ABC", 8, 1), "曲名称AB");
}

#[test]
fn playback_line_contains_song_artist_and_localized_status() {
    assert_eq!(
        playback_line(Locale::ZhCn, "晚风", "伍佰", "playing"),
        "晚风 - 伍佰 · 播放中"
    );
    assert_eq!(
        playback_line(Locale::En, "Song", "Artist", "paused"),
        "Song - Artist · paused"
    );
}

#[test]
fn playback_line_handles_missing_metadata_and_menu_characters() {
    assert_eq!(
        playback_line(Locale::En, "  A_song\nname ", "", "other"),
        "A_song name · unknown"
    );
    assert_eq!(
        playback_line(Locale::ZhCn, "", "", "stopped"),
        "未知歌曲 · 已停止"
    );
}
