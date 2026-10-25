# Wavelane

See where every application's sound goes, and drag it onto any output.

Wavelane opens a map over the screen (tray icon, a system-wide shortcut, or
`wavelane --map`): every application that plays sound is a puck orbiting the
output it plays on. Drag one onto another output and it plays there from now
on, new streams included; drop the *Everything* orb on an output to send all
sound there. Scroll on an app for its volume, middle-click to mute; every move
can be undone. A device with several outputs (a GoXLR, a RØDECaster, BEACN
Studio channels through LinX) shows as a hub with one port per output.

Clicking the tray icon opens a small flyout: the default output, and each
application's volume, mute and output. The map's shortcut is suggested as
**Ctrl+Alt+M** (change it in your desktop's shortcut settings). On weak
graphics the map switches itself to light drawing: no blur or glows, and it
redraws only when something changes.

Linux (PipeWire) first; Windows later.

## Install

Packages for Fedora (rpm), Debian/Ubuntu (deb) and Arch, and a portable
tarball, come with each [release](https://github.com/joenilan/wavelane/releases);
the signed `joenilan apps` repositories at
<https://joenilan.github.io/packages/> carry them too. Wavelane checks for
updates (tray menu → *Check for updates*) and installs them from the tray
flyout.

## Build

Needs Rust 1.99+, a C compiler and PipeWire development headers
(`pipewire-devel` on Fedora, `libpipewire-0.3-dev` on Debian).

    cargo build --release
    cargo build --release --features dev-shortcut   # suggests Ctrl+Alt+W, next to LinX's Ctrl+Alt+M
    ./target/release/wavelane          # opens the map (Wavelane stays in the tray)
    ./target/release/wavelane --dump   # what it sees, without the interface

## Rendering and backend

Wavelane uses GPUI Kit 0.8.0 with its default upstream GPUI backend. The map
stops redrawing when idle, and live metering stops when its window closes,
including compositor-driven closes. Music keeps the existing animation policy;
dragging and sliders redraw when input or visible animation changes.

GPUI Kit also supports Fast as a compile-time backend. Wavelane's optional
Fast backend and comparison tools remain on the
[`experiment/gpui-fast-0.8.0` branch](https://github.com/joenilan/wavelane/tree/experiment/gpui-fast-0.8.0).
Fast is not selected in release packages. Each application needs its own
GPU/power measurements before choosing a backend.

DEV builds are visibly marked and suggest Ctrl+Alt+W with a separate app ID
and instance lock. Release builds retain Ctrl+Alt+M. Closing the map/flyout
leaves Wavelane running in the tray; use its tray menu to quit.

To verify native meter teardown without touching desktop audio, run
`python3 scripts/check_meter_lifecycle.py`. It creates its own PipeWire server
and null sink and stops only that test server.

## How it routes

Like a desktop volume control: an application's stream is pointed at an
output through the session manager's `target.object` metadata, and the
session manager moves it. Wavelane remembers each placement
(`~/.config/wavelane/placements`) and applies it to that application's new
streams. Live levels come from passive captures that never keep a device
awake and never store audio.

## Releases

`packaging/release.sh` builds the signed rpm, the deb and tarball (Debian 13
container) and the Arch package (Arch container) from the current commit into
`local/release/v<version>/`; `packaging/publish.sh NOTES.md` publishes them
(GitHub release and the package repositories). Versions are SemVer; a
pre-release `0.2.0-preview.3` is `0.2.0~preview.3` in rpm and deb.

## License

GPL-3.0-only. Third-party notices ship with every package.
