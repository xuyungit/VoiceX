#!/bin/sh
set -eu
cd "$(dirname "$0")/../.."
fixture_dir="$(mktemp -d "${TMPDIR:-/tmp}/voicex-post-edit-interfaces.XXXXXX")"
fixture_dir="$(cd "$fixture_dir" && pwd -P)"
trap 'rm -rf "$fixture_dir"' EXIT

# These fixtures explicitly activate synthetic windows to initialize AX.
# Use only during an arranged foreground validation slot.
for kind in ime webkit; do
  bundle="$fixture_dir/$kind.app/Contents"
  mkdir -p "$bundle/MacOS"
  swiftc -swift-version 5 "scripts/post-edit/$kind-fixture.swift" -o "$bundle/MacOS/fixture"
  cat > "$bundle/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.voicex.fixture.$kind</string>
<key>CFBundleName</key><string>VoiceX synthetic $kind fixture</string>
<key>CFBundleExecutable</key><string>fixture</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>NSPrincipalClass</key><string>NSApplication</string>
</dict></plist>
PLIST
  codesign --force --sign - "$fixture_dir/$kind.app"
done

export VOICEX_POST_EDIT_IME_FIXTURE="$fixture_dir/ime.app/Contents/MacOS/fixture"
export VOICEX_POST_EDIT_WEBKIT_FIXTURE="$fixture_dir/webkit.app/Contents/MacOS/fixture"
if [ -n "${VOICEX_POST_EDIT_TEST_BINARY:-}" ]; then
  "$VOICEX_POST_EDIT_TEST_BINARY" post_edit::platform::macos_tests::live_appkit_marked_text --exact --ignored --nocapture --test-threads=1
  "$VOICEX_POST_EDIT_TEST_BINARY" post_edit::platform::macos_tests::live_webkit_marker_ranges --exact --ignored --nocapture --test-threads=1
else
  cargo test --manifest-path src-tauri/Cargo.toml --lib post_edit::platform::macos_tests::live_appkit_marked_text -- --exact --ignored --nocapture --test-threads=1
  cargo test --manifest-path src-tauri/Cargo.toml --lib post_edit::platform::macos_tests::live_webkit_marker_ranges -- --exact --ignored --nocapture --test-threads=1
fi
