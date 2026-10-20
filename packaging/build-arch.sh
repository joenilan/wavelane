#!/usr/bin/env bash
# Runs inside the Arch Linux container started by packaging/release.sh:
# /out = release directory holding the portable tarball (built on Debian 13,
# so its binary runs on Arch's newer glibc). Packages that tree with makepkg.
set -euo pipefail
VERSION=$1
pacman -Sy --noconfirm --needed base-devel >/dev/null 2>&1
useradd -m builder
WORK=/home/builder/wavelane
mkdir -p "$WORK/src-tree"
tar -C "$WORK/src-tree" -xzf "/out/wavelane-${VERSION}-x86_64-linux.tar.gz" --strip-components=1 "wavelane-${VERSION}-x86_64-linux/usr"
# Arch keeps licenses under /usr/share/licenses.
install -Dm644 /src/LICENSE "$WORK/src-tree/usr/share/licenses/wavelane/LICENSE"
# pkgver allows no '-': a SemVer pre-release 0.2.0-preview.3 is 0.2.0preview.3.
cat > "$WORK/PKGBUILD" <<EOF
pkgname=wavelane
pkgver=${VERSION//-/}
pkgrel=1
pkgdesc="See where every app's sound goes, and drag it onto any output"
arch=(x86_64)
url='https://github.com/joenilan/wavelane'
license=(GPL-3.0-only)
depends=(glibc gcc-libs libx11 libxcb libxkbcommon libxkbcommon-x11 'pipewire>=1.0' wireplumber vulkan-icd-loader wayland)
optdepends=('vulkan-driver: GPU rendering (for example vulkan-radeon, vulkan-intel or nvidia-utils)')
options=(!strip !debug)
package() {
  cp -a "\$startdir/src-tree/usr" "\$pkgdir/"
}
EOF
chown -R builder "$WORK"
su builder -c "cd $WORK && PKGDEST=$WORK makepkg --nodeps --noconfirm >/dev/null"
cp "$WORK"/wavelane-*.pkg.tar.zst /out/
chown "$(stat -c %u:%g /out)" /out/*.pkg.tar.zst
echo "Arch package: $(basename "$WORK"/wavelane-*.pkg.tar.zst)"
