#!/usr/bin/env bash
#
# Assemble a runnable Ox Tuner.app for macOS, plus the release artifacts CI
# attaches to a GitHub Release: a zipped .app and a DMG.
#
# A plain `cargo build` produces a bare executable, and macOS only shows a real
# application icon for a bundle. This wraps the binary in one, with the Info.plist
# keys and both icon formats the system expects.
#
# Usage:
#   ./build.sh                     release bundle + zip + dmg into dist/
#   ./build.sh --debug             debug bundle, quicker while developing
#   ./build.sh --debug --open      build and launch it
#   ./build.sh --target <triple>   cross-compile, e.g. x86_64-apple-darwin

set -euo pipefail

cd "$(dirname "$0")/../.."
ROOT="$(pwd)"

# Both architectures target the same floor: macOS 15 Sequoia. Without this,
# the toolchain default applies, which may be older than what the bundle
# promises in LSMinimumSystemVersion.
export MACOSX_DEPLOYMENT_TARGET="15.0"

APP_NAME="Ox Tuner"
EXECUTABLE="oxtuner"
BUNDLE_ID="com.oxtuner.OxTuner"
DIST="$ROOT/dist"
APP="$DIST/$APP_NAME.app"

PROFILE="release"
TARGET=""
OPEN_AFTER=0

while [ $# -gt 0 ]; do
    case "$1" in
        --debug) PROFILE="debug"; shift ;;
        --release) PROFILE="release"; shift ;;
        --target) TARGET="${2:?error: --target needs a triple}"; shift 2 ;;
        --open) OPEN_AFTER=1; shift ;;
        *) echo "error: unknown argument: $1" >&2; exit 2 ;;
    esac
done

command -v plutil >/dev/null 2>&1 || { echo "error: plutil is required" >&2; exit 1; }

[ -f assets/icon/Assets.car ] || { echo "error: assets/icon/Assets.car is missing, run assets/icon/build-icons.sh" >&2; exit 1; }
[ -f assets/icon/AppIcon.icns ] || { echo "error: assets/icon/AppIcon.icns is missing, run assets/icon/build-icons.sh" >&2; exit 1; }

# The workspace version is the one true release version. The build number is the
# commit count, which only ever moves forward and keeps Finder's Get Info honest.
VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
BUILD_NUMBER="$(git rev-list --count HEAD 2>/dev/null || echo 1)"

[ -n "$VERSION" ] || { echo "error: could not read the workspace version" >&2; exit 1; }

echo "building $APP_NAME $VERSION ($BUILD_NUMBER), $PROFILE"

BUILD_ARGS=(--locked -p oxtuner-app)
[ "$PROFILE" = "release" ] && BUILD_ARGS+=(--release)
[ -n "$TARGET" ] && BUILD_ARGS+=(--target "$TARGET")
cargo build "${BUILD_ARGS[@]}"

if [ -n "$TARGET" ]; then
    BINARY="$ROOT/target/$TARGET/$PROFILE/$EXECUTABLE"
else
    BINARY="$ROOT/target/$PROFILE/$EXECUTABLE"
fi

[ -x "$BINARY" ] || { echo "error: $BINARY was not produced" >&2; exit 1; }

if [ -n "$TARGET" ]; then
    ARCH="${TARGET%%-*}"
else
    ARCH="$(uname -m)"
fi
[ "$ARCH" = "arm64" ] && ARCH="aarch64"

# Fail early if the binary is not actually the requested architecture.
if command -v lipo >/dev/null 2>&1; then
    LIPO_ARCH="$ARCH"
    [ "$LIPO_ARCH" = "aarch64" ] && LIPO_ARCH="arm64"
    lipo "$BINARY" -verify_arch "$LIPO_ARCH"
fi

# Rebuild the bundle from scratch so a stale Resources folder can never survive
# an icon change.
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$BINARY" "$APP/Contents/MacOS/$EXECUTABLE"
chmod +x "$APP/Contents/MacOS/$EXECUTABLE"

cp assets/icon/Assets.car "$APP/Contents/Resources/Assets.car"
cp assets/icon/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"

sed \
    -e "s|@APP_NAME@|$APP_NAME|g" \
    -e "s|@EXECUTABLE@|$EXECUTABLE|g" \
    -e "s|@BUNDLE_ID@|$BUNDLE_ID|g" \
    -e "s|@VERSION@|$VERSION|g" \
    -e "s|@BUILD_NUMBER@|$BUILD_NUMBER|g" \
    packaging/macos/Info.plist.in > "$APP/Contents/Info.plist"

plutil -lint "$APP/Contents/Info.plist" >/dev/null

IDENTITY="${APPLE_SIGNING_IDENTITY:--}"
codesign --force --sign "$IDENTITY" "$APP" >/dev/null 2>&1 || echo "warning: signing failed, the bundle is still runnable locally"

# Finder and the Dock cache icons per path. Registering the fresh bundle makes
# an icon change show up without logging out.
LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
[ -x "$LSREGISTER" ] && "$LSREGISTER" -f "$APP" || true

echo "built $APP"

# Release artifacts. Debug builds skip them: they only slow down iteration.
if [ "$PROFILE" = "release" ]; then
    ARTIFACT="OxTuner-$VERSION-macos-$ARCH"
    ZIP="$DIST/$ARTIFACT.app.zip"
    DMG="$DIST/$ARTIFACT.dmg"

    # ditto keeps the signature intact, which zip would not.
    (cd "$DIST" && ditto -c -k --keepParent "$APP_NAME.app" "$ARTIFACT.app.zip")
    echo "built $ZIP"

    STAGE="$(mktemp -d)"
    trap 'rm -rf "$STAGE"' EXIT
    cp -R "$APP" "$STAGE/"
    ln -s /Applications "$STAGE/Applications"
    # hdiutil's own size estimate once came out too small ("No space left on
    # device"), so the size is given with room to spare. It also fails now
    # and then with "Resource busy", hence the retries.
    size=$(( $(du -sm "$STAGE" | cut -f1) * 2 + 20 ))
    for attempt in 1 2 3; do
        if hdiutil create -volname "$APP_NAME" -srcfolder "$STAGE" -ov \
            -fs HFS+ -size "${size}m" -format UDZO "$DMG"; then
            break
        fi
        [ "$attempt" = 3 ] && exit 1
        sleep 10
    done
    codesign --force --sign "$IDENTITY" "$DMG" >/dev/null 2>&1 || echo "warning: DMG signing failed"
    echo "built $DMG"
fi

if [ "$OPEN_AFTER" -eq 1 ]; then
    open "$APP"
fi
