#!/usr/bin/env bash
# Build Doki.app and doki-<version>-<arch>-apple-darwin.zip from a release binary.
# Runnable locally on macOS.
set -euo pipefail

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
    -h|--help) sed -n '2,3p' "${BASH_SOURCE[0]}"; exit 0 ;;
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

command -v uvx >/dev/null || die "uvx not found; install uv to build the dmg"

minos="$(otool -l "$binary" | awk '/minos/{print $2; exit}')"
[[ -n "$minos" ]] || minos="11.0"

work="$(mktemp -d "${TMPDIR:-/tmp}/dokidoki-macos.XXXXXX")"
trap 'rm -rf "$work"' EXIT

app="$outdir/Doki.app"
iconset="$work/AppIcon.iconset"
mkdir -p "$outdir" "$iconset"

for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$ROOT/assets/icons/dokidoki-256.png" \
    --out "$iconset/icon_${size}x${size}.png" >/dev/null
  sips -z "$((size * 2))" "$((size * 2))" "$ROOT/assets/icons/dokidoki-256.png" \
    --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$work/AppIcon.icns"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" \
  "$app/Contents/Resources/fonts/inter" \
  "$app/Contents/Resources/fonts/jetbrains-mono"

install -m 755 "$binary" "$app/Contents/MacOS/doki"
install -m 644 "$work/AppIcon.icns" "$app/Contents/Resources/AppIcon.icns"
install -m 644 "$ROOT/LICENSE" "$app/Contents/Resources/LICENSE"
install -m 644 "$ROOT/assets/fonts/inter/OFL.txt" "$app/Contents/Resources/fonts/inter/OFL.txt"
install -m 644 "$ROOT/assets/fonts/jetbrains-mono/OFL.txt" \
  "$app/Contents/Resources/fonts/jetbrains-mono/OFL.txt"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>dokidoki</string>
  <key>CFBundleDisplayName</key><string>dokidoki</string>
  <key>CFBundleIdentifier</key><string>dev.dokidoki.doki</string>
  <key>CFBundleExecutable</key><string>doki</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>LSMinimumSystemVersion</key><string>$minos</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

plutil -lint "$app/Contents/Info.plist" >/dev/null
codesign --force --deep -s - "$app"

out="$outdir/doki-${version}-${arch}-apple-darwin.zip"
rm -f "$out"

note "packing $(basename "$out")"
ditto -c -k --keepParent --norsrc --noextattr "$app" "$out"

note "built $out ($(du -h "$out" | cut -f1))"

dmg="$outdir/doki-${version}-${arch}-apple-darwin.dmg"
rm -f "$dmg"

note "packing $(basename "$dmg")"
uvx --from dmgbuild dmgbuild -s "$ROOT/packaging/macos/dmg-settings.py" \
  -D app="$app" \
  -D icon="$app/Contents/Resources/AppIcon.icns" \
  -D background="$ROOT/packaging/macos/background.png" \
  "Doki" "$dmg"

note "built $dmg ($(du -h "$dmg" | cut -f1))"
