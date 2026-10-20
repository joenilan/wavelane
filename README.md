# Wavelane

See where every application's sound goes, and drag it onto any output.

Wavelane opens a map over the screen (tray icon, a system-wide shortcut, or
`wavelane --map`): every application that plays sound is a puck orbiting the
output it plays on. Drag one onto another output and it plays there from now
on, new streams included; drop the *Everything* orb on an output to send all
sound there. Scroll on an app for its volume, middle-click to mute; every move
can be undone. A device with several outputs (a GoXLR, a RØDECaster, BEACN
Studio channels through LinX) shows as a hub with one port per output.

Linux (PipeWire) first; Windows later.

## Build

Needs Rust 1.99+, a C compiler and PipeWire development headers
(`pipewire-devel` on Fedora, `libpipewire-0.3-dev` on Debian).

    cargo build --release
    ./target/release/wavelane          # opens the map (Wavelane stays in the tray)
    ./target/release/wavelane --dump   # what it sees, without the interface

## How it routes

Like a desktop volume control: an application's stream is pointed at an
output through the session manager's `target.object` metadata, and the
session manager moves it. Wavelane remembers each placement
(`~/.config/wavelane/placements`) and applies it to that application's new
streams. Live levels come from passive captures that never keep a device
awake and never store audio.
