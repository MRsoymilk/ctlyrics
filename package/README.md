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
./package/dist/ctlyrics-0.2.0-x86_64.AppImage tools get-songs --help
./package/dist/ctlyrics-0.2.0-x86_64.AppImage tools get-lyrics --help
./package/dist/ctlyrics-0.2.0-x86_64.AppImage tools auto-map --help
```

Tool arguments containing relative paths are evaluated from the caller's
current working directory. Runtime defaults use the XDG user directories.

The TUI system tray supports StatusNotifierItem hosts on Wayland and falls
back to XEmbed on X11. Its right-click menu shows playback progress, media
controls, and an action that starts and opens the Web interface. Left-clicking
shows the current lyric in an always-above bubble or system notification. GNOME requires an
AppIndicator/KStatusNotifier shell extension. If no tray host is available,
the TUI continues without an icon.

# Ubuntu 26.04 package

Build the amd64 installation package in Ubuntu 26.04 without running it in the
container:

```bash
./package/build-deb.sh
```

The result is written to `package/dist/ctlyrics_0.2.0-1_amd64.deb`. Install it
on an Ubuntu 26.04 test system with:

```bash
sudo apt install ./package/dist/ctlyrics_0.2.0-1_amd64.deb
```

This binary package is intended for manual testing. Official archive source
packaging will use distribution-provided Rust crates after the remaining crate
dependencies are available in Resolute Backports.
