#!/bin/sh
# Tests install.sh against a fake release served from localhost: the
# latest release, a pinned version, a bad checksum, and a missing build.
#
#   ci/test-install.sh [shell]     # default: sh
#
# Needs python3 for the web server. Nothing outside a temp folder is touched.

set -eu

shell=${1:-sh}
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
server_pid=
cleanup() {
    if [ -n "$server_pid" ]; then kill "$server_pid" 2>/dev/null || true; fi
    rm -rf "$work"
}
trap cleanup EXIT

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
    Darwin-arm64) target=aarch64-apple-darwin ;;
    Darwin-x86_64)
        if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
            target=aarch64-apple-darwin
        else
            target=x86_64-apple-darwin
        fi
        ;;
    *) echo "no release target for $(uname -s) $(uname -m)"; exit 1 ;;
esac

sha() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }

# A fake release: the "binary" is a script that prints its version.
make_release() { # dir version
    mkdir -p "$1"
    printf '#!/bin/sh\necho "porthole %s"\n' "$2" > "$1/porthole-$2-$target"
    printf 'other build\n' > "$1/porthole-$2-x86_64-pc-windows-msvc.exe"
    (cd "$1" && sha porthole-* > SHA256SUMS)
}
srv=$work/srv/releases
make_release "$srv/download/v1.2.3" 1.2.3
make_release "$srv/download/v1.1.0" 1.1.0
mkdir -p "$srv/latest/download"
cp "$srv/download/v1.2.3/SHA256SUMS" "$srv/latest/download/SHA256SUMS"
# v0.9.0 has a checksum that does not match; v0.8.0 has no build for us.
make_release "$srv/download/v0.9.0" 0.9.0
printf 'tampered\n' >> "$srv/download/v0.9.0/porthole-0.9.0-$target"
mkdir -p "$srv/download/v0.8.0"
grep -v -- "-$target\$" "$srv/download/v1.2.3/SHA256SUMS" > "$srv/download/v0.8.0/SHA256SUMS"

port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
python3 -m http.server "$port" --bind 127.0.0.1 -d "$work/srv" > "$work/http.log" 2>&1 &
server_pid=$!
i=0
until curl -fs "http://127.0.0.1:$port/releases/latest/download/SHA256SUMS" >/dev/null 2>&1; do
    i=$((i + 1)); [ $i -lt 100 ] || { echo "server did not start"; cat "$work/http.log"; exit 1; }
    sleep 0.1
done

base=http://127.0.0.1:$port/releases
fail() { echo "FAIL: $1"; exit 1; }

run_install() { # install_dir [version]
    PORTHOLE_BASE_URL=$base PORTHOLE_INSTALL_DIR=$1 PORTHOLE_VERSION=${2:-} \
        "$shell" "$root/install.sh"
}

echo "== latest"
run_install "$work/bin" 2> "$work/out" || { cat "$work/out"; fail "latest install failed"; }
cat "$work/out"
[ "$("$work/bin/porthole")" = "porthole 1.2.3" ] || fail "latest did not install 1.2.3"
grep -q "is not on your PATH" "$work/out" || fail "no PATH hint"

echo "== pinned version, replacing the existing binary"
run_install "$work/bin" v1.1.0 2> "$work/out" || { cat "$work/out"; fail "pinned install failed"; }
[ "$("$work/bin/porthole")" = "porthole 1.1.0" ] || fail "pinned did not install 1.1.0"
[ ! -e "$work/bin/.porthole.new" ] || fail "temp file left behind"

echo "== checksum mismatch"
if run_install "$work/bad" 0.9.0 2> "$work/out"; then fail "bad checksum was accepted"; fi
cat "$work/out"
grep -q "checksum mismatch" "$work/out" || fail "wrong error for a bad checksum"
[ ! -e "$work/bad/porthole" ] || fail "installed despite a bad checksum"

echo "== no build for this platform"
if run_install "$work/none" 0.8.0 2> "$work/out"; then fail "missing build was accepted"; fi
cat "$work/out"
grep -q "no build for $target" "$work/out" || fail "wrong error for a missing build"

echo "== release that does not exist"
if run_install "$work/none" 9.9.9 2> "$work/out"; then fail "missing release was accepted"; fi
grep -q "Is there a release v9.9.9" "$work/out" || fail "wrong error for a missing release"

echo "install.sh: all checks passed ($shell, $target)"
