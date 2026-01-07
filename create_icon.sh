#!/bin/bash
# Robust Icon Generation
set -e

ICON_SRC="AppIcon.png"
ICONSET_DIR="AppIcon.iconset"

if [ ! -f "$ICON_SRC" ]; then
    echo "Error: $ICON_SRC not found!"
    exit 1
fi

echo "Cleaning up..."
rm -rf "$ICONSET_DIR"
mkdir -p "$ICONSET_DIR"

echo "Generating iconset from $ICON_SRC..."

# Function to resize
function make_icon() {
    name=$1
    size=$2
    sips -z $size $size -s format png "$ICON_SRC" --out "$ICONSET_DIR/$name" > /dev/null
}

# Standard macOS sizes
make_icon "icon_16x16.png" 16
make_icon "icon_16x16@2x.png" 32
make_icon "icon_32x32.png" 32
make_icon "icon_32x32@2x.png" 64
make_icon "icon_128x128.png" 128
make_icon "icon_128x128@2x.png" 256
make_icon "icon_256x256.png" 256
make_icon "icon_256x256@2x.png" 512
make_icon "icon_512x512.png" 512
make_icon "icon_512x512@2x.png" 1024

echo "Converting iconset to ICNS..."
iconutil -c icns "$ICONSET_DIR"

if [ -f "AppIcon.icns" ]; then
    echo "✅ AppIcon.icns generated."
    
    # Install into bundle
    DEST="Folder Compare Pro.app/Contents/Resources/AppIcon.icns"
    cp "AppIcon.icns" "$DEST"
    echo "✅ Installed to $DEST"
    
    rm -rf "$ICONSET_DIR"
else
    echo "❌ Failed to generate AppIcon.icns"
    exit 1
fi
