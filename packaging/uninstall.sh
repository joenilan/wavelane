#!/bin/sh
# Remove a tarball installation of Wavelane (same option as install.sh).
set -eu
if [ "${1:-}" = "--system" ]; then
  PREFIX=/usr/local
  SUDO=sudo
else
  PREFIX=$HOME/.local
  SUDO=
fi
$SUDO rm -f "$PREFIX/bin/wavelane" \
  "$PREFIX/share/applications/digital.zombie.wavelane.desktop" \
  "$PREFIX/share/icons/hicolor/scalable/apps/digital.zombie.wavelane.svg" \
  "$PREFIX/share/doc/wavelane/README.md"
$SUDO rmdir "$PREFIX/share/doc/wavelane" 2>/dev/null || true
rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/digital.zombie.wavelane.desktop"
echo "Wavelane removed. Placements in ~/.config/wavelane were kept."
