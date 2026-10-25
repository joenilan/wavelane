# Wavelane agent context

GPUI Kit is pinned to 0.8.0 with the default upstream backend. Preserve the
vendored Wayland activation/raising/blur and rounded-frame patches. Optional
GPUI Fast work and synthetic comparison tools belong to the
`experiment/gpui-fast-0.8.0` branch; do not switch release backends based on
CPU benchmarks alone.

Preserve existing routing, undo, device visibility, updater and versioning
work. Use isolated windows/data for tests. Do not change live audio routes,
volumes, placements, installed apps or LinX for benchmarks. Check readiness
before desktop overlays. DEV suggests Ctrl+Alt+W and release Ctrl+Alt+M.
Publication, tagging, releases and installation require explicit owner approval.
The owner authorized publication of the default-backend 0.1.1 update in this
session after automated validation, waiving a separate hands-on test.
