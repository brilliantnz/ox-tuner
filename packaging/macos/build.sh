#!/usr/bin/env bash
#
# Assemble a runnable Ox Tuner.app for macOS.
#
# A plain `cargo build` produces a bare executable, and macOS only shows a real
# application icon for a bundle. This wraps the binary in one, with the Info.plist
# keys and both icon formats the system expects.
#
# Usage:
#   ./build.sh                 release build into dist/
#   ./build.sh --debug         debug build, quicker while developing
#   ./build.sh --debug --open  build and launch it
#   ./build.sh --open          release build and launch it

set -euo pipefail

cd "$(dirname "$0")/../.."
ROOT="$(pwd)"

APP_NAME="Ox Tuner"
EXECUTABLE="oxtuner"
BUNDLE_ID="com.oxtuner.OxTuner"
DIST="$ROOT/dist"
APP="$DIST/$APP_NAME.app"

PROFILE="release"
OPEN_AFTER=0

for argument in "$@"; do
    case "$argument" in
        --debug) PROFILE="debug" ;;
        --release) PROFILE="release" ;;
        --open) OPEN_AFTER=1 ;;
        *) echo "error: unknown argument: $argument" >&2; exit 2 ;;
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

if [ "$PROFILE" = "release" ]; then
    cargo build --locked --release -p oxtuner-app
    BINARY="$ROOT/target/release/$EXECUTABLE"
else
    cargo build --locked -p oxtuner-app
    BINARY="$ROOT/target/debug/$EXECUTABLE"
fi

[ -x "$BINARY" ] || { echo "error: $BINARY was not produced" >&2; exit 1; }

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

# Ad-hoc signing is enough for a local run and keeps macOS from treating the app
# as damaged after it is copied around. Public releases need a Developer ID.
codesign --force --sign - "$APP" >/dev/null 2>&1 || echo "warning: ad-hoc signing failed, the bundle is still runnable locally"

# Finder and the Dock cache icons per path. Registering the fresh bundle makes
# an icon change show up without logging out.
LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
[ -x "$LSREGISTER" ] && "$LSREGISTER" -f "$APP" || true

echo
echo "built $APP"

if [ "$OPEN_AFTER" -eq 1 ]; then
    open "$APP"
fi
