#!/usr/bin/env bash
set -euo pipefail
[[ "$(uname -s)" == Linux ]] || { echo 'Run packaging on Linux.' >&2; exit 1; }
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd -- "$root"
version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
arch="$(dpkg --print-architecture)"
[[ "$arch" == amd64 ]] || { echo 'This package configuration targets amd64.' >&2; exit 1; }
binary="$root/target/release/lumen"
[[ -x "$binary" ]] || { echo 'Run cargo build --release --locked first.' >&2; exit 1; }
out="$root/dist"
mkdir -p -- "$out"
work="$(mktemp -d -t lumen-package.XXXXXX)"
cleanup() { [[ -n "$work" && -d "$work" && "${work##*/}" == lumen-package.* ]] && rm -rf -- "$work"; }
trap cleanup EXIT
package="$work/package"
install -Dm755 "$binary" "$package/usr/bin/lumen"
install -Dm644 installer/lumen.desktop "$package/usr/share/applications/io.github.L4NOTTE.Lumen.desktop"
install -Dm644 assets/lumen.svg "$package/usr/share/icons/hicolor/scalable/apps/lumen.svg"
install -Dm644 assets/lumen.png "$package/usr/share/icons/hicolor/256x256/apps/lumen.png"
install -Dm644 LINUX.md "$package/usr/share/doc/lumen/README.md"
mkdir -p "$work/debian" "$package/DEBIAN"
cat > "$work/debian/control" <<'EOF'
Source: lumen
Section: web
Priority: optional
Maintainer: Lumen contributors

Package: lumen
Architecture: any
Description: Lumen web browser
EOF
dependencies="$(cd "$work" && dpkg-shlibdeps -O -e "$binary" | sed -n 's/^shlibs:Depends=//p')"
[[ -n "$dependencies" ]] || { echo 'Cannot determine runtime dependencies.' >&2; exit 1; }
cat > "$package/DEBIAN/control" <<EOF
Package: lumen
Version: $version
Section: web
Priority: optional
Architecture: $arch
Maintainer: Lumen contributors
Depends: $dependencies
Recommends: gstreamer1.0-libav, gstreamer1.0-plugins-good
Description: Lumen web browser for Ubuntu and Linux Mint
 Rust application using GTK and the system WebKitGTK engine.
 Initial Linux version; Chromium extensions are not supported.
EOF
dpkg-deb --build --root-owner-group "$package" "$out/lumen_${version}_${arch}.deb"
portable="$work/lumen-${version}-linux-x86_64"
mkdir -p "$portable"
install -m755 "$binary" "$portable/lumen"
cp LINUX.md "$portable/README.md"
cp installer/lumen.desktop assets/lumen.svg "$portable/"
tar -C "$work" -czf "$out/lumen-${version}-linux-x86_64.tar.gz" "${portable##*/}"
(cd "$out" && sha256sum "lumen_${version}_${arch}.deb" "lumen-${version}-linux-x86_64.tar.gz" > SHA256SUMS)
echo "Created Linux packages in $out"
