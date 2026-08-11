#!/usr/bin/env bash
# Package target/release/orion as an AppImage using linuxdeploy.
#
# Usage: packaging/appimage/build-appimage.sh [output-dir]
#
# The release binary must already exist at target/release/orion (build with
# `cargo build --release --locked` first). linuxdeploy is downloaded on first
# use into packaging/appimage/tools/.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT_DIR="${1:-"${ROOT}/dist"}"
TOOLS_DIR="${ROOT}/packaging/appimage/tools"
APPDIR="${ROOT}/packaging/appimage/Orion.AppDir"
LINUXDEPLOY="${TOOLS_DIR}/linuxdeploy-x86_64.AppImage"
LINUXDEPLOY_URL="https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage"

BIN="${ROOT}/target/release/orion"
DESKTOP="${ROOT}/assets/linux/io.github.gardun0.orion.desktop"
ICON="${ROOT}/assets/app-icon/io.github.gardun0.orion.svg"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "${ROOT}/Cargo.toml" | head -1)"
OUTPUT="orion-v${VERSION}-x86_64.AppImage"

if [[ ! -x "${BIN}" ]]; then
  echo "error: ${BIN} not found; run \`cargo build --release --locked\` first" >&2
  exit 1
fi

mkdir -p "${TOOLS_DIR}" "${OUT_DIR}"
if [[ ! -x "${LINUXDEPLOY}" ]]; then
  echo "downloading linuxdeploy..."
  curl -fL "${LINUXDEPLOY_URL}" -o "${LINUXDEPLOY}"
  chmod +x "${LINUXDEPLOY}"
fi

rm -rf "${APPDIR}"

# linuxdeploy bundles the binary plus its linked shared-library dependencies
# (its built-in excludelist keeps glibc/wayland stack libraries that must come
# from the host). appimagetool is fetched automatically for --output appimage.
# APPIMAGE_EXTRACT_AND_RUN avoids requiring FUSE on the build host.
export APPIMAGE_EXTRACT_AND_RUN=1
export ARCH=x86_64
export VERSION
export OUTPUT

cd "${ROOT}"
"${LINUXDEPLOY}" \
  --appdir "${APPDIR}" \
  --executable "${BIN}" \
  --desktop-file "${DESKTOP}" \
  --icon-file "${ICON}" \
  --output appimage

mv -f "${ROOT}/${OUTPUT}" "${OUT_DIR}/${OUTPUT}"
echo "wrote ${OUT_DIR}/${OUTPUT}"
