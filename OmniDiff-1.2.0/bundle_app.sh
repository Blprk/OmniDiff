#!/bin/bash
APP_NAME="OmniDiff"
EXECUTABLE_NAME="omnidiff"
TARGET_DIR="target/release"

# Ensure the executable exists
if [ ! -f "$TARGET_DIR/$EXECUTABLE_NAME" ]; then
    echo "Error: Release executable not found. Running cargo build --release..."
    cargo build --release
fi

# Create .app structure
APP_BUNDLE="$APP_NAME.app"
CONTENTS="$APP_BUNDLE/Contents"
MACOS="$CONTENTS/MacOS"
RESOURCES="$CONTENTS/Resources"

echo "Creating $APP_BUNDLE..."
rm -rf "$APP_BUNDLE"
mkdir -p "$MACOS"
mkdir -p "$RESOURCES"

# Copy binary
cp "$TARGET_DIR/$EXECUTABLE_NAME" "$MACOS/$APP_NAME"
chmod +x "$MACOS/$APP_NAME"

# Create Info.plist
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
    <string>1.2.0</string>
    <key>CFBundleVersion</key>
    <string>1.2.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
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

echo "✅ $APP_NAME has been successfully created!"
echo "📍 Location: $(pwd)/$APP_BUNDLE"
echo "👉 You can now drag this app to your Applications folder."
