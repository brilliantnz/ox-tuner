# macOS packaging

`build.sh` wraps the `oxtuner` binary in a runnable `Ox Tuner.app` under `dist/`.

```sh
./packaging/macos/build.sh                 # release bundle
./packaging/macos/build.sh --debug         # debug bundle, faster to iterate
./packaging/macos/build.sh --debug --open  # build and launch
```

The bundle gets the release binary, `Info.plist` rendered from
`packaging/macos/Info.plist.in`, both icon artifacts from `assets/icon/`, an
ad-hoc signature, and a LaunchServices registration so Finder picks up an icon
change immediately.

## Why the bundle matters for the icon

macOS reads the icon from the bundle. A bare executable started with
`cargo run -p oxtuner-app` is not a bundle, so the Dock shows the generic
executable icon no matter what sits in `assets/icon/`. There is no Info.plist to
read and no `Resources` folder to look in.

When working on the icon, build the bundle and open that instead:

```sh
./packaging/macos/build.sh --debug --open
```

Iterating on code itself still goes through `cargo run`, which is faster. The
icon difference is expected there.

## Bundle layout

```
dist/Ox Tuner.app/
  Contents/
    Info.plist
    MacOS/oxtuner
    Resources/Assets.car      macOS 26 and later
    Resources/AppIcon.icns    macOS 15 and earlier
```

## Identity

| Key | Value |
| --- | --- |
| `CFBundleIdentifier` | `com.oxtuner.OxTuner` |
| `CFBundleName` | `Ox Tuner` |
| `CFBundleShortVersionString` | workspace version from `Cargo.toml` |
| `CFBundleVersion` | commit count |

The identifier matches `app_identity()` in `crates/oxtuner-app/src/app/mod.rs`.
Changing one without the other splits window state and notification identity.

## Icon sources

`assets/icon/build-icons.sh` produces `Assets.car` and `AppIcon.icns` from the
Icon Composer document. Run it after changing the icon, then rebuild the bundle.

## Not covered yet

Signing with a Developer ID, notarization, and DMG creation. The current
signature is ad hoc, which covers local runs only and does not let another Mac
open the app without a Gatekeeper warning.
