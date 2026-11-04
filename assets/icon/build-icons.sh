#!/usr/bin/env bash
#
# Build every platform icon from the Icon Composer document.
#
# macOS 26 and later read the layered icon from Assets.car, compiled here from
# AppIcon.icon. Older macOS reads AppIcon.icns. Windows and Linux get rasters.
#
# Run: ./build-icons.sh

set -euo pipefail

cd "$(dirname "$0")"

ICON_NAME="AppIcon"
ICON_DOC="$ICON_NAME.icon"
BINARY_NAME="oxtuner"
SQUARE="source/square-1024.png"
LEGACY="source/macos-legacy-1024.png"
BUILD=".build"

die() {
    echo "error: $*" >&2
    exit 1
}

# actool ships with Xcode, not with the Command Line Tools, and it also insists
# on a developer directory that contains Xcode, so set DEVELOPER_DIR for the
# call even when xcode-select points somewhere else.
find_actool() {
    local candidates=(
        "/Applications/Xcode.app/Contents/Developer/usr/bin/actool"
        "${DEVELOPER_DIR:-}/usr/bin/actool"
    )

    for candidate in "${candidates[@]}"; do
        if [ -x "$candidate" ]; then
            DEVELOPER_DIR="${candidate%/usr/bin/actool}"
            export DEVELOPER_DIR
            echo "$candidate"
            return 0
        fi
    done

    if command -v xcrun >/dev/null 2>&1; then
        xcrun --find actool 2>/dev/null && return 0
    fi

    return 1
}

command -v python3 >/dev/null 2>&1 || die "python3 is required to build the Windows and Linux icons"
command -v iconutil >/dev/null 2>&1 || die "iconutil is required to build the legacy .icns"
command -v sips >/dev/null 2>&1 || die "sips is required to build the legacy .icns"

python3 -c "import PIL" 2>/dev/null || die "Pillow is required: python3 -m pip install pillow"

[ -d "$ICON_DOC" ] || die "missing $ICON_DOC"
[ -f "$SQUARE" ] || die "missing $SQUARE"
[ -f "$LEGACY" ] || die "missing $LEGACY"

ACTOOL="$(find_actool)" || die "actool not found. Install Xcode 26 or newer, accept its licence (sudo xcodebuild -license accept) and retry"

rm -rf "$BUILD" "hicolor"
mkdir -p "$BUILD" "$BUILD/icon.iconset" hicolor

# actool resolves paths against its own working directory, so hand it absolute
# ones for both the document and the output folder.
BUILD_ABS="$(pwd)/$BUILD"
ICON_ABS="$(pwd)/$ICON_DOC"

# The document compiles into the layered catalog entry macOS 26 reads. The asset
# name is the document name, which must match CFBundleIconName in Info.plist.
"$ACTOOL" \
    --compile "$BUILD_ABS" \
    --platform macosx \
    --minimum-deployment-target 26.0 \
    --app-icon "$ICON_NAME" \
    --output-partial-info-plist "$BUILD_ABS/partial.plist" \
    --errors --warnings \
    "$ICON_ABS" >/dev/null

[ -s "$BUILD/Assets.car" ] || die "actool produced no Assets.car"

cp "$BUILD/Assets.car" Assets.car

# The .icns is built from the legacy export instead of from the document, so the
# full 16 to 1024 ladder is present and the artwork keeps the margin and shadow
# that pre-Tahoe macOS expects.
while read -r size name; do
    sips -z "$size" "$size" "$LEGACY" --out "$BUILD/icon.iconset/icon_$name.png" >/dev/null
done <<'SIZES'
16 16x16
32 16x16@2x
32 32x32
64 32x32@2x
128 128x128
256 128x128@2x
256 256x256
512 256x256@2x
512 512x512
1024 512x512@2x
SIZES

iconutil -c icns "$BUILD/icon.iconset" -o "$ICON_NAME.icns"

python3 - "$SQUARE" "$BINARY_NAME" <<'PY'
import os
import sys

from PIL import Image

square_path, name = sys.argv[1], sys.argv[2]
master = Image.open(square_path).convert("RGBA")

# One generic PNG for packagers that take a single image.
master.resize((512, 512), Image.LANCZOS).save(f"{name}.png")

# Windows takes one multi-resolution ICO.
master.save(
    f"{name}.ico",
    sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
)

# Linux wants one PNG per size in the hicolor tree.
for size in (16, 24, 32, 48, 64, 128, 256, 512):
    folder = f"hicolor/{size}x{size}/apps"
    os.makedirs(folder, exist_ok=True)
    master.resize((size, size), Image.LANCZOS).save(f"{folder}/{name}.png")

print(f"wrote {name}.png, {name}.ico and hicolor/*/apps/{name}.png")
PY

rm -rf "$BUILD"

echo
echo "macOS 26+    $ICON_DOC + Assets.car   -> CFBundleIconName = $ICON_NAME"
echo "macOS older  $ICON_NAME.icns           -> CFBundleIconFile = $ICON_NAME"
echo "Windows      $BINARY_NAME.ico"
echo "Linux        $BINARY_NAME.png + hicolor/<size>/apps/$BINARY_NAME.png"
