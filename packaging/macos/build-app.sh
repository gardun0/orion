#!/usr/bin/env bash
# Package target/release/orion as an (unsigned) Orion.app and zip it.
# Runs on macOS (CI: macos-latest, Apple Silicon).
#
# Usage: packaging/macos/build-app.sh [output-dir]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT_DIR="${1:-"${ROOT}/dist"}"
BIN="${ROOT}/target/release/orion"
ICON_PNG="${ROOT}/assets/app-icon/io.github.gardun0.orion-512.png"
PLIST_TEMPLATE="${ROOT}/packaging/macos/Info.plist"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "${ROOT}/Cargo.toml" | head -1)"
APP="${ROOT}/packaging/macos/Orion.app"
ZIP_NAME="orion-v${VERSION}-aarch64-apple-darwin.zip"

if [[ ! -x "${BIN}" ]]; then
  echo "error: ${BIN} not found; run \`cargo build --release --locked\` first" >&2
  exit 1
fi
mkdir -p "${OUT_DIR}"

rm -rf "${APP}"
mkdir -p "${APP}/Contents/MacOS" "${APP}/Contents/Resources"
cp "${BIN}" "${APP}/Contents/MacOS/orion"
sed "s/@VERSION@/${VERSION}/g" "${PLIST_TEMPLATE}" > "${APP}/Contents/Info.plist"

# App icon: build an .icns from the bundled PNGs (partial iconset is fine;
# iconutil fills what it can).
ICONSET="$(mktemp -d)/Orion.iconset"
mkdir -p "${ICONSET}"
sips -z 512 512 "${ICON_PNG}" --out "${ICONSET}/icon_512x512.png" >/dev/null
sips -z 256 256 "${ICON_PNG}" --out "${ICONSET}/icon_256x256.png" >/dev/null
sips -z 512 512 "${ICON_PNG}" --out "${ICONSET}/icon_256x256@2x.png" >/dev/null
sips -z 128 128 "${ICON_PNG}" --out "${ICONSET}/icon_128x128.png" >/dev/null
sips -z 64 64 "${ICON_PNG}" --out "${ICONSET}/icon_32x32@2x.png" >/dev/null
sips -z 32 32 "${ICON_PNG}" --out "${ICONSET}/icon_32x32.png" >/dev/null
iconutil -c icns "${ICONSET}" -o "${APP}/Contents/Resources/Orion.icns"

# Ad-hoc signature so Gatekeeper shows a recoverable warning instead of
# rejecting the bundle outright. Proper signing/notarization needs an Apple
# Developer ID — tracked as future work.
codesign --force --deep --sign - "${APP}"

rm -f "${OUT_DIR}/${ZIP_NAME}"
ditto -c -k --sequesterRsrc --keepParent "${APP}" "${OUT_DIR}/${ZIP_NAME}"
echo "wrote ${OUT_DIR}/${ZIP_NAME}"
