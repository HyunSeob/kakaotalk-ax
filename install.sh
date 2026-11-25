#!/usr/bin/env bash
set -euo pipefail

REPO="${KAX_REPO:-hyunseob/kakaotalk-ax}"
VERSION="${KAX_VERSION:-latest}"
INSTALL_DIR="${KAX_INSTALL_DIR:-$HOME/.local/bin}"
ASSET="kax-macos-universal.tar.gz"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: install.sh is for macOS (Darwin). On Windows, use install.ps1." >&2
  exit 1
fi

if [[ "$VERSION" == "latest" ]]; then
  BASE_URL="https://github.com/${REPO}/releases/latest/download"
else
  BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"
fi

TMP_DIR="$(mktemp -d)"
cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT

echo "Downloading ${ASSET} and checksums.txt from ${BASE_URL}..."
curl -fsSL "${BASE_URL}/${ASSET}" -o "${TMP_DIR}/${ASSET}"
curl -fsSL "${BASE_URL}/checksums.txt" -o "${TMP_DIR}/checksums.txt"

EXPECTED="$(awk -v file="$ASSET" '$2 == file || $2 == ("*" file) { print $1; exit }' "${TMP_DIR}/checksums.txt")"
if [[ -z "$EXPECTED" ]]; then
  echo "error: checksum entry for ${ASSET} not found in checksums.txt" >&2
  exit 1
fi

ACTUAL="$(shasum -a 256 "${TMP_DIR}/${ASSET}" | awk '{ print $1 }')"
if [[ "$ACTUAL" != "$EXPECTED" ]]; then
  echo "error: SHA-256 mismatch for ${ASSET}" >&2
  echo "  expected: ${EXPECTED}" >&2
  echo "  actual:   ${ACTUAL}" >&2
  exit 1
fi

tar -xzf "${TMP_DIR}/${ASSET}" -C "${TMP_DIR}"
mkdir -p "$INSTALL_DIR"
install -m 0755 "${TMP_DIR}/kax" "${INSTALL_DIR}/kax"

echo "Installed kax to ${INSTALL_DIR}/kax (SHA-256 verified)."

# `curl ... | bash` runs in a child shell, so it cannot change the PATH of the
# terminal that launched it. Add INSTALL_DIR to the login shell's startup file
# instead, so every new terminal finds `kax`. Set KAX_NO_MODIFY_PATH=1 to skip.
if [[ ":$PATH:" == *":${INSTALL_DIR}:"* ]]; then
  exit 0
fi

if [[ "$INSTALL_DIR" == "$HOME"/* ]]; then
  PATH_DIR="\$HOME/${INSTALL_DIR#"$HOME"/}"
else
  PATH_DIR="$INSTALL_DIR"
fi

case "$(basename "${SHELL:-/bin/zsh}")" in
  zsh)
    RC_FILE="${ZDOTDIR:-$HOME}/.zshrc"
    PATH_LINE="export PATH=\"${PATH_DIR}:\$PATH\""
    ;;
  bash)
    # A login bash reads only the first of these that exists. Creating a new
    # ~/.bash_profile would hide an existing ~/.bash_login or ~/.profile.
    RC_FILE="$HOME/.bash_profile"
    if [[ ! -e "$RC_FILE" ]]; then
      for candidate in "$HOME/.bash_login" "$HOME/.profile"; do
        if [[ -e "$candidate" ]]; then
          RC_FILE="$candidate"
          break
        fi
      done
    fi
    PATH_LINE="export PATH=\"${PATH_DIR}:\$PATH\""
    ;;
  fish)
    RC_FILE="${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish"
    PATH_LINE="fish_add_path \"${PATH_DIR}\""
    ;;
  *)
    RC_FILE="$HOME/.profile"
    PATH_LINE="export PATH=\"${PATH_DIR}:\$PATH\""
    ;;
esac

if [[ "${KAX_NO_MODIFY_PATH:-}" == "1" ]]; then
  echo "${INSTALL_DIR} is not on your PATH. Add this line to ${RC_FILE}:"
  echo "  ${PATH_LINE}"
  exit 0
fi

if [[ -f "$RC_FILE" ]] && grep -qF "$PATH_LINE" "$RC_FILE"; then
  echo "${RC_FILE} already adds ${INSTALL_DIR} to PATH."
elif mkdir -p "$(dirname "$RC_FILE")" 2>/dev/null \
  && printf '\n# kax (kakaotalk-ax)\n%s\n' "$PATH_LINE" 2>/dev/null >> "$RC_FILE"; then
  echo "Added ${INSTALL_DIR} to PATH in ${RC_FILE}."
else
  echo "Could not write ${RC_FILE}. Add this line to your shell startup file:"
  echo "  ${PATH_LINE}"
fi
echo "Open a new terminal window, then run: kax status"
echo "To use kax in this window right away, run: export PATH=\"${INSTALL_DIR}:\$PATH\""
