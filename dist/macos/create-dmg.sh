#!/bin/sh
# macOS DMG Packaging Script for Inscribe
# Creates a drag-and-drop .dmg disk image with Applications folder link
set -e

APP_NAME="Inscribe"
APP_DIR="target/macos/${APP_NAME}.app"
DMG_OUTPUT="target/macos/${APP_NAME}.dmg"
DMG_VOLUME_NAME="${APP_NAME}"
DMG_TEMP_DIR="target/macos/dmg_root"

echo "Building macOS application bundle structure..."
mkdir -p "${APP_DIR}/Contents/MacOS"
mkdir -p "${APP_DIR}/Contents/Resources"

# Copy binary and metadata
cp "target/release/inscribe-gui" "${APP_DIR}/Contents/MacOS/inscribe-gui"
cp "dist/macos/Info.plist" "${APP_DIR}/Contents/Info.plist"

# Prepare staging folder for DMG creation
rm -rf "${DMG_TEMP_DIR}"
mkdir -p "${DMG_TEMP_DIR}"
cp -R "${APP_DIR}" "${DMG_TEMP_DIR}/"
ln -s /Applications "${DMG_TEMP_DIR}/Applications"

echo "Creating DMG disk image with hdiutil..."
rm -f "${DMG_OUTPUT}"
hdiutil create -volname "${DMG_VOLUME_NAME}" \
    -srcfolder "${DMG_TEMP_DIR}" \
    -ov -format UDZO \
    "${DMG_OUTPUT}"

echo "macOS DMG created successfully at ${DMG_OUTPUT}"
