#!/usr/bin/env bash
# Contract test only: the build, platform and dependency probes are mocked.
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
work="$(mktemp -d "$root/checks/linux-install-test.XXXXXX")"
mkdir -p "$work/shims"
cat > "$work/shims/uname" <<'EOF'
#!/usr/bin/env bash
echo Linux
EOF
cat > "$work/shims/rustc" <<'EOF'
#!/usr/bin/env bash
echo 'host: x86_64-unknown-linux-gnu'
EOF
cat > "$work/shims/pkg-config" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
cat > "$work/shims/cargo" <<'EOF'
#!/usr/bin/env bash
[[ "${LUMEN_TEST_FAIL_BUILD:-}" != 1 ]] || exit 42
mkdir -p "$CARGO_TARGET_DIR/x86_64-unknown-linux-gnu/release"
printf '#!/bin/sh\nexit 0\n' > "$CARGO_TARGET_DIR/x86_64-unknown-linux-gnu/release/lumen"
chmod 755 "$CARGO_TARGET_DIR/x86_64-unknown-linux-gnu/release/lumen"
EOF
chmod 755 "$work/shims/"*
export PATH="$work/shims:$PATH"
export CARGO_TARGET_DIR="$work/build"
export LUMEN_INSTALL_PREFIX="$work/local with spaces"
export XDG_DATA_HOME="$work/data"
shell="${BASH:-bash}"
"$shell" "$root/tools/install-linux.sh" --check
[[ ! -e "$LUMEN_INSTALL_PREFIX/bin/lumen" ]]
"$shell" "$root/tools/install-linux.sh"
[[ -x "$LUMEN_INSTALL_PREFIX/bin/lumen" ]]
entry="$XDG_DATA_HOME/applications/io.github.L4NOTTE.Lumen.desktop"
grep -F "Exec=\"$LUMEN_INSTALL_PREFIX/bin/lumen\" --url %u" "$entry"
[[ -f "$XDG_DATA_HOME/icons/hicolor/scalable/apps/lumen.svg" ]]
before="$(cksum "$LUMEN_INSTALL_PREFIX/bin/lumen")"
if LUMEN_TEST_FAIL_BUILD=1 "$shell" "$root/tools/install-linux.sh"; then echo 'Failed build was accepted' >&2; exit 1; fi
[[ "$(cksum "$LUMEN_INSTALL_PREFIX/bin/lumen")" == "$before" ]]
if "$shell" "$root/tools/install-linux.sh" --invalid; then exit 1; fi
echo "Installer contract passed. Test artifacts: $work"
