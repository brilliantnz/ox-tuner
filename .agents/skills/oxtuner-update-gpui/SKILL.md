---
name: oxtuner-update-gpui
description: 'Update the gpui-kit stack in Ox Tuner to a requested version or to latest. Use when the user says "update gpui", "bump gpui-kit to 0.11.0", "use oxtuner-update-gpui to latest", or asks to adopt a gpui-kit release. Covers release-note and breaking-change triage, the exact workspace pin, Cargo.lock and one-GPUI verification, porting upstream fixes into the vendored dock/tab widgets in oxtuner-ui, theme-schema alignment, the version sweep across text and comments, the full cross-platform check set, and the Rust review/PR loop.'
---

# Ox Tuner: update gpui-kit

`gpui-kit` is the workspace's only UI dependency. It re-exports GPUI, GPUI Base,
GPUI Component and the assets crate, and the workspace pins it to an exact
version so exactly one GPUI snapshot exists in the tree (the one-GPUI pinning
rule). Updating it
is therefore one dependency change plus a set of migrations in our own code and
vendored widgets, not a `cargo update` sweep.

Read `AGENTS.md` first. This skill adds the gpui-kit-specific procedure; the
repository rules for review, checks and commits still apply.

## 0. Parse the request

The request names a version or says `latest`:

- `use oxtuner-update-gpui to 0.11.0` / `to v0.11.0` — target that exact tag.
- `use oxtuner-update-gpui to latest` — resolve the newest non-prerelease
  release. Do not silently take a prerelease; ask if the newest one is a
  prerelease or a yanked version.
- No version — ask, do not guess.

Record the starting state before touching anything:

```sh
grep -n '^gpui-kit = ' Cargo.toml
cargo tree -p oxtuner-ui --depth 1 | rg 'gpui|oxtuner-ui'
cargo tree -i gpui-pre --depth 1
git status --short
```

Read the pin, the installed versions and the vendored base from the repository
itself, never from this skill: the workspace manifest for the pin,
`Cargo.lock` / `cargo tree` for the installed `gpui-kit`, `gpui-component`,
`gpui-pre` and `gpui-fast`, and the `Vendored from ...` headers for the base the
widget copy came from.

Also confirm the version is real before planning:

```sh
cargo info gpui-kit@<version>
```

If crates.io does not serve it, stop; a Git tag alone is not a usable pin.

## 1. Read every release note on the way to the target

The release notes are the upgrade checklist. Read the target release and every
release between the current pin and the target, not just the target: a breaking
change can land in an intermediate version.

```sh
gh release list -R longbridge/gpui-kit --limit 30
gh release view v<version> -R longbridge/gpui-kit \
  --json tagName,name,isPrerelease,publishedAt,url,body
gh release view v<version> -R longbridge/gpui-component \
  --json tagName,name,isPrerelease,publishedAt,url,body
```

`gpui-kit` and `gpui-component` publish the same versions from the same
repository. The component release is what the vendored widgets are compared
against.

Categorise the notes before editing:

| Category | Action |
| --- | --- |
| Breaking changes | Find every affected call site in the workspace and plan the migration. |
| New APIs | Only adopt when the app needs them; do not add unused surface. |
| Behavior changes (no API break) | Verify the affected screens and update any text that records the old behavior. |
| Fixes in `dock`, `tabs`, `tab`, `DockSkin` | Candidate ports into `crates/oxtuner-ui` (step 4). |
| Fixes in components we use | Verify the app benefits; add a regression test if the fix changes our behavior. |
| Dependencies | Note the new `gpui-pre` / `gpui-fast` snapshots and any new default features. |
| Skills / version references | Refresh the installed skills with `npx skills@latest update` and sweep the current-version text. |

### Refresh the installed gpui-kit skills

When the release ships updated `gpui-kit` skills or design guides, refresh the
installed copy with the workspace skill manager; do not edit those files by
hand:

```sh
npx skills@latest update
# pnpm users:
pnpx skills@latest update
```

That updates `.agents/skills/gpui-kit*` and `skills-lock.json`. Re-read the
updated skill before continuing, because its guidance still defines how the app
is built.

For each release-note item with a PR link, the PR diff is the precise contract:

```sh
gh pr diff <number> -R longbridge/gpui-kit
gh pr view <number> -R longbridge/gpui-kit
```

Do not infer an API from the summary. Read the changed source or PR diff.

### Feature and default-feature changes

`crates/oxtuner-ui` disables default features and asks for
`["assets", "component", "x11"]`; `crates/oxtuner-app` takes the defaults.
The `x11` entry is explicit because the crate disables the defaults: gpui-kit
0.11 made the Linux X11 backend an opt-in default feature, so without it the
backend would be silently lost.

## 2. Move the pin

Edit `[workspace.dependencies]` in the root `Cargo.toml`:

```toml
gpui-kit = "=<version>"
```

Keep the exact `=` pin. Never use a caret or range, never add a direct `gpui`,
`gpui-pre`, `gpui-base` or `gpui-component` dependency to a member. Those
arrive through gpui-kit's re-export; adding one breaks the one-GPUI rule and
`cargo deny`'s bans.

Then update the lockfile and build:

```sh
cargo update -p gpui-kit --precise <version>
cargo check -p oxtuner-ui -p oxtuner-app --all-features
```

If a transitive package still sits at the old version, update it explicitly
rather than hand-editing `Cargo.lock`:

```sh
cargo update -p gpui-component --precise <version>
cargo update -p gpui-kit-assets --precise <version>
```

Verify the invariant before going further:

```sh
cargo tree -d | rg '^gpui'
cargo deny check bans licenses sources
```

Exactly one `gpui-pre` snapshot may appear. If two appear, find the dependency
that pulled the extra one and remove or realign it; do not proceed with a
duplicate.

## 3. Compile the workspace and migrate the app

```sh
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo nextest run -p oxtuner-ui -p oxtuner-app --all-features --locked
```

Fix the code; never silence a real break with `#[allow]` or a local shim. The
areas that usually change:

- `crates/oxtuner-app`: window options and titlebar
  (`src/app/window.rs`, `src/layout/titlebar.rs`), theme installation
  (`src/theme.rs`), settings and dialogs (`src/settings/`), layout and
  scrolling (`src/layout/`), keybindings (`src/app/shortcuts.rs`).
- `crates/oxtuner-ui`: chart/curve/table/indicator widgets and the vendored
  dock/tabs. UI tests and the frame-budget tests are the contract for these.
- GPUI API migrations: renamed imports/methods, `IntoElement` blanket impls,
  `Path` generics, `WindowBounds`/`Bounds` signatures, runtime `Element`
  changes, `FillOptions`, focus and event APIs.

When an API was replaced, migrate every call site in one change and update the
comment that named the old shape. When a release removes a feature the app
used, find the new supported path in the pinned source before inventing a
workaround.

## 4. Port upstream fixes into the vendored widgets

`crates/oxtuner-ui` owns a vendored copy of the dock skin and tab bar so the
workspace owns its chrome. The copy must follow upstream fixes without becoming
a fork:

- Vendored files: `src/dock/{mod,panel,tab_panel,dock,invalid_panel}.rs` and
  `src/tabs/{mod,tab,tab_bar}.rs`.
- Read `crates/oxtuner-ui/NOTICE` first: it records the base version, the
  ported fixes and the deliberately skipped features.
- After the bump, read the now-installed `gpui-component` version from
  `Cargo.lock` / `cargo tree -p oxtuner-ui | rg gpui-component`. The vendored
  base recorded in the headers and `NOTICE` must match that installed version
  when the update is done: diff and port against the installed source, never
  against an older memory of it.
- Locate the installed upstream source:

  ```sh
  cargo tree -p oxtuner-ui | rg 'gpui-component|gpui-kit'
  find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -maxdepth 2 \
    -type d -name "gpui-component-<installed-version>"
  ```

  Then diff each vendored file against its upstream counterpart, and read the
  release-note PRs that touch `dock`/`tabs` directly. A file-by-file diff
  against the recorded base version is more useful than against the new one
  alone, because our copy carries adaptations; the finished copy, however, is
  based on the installed version.

Port what the app actually uses: layout, tab rendering, drag/drop, panel
frames, icon sizing, focus and keyboard behavior where the vendored copy has a
matching concept. Skip features that are out of scope for this app (the
left-to-right-only shell, the roving tab focus model, app-owned bottom panels)
and record each skip in `NOTICE` with the reason instead of silently dropping
it. If the upstream diff is small, port the relevant changes; if it is large,
rebase the file on the installed source and re-apply our adaptations (imports
from `gpui_kit`, `crate::dock`/`crate::tabs` module paths, the skin-driven
variant and pick callback, plain-string chrome labels).

Update, in the same change:

- the `Vendored from gpui-component ...` header on every touched vendored file,
  naming the installed version;
- `crates/oxtuner-ui/NOTICE` (installed version, date, ported PRs/fixes, skips);
- any comment that names a component version, so it follows the installed one;
- the vendored modules' tests, and the `oxtuner-ui` frame-budget tests.

Do not re-vendor wholesale unless the upstream diff is unmanageably large; the
crate's contract is "close to upstream" so a later upgrade can keep diffing.

## 5. Align the theme with the release

`crates/oxtuner-app/src/theme.rs` compiles in
`crates/oxtuner-app/themes/oxtuner.json`. The JSON pins `$schema` to a tagged
release; update `$schema` to the new tag.

Run the theme guard tests:

```sh
cargo nextest run -p oxtuner-app --all-features --locked theme::
```

A new `ThemeConfigColors` token fails
`every_theme_token_is_defined_in_the_bundled_set`. Add the token to both themes
with real values, keeping the existing button contrast floor and the
"light/dark default" contract. Remove tokens the release deleted rather than
leaving inert keys; unknown keys are ignored, which makes stale entries easy to
miss. If `Theme::apply_config`, `ThemeRegistry` or `ThemeMode` changed, adapt
`theme.rs` and the startup ordering in `main.rs` (globals first, then install
the pair, then apply the stored preference).

If `gpui-kit-assets` changed icon names, the compiler catches the `IconName`
usages; update `src/layout/` and `src/settings/` where the icon set is used.

## 6. Sweep every version reference

Every text, comment, header, README or configuration value that states the
current `gpui-kit` / `gpui-component` version must name the version that is now
installed. Search the workspace:

```sh
rg -n "gpui-kit|gpui-component|gpui-pre|gpui-fast|GPUI Kit [0-9]|v0\.[0-9]+\.[0-9]+" \
  --glob '!target/**' --glob '!Cargo.lock'
```

Update at least:

- the workspace pin and any comment that names a crate version;
- `crates/oxtuner-ui/NOTICE` and every vendored file header;
- `crates/oxtuner-app/themes/oxtuner.json` — the `$schema` tag;
- README installation links and commands;
- comments and prose that describe current behavior or the current version.

A statement about what the current build does must match the installed version.
Genuinely historical notes keep their original date; add the re-check against
the installed version instead of leaving a stale "current" claim.

Do not rely on a version written in this skill. The installed versions in
`Cargo.lock` are the authority.

## 7. Run the required checks

Focused loop while implementing:

```sh
cargo fmt --all --check
cargo clippy -p oxtuner-ui -p oxtuner-app --all-targets --all-features --locked -- -D warnings
cargo nextest run -p oxtuner-ui -p oxtuner-app --all-features --locked
```

Full pre-PR set (from `AGENTS.md`):

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo nextest run --workspace --all-features --locked
cargo test --workspace --all-features --locked --doc
cargo deny check
pnpm exec oxfmt --check
```

UI-specific checks:

- The `oxtuner-ui` frame-budget tests must pass. A release that changes render
  cost is a performance change: run the matching benches before and after on
  the same machine and report absolute and percentage deltas. Do not loosen a
  budget without the owner's decision.
- Run the app and smoke-test the surfaces a UI release can break:

  ```sh
  cargo run -p oxtuner-app -- --sim
  ```

  Open a project, switch System/Light/Dark, resize the window (including the
  1600×980 minimum), drag/dock panels, switch tabs, scroll a table and a
  sidebar, open Settings, and on macOS check the traffic lights and titlebar.
- Cross-platform: macOS locally, Windows and Linux through CI
  (`.github/workflows/platforms.yml` and `ci.yml`). Watch Linux X11/Wayland
  feature changes and Windows titlebar/keybinding changes especially.
- No INI, protocol, tune or project code should change for a gpui-kit update.
  If it does, the same skill does not cover that migration; treat it as a
  separate feature and run the corresponding golden/property/fuzz work.

## 8. Review and deliver

All work goes through a branch and a pull request. Never commit the update
directly to the default branch and never push to it.

1. Create a new branch from the current default branch before the first edit:

   ```sh
   git switch main
   git pull --ff-only
   git switch -c deps/gpui-kit-<version>
   ```

   Use a focused name such as `deps/gpui-kit-0.12.0` (or
   `chore/gpui-kit-<version>`); one branch per update.

2. Run a focused Rust review (`rust-reviewer`) on the dependency bump, the API
   migrations, the vendored ports and the theme changes.
3. Fix every Must fix and Should fix, then the reasonable Consider findings.
   Repeat until the review is clean.
4. Commit with Conventional Commits, one concern per commit: dependency pin +
   lockfile, app API migration, vendored widget ports, theme/schema, version
   sweep. Inspect staged paths; never stage `.agents/` unless the skill refresh
   is part of the change, local `.cargo/config.toml`, or untracked notes.
5. Push the branch, fill `.github/PULL_REQUEST_TEMPLATE.md`, and open the pull
   request **as a draft**. Run the PR review loop against the PR number, and
   only after it is clean mark the PR ready. Do not merge it yourself unless
   the user explicitly asks.

The same rule applies to changes to this skill: start a branch for the skill
edit and deliver it through a pull request, never as a commit on the default
branch.

Final report to the user:

- old → new `gpui-kit` version and the resolved `gpui-pre`/`gpui-fast` snapshots;
- every breaking change from the notes and how it was handled;
- new APIs adopted (and why) or deliberately not adopted;
- each vendored fix ported into `oxtuner-ui`, each skip with its reason;
- theme tokens/schema changes;
- text and comments updated;
- checks run and their results, plus any benchmark deltas;
- remaining risks or follow-ups.

## Guardrails

- One GPUI in the tree. No direct `gpui*` dependency outside gpui-kit.
- Exact pin (`=x.y.z`), committed `Cargo.lock`.
- Never edit sources under `~/.cargo/registry`.
- Every change reaches the default branch through a pull request on its own
  branch. Never commit or push the update (or an edit to this skill) directly
  to the default branch.
- Never bump a format constant (`SETTINGS_SCHEMA_VERSION`,
  `THEME_SCHEMA_VERSION`, `STATE_SCHEMA_VERSION`, `PARSER_VERSION`,
  `CACHE_FORMAT_VERSION`) for a dependency update. Format constants stay at
  their initial value.
- Never bypass ECU write, read-back or burn guardrails; a UI dependency update
  has no reason to touch them.
- If the target is missing from crates.io, the release notes are missing for a
  breaking range, or two GPUI snapshots appear in the graph, stop and report
  instead of forcing the upgrade.
