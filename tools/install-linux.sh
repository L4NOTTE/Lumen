#!/usr/bin/env bash
# Per-user installation from a checked-out source tree. Never runs sudo.
set -euo pipefail
[[ "$(uname -s)" == Linux ]] || { echo 'This installer requires Linux.' >&2; exit 1; }
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
prefix="${LUMEN_INSTALL_PREFIX:-$HOME/.local}"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
check_only=false
case "${1:-}" in
  --check) check_only=true ;;
  '') ;;
  *) echo 'Usage: bash tools/install-linux.sh [--check]' >&2; exit 2 ;;
esac
[[ $# -le 1 ]] || { echo 'Too many arguments.' >&2; exit 2; }
for folder in "$prefix" "$data"; do
  [[ "$folder" == /* && "$folder" != *$'\n'* && "$folder" != *$'\r'* && "$folder" != *$'\t'* && "$folder" != *'='* && "$folder" != *'%'* ]] || {
    echo 'Installation directories must be absolute paths without control characters, = or %.' >&2; exit 2;
  }
done
for tool in cargo rustc pkg-config install mktemp; do
  command -v "$tool" >/dev/null || { echo "Missing $tool. See LINUX.md for dependencies." >&2; exit 1; }
done
pkg-config --exists gtk+-3.0 webkit2gtk-4.1 x11 xrandr xkbcommon wayland-client || {
  echo 'Missing GTK/WebKitGTK development libraries. See LINUX.md; no files installed.' >&2; exit 1;
}
host="$(rustc -vV | sed -n 's/^host: //p')"
[[ "$host" == x86_64-unknown-linux-gnu ]] || { echo "Supported target: Ubuntu/Mint x86_64 GNU, found $host" >&2; exit 1; }
$check_only && { echo 'Linux build dependencies found.'; exit 0; }
cd -- "$root"
echo 'Building Lumen Linux preview (WebKitGTK; Chrome extensions are not yet supported).'
cargo build --release --locked --target "$host"
binary="${CARGO_TARGET_DIR:-$root/target}/$host/release/lumen"
[[ -x "$binary" ]] || { echo "Built binary missing: $binary" >&2; exit 1; }
mkdir -p -- "$prefix/bin" "$data/applications" "$data/icons/hicolor/scalable/apps"
temporary=''
trap '[[ -z "$temporary" ]] || rm -f -- "$temporary"' EXIT
temporary="$(mktemp "$prefix/bin/.lumen.XXXXXX")"
install -m755 -- "$binary" "$temporary"
mv -f -- "$temporary" "$prefix/bin/lumen"
temporary=''
install -m644 -- assets/lumen.svg "$data/icons/hicolor/scalable/apps/lumen.svg"
# Escape both the Desktop Entry string layer and the quoted Exec argument.
escaped=''
executable="$prefix/bin/lumen"
for ((i=0;i<${#executable};i++)); do
  char="${executable:i:1}"
  case "$char" in
    \\) escaped+='\\\\' ;;
    '"'|'`'|'$') escaped+='\\'; escaped+="$char" ;;
    *) escaped+="$char" ;;
  esac
done
temporary="$(mktemp "$data/applications/.lumen.XXXXXX")"
{
  printf '[Desktop Entry]\nVersion=1.0\nType=Application\nName=Lumen\nComment=Lumen Linux preview\n'
  printf 'Exec="%s" --url %%u\n' "$escaped"
  printf 'Icon=lumen\nTerminal=false\nCategories=Network;WebBrowser;\nMimeType=x-scheme-handler/http;x-scheme-handler/https;\nStartupNotify=true\nStartupWMClass=lumen\n'
} > "$temporary"
chmod 644 -- "$temporary"
mv -f -- "$temporary" "$data/applications/io.github.L4NOTTE.Lumen.desktop"
temporary=''
if command -v update-desktop-database >/dev/null; then update-desktop-database "$data/applications" || true; fi
printf '\nInstalled: %s\nLaunch Lumen from the application menu or run:\n  "%s"\n' "$executable" "$executable"
echo 'Default browser and user profile were not changed. For updates: git pull --ff-only, then run this installer again.'
