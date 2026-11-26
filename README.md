# Ox Tuner

**An experimental, native ECU tuning workbench built with Rust and GPUI.**

Ox Tuner is a desktop application for working with Engine Control Units (ECUs). It is designed around a modular, definition-driven architecture, with a strong separation between ECU definitions, project and tune management, protocol handling, hardware communication, and the user interface.

The project targets open-source ECU platforms and MegaSquirt-dialect firmware, including Speeduino and rusEFI.

Built from the ground up in Rust, Ox Tuner aims to provide a maintainable and extensible foundation for modern ECU tuning software.

## Highlights

- **Native Desktop Application.** Built with Rust and GPUI, using [gpui-kit](https://github.com/longbridge/gpui-kit).
- **Definition-Driven Architecture.** Parses TunerStudio-style INI definitions to describe ECU configuration, parameters, commands, tables, gauges, and dialogs.
- **Tune Management.** Handles TunerStudio `.msq` projects and provides structured tune editing and change tracking.
- **Modular Protocol Stack.** Separates ECU-specific behavior, protocol implementation, and byte-level transport.
- **Realtime Data.** Decodes ECU telemetry and provides the foundation for realtime monitoring and visualization.
- **ECU Simulation.** Includes a deterministic virtual ECU with fault injection, allowing communication and recovery scenarios to be tested without physical hardware.
- **Recording and Replay.** Supports transport recording and replay for debugging and repeatable testing.

## Architecture

Ox Tuner is organized as a modular Rust workspace, with responsibilities separated into dedicated crates.

| Crate               | Responsibility                                                              |
| ------------------- | --------------------------------------------------------------------------- |
| `oxtuner-app`       | Application entry point, window management, and application state           |
| `oxtuner-core`      | Headless application core, ECU session, commands, and realtime coordination |
| `oxtuner-ini`       | TunerStudio-style INI definition parser                                     |
| `oxtuner-project`   | Project and `.msq` file management                                          |
| `oxtuner-tune`      | Tune data model, editing, diffing, and write-state tracking                 |
| `oxtuner-protocol`  | ECU commands, framing, and firmware-specific behavior                       |
| `oxtuner-transport` | Serial communication, recording, and replay                                 |
| `oxtuner-sim`       | Virtual ECU implementation and fault simulation                             |
| `oxtuner-datalog`   | Realtime data logging                                                       |
| `oxtuner-encoding`  | ISO-8859-1 encode/decode boundaries for TunerStudio formats                 |
| `oxtuner-registry`  | Firmware/definition lookup, signature resolution, and OS cache              |
| `oxtuner-log`       | Application logging infrastructure                                          |
| `oxtuner-ui`        | Reusable native UI components                                               |

The architecture is designed to keep the ECU communication and tuning core independent from the graphical interface, making it easier to test, extend, and maintain individual components.

## Project Status

**Ox Tuner is experimental and under active development.**

It is not production-ready. Public APIs, file formats, and behavior may change
without notice, and bugs should be expected. Hardware validation is limited,
and protocol behavior and tune-writing operations must not be assumed safe or
complete across all target ECUs.

Development and testing should begin with the built-in simulator, followed by controlled bench testing. Never perform tune writes or burns on a running engine.

See [DISCLAIMER.md](DISCLAIMER.md) for safety information and limitations.

## Development

### Prerequisites

Before building Ox Tuner, make sure you have:

- [Git](https://git-scm.com/)
- [Rustup](https://rustup.rs/) for managing Rust toolchains
- [Node.js](https://nodejs.org/) 24 or newer, for the JS tooling
  (`pnpm fmt:check`, the `clean:*` scripts)
- [pnpm](https://pnpm.io/) matching the version pinned in `package.json`
- The platform-specific build dependencies listed below

The required Rust toolchain is pinned in [`rust-toolchain.toml`](rust-toolchain.toml). Rustup automatically installs the pinned toolchain and the `clippy` and `rustfmt` components when you run a Cargo command from the repository.

You do not need to install or select a separate Rust version manually. To verify the active toolchain, run:

```sh
rustc --version
cargo --version
rustup show active-toolchain
```

`sccache` (>= 0.20) is optional. Install it with `cargo install sccache` or
`cargo binstall sccache`, then uncomment the sccache lines in
`.cargo/config.toml` to share compiler artifacts across checkouts — a second
checkout or worktree then reuses the first build instead of recompiling the
GPUI tree. Keep that change local and do not commit it; plain `cargo build`
works without it.

### Platform Setup

<details>
<summary><strong>macOS</strong></summary>

Ox Tuner requires macOS 15 (Sequoia) or later, on Apple Silicon or Intel, and
the Xcode Command Line Tools.

Install the Command Line Tools:

```sh
xcode-select --install
```

Verify the selected developer directory:

```sh
xcode-select -p
```

If you have a full Xcode installation and need to select it explicitly:

```sh
sudo xcode-select --switch /Applications/Xcode.app/Contents/Developer
```

</details>

<details>
<summary><strong>Windows</strong></summary>

Ox Tuner requires Windows 10 or later, the MSVC Rust toolchain, and CMake.

1. Install [Visual Studio 2022 Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) or Visual Studio 2022 or newer.
2. Select the **Desktop development with C++** workload, including the MSVC C++ build tools and a Windows SDK.
3. Install [CMake](https://cmake.org/download/) and make sure `cmake` is available on `PATH`.
4. Install Rust using [rustup](https://rustup.rs/) and use the **MSVC** toolchain. Do not use the GNU toolchain.

</details>

<details>
<summary><strong>Ubuntu / Debian</strong></summary>

Install the build tools and Linux development libraries used by GPUI and the Ox Tuner workspace:

```sh
sudo apt-get update
sudo apt-get install --no-install-recommends -y \
  build-essential \
  clang \
  pkg-config \
  libfontconfig-dev \
  libfreetype-dev \
  libudev-dev \
  libwayland-dev \
  libxkbcommon-dev \
  libxkbcommon-x11-dev \
  libx11-xcb-dev \
  libxcb1-dev \
  libxcb-render0-dev \
  libxcb-shm0-dev \
  libwebkit2gtk-4.1-dev \
  libssl-dev \
  libzstd-dev \
  vulkan-validationlayers \
  libvulkan1 \
  mesa-vulkan-drivers
```

</details>

<details>
<summary><strong>Fedora</strong></summary>

Install the build tools and Linux development libraries:

```sh
sudo dnf install -y \
  gcc \
  gcc-c++ \
  clang \
  make \
  pkgconf-pkg-config \
  fontconfig-devel \
  freetype-devel \
  wayland-devel \
  libxkbcommon-devel \
  libxkbcommon-x11-devel \
  libX11-devel \
  libX11-xcb \
  libxcb-devel \
  webkit2gtk4.1-devel \
  openssl-devel \
  libzstd-devel \
  systemd-devel \
  vulkan-headers \
  vulkan-loader-devel \
  vulkan-validation-layers \
  mesa-vulkan-drivers
```

</details>

<details>
<summary><strong>Arch Linux</strong></summary>

Install the build tools and Linux development libraries:

```sh
sudo pacman -S --needed \
  base-devel \
  clang \
  pkgconf \
  fontconfig \
  freetype2 \
  wayland \
  libxkbcommon \
  libxkbcommon-x11 \
  libx11 \
  libxcb \
  webkit2gtk-4.1 \
  openssl \
  zstd \
  vulkan-headers \
  vulkan-icd-loader \
  vulkan-validation-layers
```

Install the Vulkan driver matching your GPU:

```sh
# Intel
sudo pacman -S --needed vulkan-intel

# AMD
sudo pacman -S --needed vulkan-radeon

# NVIDIA (proprietary driver)
sudo pacman -S --needed nvidia-utils

# NVIDIA (open-source NVK driver, needs compatible hardware and setup)
sudo pacman -S --needed vulkan-nouveau
```

</details>

The Linux package lists follow the [GPUI Kit 0.10.0 installation guide](https://gpui-kit.com/versions/v0.10.0/docs/installation/), supplemented with the X11, Wayland, and font development packages used by this workspace, plus the `libudev` headers needed for serial-port enumeration (`libudev-dev` on Ubuntu/Debian, `systemd-devel` on Fedora, provided by `systemd` on Arch). Ubuntu 24.04 is the documented upstream baseline. Fedora and Arch Linux use their equivalent distribution packages.

On Linux, run Ox Tuner from an active Wayland or X11 graphical session with a working Vulkan driver. Installing the Vulkan loader alone does not provide a GPU driver. The Mesa Vulkan packages provide drivers for supported Intel and AMD GPUs. NVIDIA users need a driver package that provides a compatible Vulkan ICD.

The ALSA development package is not included because the current Ox Tuner application does not enable GPUI's speech feature. If that feature is enabled in the future, install the corresponding package (`libasound2-dev` on Ubuntu/Debian, `alsa-lib-devel` on Fedora, or `alsa-lib` on Arch Linux).

### Clone the Repository

```sh
git clone https://github.com/brilliantnz/ox-tuner.git
cd ox-tuner
```

Rustup reads `rust-toolchain.toml` from the repository root and installs the pinned toolchain automatically when needed.

### Essential Checks

Before submitting a pull request, run the following checks:

**Check Rust formatting**

```sh
cargo fmt --all --check
```

**Run Clippy**

```sh
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

**Run tests**

```sh
cargo test --workspace --all-features --locked
```

These are the essential local checks to run before submitting a pull request.

### Cleaning

`pnpm clean` reclaims `target/` via `cargo sweep` (it shows a dry-run
preview before deleting). `pnpm clean:all` goes further and also wipes the
sccache disk cache and `node_modules`. The node clean scripts only delete
with `--force`, which the pnpm scripts pass for you; run them directly with
`--dry-run` to preview:

```sh
node scripts/clean-sccache.mjs --dry-run
node scripts/clean-node-modules.mjs --dry-run
```

## Run

Requires the toolchain pinned in `rust-toolchain.toml` and the platform dependencies listed in [Development](#development).

Start the application with this command

```sh
cargo run --locked -p oxtuner-app
```

This opens the Ox Tuner window. The first build compiles the full GPUI dependency tree and may take considerably longer than subsequent builds. Later builds are incremental.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE) for details.
