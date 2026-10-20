#!/usr/bin/env bash
# Runs inside the Debian 13 container started by packaging/release.sh:
# /src = repository (read-only), /out = release directory, /target = Cargo
# target cache. Builds the Debian package and the portable tarball.
set -euo pipefail
VERSION=$1
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
  ca-certificates curl build-essential pkg-config clang cmake dpkg-dev \
  libpipewire-0.3-dev libxkbcommon-dev libxkbcommon-x11-dev \
  libwayland-dev libvulkan-dev libfontconfig-dev libfreetype-dev libxcb1-dev \
  libx11-dev libx11-xcb-dev libdbus-1-dev libglib2.0-dev libzstd-dev >/dev/null

TOOLCHAIN=$(sed -n 's/^channel = "\(.*\)"/\1/p' /src/rust-toolchain.toml)
if ! [ -x /root/.cargo/bin/rustup ]; then
  curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal --default-toolchain "$TOOLCHAIN"
fi
source /root/.cargo/env
rustup toolchain install --profile minimal "$TOOLCHAIN" 2>&1 | grep -v -E "^(info|  )" || true

cd /src
CARGO_TARGET_DIR=/target cargo build --release --locked
BIN=/target/release/wavelane

stage() {
  local dest=$1
  install -Dm755 "$BIN" "$dest/usr/bin/wavelane"
  install -Dm644 packaging/digital.zombie.wavelane.desktop "$dest/usr/share/applications/digital.zombie.wavelane.desktop"
  install -Dm644 packaging/digital.zombie.wavelane.svg "$dest/usr/share/icons/hicolor/scalable/apps/digital.zombie.wavelane.svg"
  install -Dm644 LICENSE "$dest/usr/share/doc/wavelane/copyright"
  install -Dm644 /out/THIRD_PARTY_NOTICES.txt "$dest/usr/share/doc/wavelane/THIRD_PARTY_NOTICES.txt"
  install -Dm644 README.md "$dest/usr/share/doc/wavelane/README.md"
}

# ---- Debian package ----
# SemVer pre-releases (0.2.0-preview.3) sort before the release as 0.2.0~preview.3.
DEB_VERSION=${VERSION/-/\~}
WORK=$(mktemp -d)
PKG=$WORK/wavelane_${DEB_VERSION}-1_amd64
stage "$PKG"
mkdir -p "$WORK/debian" "$PKG/DEBIAN"
printf 'Source: wavelane\n\nPackage: wavelane\nArchitecture: amd64\n' > "$WORK/debian/control"
DEPENDS=$(cd "$WORK" && dpkg-shlibdeps -O -e "$PKG/usr/bin/wavelane" 2>/dev/null | sed -n 's/^shlibs:Depends=//p')
cat > "$PKG/DEBIAN/control" <<EOF
Package: wavelane
Version: ${DEB_VERSION}-1
Architecture: amd64
Maintainer: DreadedZombie <joenilan+dev@gmail.com>
Depends: ${DEPENDS}, libvulkan1, libwayland-client0, libwayland-egl1, pipewire (>= 1.0), wireplumber
Recommends: mesa-vulkan-drivers
Section: sound
Priority: optional
Homepage: https://github.com/joenilan/wavelane
Description: See where every app's sound goes, and drag it onto any output
 Wavelane lives in the tray and shows every application that plays sound on
 a map over the screen: drag one onto any output (speakers, headphones, a
 GoXLR or Rode channel, a virtual device) and it plays there from now on.
 The tray flyout sets each application's volume and output.
EOF
dpkg-deb --root-owner-group --build "$PKG" /out/ >/dev/null

# ---- portable tarball ----
TAR=wavelane-${VERSION}-x86_64-linux
mkdir -p "$WORK/$TAR"
stage "$WORK/$TAR/root"
mv "$WORK/$TAR/root"/* "$WORK/$TAR/" && rmdir "$WORK/$TAR/root"
install -m755 packaging/install.sh packaging/uninstall.sh "$WORK/$TAR/"
tar -C "$WORK" -czf "/out/$TAR.tar.gz" "$TAR"
chown "$(stat -c %u:%g /out)" /out/*.deb /out/*.tar.gz
echo "Debian 13 build: glibc $(ldd --version | head -1 | awk '{print $NF}'), Depends: $DEPENDS"
