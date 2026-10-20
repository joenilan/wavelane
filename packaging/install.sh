#!/bin/sh
# Install Wavelane from the portable tarball.
#   ./install.sh            for this user (~/.local); updates itself from there
#   ./install.sh --system   for all users (/usr/local), needs sudo
set -eu
cd "$(dirname "$0")"
if [ "${1:-}" = "--system" ]; then
  PREFIX=/usr/local
  SUDO=sudo
else
  PREFIX=$HOME/.local
  SUDO=
fi
$SUDO install -Dm755 usr/bin/wavelane "$PREFIX/bin/wavelane"
$SUDO install -Dm644 usr/share/applications/digital.zombie.wavelane.desktop \
  "$PREFIX/share/applications/digital.zombie.wavelane.desktop"
$SUDO install -Dm644 usr/share/icons/hicolor/scalable/apps/digital.zombie.wavelane.svg \
  "$PREFIX/share/icons/hicolor/scalable/apps/digital.zombie.wavelane.svg"
$SUDO install -Dm644 usr/share/doc/wavelane/README.md "$PREFIX/share/doc/wavelane/README.md"
update-desktop-database "$PREFIX/share/applications" >/dev/null 2>&1 || true
echo "Wavelane installed to $PREFIX/bin/wavelane. Start it from your application menu."
case ":$PATH:" in *":$PREFIX/bin:"*) ;; *) echo "Note: $PREFIX/bin is not on your PATH." ;; esac
