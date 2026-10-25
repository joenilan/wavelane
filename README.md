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

## Two DEV builds

Run `python3 scripts/build_dev_backends.py` to build separately named optimized
**Wavelane DEV Default** and **Wavelane DEV Fast** executables in
`target/dev-review/`, with a hash/feature manifest in `backend-builds.json`.
They suggest Ctrl+Alt+W and identify the backend in their window/tray titles
and `--version`. Nothing is installed or launched. Quit the current DEV app
from its tray menu before switching executables; closing its windows leaves
it running, and a second launch would forward to that existing DEV instance.

Agents testing other GPUI apps should read
[the shared backend handoff](docs/GPUI_BACKEND_HANDOFF.md). It covers Kit 0.8.0's
built-in engine selection, paired builds, custom platform patches, and the
limits of Wavelane's measurements. Each app needs its own benchmark decision.

## Private backend measurements

GPUI Kit is pinned to **0.8.0**. The default backend remains `gpui-pre` 0.3.9.
`--features gpui-fast` selects the experimental Fast 0.1.10 backend and its
vendored Wayland activation/blur patches; it does not change release defaults.
The upstream [Kit feature setup](https://github.com/longbridge/gpui-kit#gpui-fast)
and [Fast rendering notes](https://github.com/longbridge/gpui-fast/blob/main/docs/retained-mode.md)
describe CPU work saved, which does not establish GPU or power savings.

The optional `perf-harness` feature supplies fixed synthetic scenes (six
outputs, sixteen apps). It never opens PipeWire, registers global shortcuts,
checks for updates, or loads/saves audio placements. A regular build rejects
`--benchmark`. To compare equivalent release builds, copy each executable
before building the next backend:

```sh
cargo build --release --locked --features perf-harness
cp target/release/wavelane local/wavelane-DEV-default
cargo build --release --locked --features perf-harness,gpui-fast
cp target/release/wavelane local/wavelane-DEV-fast
python3 scripts/private_benchmark_session.py python3 scripts/benchmark_backends.py default="$PWD/local/wavelane-DEV-default" fast="$PWD/local/wavelane-DEV-fast"
```

Requires KWin, `dbus-run-session`, Python 3 and `nvidia-smi` for this runner.
It uses a private 1280×800 Wayland compositor, config, cache and bus, with
identical effects and scheduling for both backends. It measures tray only,
idle flyout, idle map, simulated music metering, pointer-driven dragging and
volume scrolling, plus a forced compositor close. Each scene warms up for five
seconds; build order reverses on the second pass. Output is under
`local/gpui-evaluation/measurements/`: raw logs, CPU/RSS/device GPU samples and
per-run JSON including executable hashes. Duplicate build hashes are rejected.
When comparing separate source snapshots, use separate Cargo target directories;
sharing a package target can reuse another snapshot's executable.

CPU percentages use one logical core as 100%. GPUI profiling measures CPU draw
work and, when available, animation presentation intervals, not GPU execution
time. Missing GPU process samples mean unknown/below measurement resolution.
`nvidia-smi` watts/utilization describe the **whole GPU**, including the owner's
other apps and both compositors. Per-process `pmon` samples are filtered past
warm-up and have coarse resolution; graphics process watts are unavailable.
[NVIDIA documents](https://docs.nvidia.com/deploy/nvidia-smi/index.html#process-monitoring)
missing readings as unavailable metrics or errors, so they are never counted
as zero.
These synthetic scenes isolate backend drawing costs, not real audio capture
cost, laptop battery life, or desktop tray/shortcut behavior. Do not infer GPU
savings from the CPU measurements. Native meter teardown can be checked safely
with `python3 scripts/check_meter_lifecycle.py`, which starts its own PipeWire
server and null output and stops only that server.

An optimized hands-on DEV copy uses `cargo build --release --features
dev-shortcut`; its visible title is **Wavelane DEV**, its suggested shortcut is
**Ctrl+Alt+W**, and its app ID/instance lock are separate from release builds.
Releases still suggest **Ctrl+Alt+M**. Nothing in these scripts installs,
publishes, or replaces the installed app.

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
