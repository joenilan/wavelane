#!/usr/bin/env bash
# Publish the packages packaging/release.sh built for the current version:
# the GitHub release v<version> (packages, tarball and SHA256SUMS; the
# in-app updater reads it) and the combined signed dnf/apt/pacman
# repositories (joenilan.github.io/packages, mirrored to apps.zombie.digital).
#   packaging/publish.sh NOTES.md
# The commit and tag v<version> must already be pushed. The repository needs
# to be public for the updater to see the release.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)
OUT="$ROOT/local/release/v$VERSION"
SITE=${JOENILAN_SITE:-$(dirname "$ROOT")/joenilan.github.io}
NOTES=${1:-}

die() { echo "publish: $*" >&2; exit 1; }
[ -n "$NOTES" ] && [ -f "$NOTES" ] || die "usage: packaging/publish.sh NOTES.md"
cd "$ROOT"
(cd "$OUT" && sha256sum -c --quiet SHA256SUMS) || die "packages in $OUT do not match SHA256SUMS"
RPM=$(ls "$OUT"/wavelane-*.x86_64.rpm | head -1)
DEB=$(ls "$OUT"/wavelane_*_amd64.deb | head -1)
ARCH=$(ls "$OUT"/wavelane-*-x86_64.pkg.tar.zst | head -1)
TAR="$OUT/wavelane-$VERSION-x86_64-linux.tar.gz"
[ -f "$RPM" ] && [ -f "$DEB" ] && [ -f "$ARCH" ] && [ -f "$TAR" ] || die "missing packages; run packaging/release.sh"
rpm -K "$RPM" | grep -q "signatures OK" || die "$RPM is not signed"

HEAD=$(git rev-parse HEAD)
REMOTE_TAG=$(git ls-remote origin "refs/tags/v$VERSION^{}" | cut -f1)
[ "$REMOTE_TAG" = "$HEAD" ] || die "tag v$VERSION on origin is not HEAD ($HEAD); push the commit and tag first"

PRERELEASE=()
case "$VERSION" in *-*) PRERELEASE=(--prerelease) ;; esac
gh release create "v$VERSION" --verify-tag --title "Wavelane $VERSION" --notes-file "$NOTES" "${PRERELEASE[@]}" \
  "$RPM" "$DEB" "$ARCH" "$TAR" "$OUT/SHA256SUMS"

"$SITE/scripts/publish-packages.sh" "$RPM" "$DEB" "$ARCH"
