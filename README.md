# Code Profile Sync

A CLI for keeping profiles in a **VS Code OSS–based editor** (VSCodium, VS Code, Cursor)
in sync with a declarative **TOML** config kept in version control. Define settings and
extensions once — globally, in reusable groups, and per profile — then push them into
the editor, pull editor changes back, or reconcile both directions with conflict
resolution.

> Status: planning. See [`PLAN.md`](./PLAN.md) for the full design.

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
- **Extensions:** hybrid writes — edit `extensions.json` directly when the extension is
  already on disk, otherwise fall back to the editor CLI to fetch it.

## Safety

The editor **must be closed** while writing — it owns these files and will overwrite
changes on exit. Mutating commands detect a running editor and refuse without
`--force`, write atomically, take backups, and support `--dry-run`.

## Building

```sh
cargo build
```

Requires the Rust toolchain pinned in `rust-toolchain.toml`.
