#!/bin/sh
# Installs the porthole CLI from the latest GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/IZ737/Porthole/main/install.sh | sh
#
# Settings, all optional, read from the environment:
#
#   PORTHOLE_VERSION      a release such as 1.0.0 (default: the latest)
#   PORTHOLE_INSTALL_DIR  where to put the binary (default: ~/.local/bin)
#   PORTHOLE_BASE_URL     where the releases live, for mirrors
#                         (default: https://github.com/IZ737/Porthole/releases)
#
# Pass them to sh, not curl:
#
#   curl -fsSL .../install.sh | PORTHOLE_VERSION=1.0.0 sh
#
# The binary is checked against the release's SHA256SUMS before it is
# installed. Nothing outside the install folder is changed.

set -eu

main() {
    base_url=${PORTHOLE_BASE_URL:-https://github.com/IZ737/Porthole/releases}
    base_url=${base_url%/}
    install_dir=${PORTHOLE_INSTALL_DIR:-${HOME:?HOME is not set}/.local/bin}
    version=${PORTHOLE_VERSION:-}
    version=${version#v}

    need uname
    need mkdir
    need mktemp
    need chmod
    need mv
    need grep
    need sed
    detect_downloader
    detect_hasher
    detect_target

    tmp=$(mktemp -d 2>/dev/null || mktemp -d -t porthole)
    trap 'rm -rf "$tmp"' EXIT
    trap 'exit 1' HUP INT TERM

    if [ -n "$version" ]; then
        sums_url="$base_url/download/v$version/SHA256SUMS"
    else
        sums_url="$base_url/latest/download/SHA256SUMS"
    fi
    say "fetching $sums_url"
    download "$sums_url" "$tmp/SHA256SUMS" \
        || die "could not download SHA256SUMS. Is there a release${version:+ v$version}?"

    # Lines look like "<sha256>  porthole-1.0.0-<target>". Taking the asset
    # name (and so the version) from SHA256SUMS means "latest" is resolved
    # once, and the binary comes from the same release as its checksum.
    line=$(grep -E "^[0-9a-fA-F]{64}[ *]+porthole-[^ ]+-$target\$" "$tmp/SHA256SUMS" | head -n 1 || true)
    [ -n "$line" ] || die "the release has no build for $target. $(from_source_hint)"
    expected=$(printf '%s\n' "$line" | sed 's/[ *].*//' | tr 'A-F' 'a-f')
    asset=$(printf '%s\n' "$line" | sed 's/^[0-9a-fA-F]*[ *]*//')
    asset_version=${asset#porthole-}
    asset_version=${asset_version%-"$target"}

    url="$base_url/download/v$asset_version/$asset"
    say "downloading porthole $asset_version for $target"
    download "$url" "$tmp/$asset" || die "could not download $url"

    actual=$(hash_file "$tmp/$asset")
    if [ "$actual" != "$expected" ]; then
        die "checksum mismatch for $asset
  expected $expected
  got      $actual
Nothing was installed."
    fi

    mkdir -p "$install_dir" || die "could not create $install_dir"
    chmod 755 "$tmp/$asset"
    # Move into the install folder under a temporary name first, so a
    # running porthole is replaced in one step and never sees half a file.
    mv -f "$tmp/$asset" "$install_dir/.porthole.new" \
        || die "could not write to $install_dir (set PORTHOLE_INSTALL_DIR to a folder you own)"
    mv -f "$install_dir/.porthole.new" "$install_dir/porthole"
    say "installed $install_dir/porthole"

    check_runtime
    path_hint
}

say() {
    printf 'porthole-install: %s\n' "$1" >&2
}

die() {
    printf 'porthole-install: error: %s\n' "$1" >&2
    exit 1
}

need() {
    command -v "$1" >/dev/null 2>&1 || die "this script needs '$1'"
}

detect_downloader() {
    if command -v curl >/dev/null 2>&1; then
        downloader=curl
    elif command -v wget >/dev/null 2>&1; then
        downloader=wget
    else
        die "this script needs curl or wget"
    fi
}

download() {
    if [ "$downloader" = curl ]; then
        curl --proto '=https,http' -fsSL --retry 3 -o "$2" "$1"
    else
        wget -q -O "$2" "$1"
    fi
}

detect_hasher() {
    if command -v sha256sum >/dev/null 2>&1; then
        hasher="sha256sum"
    elif command -v shasum >/dev/null 2>&1; then
        hasher="shasum -a 256"
    elif command -v openssl >/dev/null 2>&1; then
        hasher="openssl"
    else
        die "this script needs sha256sum, shasum or openssl to check the download"
    fi
}

hash_file() {
    if [ "$hasher" = openssl ]; then
        openssl dgst -sha256 -r "$1" | sed 's/ .*//' | tr 'A-F' 'a-f'
    else
        $hasher "$1" | sed 's/ .*//' | tr 'A-F' 'a-f'
    fi
}

detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Linux)
            case "$arch" in
                x86_64 | amd64) target=x86_64-unknown-linux-gnu ;;
                *) die "there is no prebuilt porthole for Linux on $arch yet. $(from_source_hint)" ;;
            esac
            ;;
        Darwin)
            # A shell running under Rosetta reports x86_64 on Apple silicon;
            # install the native build there.
            if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
                arch=arm64
            fi
            case "$arch" in
                arm64 | aarch64) target=aarch64-apple-darwin ;;
                x86_64) target=x86_64-apple-darwin ;;
                *) die "there is no prebuilt porthole for macOS on $arch. $(from_source_hint)" ;;
            esac
            ;;
        MINGW* | MSYS* | CYGWIN*)
            die "on Windows, install from PowerShell instead:
  irm https://raw.githubusercontent.com/IZ737/Porthole/main/install.ps1 | iex"
            ;;
        *)
            die "there is no prebuilt porthole for $os. $(from_source_hint)"
            ;;
    esac
}

from_source_hint() {
    printf '%s' "Build it from source instead: cargo install --git https://github.com/IZ737/Porthole porthole-cli"
}

# The launcher links WebKitGTK 4.1 on Linux. Say how to get it when it is
# missing, rather than leaving a loader error for the first run.
check_runtime() {
    [ "$os" = Linux ] || return 0
    if command -v ldconfig >/dev/null 2>&1 \
        && ! ldconfig -p 2>/dev/null | grep -q 'libwebkit2gtk-4\.1\.so'; then
        say "note: WebKitGTK 4.1 was not found, and porthole needs it to open windows."
        say "  Debian/Ubuntu: sudo apt install libwebkit2gtk-4.1-0"
        say "  Fedora:        sudo dnf install webkit2gtk4.1"
        say "  Arch:          sudo pacman -S webkit2gtk-4.1"
    fi
}

path_hint() {
    case ":${PATH:-}:" in
        *":$install_dir:"*)
            say "run 'porthole --help' to get started"
            ;;
        *)
            say "$install_dir is not on your PATH. Add it with:"
            case "${SHELL:-}" in
                */fish) say "  fish_add_path $install_dir" ;;
                */zsh) say "  echo 'export PATH=\"$install_dir:\$PATH\"' >> ~/.zshrc" ;;
                *) say "  echo 'export PATH=\"$install_dir:\$PATH\"' >> ~/.bashrc" ;;
            esac
            say "then open a new terminal and run 'porthole --help'"
            ;;
    esac
}

# Everything runs from main, called on the last line, so a download cut off
# halfway through runs nothing.
main "$@"
