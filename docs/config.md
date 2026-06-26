# Config reference

The editor config is a declarative TOML file describing the **desired state** of one
editor's profiles. There is one self-contained file per editor (e.g. `vscodium.toml`),
selected with `--editor`/`--config`. This document is the authoritative reference; it
stands alone and needs no editor tooling.

A machine-readable [JSON Schema](../schema/config.schema.json) is published alongside it.
Generated configs carry a first-line directive so TOML-aware editors (Taplo / the "Even
Better TOML" extension) offer completion, hover docs, and validation:

```toml
#:schema https://raw.githubusercontent.com/viell-dev/code-profile-manager/main/schema/config.schema.json
```

In-editor validation is best-effort: it fetches the schema over the network and tracks
the `main` branch, so it may run ahead of an older installed binary. The binary itself
always validates structurally via `serde` on load — the schema only adds authoring
ergonomics.

## Layout

```toml
[editor]                 # which editor this config targets (optional overrides)
[global]                 # settings + extensions applied to every profile
[groups.<name>]          # reusable bundles a profile can include
[default]                # the built-in Default profile
[profiles.<name>]        # a named profile
```

An effective per-profile state is computed by layering these (see
[Merge precedence](#merge-precedence)).

## Push, pull, and sync — mirror vs overlay

The config is the source of truth. The three operations are:

- **push** — make the editor match the config. **Destructive:** in the default *mirror*
  mode, editor profiles not in the config are deleted, and within each managed profile
  extra settings keys are removed and extra extensions uninstalled. Take a `<editor>.toml`
  to a fresh machine and `push` to recreate all your profiles.
- **pull** — make the config match the editor. **Destructive:** in mirror mode, named
  profiles in the config that no longer exist in the editor are removed.
- **sync** — the only non-destructive operation: a 3-way merge that reconciles both sides
  with per-item conflict resolution.

**Overlay (managed) mode** turns off the "delete undefined" behavior so the config manages
only the profiles it defines, leaving everything else untouched. Enable it with
`[options] managed = true` (see below) or per-run with `--profile <name>` (repeatable). Use
it to push a few admin/shared profiles to machines without disturbing users' personal
profiles. In overlay mode, deleting a profile requires an explicit
[`delete = true`](#tombstones-delete--true) tombstone.

Destructive changes are listed and confirmed before any write (`--dry-run` to preview,
`--yes` to skip the prompt; the editor must be closed for `push`/`sync`).

The target is just a path: a config or app-home on a network/SFTP-style mount is treated
as an ordinary directory.

## `[options]`

| Field | Type | Description |
|-------|------|-------------|
| `managed` | bool | Overlay/managed mode. `true` = manage only the profiles defined in this config (plus `delete = true` tombstones) and never create or delete undefined profiles. Default `false` = full mirror (the config owns the entire profile set). |

```toml
[options]
managed = true
```

## `[editor]`

How the config refers to / overrides the target editor. All fields optional.

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Editor name to match during discovery — `product.json` `nameShort` or `applicationName`, e.g. `"VSCodium"` or `"Code - OSS"`. |
| `binary` | path | Explicit launcher path, bypassing `PATH` discovery. |
| `user_dir` | path | Override for the editor's `User/` directory (wrapper script, `--user-data-dir`, or portable install). |
| `extensions_dir` | path | Override for the shared extensions directory. Note: Code - OSS and VSCodium share `~/.vscode-oss/extensions` by default. |

```toml
[editor]
name = "VSCodium"
# binary = "/usr/bin/codium"
# user_dir = "…"
# extensions_dir = "…"
```

## `[global]` and `[groups.<name>]`

Both are **layers**: a reusable bundle of settings and extensions.

| Field | Type | Description |
|-------|------|-------------|
| `settings` | table | VS Code settings keys → values (see [Settings](#settings)). |
| `extensions` | array | Extension IDs (see [Extensions](#extensions)). |

`[global]` applies to **every** managed profile. A `[groups.<name>]` applies only to
profiles that list it in their `groups`. Groups model "common across most" without
repeating items in each profile.

```toml
[global]
extensions = ["editorconfig.editorconfig", "usernamehw.errorlens"]
[global.settings]
"editor.formatOnSave" = true

[groups.web]
extensions = ["dbaeumer.vscode-eslint", "esbenp.prettier-vscode"]
[groups.web.settings]
"editor.defaultFormatter" = "esbenp.prettier-vscode"
```

## `[default]`

The built-in **Default** profile. It is always present and cannot be renamed. It takes
the same fields as a named profile **except** `icon` and `use_default` (it is the profile
others inherit *from*, so inheritance flags don't apply).

| Field | Type | Description |
|-------|------|-------------|
| `groups` | array | Group names this profile includes. |
| `settings` | table | Profile-specific settings (highest precedence). |
| `extensions` | array | Profile-specific extensions. |
| `exclude_extensions` | array | Extension IDs to drop even if a group/global adds them. |

The Default profile is configured **here**, never under `[profiles.*]`. A config
containing `[profiles.Default]` is rejected on load.

```toml
[default]
extensions = ["brunnerh.insert-unicode"]
```

## `[profiles.<name>]`

A named profile. Use a quoted key for names with spaces (e.g. `[profiles."TaqsWeb V2"]`).

| Field | Type | Description |
|-------|------|-------------|
| `icon` | string | Codicon ID used as the profile icon (e.g. `"package"`, `"briefcase"`). See the [codicon reference](https://microsoft.github.io/vscode-codicons/dist/codicon.html). |
| `groups` | array | Group names to include, applied in listed order. |
| `settings` | table | Profile-specific settings (highest precedence). |
| `extensions` | array | Profile-specific extensions. |
| `exclude_extensions` | array | Extension IDs to drop even if a group/global adds them. |
| `use_default` | table | Resources inherited from Default (see [`use_default`](#use_default)). |
| `delete` | bool | Tombstone — delete this profile (see [Tombstones](#tombstones-delete--true)). |

```toml
[profiles.Rust]
icon = "package"
extensions = ["rust-lang.rust-analyzer", "tamasfe.even-better-toml"]
use_default = { keybindings = true }

[profiles."TaqsWeb V2"]
icon = "briefcase"
groups = ["web"]
exclude_extensions = ["usernamehw.errorlens"]
use_default = { keybindings = true }
```

## Tombstones (`delete = true`)

`delete = true` marks a profile for **deletion** from the editor on `push`/`sync`:

```toml
[profiles.Legacy]
delete = true
```

Rules:

- **Overlay mode only.** A tombstone is valid only when the run is scoped — `[options]
  managed = true` or `--profile`. In full *mirror* mode a tombstone is rejected, because
  there you delete a profile simply by removing its `[profiles.<name>]` block (absence
  already means deletion). This keeps the two ways of deleting from overlapping.
- **No other fields.** A tombstone must not carry `settings`, `extensions`, `groups`,
  `icon`, `use_default`, etc. — that combination is rejected on load.
- **Kept after push.** The tombstone stays in the config so it keeps re-asserting absence
  across machines that come and go (idempotent).
- **Cleared by pull.** If the editor actually has the profile, `pull` captures it and
  clears the tombstone (the editor is the source of truth for `pull`).
- Deletion removes the profile's `storage.json` entry and its data directory; it **never**
  deletes shared extension-pool folders (membership only). The Default profile cannot be
  deleted. Workspace pins (`profileAssociations`) referencing it are warned about, not
  rewritten.

## Settings

A `settings` table maps dotted VS Code setting names to any JSON-compatible value:

```toml
[global.settings]
"editor.formatOnSave" = true
"files.trimTrailingWhitespace" = true
```

`null` is **not** supported — TOML has no null type, and any `null` is stripped on write,
so a setting explicitly set to `null` is not managed.

## Extensions

Extension IDs take the form `publisher.name`, optionally pinned as
`publisher.name@version`. Pins are parsed but **not** enforced for membership in v1
(newest compatible is acceptable; the resolved version is recorded in the snapshot so it
doesn't churn). The ID must match:

```text
^[a-z0-9][a-z0-9-]*\.[a-z0-9][a-z0-9-]*(@.+)?$
```

Local **VSIX-source** extensions need no special config field: they are vendored
automatically into the app home's `vendor/extensions/` on pull/sync and restored on push,
so the config stays portable across machines without a marketplace.

## `use_default`

Records, per profile, which resource types are **inherited from the Default profile**
instead of being profile-local (VS Code's `useDefaultFlags`). A `true` value means the
editor reads the Default profile's copy and the tool writes no profile-local file for that
resource.

```toml
use_default = { keybindings = true }
```

Recognized keys (VS Code's `ProfileResourceType`):

`settings`, `keybindings`, `snippets`, `tasks`, `extensions`, `globalState`, `mcp`,
`prompts`, `languageModels`.

`[default]` has no `use_default` (nothing to inherit from).

## Merge precedence

The effective desired state for a profile is computed deterministically:

- **settings** — `[global].settings` → each included `[groups.*].settings` (in listed
  order) → the profile's / `[default]`'s `settings`. Later wins, per **top-level key**
  (the value is replaced wholesale; recursive object deep-merge is future work).
- **extensions** — `union(global, groups…, profile)` minus `exclude_extensions`. A set of
  IDs; versions are ignored for membership.
- **use_default** — taken from the profile itself, not inherited from global/groups.
- A profile that inherits a resource (`use_default.<resource> = true`) ignores the
  resolved settings/extensions for that resource.

**Consolidation** is the inverse refactor (run at `init` and from the interactive menu):
items shared across all profiles are hoisted into `[global]` while keeping the resolved
per-profile state identical.
