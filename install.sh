#!/bin/sh
# Install mcp-sandman.
#
#   curl -fsSL https://raw.githubusercontent.com/xiaoy-ovo/mcp-sandman/main/install.sh | sh
#
# Tries a published release binary first, and falls back to building from
# source with cargo. Note that no prebuilt binaries are published yet, so the
# source path is what most people will actually take.

set -eu

REPO="xiaoy-ovo/mcp-sandman"
BINARY="mcp-sandman"

info()  { printf '\033[1;34m==>\033[0m %s\n' "$1"; }
warn()  { printf '\033[1;33mwarning:\033[0m %s\n' "$1" >&2; }
die()   { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

# Resolve OS and architecture to the naming used by the release artifacts.
detect_platform() {
    os=$(uname -s | tr '[:upper:]' '[:lower:]')
    case "$os" in
        linux|darwin) ;;
        *) warn "$os is not supported by this installer; install from source with 'cargo install --path .'" ; return 1 ;;
    esac

    arch=$(uname -m)
    case "$arch" in
        x86_64|amd64) arch="x86_64" ;;
        arm64|aarch64) arch="aarch64" ;;
        *) warn "$arch is not supported by this installer; install from source with 'cargo install --path .'" ; return 1 ;;
    esac

    printf '%s-%s' "$os" "$arch"
}

install_dir() {
    # Prefer a user-writable directory on PATH over requiring sudo.
    if [ -w "${HOME}/.local/bin" ] 2>/dev/null || mkdir -p "${HOME}/.local/bin" 2>/dev/null; then
        printf '%s' "${HOME}/.local/bin"
    elif [ -w /usr/local/bin ]; then
        printf '%s' "/usr/local/bin"
    else
        die "no writable bin directory found; install from source with 'cargo install --path .'"
    fi
}

download() {
    target=$1
    dest=$2
    url="https://github.com/${REPO}/releases/latest/download/mcp-sandman-${target}"

    info "downloading ${url}"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$url" -o "$dest"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$dest" "$url"
    else
        warn "neither curl nor wget found; falling back to a source build"
        return 1
    fi
}

build_from_source() {
    command -v cargo >/dev/null 2>&1 || die "cargo not found; install Rust from https://rustup.rs"
    info "building from source"
    cargo install --path . --locked
}

main() {
    command -v uname >/dev/null 2>&1 || die "uname not available"

    if target=$(detect_platform); then
        bin_dir=$(install_dir)
        tmp=$(mktemp 2>/dev/null || echo "./${BINARY}.download")
        trap 'rm -f "$tmp"' EXIT INT TERM

        if download "$target" "$tmp"; then
            chmod +x "$tmp"
            mv "$tmp" "${bin_dir}/${BINARY}"
            info "installed ${bin_dir}/${BINARY}"
        else
            warn "could not fetch a release binary for ${target}"
            build_from_source
        fi
    else
        build_from_source
    fi

    case ":${PATH}:" in
        *":${bin_dir}:"*) ;;
        *) warn "${bin_dir} is not on your PATH; add it to your shell profile to use ${BINARY}" ;;
    esac

    "${BINARY}" --version 2>/dev/null || "${bin_dir}/${BINARY}" --version 2>/dev/null || true
    info "done"
}

main "$@"