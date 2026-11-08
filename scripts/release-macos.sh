#!/usr/bin/env bash
# Build the official macOS release and attach it to the GitHub Release for TAG:
# the DMG for new installs, plus the signed update package and latest.json
# that installed copies update from.
#
# Usage: scripts/release-macos.sh vX.Y.Z [--notes-file FILE] [--publish]
#
#   --notes-file FILE  Release notes; needed only when the release does not
#                      exist yet, in which case it is created as a draft.
#   --publish          Publish the release at the end. Publishing starts the
#                      Windows CI build, which adds its own entry to latest.json.
#
# The updater signing key is read from TAURI_SIGNING_PRIVATE_KEY and
# TAURI_SIGNING_PRIVATE_KEY_PASSWORD, or else from
# ~/.tauri/voicex-updater.key and ~/.tauri/voicex-updater.password.
set -euo pipefail

usage() {
  sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 1
}

TAG=""
NOTES_FILE=""
PUBLISH=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --notes-file) NOTES_FILE="${2:?--notes-file needs a path}"; shift 2 ;;
    --publish) PUBLISH=1; shift ;;
    -h|--help) usage ;;
    -*) echo "Unknown option: $1" >&2; usage ;;
    *) [[ -z "${TAG}" ]] || usage; TAG="$1"; shift ;;
  esac
done
[[ -n "${TAG}" ]] || usage

fail() {
  echo "error: $*" >&2
  exit 1
}

[[ "$(uname -s)" == "Darwin" ]] || fail "this script only runs on macOS"
[[ "${TAG}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "tag must look like v1.2.3, got ${TAG}"
VERSION="${TAG#v}"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "${ROOT}"

# --- The tagged source, and nothing else, is what gets built -----------------

for file_version in \
  "package.json:$(node -p "require('./package.json').version")" \
  "src-tauri/tauri.conf.json:$(node -p "require('./src-tauri/tauri.conf.json').version")" \
  "src-tauri/Cargo.toml:$(sed -n 's/^version = "\(.*\)"/\1/p' src-tauri/Cargo.toml | head -1)"; do
  [[ "${file_version#*:}" == "${VERSION}" ]] \
    || fail "${file_version%%:*} has version ${file_version#*:}, expected ${VERSION}"
done

git rev-parse -q --verify "refs/tags/${TAG}" >/dev/null || fail "tag ${TAG} does not exist locally"
[[ "$(git rev-parse HEAD)" == "$(git rev-parse "${TAG}^{commit}")" ]] \
  || fail "HEAD is not at ${TAG}; check out the tag first"
git diff --quiet HEAD || fail "tracked files have uncommitted changes"
git ls-remote --exit-code --tags origin "refs/tags/${TAG}" >/dev/null \
  || fail "tag ${TAG} is not on origin; push it first"

# --- Signing ------------------------------------------------------------------

IDENTITY_NAME="$(node -p "require('./src-tauri/tauri.conf.json').bundle.macOS.signingIdentity")"
security find-identity -v -p codesigning | grep -Fq "${IDENTITY_NAME}" \
  || fail "code-signing identity '${IDENTITY_NAME}' is missing. Updates keep macOS permissions only when signed with the same certificate, so restore it from backup rather than creating a new one"

KEY_FILE="${HOME}/.tauri/voicex-updater.key"
PASSWORD_FILE="${HOME}/.tauri/voicex-updater.password"
if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
  [[ -f "${KEY_FILE}" ]] || fail "no updater key: set TAURI_SIGNING_PRIVATE_KEY or create ${KEY_FILE}"
  TAURI_SIGNING_PRIVATE_KEY="$(cat "${KEY_FILE}")"
fi
if [[ -z "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD+set}" ]]; then
  [[ -f "${PASSWORD_FILE}" ]] || fail "no updater key password: set TAURI_SIGNING_PRIVATE_KEY_PASSWORD or create ${PASSWORD_FILE}"
  TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$(cat "${PASSWORD_FILE}")"
fi
export TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD

# --- Release on GitHub ----------------------------------------------------------

if ! gh release view "${TAG}" >/dev/null 2>&1; then
  [[ -n "${NOTES_FILE}" ]] || fail "release ${TAG} does not exist; pass --notes-file to create it"
  [[ -f "${NOTES_FILE}" ]] || fail "notes file not found: ${NOTES_FILE}"
  echo "Creating draft release ${TAG}..."
  gh release create "${TAG}" --draft --verify-tag --title "${TAG}" --notes-file "${NOTES_FILE}"
fi

# --- Build ----------------------------------------------------------------------

case "$(uname -m)" in
  arm64) UPDATER_ARCH="aarch64"; DMG_ARCH="aarch64" ;;
  x86_64) UPDATER_ARCH="x86_64"; DMG_ARCH="x64" ;;
  *) fail "unsupported architecture $(uname -m)" ;;
esac

BUNDLE_DIR="src-tauri/target/release/bundle"
APP="${BUNDLE_DIR}/macos/VoiceX.app"
UPDATE_PACKAGE="${BUNDLE_DIR}/macos/VoiceX.app.tar.gz"
DMG="${BUNDLE_DIR}/dmg/VoiceX_${VERSION}_${DMG_ARCH}.dmg"
# The update package's name carries no version, so a leftover from an earlier
# build must not be mistaken for this one.
rm -f "${UPDATE_PACKAGE}" "${UPDATE_PACKAGE}.sig" "${DMG}"

echo "Building VoiceX ${VERSION} (${UPDATER_ARCH})..."
pnpm tauri build --bundles app,dmg --config src-tauri/tauri.updater.conf.json

for artifact in "${APP}" "${UPDATE_PACKAGE}" "${UPDATE_PACKAGE}.sig" "${DMG}"; do
  [[ -e "${artifact}" ]] || fail "build did not produce ${artifact}"
done
codesign --verify --deep --strict "${APP}"

# --- Assets and updater manifest ----------------------------------------------

STAGE="${BUNDLE_DIR}/release-upload"
rm -rf "${STAGE}"
mkdir -p "${STAGE}"
PACKAGE_NAME="VoiceX_${VERSION}_${UPDATER_ARCH}.app.tar.gz"
cp "${DMG}" "${STAGE}/"
cp "${UPDATE_PACKAGE}" "${STAGE}/${PACKAGE_NAME}"
cp "${UPDATE_PACKAGE}.sig" "${STAGE}/${PACKAGE_NAME}.sig"

REPO="$(gh repo view --json nameWithOwner -q .nameWithOwner)"
gh release view "${TAG}" --json body -q .body > "${STAGE}/notes.md"
# Keep entries another platform already added (Windows CI merges the same way).
if gh release view "${TAG}" --json assets -q '.assets[].name' | grep -qx 'latest.json'; then
  gh release download "${TAG}" --pattern latest.json --dir "${STAGE}/existing"
fi

VERSION="${VERSION}" \
PLATFORM="darwin-${UPDATER_ARCH}" \
URL="https://github.com/${REPO}/releases/download/${TAG}/${PACKAGE_NAME}" \
SIGNATURE_FILE="${STAGE}/${PACKAGE_NAME}.sig" \
NOTES_FILE="${STAGE}/notes.md" \
EXISTING_FILE="${STAGE}/existing/latest.json" \
OUT_FILE="${STAGE}/latest.json" \
node --input-type=module -e '
import { existsSync, readFileSync, writeFileSync } from "node:fs";
const env = process.env;
let platforms = {};
if (existsSync(env.EXISTING_FILE)) {
  const existing = JSON.parse(readFileSync(env.EXISTING_FILE, "utf8"));
  if (existing.version === env.VERSION) platforms = existing.platforms ?? {};
}
const entry = { signature: readFileSync(env.SIGNATURE_FILE, "utf8"), url: env.URL };
platforms[env.PLATFORM] = entry;
platforms[`${env.PLATFORM}-app`] = entry;
const manifest = {
  version: env.VERSION,
  notes: readFileSync(env.NOTES_FILE, "utf8").trim(),
  pub_date: new Date().toISOString(),
  platforms,
};
writeFileSync(env.OUT_FILE, JSON.stringify(manifest, null, 2) + "\n");
console.log(`latest.json platforms: ${Object.keys(platforms).join(", ")}`);
'

echo "Uploading to ${TAG}..."
gh release upload "${TAG}" --clobber \
  "${STAGE}/$(basename "${DMG}")" \
  "${STAGE}/${PACKAGE_NAME}" \
  "${STAGE}/${PACKAGE_NAME}.sig" \
  "${STAGE}/latest.json"

if [[ "${PUBLISH}" == 1 ]]; then
  gh release edit "${TAG}" --draft=false --latest
  echo "Published ${TAG}. The Windows CI build adds its installer and latest.json entry when it finishes."
elif [[ "$(gh release view "${TAG}" --json isDraft -q .isDraft)" == "true" ]]; then
  echo "Draft ${TAG} is ready. Publish it to start the Windows build:"
  echo "  gh release edit ${TAG} --draft=false --latest"
fi
