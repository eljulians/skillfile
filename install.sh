#!/bin/sh
# Install skillfile — tool-agnostic AI skill & agent manager
#
# Usage:
#   curl -fsSL https://github.com/eljulians/skillfile/releases/latest/download/install.sh | sh
#   wget -qO- https://github.com/eljulians/skillfile/releases/latest/download/install.sh | sh
#
# Environment variables:
#   SKILLFILE_INSTALL_DIR  Override install directory (default: ~/.local/bin)
#   SKILLFILE_VERSION      Install a specific version (default: latest)

set -eu

REPO="eljulians/skillfile"
INSTALL_DIR="${SKILLFILE_INSTALL_DIR:-$HOME/.local/bin}"
BIN_NAME="skillfile"
TMP_FILE=""

cleanup() {
    if [ -n "$TMP_FILE" ] && [ -f "$TMP_FILE" ]; then
        rm -f "$TMP_FILE"
    fi
}

trap cleanup EXIT INT TERM

main() {
    detect_platform
    resolve_version
    download_binary
    print_success
}

detect_platform() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"

    case "$OS" in
        Linux*)  PLATFORM_OS="linux" ;;
        Darwin*) PLATFORM_OS="macos" ;;
        *)
            err "Unsupported operating system: $OS"
            err "skillfile supports Linux and macOS."
            exit 1
            ;;
    esac

    case "$ARCH" in
        x86_64|amd64)   PLATFORM_ARCH="x86_64" ;;
        aarch64|arm64)   PLATFORM_ARCH="aarch64" ;;
        *)
            err "Unsupported architecture: $ARCH"
            err "skillfile supports x86_64 and aarch64."
            exit 1
            ;;
    esac

    ASSET_NAME="${BIN_NAME}-${PLATFORM_ARCH}-${PLATFORM_OS}"
    log "Detected platform: ${PLATFORM_OS}/${PLATFORM_ARCH}"
}

resolve_version() {
    if [ -n "${SKILLFILE_VERSION:-}" ]; then
        VERSION="$SKILLFILE_VERSION"
        log "Using requested version: $VERSION"
        return
    fi

    log "Fetching latest release..."
    LATEST_URL="https://api.github.com/repos/${REPO}/releases/latest"

    if has curl; then
        VERSION="$(curl -fsSL "$LATEST_URL" | parse_tag_name)"
    elif has wget; then
        VERSION="$(wget -qO- "$LATEST_URL" | parse_tag_name)"
    else
        err "Neither curl nor wget found. Install one and retry."
        exit 1
    fi

    if [ -z "$VERSION" ]; then
        err "Failed to determine latest version."
        exit 1
    fi

    log "Latest version: $VERSION"
}

parse_tag_name() {
    # Extract tag_name value from JSON without jq.
    # POSIX sed — no grep -o (GNU extension).
    sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1
}

download_binary() {
    DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/${ASSET_NAME}"

    log "Downloading ${ASSET_NAME} (${VERSION})..."
    mkdir -p "$INSTALL_DIR"

    TMP_FILE="$(mktemp)"

    if has curl; then
        curl -fsSL "$DOWNLOAD_URL" -o "$TMP_FILE"
    elif has wget; then
        wget -qO "$TMP_FILE" "$DOWNLOAD_URL"
    fi

    # Verify download produced a non-empty file
    if [ ! -s "$TMP_FILE" ]; then
        err "Download failed: received empty file."
        exit 1
    fi

    mv "$TMP_FILE" "${INSTALL_DIR}/${BIN_NAME}"
    TMP_FILE=""
    chmod +x "${INSTALL_DIR}/${BIN_NAME}"
}

print_success() {
    log ""
    log "skillfile ${VERSION} installed to ${INSTALL_DIR}/${BIN_NAME}"

    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*)
            log "Run 'skillfile --help' to get started."
            ;;
        *)
            log ""
            log "${INSTALL_DIR} is not in your PATH. Add it with:"
            log ""
            log "  export PATH=\"${INSTALL_DIR}:\$PATH\""
            log ""
            log "To make it permanent, add that line to your shell profile (~/.bashrc, ~/.zshrc, etc.)."
            ;;
    esac
}

has() {
    command -v "$1" >/dev/null 2>&1
}

log() {
    printf '%s\n' "$1"
}

err() {
    printf '%s\n' "$1" >&2
}

main
