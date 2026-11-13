#!/bin/bash
# Bundle OmniDiff Pro 2.0 as a macOS .app
set -e
APP_NAME="OmniDiff"
EXECUTABLE_NAME="omnidiff"
TARGET_DIR="target/release"

if [ ! -f "$TARGET_DIR/$EXECUTABLE_NAME" ]; then
    echo "Error: Release executable not found. Running cargo build --release..."
    cargo build --release
fi

APP_BUNDLE="$APP_NAME.app"
CONTENTS="$APP_BUNDLE/Contents"
MACOS="$CONTENTS/MacOS"
RESOURCES="$CONTENTS/Resources"

echo "Creating $APP_BUNDLE..."
rm -rf "$APP_BUNDLE"
mkdir -p "$MACOS"
mkdir -p "$RESOURCES"

cp "$TARGET_DIR/$EXECUTABLE_NAME" "$MACOS/$APP_NAME"
chmod +x "$MACOS/$APP_NAME"

cat > "$CONTENTS/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>$APP_NAME</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundleIdentifier</key>
    <string>com.blprk.omnidiff</string>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>2.0.0</string>
    <key>CFBundleVersion</key>
    <string>2.0.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSHumanReadableCopyright</key>
    <string>© 2026 Blprk. MIT License.</string>
</dict>
</plist>
EOF

# Install the app icon
if [ -f "AppIcon.icns" ]; then
    cp "AppIcon.icns" "$RESOURCES/AppIcon.icns"
    echo "Icon installed."
elif [ -f "AppIcon.png" ]; then
    echo "Generating AppIcon.icns from AppIcon.png..."
    ./create_icon.sh "$RESOURCES"
fi

echo "✅ $APP_NAME (v2.0.0) has been successfully created!"
echo "📍 Location: $(pwd)/$APP_BUNDLE"
