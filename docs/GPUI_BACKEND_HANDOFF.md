# GPUI Kit 0.8.0 backend handoff for agents

Verified 2026-10-25 in Wavelane. The owner requested this context be shared with
agents testing Fast in Wavelane, LinX, Haiwire and Jawjack. This informs existing
work; it does not authorize unrelated changes, installs, releases or live tests.

## Kit supports both engines

GPUI Kit **0.8.0** officially supports its default upstream GPUI snapshot and
GPUI Fast through the compile-time `gpui-fast` feature. These are two builds
of the same application, with the same Kit components and imports. There is
no runtime backend toggle. Kit and Fast versions are independent: Wavelane's
lockfile selects Kit 0.8.0, upstream `gpui-pre` 0.3.9 and Fast 0.1.10. Inspect
each project's own lockfile and current upstream fixes before choosing a revision.

Official source: https://gpui-kit.com/docs/getting-started/#use-gpui-fast

Expose an optional application feature rather than permanently enabling Fast:

```toml
[dependencies]
gpui-kit = "=0.8.0"

[features]
gpui-fast = ["gpui-kit/gpui-fast"]
```

Kit selects the engine, platform and enabled Kit layers together. Keep using
`gpui_kit::*` and Kit component imports. Cargo features are additive, so
upstream packages may still compile in a Fast build. Avoid mixing direct
upstream GPUI types with Kit's Fast types. Normal Kit integration needs no
engine-substitution patches; Wavelane's platform patches are app-specific.
Do not enable Fast unconditionally in dependencies and expect an application's
`--no-default-features` to disable it.

## Build both executables

With the application feature above:

```sh
cargo build --release --locked
# Save this executable as the default build before the next command.
cargo build --release --locked --features gpui-fast
# Save this executable under a separate Fast name.
```

Both commands normally overwrite the same output. Copy each successful build
immediately, or use distinct Cargo target directories. Record actual backend,
source revision, lockfile, features and executable hashes with measurements.

Wavelane's feature additionally selects its patched Linux platform:

```toml
gpui-fast = ["gpui-kit/gpui-fast", "dep:wavelane-fast-linux"]
```

From `/home/DreadedZombie/git/wavelane`, build both ordinary optimized DEV
executables with:

```sh
python3 scripts/build_dev_backends.py
```

Outputs:

- `/home/DreadedZombie/git/wavelane/target/dev-review/wavelane-DEV-default`
- `/home/DreadedZombie/git/wavelane/target/dev-review/wavelane-DEV-fast`
- `/home/DreadedZombie/git/wavelane/target/dev-review/backend-builds.json`

Both use `--release` and `dev-shortcut`; only Fast enables `gpui-fast`. Window
headers, tray titles and `--version` identify the selected DEV backend. These
builds omit the synthetic benchmark harness and are not installed or launched.

**Quit the running DEV app from its tray menu before launching the other.**
Closing its map/flyout leaves it running. Both builds deliberately share the
DEV instance lock: launching the second while the first runs forwards to the
first, without switching engines. Keep real-audio engines and global shortcut
registrations sequential. DEV suggests Ctrl+Alt+W; release suggests Ctrl+Alt+M.
Leave LinX and installed apps untouched.

## Preserve platform behavior and notifications

Audit each application's vendored patches. Wavelane's `gpui-pre-linux` patches
provide activation tokens, raising and standard background blur; `ksni` passes
tray activation tokens. The Linux patches were also ported to
`vendor/gpui-fast-linux`. Direct patch API calls select the platform actually
opening the windows, because the two crates have separate state. Calling the
upstream platform's patch API while Fast owns the windows affects the wrong
backend. The patched Component 0.8.0 source matches the published crate apart
from its documented rounded frame changes.

Preserve existing migration edits, routing, undo, device visibility, updater
and versioning work. Fast's retained renderer needs notification when inputs
outside tracked entities/globals change. Audit interior-mutability state,
timers, meter buffers and other external state in the target app. Consult
current upstream fixes and https://github.com/longbridge/gpui-fast/blob/main/docs/retained-mode.md.

## Wavelane evidence does not decide other apps

Branch `experiment/gpui-fast-0.8.0` completed 48 runs: original and optimized
native code, both backends, six scenes, two passes with reversed build order.
Each run warmed up for five seconds and measured twelve seconds. Private KWin
used a hardware RTX 3080, a 1280x800 map, a 380x657 flyout and scale 1. Synthetic
data used six outputs and sixteen apps, without live audio or desktop overlays.

Native fixes removed idle hub animation and stationary pointer redraws, and
stopped metering on compositor closes and failed window opens. Default idle
map work fell from about 27 draws/second and 10.17% of one core to zero draws
and 0.54%. Both optimized backends stop drawing at idle and stop metering
after forced closes.

Optimized default / Fast results:

| Scene | CPU percent of one core | RSS MiB | Mean run p95 CPU draw ms |
| --- | --- | --- | --- |
| Tray only | 0.46 / 0.46 | 38 / 41 | no steady-state draws |
| Flyout idle | 0.54 / 0.50 | 180 / 196 | no steady-state draws |
| Map idle | 0.54 / 0.54 | 187 / 218 | no steady-state draws |
| Music metering | 10.25 / 8.75 | 193 / 218 | 2.72 / 2.13 |
| Dragging and scrolling | 12.71 / 12.87 | 192 / 246 | 2.94 / 2.36 |

Fast reduced CPU draw time but used more memory. Wavelane kept its default;
Fast remains optional. This does not establish the best engine for Haiwire,
Jawjack, LinX or any other workload. Match scenes, dimensions, effects, input
cadence and frame-cap policies in each application's tests. Do not infer GPU
savings from CPU timings or higher FPS. Sparse/missing process GPU readings
are unknown, never zero. Whole-GPU per-run power averages varied about 96-149 W
with live desktop activity and do not establish app-specific power savings.
Synthetic levels omit native capture CPU cost; no laptop battery measurements
were made. CPU draw timing is not GPU execution timing.

Private tests verified blur protocol requests and native meter teardown. Actual
desktop tray activation/raising and shortcut binding still require owner review.

Historical evidence, whose measured binaries are preserved when DEV labels change:

- `/home/DreadedZombie/git/wavelane/local/gpui-evaluation/summary/comparison.json`
- `/home/DreadedZombie/git/wavelane/local/gpui-evaluation/verified-results.jsonl`
- `/home/DreadedZombie/git/wavelane/local/gpui-evaluation/measured-build-manifest.json`
- `/home/DreadedZombie/git/wavelane/local/gpui-evaluation/final-validation.json`

Use `perf-harness` for synthetic Wavelane benchmark builds, adding `gpui-fast`
only for Fast. `scripts/benchmark_backends.py` rejects identical build hashes.
`scripts/private_benchmark_session.py` creates a private compositor;
`scripts/check_meter_lifecycle.py` creates its own PipeWire server and null sink.
Use separate Cargo targets for separate source snapshots: shared package targets
previously reused a different snapshot's binary; those runs were excluded.

No live routes, volumes, saved placements, installed apps, or other projects
may be changed for Wavelane benchmarks. Ask readiness before desktop overlays.
Owner testing and approval are required before publication, tagging or release.

## Publication update

The owner subsequently waived a separate hands-on test and authorized committing,
pushing and publishing the validated default-backend update as 0.1.1. Main
contains the Kit 0.8.0 upgrade and native fixes. Optional Fast, the two DEV
builds and comparison tooling remain on `experiment/gpui-fast-0.8.0`. Historical
0.1.0 measurement binaries and evidence remain unchanged. This is not approval
to enable Fast in releases or to change other applications.
