# Code Profile Manager

A CLI for managing profiles in a **VS Code OSS–based editor** (VSCodium, VS Code,
Cursor) from declarative **TOML** config. Define settings and extensions once —
globally, in reusable groups, and per profile — then push them into the editor, pull
editor changes back, or reconcile both directions with conflict resolution.

> Status: working MVP (settings + extensions). See [`PLAN.md`](./PLAN.md) for the full
> design and what's still deferred.

## Why

I run one profile per language/framework (plus separate profiles for work projects).
Some extensions and settings are common to all profiles, some to most. This tool keeps
them consistent without hand-editing each profile, while respecting VS Code's "use
default" inheritance (e.g. profiles that share the default keybindings). The storage
layout is intended to support both local use and managed app-home directories for
sandboxes, remote systems, or shared profile baselines.

## v1 scope

- **Editors:** discovered by binary + `product.json` (handles forks generically); tested
  against **Code - OSS** and **VSCodium**. Per-editor config; `--editor` selects one by
  product name, application name, generated alias, or user alias.
- **Resources:** settings + extensions.
- **Config:** TOML — `[global]`, reusable `[groups.*]`, the built-in `[default]` profile,
  and named `[profiles.*]`. The interactive flow (and `init`) can **consolidate** settings
  and extensions shared across your profiles into `[global]` for you.
- **Interactive by default:** run with no arguments → pick an editor (or enter a custom
  path), optionally create a config from the editor's current profiles, then a menu:
  Sync / overwrite profiles from config / overwrite config from profiles / exit. Each is
  also a direct subcommand (`status`/`pull`/`push`/`sync`) for scripting.
- **Sync:** 3-way with per-item conflict resolution (keep editor / keep repo). You're
  prompted to close the editor before any write.
- **App home:** default state lives under the platform app config directory, with
  `--app-dir` for sandboxes/remote-mounted homes and `--config` for an exact editor
  config override.
- **Extensions:** adds are tiered — shared pool → vendored copy → editor CLI (no
  marketplace lookups of our own). Local **VSIX-source** extensions are vendored into
  the app home on pull/sync and restored from there on push, so a
  config is portable even for extensions that aren't on any marketplace. IDs enter a
  config by hand or via `pull`.
- **Interactive extras:** the menu can consolidate shared settings/extensions into
  `[global]` and switch the target editor without restarting.

## Usage

```sh
# Discover installed editors
code-profile-manager detect

# Inspect a selected editor's profiles (read-only)
code-profile-manager --editor vscodium list-profiles

# Create a config from an editor's current profiles
code-profile-manager --editor "Code - OSS" init

# See what would change, then apply
code-profile-manager --editor code-oss status
code-profile-manager --editor code-oss --dry-run push
code-profile-manager --editor code-oss push

# Reconcile both directions (prompts on conflict; or --prefer editor|repo)
code-profile-manager --editor code-oss sync

# No subcommand → interactive wizard + menu
code-profile-manager
```

`code-pm` is installed as the shorthand binary.

Selectors are case-insensitive and normalize punctuation, so `VSCodium`, `vscodium`,
`codium`, `Code - OSS`, `code-oss`, and `codeoss` all work when they identify a
discovered editor. Launcher names such as `code` are aliases only for the product they
actually resolve to on the current machine. `--profile <name>` limits an operation to
one profile.

**Where files live.** By default, application files live under:

```text
Linux:   $XDG_CONFIG_HOME/code-profile-manager/
         fallback ~/.config/code-profile-manager/
macOS:   ~/Library/Application Support/code-profile-manager/
Windows: %LOCALAPPDATA%\code-profile-manager\
```

The default layout is:

```text
config.toml
editors/<editor>.toml
snapshots/<editor>.snapshot.json
backups/<timestamp>/
vendor/extensions/
```

`config.toml` stores app preferences such as `default_editor` and known editor aliases.
The per-editor TOML files under `editors/` hold managed profile state. Use
`--app-dir <dir>` to move the whole app home, or `--config <path>` to operate on one
specific editor config. With `--config` alone, derived state stays next to that config:

```text
.code-pm/
  snapshots/<editor>.snapshot.json
  backups/<timestamp>/
  vendor/extensions/
```

### Behavior notes

- **`push`** is non-destructive: it sets the config's settings keys and installs missing
  extensions, but does not delete editor-only settings or uninstall extras. **`sync`** is
  the bidirectional path that also propagates removals (gated by the snapshot).
- Code - OSS and VSCodium share one extension pool (`~/.vscode-oss/extensions`), and the
  **Default** profile's extension list is that pool's own `extensions.json`. Be aware that
  syncing the Default profile's extensions can affect both editors.

## Safety

The editor **must be closed** while writing — it owns these files and will overwrite
changes on exit. Mutating commands detect a running editor and refuse without
`--force`, write atomically, take backups, and
support `--dry-run`.

## Roadmap

Working today: settings + extensions, across the built-in Default and named profiles,
with consolidation and VSIX vendoring. Planned (see [`PLAN.md`](./PLAN.md) for detail):
keybindings / snippets / tasks / MCP, a destructive `--prune` push, schema-assisted
config editing, and a GUI over the same engine.

## Building

```sh
cargo build      # binaries at target/debug/code-profile-manager and target/debug/code-pm
cargo test       # unit tests
cargo clippy --all-targets
```

Requires the Rust toolchain pinned in `rust-toolchain.toml`.

The repo also has a `justfile` for common development commands:

```sh
just check
just dev status
just init
just sync Rust
```

The `dev`/`init`/`status`/`push`/`pull`/`sync` recipes default to **Code - OSS** and
store local run artifacts under `run/`.

## Testing

CI runs formatting, clippy with warnings denied, and unit tests on Ubuntu, Windows, and
macOS. The current fixture tests cover cross-platform editor paths, fake
`product.json` install discovery, fake profile registries, and temp-backed
`push`/`pull`/`sync` engine flows without requiring a real editor installation.

There is also a manual VSCodium smoke workflow for the real editor CLI extension-install
fallback:

```sh
gh workflow run vscodium-smoke.yml
```

It is intentionally manual rather than scheduled or PR-gated.
