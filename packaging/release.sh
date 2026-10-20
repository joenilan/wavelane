#!/usr/bin/env bash
# Build Wavelane's release packages from the current commit into
# local/release/v<version>/: a signed Fedora RPM (native), a Debian package
# and portable tarball (Debian 13 container), an Arch package (Arch
# container), and SHA256SUMS. Publishing is separate (packaging/publish.sh).
#   packaging/release.sh [--allow-dirty] [--unsigned]
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)
OUT="$ROOT/local/release/v$VERSION"
DIRTY=0
SIGN=1
KEY=${WAVELANE_SIGNING_KEY:-E72D6EE9C417012ECE280124E14B87807621D25D}
for arg in "$@"; do
  case "$arg" in
    --allow-dirty) DIRTY=1 ;;
    --unsigned) SIGN=0 ;;
    *) echo "Unknown argument: $arg" >&2; exit 2 ;;
  esac
done

die() { echo "release: $*" >&2; exit 1; }
cd "$ROOT"
if [ "$DIRTY" = 0 ] && ! git diff --quiet HEAD --; then
  die "uncommitted changes; commit first (a release builds the exact commit)"
fi
if [ "$SIGN" = 1 ]; then
  command -v rpmsign >/dev/null || die "rpmsign is not installed (sudo dnf install rpm-sign), or use --unsigned"
  gpg --list-secret-keys "$KEY" >/dev/null 2>&1 || die "signing key $KEY is not available; use --unsigned"
fi
COMMIT=$(git rev-parse --short HEAD)
rm -rf "$OUT"
mkdir -p "$OUT"
echo "Wavelane $VERSION ($COMMIT) -> $OUT"

# ---- third-party notices -------------------------------------------------
supplemental=()
[ -f local/supplemental-notices.json ] && supplemental=(--supplemental local/supplemental-notices.json)
python3 scripts/collect_notices.py "${supplemental[@]}"
python3 - "$OUT/THIRD_PARTY_NOTICES.txt" <<'EOF'
import json, re, sys
out = sys.argv[1]
data = json.load(open("local/third-party-notices.json"))
text = open("local/THIRD_PARTY_NOTICES.txt").read()
missing = []
for w in data["warnings"]:
    m = re.match(r"No readable license/notice text for (\S+) (\S+)$", w)
    if m:
        missing.append(m.groups())
licenses = {p["name"] + " " + p["version"]: p.get("license") or "" for p in data["packages"]}
if missing:
    text += ("\n\n" + "=" * 78 + "\nPackages whose source archive contains no license file\n" + "=" * 78 +
             "\nFor the packages below, the published crate archive ships no license or notice\n"
             "file. Each is listed with the license expression declared in its Cargo\n"
             "metadata, followed by the standard text of that license from SPDX\n"
             "license-list-data v3.27.0. No copyright holder line was available upstream.\n")
    used = set()
    for name, version in missing:
        expr = licenses.get(f"{name} {version}", "")
        ids = sorted(set(re.findall(r"[A-Za-z0-9.\-]+", expr)) - {"OR", "AND", "WITH"})
        text += f"\n- {name} {version}: {expr}\n"
        used.update(ids)
    for spdx in sorted(used):
        try:
            body = open(f"packaging/licenses/{spdx}.txt").read()
        except FileNotFoundError:
            sys.exit(f"release: no standard text for {spdx} in packaging/licenses")
        text += "\n" + "-" * 78 + f"\n{spdx}\n" + "-" * 78 + "\n" + body
open(out, "w").write(text)
print(f"Notices: {len(data['packages'])} packages, {len(missing)} with declared-license texts")
EOF

# ---- staging (shared layout for every package) -----------------------------
stage() { # stage <binary> <destdir>
  local bin=$1 dest=$2
  install -Dm755 "$bin" "$dest/usr/bin/wavelane"
  install -Dm644 packaging/digital.zombie.wavelane.desktop "$dest/usr/share/applications/digital.zombie.wavelane.desktop"
  install -Dm644 packaging/digital.zombie.wavelane.svg "$dest/usr/share/icons/hicolor/scalable/apps/digital.zombie.wavelane.svg"
  install -Dm644 LICENSE "$dest/usr/share/licenses/wavelane/LICENSE"
  install -Dm644 "$OUT/THIRD_PARTY_NOTICES.txt" "$dest/usr/share/licenses/wavelane/THIRD_PARTY_NOTICES.txt"
  install -Dm644 README.md "$dest/usr/share/doc/wavelane/README.md"
}

# ---- Fedora RPM (native) ---------------------------------------------------
source "$HOME/.cargo/env" 2>/dev/null || true
FEDORA_TARGET="$ROOT/local/release-target/fedora"
# Release builds suggest Ctrl+Alt+M (no dev-shortcut feature).
CARGO_TARGET_DIR="$FEDORA_TARGET" cargo build --release --locked
rm -rf "$OUT/stage-rpm"
stage "$FEDORA_TARGET/release/wavelane" "$OUT/stage-rpm"
# SemVer pre-releases (0.2.0-preview.3) sort before the release as 0.2.0~preview.3.
rpmbuild -bb packaging/wavelane.spec \
  --define "_topdir $OUT/rpmbuild" \
  --define "wavelane_version ${VERSION/-/\~}" \
  --define "wavelane_stage $OUT/stage-rpm" >/dev/null
find "$OUT/rpmbuild/RPMS" -name '*.rpm' -exec cp {} "$OUT/" \;
rm -rf "$OUT/rpmbuild" "$OUT/stage-rpm"
if [ "$SIGN" = 1 ]; then
  rpmsign --addsign --define "_gpg_name $KEY" "$OUT"/*.rpm >/dev/null
fi

# ---- Debian package and portable tarball (Debian 13 container) -------------
podman run --rm --security-opt label=disable \
  -v "$ROOT":/src:ro \
  -v "$OUT":/out \
  -v wavelane-debian-cargo:/root/.cargo \
  -v wavelane-debian-rustup:/root/.rustup \
  -v wavelane-debian-target:/target \
  docker.io/library/debian:13 \
  bash /src/packaging/build-debian.sh "$VERSION"

# ---- Arch Linux package (repackages the Debian-built tree) ------------------
podman run --rm --security-opt label=disable \
  -v "$ROOT":/src:ro \
  -v "$OUT":/out \
  docker.io/library/archlinux:latest \
  bash /src/packaging/build-arch.sh "$VERSION"

(cd "$OUT" && sha256sum -- *.rpm *.deb *.pkg.tar.zst *.tar.gz > SHA256SUMS)
echo
cat "$OUT/SHA256SUMS"
