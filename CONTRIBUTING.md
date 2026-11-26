# Contributing to Ox Tuner

Thank you for looking at the project. Ox Tuner is an experimental early-stage
ECU tuner written in Rust with a native GPUI interface. It works, it is tested,
and it is not finished: public APIs and behavior may still change without
notice, and you should expect rough edges and bugs. This document is the
shortest path from a clone to a reviewed pull request.

## Before you start

**Licence.** GPL-3.0-or-later. By contributing you agree your work is published
under the same licence.

**Safety above convenience.** The engine layer refuses rather than guesses.
Never add a code path that bypasses the write safeguards: explicit writes,
read-back verification, no automatic burn, and a signature mismatch puts the
session in read-only mode. A review will reject a change that trades one of
those away for a smoother workflow.

**Scope.** Ox Tuner targets open-source ECU platforms and MegaSquirt-dialect
firmware, including Speeduino and rusEFI. INI definitions are user-supplied and
never bundled. Do not commit real vendor INI, tune (`.msq`), or log files to
this repository.

## Setup

You need the Rust toolchain pinned in `rust-toolchain.toml` (rustup installs it
for you), and Node.js 24 or newer with `pnpm` for the non-Rust formatting
check and the `clean:*` scripts (use the pnpm version pinned in
`package.json`).

```sh
cargo build --workspace
pnpm install
```

Platform dependencies (system libraries for the GUI and serial support,
Xcode tools on macOS, MSVC + CMake on Windows) are listed in the
[Development](README.md#development) section of [README.md](README.md).
Follow that section for your OS before building `oxtuner-app`.

The `clean:*` scripts delete build outputs: `pnpm clean` reclaims `target/`
via `cargo sweep`, while `pnpm clean:all` also wipes the sccache disk cache
and `node_modules`. They never touch your source, projects, or tunes, but
preview first when unsure — `cargo sweep --time 30 --dry-run` and
`node scripts/clean-sccache.mjs --dry-run`.

## The checks

Run these before you push. CI runs the same set on every pull request and every
push to `main`:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo nextest run --workspace --all-features --locked
cargo test --workspace --all-features --locked --doc
cargo deny check
pnpm fmt:check
```

`cargo nextest` is the project's test runner. Install it with
`cargo install cargo-nextest` or `cargo binstall cargo-nextest`.
`cargo test` works too; nextest is faster and its output is easier to read.
Note that nextest does not run doctests, which is why `cargo test --doc`
is a separate step.

`sccache` (>= 0.20) is optional. Install it with `cargo install sccache` or
`cargo binstall sccache`, then uncomment the sccache lines in
`.cargo/config.toml` to share compiler artifacts across checkouts — a fresh
checkout reuses the first build instead of recompiling the GPUI tree. Keep
that change local and do not commit it. Builds work fine without sccache, just
slower on second checkouts. Verify with
`cargo build -p oxtuner-ini && sccache --show-stats`. Never set
`CARGO_INCREMENTAL=1` without `SCCACHE_ALLOW_INCREMENTAL=1`: sccache 0.20
hard-errors on incremental builds without the allow flag.

Parser changes (INI, protocol framing, trace replay) are additionally covered
by short `cargo-fuzz` runs in CI. When you touch one of those parsers, add or
update the corresponding golden, property (`proptest`), and fuzz targets.

## Testing without hardware

Most of the project is testable with no ECU on the desk. The `oxtuner-sim`
crate is a virtual ECU: it answers the protocol commands, holds page memory,
and can be told to misbehave through `oxtuner_sim::Fault` — a partial write, a
refused burn, a slow reply, a cable pulled mid-conversation.

`crates/oxtuner-core/tests/writes.rs` is the example to copy. It drives the
production command path against the simulator, so a refused write is a real
refusal and not a mock agreeing with itself. When you change behaviour that
touches the wire, add a case there: the simulator makes the failure reproducible
in a way a bench ECU never can.

Two kinds of test do need real input files:

- `crates/oxtuner-tune/tests/real_ini.rs` checks the layout and codec against
  real vendor definitions. Those files are not in the repository. Put them
  where the test expects and it runs; without them it skips. Set
  `OXTUNER_INI_REQUIRED=1` to turn a skip into a failure when you want to be
  sure they were found.
- `crates/oxtuner-core/tests/hardware_matrix.rs` needs an ECU. See below.

**A simulator result never stands in for a hardware result.** The two answer
different questions. The simulator says the code does what it intends; the
bench says the controller agrees.

## The Speeduino hardware matrix

`crates/oxtuner-core/tests/hardware_matrix.rs` is the manual regression test
for everything that writes to a controller. It is `#[ignore]`d by default
because it writes a whole tune to a bench ECU and burns it.

Run it after any change to **write, burn, recovery or read-back**, without
exception:

```sh
OX_TUNER_MATRIX_DIR=<fresh-empty-folder> \
  cargo nextest run -p oxtuner-core --run-ignored all -E 'test(hardware_matrix)'
```

It needs a bench ECU with a backed-up tune, write protection on, and an
operator at the bench. The stages run in order and each one has to pass before
the next: baseline read, save and import, write, read-back, burn, cold-boot
verification by page CRC, restore, and a cable pulled during a burn followed by
recovery. Never start with the burn.

`OX_TUNER_MATRIX_DIR` must point at a folder the test owns. It copies the
project there and clears it afterwards, and it refuses to touch a folder that
is not empty and does not carry its ownership marker. Pointing it at your own
project folder is exactly what that check exists to stop.

Treat a green matrix as the only evidence that a write change is safe on real
hardware. A green simulator run is evidence of something else, and the two are
never substituted for one another.

## Adding an ECU definition

The project reads TunerStudio-style INI definitions. It does not bundle any;
the user supplies the one for their controller.

1. **Parse it first.** `oxtuner-ini` turns the dialect into a `Definition`.
   Start with `crates/oxtuner-ini` and the tests there, and add a case for
   whatever your definition does that the existing ones do not.
2. **Check the layout.** `oxtuner-tune` freezes a `Definition` into a
   `TuneLayout`: addresses, shapes, storage types, byte order. Constants that
   share bytes are classified here — an owner with a view, a set of bit fields
   splitting one word, or an overlap with no owner, which is reported and
   refuses the page. Add a layout test for your definition's shape.
3. **Family and probing.** `oxtuner-registry` decides which family a signature
   belongs to and which behaviour the protocol should use. Detection must be
   strict: a loose substring match that turns serial noise into a known family
   is a bug, not a convenience.
4. **Behaviour.** `oxtuner-protocol` holds the per-family conversation. If your
   controller answers differently, add a behaviour rather than a special case
   in a call site.
5. **Prove it.** A new definition needs the layout and codec tests green, a
   simulator case for anything it does differently on the wire, and a run of
   the hardware matrix against a real controller before anyone claims it
   works.

Real shapes beat synthetic fixtures. If a real definition exposes a shape the
tests do not cover, add that shape to the tests. Do not commit a vendor's INI
or tune file.

## Changing protocol, write or burn

This is the part of the project where a mistake costs someone an engine. The
rules are not negotiable and a review will hold you to them.

- **Write explicitly, never implicitly.** Every byte sent to a controller is a
  value someone chose to send.
- **Read back and verify.** A write is not done until the bytes on the
  controller match what was sent.
- **No automatic burn.** Burning is a separate, deliberate action.
- **A signature mismatch is read-only.** The session may read, and may not
  write.
- **Refuse before you talk.** Anything that can be caught before the first byte
  goes on the wire is caught there: a page that cannot be represented, two
  values fighting over one byte, a write to a page the definition cannot
  rebuild. The refusal names the problem and nothing is sent.
- **Per family means per family.** An acknowledgement shape that holds for one
  family is not assumed to hold for another. Where the contract is not yet
  proven against real firmware, the code says so and the behaviour stays
  limited to what has been proven.

When you change one of these, the pull request description has to say what the
old behaviour was, what it is now, and which hardware run covers it.

## Commits and pull requests

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/):
`fix(tune): refuse values that disagree over bytes they share`. One branch, one
concern, and the diff contains only what the pull request describes.

Every pull request uses `.github/pull_request_template.md`. Fill in every
section. Something you did not run gets marked as not run with a short reason;
an empty section is not a completed template.

Keep the branch up to date with `main` by rebasing. Do not force-push or squash
while a review is open, so every change stays visible in the history.

## Rust style

Readability comes before saving vertical space.

- Insert a blank line between logically distinct operations in a function.
  Separate file I/O, validation, state changes and synchronisation.
- Do not add blank lines mechanically after every statement, and do not
  compress sequential operations into a dense block.
- Everything passes `cargo fmt` and `cargo clippy --workspace --all-targets -- -D warnings`.
- No `unwrap` outside tests and `main`. Every `expect`, `assert` and `panic`
  carries a message with the values that matter.
- Public functions take `&str`, `&[T]`, `impl AsRef<_>` or
  `impl IntoIterator`, not `&String` or `&Vec`. No `bool` parameters in public
  signatures: use a two-variant enum or two methods.
- Error types are named `ParseBoolError`, implement `std::error::Error`, and
  say what failed in lowercase with no trailing punctuation. Document every
  failure under `# Errors`.
- Comments explain why, not what. No design journals in code comments.

## The interface

The UI is GPUI, through `gpui-kit`. Two rules apply to every screen:

- **Colour comes from the theme.** No raw hex, no `rgb(...)` in code. Use
  `cx.theme().tokens.X` for anything that fills an area and `cx.theme().X` for
  text, borders and opacity. Edit theme colours only through `Theme::update`.
- **Every scrolling container shows a scrollbar.** Use `Scrollable`, which
  brings scrollbars with tracking and virtualisation, and keep content inset
  inside it so the scrollbar sits on the region edge.

## Benchmarks

Criterion benches live in `crates/<crate>/benches/`. Run them with
`cargo bench --workspace`, or narrow with `cargo bench -p oxtuner-tune parse`.

When your change touches a hot path — INI parsing, project open or save, tune
layout, core summaries, realtime decode, table models, anything that runs per
frame or per edit — run the matching bench before and after and put both
numbers in the pull request. "Faster" without a number is not finished work.

Compare across versions with `--save-baseline <name>` on the older tree and
`--baseline <name>` on the newer one. Numbers are machine-relative, so compare
runs from one machine and never quote a bench result as an absolute promise.

## Where things live

| Crate               | What it does                                                       |
| ------------------- | ------------------------------------------------------------------ |
| `oxtuner-app`       | The binary: window management and application state                |
| `oxtuner-core`      | The headless engine of the application, on OS threads and channels |
| `oxtuner-ini`       | The INI dialect, parsed into a `Definition`                        |
| `oxtuner-project`   | The project folder, the tune file and the load gate                |
| `oxtuner-tune`      | The tune, the values, the page codec and the derived write state   |
| `oxtuner-protocol`  | The conversation: framing, per-family behaviour, retries           |
| `oxtuner-transport` | Serial and the other links, recording and replay                   |
| `oxtuner-registry`  | Definitions, family detection and where definitions come from      |
| `oxtuner-sim`       | The virtual ECU the tests drive                                    |
| `oxtuner-datalog`   | Realtime data logging                                              |
| `oxtuner-encoding`  | Latin-1 and the text handling the wire and the file format share   |
| `oxtuner-log`       | Application logging infrastructure                                 |
| `oxtuner-ui`        | The GPUI interface                                                 |

Three words are used precisely and are not interchangeable:

- **core** is `oxtuner-core` and its thread, the headless engine of the
  application.
- **ECU** is the controller on the other end of the wire.
- **engine** is the internal-combustion engine the ECU controls, and nothing
  else. The rendering stack is never "the engine"; say GPUI, `gpui-kit` or the
  renderer.

Diagrams are Mermaid fenced blocks, never ASCII box art.

## Questions

Open an issue. Describing what you expected, what happened, and which
controller and firmware signature you use is enough to start.
