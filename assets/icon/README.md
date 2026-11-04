# Ox Tuner icons

The Icon Composer document `AppIcon.icon` is the single source of truth. One
document feeds every platform, and `build-icons.sh` turns it into the files each
system expects.

## Why the name is `AppIcon`

The document name becomes the asset name, and that name has to match in three
places:

| Place | Value |
| --- | --- |
| Icon Composer document | `AppIcon.icon` |
| `actool --app-icon` | `AppIcon` |
| `Info.plist` `CFBundleIconName` | `AppIcon` |
| `Info.plist` `CFBundleIconFile` | `AppIcon` (resolves to `AppIcon.icns`) |

`AppIcon` is what Xcode uses by default for an app icon set, so anyone who opens
the project in Xcode later sees the expected name. `AppIcons` is not a
convention and buys nothing. What matters is that all four agree, otherwise
macOS silently falls back to the generic app icon.

## Files

| Target | File | Consumed as |
| --- | --- | --- |
| macOS 26 and later | `AppIcon.icon` | Layered icon with Light, Dark and Tinted built in |
| macOS 26 and later | `Assets.car` | The same document, already compiled by `actool` |
| macOS 15 and earlier | `AppIcon.icns` | Flat 16 to 1024 bitmap with the classic margin and shadow |
| Windows | `oxtuner.ico` | Multi-size icon, 16 to 256 |
| Linux | `oxtuner.png` | One 512 image for packagers that take a single file |
| Linux | `hicolor/<size>/apps/oxtuner.png` | Freedesktop icon theme, 16 to 512 |

`source/` holds the two Icon Composer exports the script needs. `square-1024.png`
is the square art with the rounded background baked in, used for Windows and
Linux. `macos-legacy-1024.png` is the macOS style export with the transparent
margin and shadow, used for the `.icns`.

## macOS app bundle

Ship both macOS artifacts in the same bundle. They do not conflict. The system
prefers the layered icon on 26 and later and falls back to the flat one before
that.

```
Ox Tuner.app/
  Contents/
    Info.plist
    Resources/
      Assets.car
      AppIcon.icns
```

```xml
<key>CFBundleIconName</key>
<string>AppIcon</string>
<key>CFBundleIconFile</key>
<string>AppIcon</string>
```

`CFBundleIconName` points at the asset catalog entry and is what macOS 26 reads.
`CFBundleIconFile` keeps older systems working. A bundle with only `Assets.car`
shows the generic icon on macOS 15 and earlier. A bundle with only the `.icns`
looks flat on 26 and later.

If the bundler accepts an Icon Composer document directly, hand it `AppIcon.icon`
and let it run `actool` itself. If it does not, ship the compiled `Assets.car`
that this script already produced. Both routes end with the same catalog entry.

## Windows

Link the icon into the executable with a resource script and `embed-resource`:

```rc
IDI_ICON1 ICON "assets/icon/oxtuner.ico"
```

## Linux

Install the tree into the icon theme and point the desktop entry at the name:

```sh
install -Dm644 hicolor/512x512/apps/oxtuner.png \
  /usr/share/icons/hicolor/512x512/apps/oxtuner.png
```

```ini
[Desktop Entry]
Name=Ox Tuner
Exec=oxtuner
Icon=oxtuner
Terminal=false
Type=Application
```

## Rebuilding

```sh
cd assets/icon
./build-icons.sh
```

Requirements: Xcode 26 or newer with its licence accepted, plus Pillow for the
Windows and Linux rasters (`python3 -m pip install pillow`). The script finds
Xcode on its own even when `xcode-select` points at the Command Line Tools.

Re-export `source/square-1024.png` and `source/macos-legacy-1024.png` from Icon
Composer after changing the document, then rerun the script.
