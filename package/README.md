# AppImage packaging

Install the local packaging dependencies and build the AppImage:

```bash
./package/install-dependencies.sh
./package/build-appimage.sh
```

The installer downloads `appimagetool` into `package/tools/`. When `rustup` is
available it installs the stable toolchain and the matching Rust musl target,
then builds explicitly with `cargo +stable`. Otherwise, the build script uses
the native Rust target. Generated files are stored in `package/build/` and
`package/dist/`.

The AppImage uses the host installation of `cmus`, `cmus-remote`, and
`python3`. The bundled Python tools can be invoked through the AppImage:

```bash
./package/dist/ctlyrics-0.1.2-x86_64.AppImage tools get-songs --help
./package/dist/ctlyrics-0.1.2-x86_64.AppImage tools get-lyrics --help
./package/dist/ctlyrics-0.1.2-x86_64.AppImage tools auto-map --help
```

Tool arguments and relative paths are evaluated from the caller's current
working directory.

The TUI system tray supports StatusNotifierItem hosts on Wayland and falls
back to XEmbed on X11. Its right-click menu scrolls the current song, artist,
and playback status on one line. GNOME requires an
AppIndicator/KStatusNotifier shell extension. If no tray host is available,
the TUI continues without an icon.
