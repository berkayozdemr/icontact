#!/bin/sh
# Build Icontact.app for macOS in one command (T-074):
#
#     scripts/make-mac-app.sh
#
# The result is target/Icontact.app. It is one folder that Finder shows as an
# app: the program, plus a small file (Info.plist) that tells macOS its name
# and why it wants the camera. Nothing else has to be copied in: the models
# and ONNX Runtime are built into the program, and everything else it uses
# ships with macOS.
#
# The app is not signed with an Apple developer account (the project has
# none). On another Mac the first start is: right-click the app > Open > Open.
#
# It is built for the kind of Mac it is built on (Apple silicon or Intel).

set -eu

cd "$(dirname "$0")/.."

# Apple's command line tools are enough; full Xcode is not needed. Keep a
# value the caller already set.
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Library/Developer/CommandLineTools}"

# The version comes from the workspace Cargo.toml, so it is written in one place.
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)

echo "building Icontact $version (release)..."
cargo build --release -p ec-app

app=target/Icontact.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp target/release/ec-app "$app/Contents/MacOS/Icontact"

# Info.plist: what macOS reads about the app.
# - NSCameraUsageDescription is the sentence in the "wants to use the camera"
#   question. Without it macOS closes the app the moment it opens the camera.
# - LSMinimumSystemVersion 13.4: the oldest macOS the built-in ONNX Runtime runs on.
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Icontact</string>
    <key>CFBundleDisplayName</key>
    <string>Icontact</string>
    <key>CFBundleExecutable</key>
    <string>Icontact</string>
    <key>CFBundleIdentifier</key>
    <string>org.icontact.Icontact</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$version</string>
    <key>CFBundleVersion</key>
    <string>$version</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.4</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSCameraUsageDescription</key>
    <string>Icontact reads your camera to correct your gaze. The picture never leaves this Mac.</string>
</dict>
</plist>
EOF

# Seal the folder with a local signature ("-" means no certificate, no Apple
# account). Apple silicon Macs refuse to start a program without at least
# this, and it lets macOS remember the camera permission for the app.
codesign --force --sign - "$app"

echo "done: $app"
echo "start it with:  open $app"
