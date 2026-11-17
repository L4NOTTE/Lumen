#!/usr/bin/env bash
set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
. /etc/os-release
if [[ "${ID:-}" != ubuntu || "${VERSION_ID:-}" != 24.04 || "$(dpkg --print-architecture)" != amd64 ]]; then
  echo 'Этот комплект предназначен для Ubuntu 24.04 x86_64.' >&2
  exit 1
fi
(cd "$root" && sha256sum -c SHA256SUMS --quiet)
shopt -s nullglob
packages=("$root"/packages/lumen_*_amd64.deb)
[[ ${#packages[@]} == 1 ]] || { echo 'В комплекте должен быть один пакет Lumen.' >&2; exit 1; }
version="$(dpkg-deb -f "${packages[0]}" Version)"
work="$(mktemp -d -t lumen-offline.XXXXXX)"
trap 'rm -rf -- "$work"' EXIT
mkdir -p "$work/lists/partial"
ln -s "$root/packages" "$work/repo"
printf 'deb [trusted=yes] file:%s/repo ./\n' "$work" > "$work/local.list"
options=(
  -o "Dir::Etc::sourcelist=$work/local.list"
  -o 'Dir::Etc::sourceparts=-'
  -o "Dir::State::lists=$work/lists"
  -o 'Acquire::Languages=none'
)
apt-get "${options[@]}" -o Debug::NoLocking=1 update
case "${1:-}" in
  --check) apt-get "${options[@]}" -o Dir::State::status=/dev/null -s --no-install-recommends install "lumen=$version" ;;
  '') sudo apt-get "${options[@]}" --no-install-recommends install "lumen=$version" ;;
  *) echo 'Usage: bash install-offline.sh [--check]' >&2; exit 2 ;;
esac
