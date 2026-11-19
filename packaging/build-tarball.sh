#!/usr/bin/env bash
# Build doki-<version>-<arch>-unknown-linux-gnu.tar.gz from a release binary.
# Used by .github/workflows/release.yml and runnable locally.
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
[[ -n "$arch" ]] || die "--arch is required"
[[ -f "$binary" ]] || die "binary not found: $binary"

name="doki-${version}-${arch}-unknown-linux-gnu"
work="$(mktemp -d "${TMPDIR:-/tmp}/dokidoki-tarball.XXXXXX")"
trap 'rm -rf "$work"' EXIT
stage="$work/$name"

install -Dm755 "$binary" "$stage/doki"
install -Dm644 "$ROOT/LICENSE" "$stage/LICENSE"
install -Dm644 "$ROOT/README.md" "$stage/README.md"
install -Dm644 "$ROOT/assets/fonts/inter/OFL.txt" "$stage/assets/fonts/inter/OFL.txt"
install -Dm644 "$ROOT/assets/fonts/jetbrains-mono/OFL.txt" "$stage/assets/fonts/jetbrains-mono/OFL.txt"
install -Dm644 "$ROOT/packaging/dokidoki.desktop" "$stage/dokidoki.desktop"
install -Dm644 "$ROOT/assets/icons/dokidoki-256.png" "$stage/dokidoki.png"

mkdir -p "$outdir"
out="$outdir/$name.tar.gz"
rm -f "$out"

epoch="${SOURCE_DATE_EPOCH:-$(git -C "$ROOT" log -1 --format=%ct 2>/dev/null || date +%s)}"
note "packing $(basename "$out")"
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$epoch" \
  -C "$work" -cf - "$name" | gzip -9n > "$out"

note "built $out ($(du -h "$out" | cut -f1))"
