#!/usr/bin/env bash
# Build dokidoki-<version>-<arch>.AppImage from a release binary.
# Used by .github/workflows/release.yml and runnable locally.
#
# Runtime libraries (Vulkan, Wayland/X11, xkbcommon, fontconfig, glibc) are
# deliberately not bundled so the AppImage uses the host's graphics stack.
set -euo pipefail

APPIMAGETOOL_VERSION="1.9.1"

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
note() { printf '==> %s\n' "$*"; }

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binary=""
version=""
arch=""
outdir="$ROOT/dist"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary) binary="${2:?}"; shift 2 ;;
    --version) version="${2:?}"; shift 2 ;;
    --arch) arch="${2:?}"; shift 2 ;;
    --outdir) outdir="${2:?}"; shift 2 ;;
    -h|--help) sed -n '2,4p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

[[ -n "$binary" ]] || die "--binary is required"
[[ -n "$version" ]] || die "--version is required"
[[ -n "$arch" ]] || die "--arch is required (x86_64 or aarch64)"
[[ -f "$binary" ]] || die "binary not found: $binary"

case "$arch" in
  x86_64|aarch64) ;;
  *) die "unsupported arch: $arch" ;;
esac

case "$arch" in
  x86_64)  tool_sha256="ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0" ;;
  aarch64) tool_sha256="f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158" ;;
esac

work="$(mktemp -d "${TMPDIR:-/tmp}/dokidoki-appimage.XXXXXX")"
trap 'rm -rf "$work"' EXIT

cache="${APPIMAGETOOL_CACHE:-${TMPDIR:-/tmp}/dokidoki-appimagetool}"
tool="$cache/appimagetool-$arch.AppImage"
mkdir -p "$cache" "$outdir"

if [[ ! -x "$tool" ]]; then
  note "downloading appimagetool $APPIMAGETOOL_VERSION ($arch)"
  curl -fsSL --retry 3 -o "$tool.part" \
    "https://github.com/AppImage/appimagetool/releases/download/${APPIMAGETOOL_VERSION}/appimagetool-${arch}.AppImage"
  mv "$tool.part" "$tool"
  chmod +x "$tool"
fi

echo "$tool_sha256  $tool" | sha256sum -c --status - || die "appimagetool checksum mismatch: $tool"
chmod +x "$tool"

appdir="$work/dokidoki.AppDir"
install -Dm755 "$binary" "$appdir/usr/bin/doki"
install -Dm644 "$ROOT/packaging/dokidoki.desktop" "$appdir/dokidoki.desktop"
install -Dm644 "$ROOT/packaging/dokidoki.desktop" "$appdir/usr/share/applications/dokidoki.desktop"
install -Dm644 "$ROOT/assets/icons/dokidoki-256.png" "$appdir/dokidoki.png"
install -Dm644 "$ROOT/assets/icons/dokidoki-256.png" "$appdir/usr/share/icons/hicolor/256x256/apps/dokidoki.png"

cat > "$appdir/AppRun" <<'APPRUN'
#!/bin/sh
# No arguments opens the UI, same as running the doki binary directly.
exec "${APPDIR:-$(dirname "$(readlink -f "$0")")}/usr/bin/doki" "$@"
APPRUN
chmod 755 "$appdir/AppRun"

out="$outdir/dokidoki-${version}-${arch}.AppImage"
rm -f "$out"

note "packing $(basename "$out")"
ARCH="$arch" VERSION="$version" \
  "$tool" --appimage-extract-and-run --no-appstream "$appdir" "$out"

chmod +x "$out"
note "built $out ($(du -h "$out" | cut -f1))"
