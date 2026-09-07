#!/usr/bin/env bash
# Kobold Installer
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/p7a-os/kobold/main/install.sh | bash
#   or from local repo: ./install.sh
set -euo pipefail

# -----------------------------------------------------------------------------
# Color and styling setup
# -----------------------------------------------------------------------------
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    BOLD="$(printf '\033[1m')"
    GREEN="$(printf '\033[32m')"
    BLUE="$(printf '\033[34m')"
    CYAN="$(printf '\033[36m')"
    YELLOW="$(printf '\033[33m')"
    RED="$(printf '\033[31m')"
    RESET="$(printf '\033[0m')"
else
    BOLD=""
    GREEN=""
    BLUE=""
    CYAN=""
    YELLOW=""
    RED=""
    RESET=""
fi

info() {
    printf "${CYAN}info:${RESET} %s\n" "$*"
}

success() {
    printf "${GREEN}✓${RESET} %s\n" "$*"
}

warn() {
    printf "${YELLOW}warning:${RESET} %s\n" "$*" >&2
}

error() {
    printf "${RED}error:${RESET} %s\n" "$*" >&2
    exit 1
}

# -----------------------------------------------------------------------------
# Banner
# -----------------------------------------------------------------------------
printf "${BOLD}${BLUE}"
cat <<'BANNER'
  _  _____  ____   ____  _     ____  
 | |/ / _ \| __ ) / __ \| |   |  _ \ 
 | ' / | | |  _ \| |  | | |   | | | |
 | . \ |_| | |_) | |__| | |___| |_| |
 |_|\_\___/|____/ \____/|_____|____/ 
BANNER
printf "${RESET}"
printf "${BOLD}Kobold: The Autonomous Agent Harness & Supervisor${RESET}\n\n"

# -----------------------------------------------------------------------------
# Platform Detection
# -----------------------------------------------------------------------------
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Darwin)
        OS_NAME="darwin"
        ;;
    Linux)
        OS_NAME="linux"
        ;;
    *)
        error "Unsupported operating system: $OS. Kobold supports Linux and macOS (Darwin)."
        ;;
esac

case "$ARCH" in
    x86_64|amd64)
        ARCH_NAME="x86_64"
        ;;
    arm64|aarch64)
        ARCH_NAME="aarch64"
        ;;
    *)
        error "Unsupported architecture: $ARCH. Kobold supports x86_64 and aarch64/arm64."
        ;;
esac

info "Detected platform: ${BOLD}${OS_NAME}-${ARCH_NAME}${RESET}"

# -----------------------------------------------------------------------------
# Target Destination Directory
# -----------------------------------------------------------------------------
INSTALL_DIR="${KOBOLD_INSTALL_DIR:-"$HOME/.local/bin"}"
mkdir -p "$INSTALL_DIR"

# -----------------------------------------------------------------------------
# Binaries to Install
# -----------------------------------------------------------------------------
# Base binaries installed by default. Adapters are downloaded on-demand
# during first run, or when ALL_ADAPTERS=1 is specified.
BINARIES=(
    "kobold"
    "koboldd"
)

# Optional adapters installed on demand or when explicitly requested
OPTIONAL_BINARIES=(
    "kobold-adapter-acp"
    "kobold-adapter-tmux"
    "kobold-openai"
    "kobold-tts"
)

# -----------------------------------------------------------------------------
# Installation Strategy
# -----------------------------------------------------------------------------
REPO_URL="https://github.com/p7a-os/kobold"
TMP_DIR=""
cleanup() {
    if [ -n "$TMP_DIR" ] && [ -d "$TMP_DIR" ]; then
        rm -rf "$TMP_DIR"
    fi
}
trap cleanup EXIT

install_from_dir() {
    local src_dir="$1"
    info "Installing binaries into ${BOLD}${INSTALL_DIR}${RESET}..."
    for bin in "${BINARIES[@]}"; do
        if [ -f "$src_dir/$bin" ]; then
            install -m 755 "$src_dir/$bin" "$INSTALL_DIR/$bin"
            success "Installed $bin"
        else
            error "Required binary '$bin' not found in $src_dir"
        fi
    done

    if [ "${ALL_ADAPTERS:-0}" = "1" ]; then
        for bin in "${OPTIONAL_BINARIES[@]}"; do
            if [ -f "$src_dir/$bin" ]; then
                install -m 755 "$src_dir/$bin" "$INSTALL_DIR/$bin"
                success "Installed $bin (adapter)"
            fi
        done
    fi
}

# 1. Check if running inside a cloned Kobold repository
if [ -f "Cargo.toml" ] && grep -q 'name = "kobold"' "Cargo.toml" 2>/dev/null; then
    info "Running inside Kobold repository root."
    if [ ! -f "target/release/kobold" ] || [ "${REBUILD:-0}" = "1" ]; then
        info "Compiling release binaries with cargo..."
        cargo build --release
    fi
    install_from_dir "target/release"

# 2. Otherwise: Remote install via prebuilt asset or cargo clone
else
    info "Attempting to download prebuilt release from GitHub..."
    TMP_DIR="$(mktemp -d 2>/dev/null || mktemp -d -t 'kobold-install')"
    ARCHIVE_URL="${REPO_URL}/releases/latest/download/kobold-${OS_NAME}-${ARCH_NAME}.tar.gz"
    DOWNLOAD_SUCCESS=0

    info "Downloading prebuilt release from GitHub ($ARCHIVE_URL)..."
    if curl -fsSL "$ARCHIVE_URL" 2>/dev/null | tar -xz -C "$TMP_DIR" 2>/dev/null; then
        DOWNLOAD_SUCCESS=1
        install_from_dir "$TMP_DIR"
    fi

    if [ "$DOWNLOAD_SUCCESS" -eq 0 ]; then
        info "Prebuilt archive not available. Building from source via Cargo..."
        if ! command -v cargo >/dev/null 2>&1; then
            echo ""
            error "Rust and Cargo are required to build Kobold from source.
Please install Rust via rustup:
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
Then re-run this script."
        fi

        if ! command -v git >/dev/null 2>&1; then
            error "git is required to clone the repository."
        fi

        info "Cloning ${REPO_URL}..."
        git clone --depth 1 "$REPO_URL" "$TMP_DIR/kobold"
        (
            cd "$TMP_DIR/kobold"
            info "Compiling release binaries..."
            cargo build --release
        )
        install_from_dir "$TMP_DIR/kobold/target/release"
    fi
fi

# -----------------------------------------------------------------------------
# Runtime Directories Setup
# -----------------------------------------------------------------------------
RUNTIME_DIR="$HOME/.local/share/kobold/sessions"
mkdir -p "$RUNTIME_DIR"

# -----------------------------------------------------------------------------
# Verification & PATH Check
# -----------------------------------------------------------------------------
echo ""
if command -v "$INSTALL_DIR/kobold" >/dev/null 2>&1; then
    VERSION_OUT="$("$INSTALL_DIR/kobold" --version 2>&1 || true)"
    success "Successfully installed ${BOLD}${VERSION_OUT}${RESET}"
else
    success "Kobold binaries installed successfully to ${INSTALL_DIR}"
fi

PATH_CONFIGURED=0
case ":$PATH:" in
    *":$INSTALL_DIR:"*)
        PATH_CONFIGURED=1
        ;;
esac

if [ "$PATH_CONFIGURED" -eq 0 ]; then
    warn "${INSTALL_DIR} is not currently in your \$PATH."
    echo ""
    echo "To access 'kobold' from any directory, add this to your shell profile:"
    if [ -n "${ZSH_VERSION:-}" ] || [ "$(basename "${SHELL:-}")" = "zsh" ]; then
        printf "  ${BOLD}echo 'export PATH=\"%s:\$PATH\"' >> ~/.zshrc && source ~/.zshrc${RESET}\n\n" "$INSTALL_DIR"
    elif [ -n "${BASH_VERSION:-}" ] || [ "$(basename "${SHELL:-}")" = "bash" ]; then
        printf "  ${BOLD}echo 'export PATH=\"%s:\$PATH\"' >> ~/.bashrc && source ~/.bashrc${RESET}\n\n" "$INSTALL_DIR"
    else
        printf "  ${BOLD}export PATH=\"%s:\$PATH\"${RESET}\n\n" "$INSTALL_DIR"
    fi
fi

# -----------------------------------------------------------------------------
# Welcome & Next Steps
# -----------------------------------------------------------------------------
printf "${BOLD}${GREEN}Kobold is ready to go!${RESET}\n\n"
echo "Quickstart commands:"
echo "  kobold                           # Launch interactive TUI"
echo "  kobold -a kobold-adapter-acp     # Drive external agents (Claude Code, Antigravity)"
echo "  kobold --ws-port 3000            # Launch with real-time Web Companion"
echo "  kobold -p \"Hello Kobold\"         # Execute single turn to stdout"
echo "  kobold list                      # List active sessions"
echo "  kobold --help                    # View all options and commands"
echo ""
echo "Documentation: https://github.com/p7a-os/kobold"
