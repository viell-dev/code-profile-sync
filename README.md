# Code Profile Sync

A CLI for keeping profiles in a **VS Code OSS–based editor** (VSCodium, VS Code, Cursor)
in sync with a declarative **TOML** config kept in version control. Define settings and
extensions once — globally, in reusable groups, and per profile — then push them into
the editor, pull editor changes back, or reconcile both directions with conflict
resolution.

> Status: working MVP (settings + extensions). See [`PLAN.md`](./PLAN.md) for the full
> design and what's still deferred.

## Why

I run one profile per language/framework (plus separate profiles for work projects).
Some extensions and settings are common to all profiles, some to most. This tool keeps
them consistent without hand-editing each profile, while respecting VS Code's "use
default" inheritance (e.g. profiles that share the default keybindings).

## v1 scope

- **Editors:** discovered by binary + `product.json` (handles forks generically); tested
  against **Code - OSS** and **VSCodium**. Per-editor config; `--editor` selects one.
- **Resources:** settings + extensions.
- **Config:** TOML — `[global]`, reusable `[groups.*]`, and `[profiles.*]`.
- **Interactive by default:** run with no arguments → pick an editor (or enter a custom
  path), optionally create a config from the editor's current profiles, then a menu:
  Sync / overwrite profiles from config / overwrite config from profiles / exit. Each is
  also a direct subcommand (`status`/`pull`/`push`/`sync`) for scripting.
- **Sync:** 3-way with per-item conflict resolution (keep editor / keep repo). You're
  prompted to close the editor before any write.
- **Extensions:** if already installed in the shared pool, membership is added directly
  (works even for extensions not on the editor's marketplace); otherwise the editor's own
  CLI fetches it. No marketplace lookups of our own. IDs enter a config by hand or via
  `pull`.

## Usage

```sh
# Discover installed editors
code-profile-sync detect

# Inspect a selected editor's profiles (read-only)
code-profile-sync --editor VSCodium list-profiles

# Create a config from an editor's current profiles
code-profile-sync --editor "Code - OSS" init

# See what would change, then apply
code-profile-sync --editor "Code - OSS" status
code-profile-sync --editor "Code - OSS" --dry-run push
code-profile-sync --editor "Code - OSS" push

# Reconcile both directions (prompts on conflict; or --prefer editor|repo)
code-profile-sync --editor "Code - OSS" sync

# No subcommand → interactive wizard + menu
code-profile-sync
```

Selectors match an editor's `nameShort` or `applicationName` (e.g. `VSCodium`,
`"Code - OSS"`, `code-oss`). `--config <path>` overrides the default config file
(`<applicationName>.toml` in the working directory). `--profile <name>` limits an
operation to one profile. The snapshot used for 3-way sync lives in
`.code-profile-sync/` next to the config.

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
`--force`, write atomically, take backups (under `.code-profile-sync/backups/`), and
support `--dry-run`.

## Building

```sh
cargo build      # binary at target/debug/code-profile-sync
cargo test       # unit tests
cargo clippy --all-targets
```

Requires the Rust toolchain pinned in `rust-toolchain.toml`.
