# Wavelane agent context

Read `docs/GPUI_BACKEND_HANDOFF.md` before GPUI Kit upgrades, GPUI Fast work,
backend comparisons, or building the two DEV variants. Kit 0.8.0 supports both
engines through a compile-time Cargo feature. Keep the default backend unless
workload-specific GPU/power evidence and owner-reviewed functionality justify
a change. Preserve existing changes and the vendored platform patches.

The owner's global working preferences still apply. Keep benchmarks isolated;
do not touch live audio routes, volumes, placements, installed apps or LinX.
Do not open overlays over the owner's desktop without checking readiness.
DEV suggests Ctrl+Alt+W; release suggests Ctrl+Alt+M. No publication, tagging,
release or installation is authorized before owner testing and approval.

The owner authorized publishing the default-backend 0.1.1 update after automated
validation in this session, waiving a separate hands-on test. Fast remains
an experiment; this approval does not authorize changing release backends.
