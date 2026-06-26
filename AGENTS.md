# AGENTS.md

Agent guide for the **Code Profile Sync** repo. Read [`PLAN.md`](./PLAN.md) for the
full design before non-trivial work.

## What this is

A Rust CLI (GUI later) that syncs settings + extensions across profiles of a VS Code
OSS–based editor against a declarative TOML config. v1: settings + extensions only.
Test targets are **Code - OSS** and **VSCodium** (both installed on the dev machine);
VS Code/Cursor are not installed.

**Editors are discovered by binary, not config dir.** Resolve launchers on PATH
(`code`, `code-oss`, `codium`, `vscodium`, …) via `readlink -f`, then read
`resources/app/product.json` for identity (`nameShort` → user-data dir,
`dataFolderName` → extensions dir, `applicationName` → CLI). The command name is
ambiguous — `code` may be Code - OSS *or* VS Code depending on the machine. Ignore
leftover config dirs from uninstalled editors.

## Build / check

```sh
cargo build
cargo clippy --all-targets
cargo fmt
cargo test
```

Toolchain is pinned in `rust-toolchain.toml` (don't bump without reason).

## Code style — non-negotiable

- Follow the user-global **`rust-code-style`** skill.
- Lints are strict (see `Cargo.toml`): `unwrap_used`, `expect_used`, `todo`,
  `unimplemented`, `panic`-adjacent, `indexing_slicing`, `arithmetic_side_effects`,
  `as_conversions`, `print_stdout`/`print_stderr` all warn; `unsafe_code` is **denied**.
  Use `?` with explicit error types (`thiserror`), checked arithmetic, and a real
  output/logging layer instead of `println!`/`eprintln!`. If you must allow a lint,
  use `#[allow(..., reason = "…")]` (the repo requires a reason).
- Match the surrounding code's idioms, naming, and comment density.

## Domain landmines (get these wrong and you corrupt a user's editor)

- **Editor must be closed for writes.** It owns `storage.json`/`extensions.json` and
  overwrites on exit. Detect a running editor; refuse without `--force`.
- **`useDefaultFlags`** in `storage.json` mean a profile *inherits* a resource from the
  Default profile. Never write a profile-local file for an inherited resource. All real
  profiles on the dev's machine inherit `keybindings` — treat inheritance as core.
- **Extensions are a shared install + per-profile membership list.** `extensions.json`
  entries with no corresponding folder in the shared dir dangle. `location.path` is
  machine-specific; portable identity is `identifier.id` + `version`. Writes are
  **hybrid** (direct-edit if on disk, else editor CLI). Local `.vsix` files (vendored
  in `vendor/extensions/`) install via the editor CLI; their `source` is local/vsix,
  not `gallery`, so keep them out of marketplace-update logic. Removing from a profile
  must not delete the shared folder.
- **The Default profile** lives at `User/` root, not under `User/profiles/`.
- **Shared extensions pool:** Code - OSS and VSCodium both default `dataFolderName` to
  `.vscode-oss`, so they share `~/.vscode-oss/extensions` while keeping separate
  per-profile membership. Don't assume one extensions dir = one editor; GC must check
  membership across all editors resolving to that dir before pruning.
- **Settings files are JSONC** (comments, trailing commas) — parse tolerantly.
- **Always write atomically** (temp + rename), back up before first write, and support
  `--dry-run`.

## Conventions

- Use the **`git-conventions`** skill for commits; **`agent-attribution`** for any
  user-visible content/commits.
- Project paths contain spaces — quote them; never `cd` to CWD; never backslash-escape.
- Keep `PLAN.md` in sync when the design changes.
