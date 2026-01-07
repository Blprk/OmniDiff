#!/bin/bash
APP_NAME="Folder Compare Pro"
EXECUTABLE_NAME="folder_compare_rust"
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
    <string>com.example.foldercompare</string>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>1.0</string>
    <key>CFBundleVersion</key>
    <string>1</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
EOF

# Create a simplified icon (Optional, system default will be used otherwise)
# We won't generate an .icns file here to keep it simple, but the app will work.

echo "✅ $APP_NAME has been successfully created!"
echo "📍 Location: $(pwd)/$APP_BUNDLE"
echo "👉 You can now drag this app to your Applications folder."
