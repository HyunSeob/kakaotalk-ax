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
if [[ ":$PATH:" != *":${INSTALL_DIR}:"* ]]; then
  echo "Add ${INSTALL_DIR} to your PATH to invoke 'kax' directly."
fi
