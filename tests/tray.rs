use ctlyrics::compatible::tray::ExitSignal;

#[test]
fn exit_signal_is_shared_between_tray_and_tui() {
    let tui_signal = ExitSignal::new();
    let tray_signal = tui_signal.clone();

    assert!(!tui_signal.is_requested());
    tray_signal.request();
    assert!(tui_signal.is_requested());
}
