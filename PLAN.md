# Code Profile Manager — Plan

A CLI (GUI later) that manages profiles in a VS Code OSS–based editor from declarative
TOML config. It can push the config into the editor, pull editor state back into the
config, and reconcile both directions with conflict resolution.

v1 scope: **settings + extensions**, **VSCodium first** (with per-editor config and a
multi-editor design), **hybrid extension writes**, **TOML config**. Keybindings,
snippets, tasks, and MCP are designed-for but deferred.

---

## Implementation status (MVP)

Done and verified (Code - OSS as testbench; VSCodium read-only): editor discovery
(`detect`), profile inspection (`list-profiles`), `status`, `init`, `pull`, `push`,
3-way `sync` with conflict resolution, the interactive flow, JSONC settings
read/merge/write, extension install/uninstall via the editor CLI, running-editor gate,
atomic writes, backups, and `--dry-run`. Unit tests cover config layering, null
stripping, id normalization, TOML round-tripping, the 3-way classify table,
cross-platform path derivation, app-home derivation, alias normalization, fake editor
install discovery via `product.json`, and profile registry fixtures. Engine fixture
tests exercise `push`, `pull`, and `sync`
against fake `User/` and extensions directories. GitHub Actions runs fmt, clippy, and
tests on Linux, Windows, and macOS; a separate manual VSCodium smoke workflow exercises
the real editor CLI extension-install fallback on Ubuntu.

Deviations from the design below, kept deliberately simple for the MVP:

- **`push`/`pull` are authoritative mirrors** — the config is the source of truth for the
  whole profile set. By default (mirror mode) `push` makes the editor match the config
  (deleting editor-only profiles, removing extra settings keys, uninstalling extra
  extensions) and `pull` makes the config match the editor (removing config-only profiles).
  `sync` remains the only non-destructive op (3-way merge). **Overlay/managed mode**
  (`[options] managed = true` or `--profile`, now repeatable) scopes a run to the profiles
  it defines and never deletes undefined ones; in that mode a `[profiles.<name>] delete =
  true` tombstone deletes a specific profile. Destructive changes are summarized and
  confirmed (`--yes`/`--dry-run`). This subsumes the previously-planned `--prune` push.
- **Settings are null-free** — JSON `null` is stripped at the read boundary (TOML has no
  null), so settings explicitly set to `null` are not managed.
- **Extension adds are tiered** — pool → vendored copy → editor CLI. If the extension is
  already in the shared pool, its catalog entry is copied into the profile's
  `extensions.json` (no marketplace; works for extensions not on Open VSX); else a
  vendored copy in the repo is restored; else the editor CLI fetches it. Removals edit
  the membership list directly and never delete shared files; removing from the
  **Default** profile is refused (its list is the shared pool). Failed installs are
  reported and skipped, never aborting the run.
- **Local (VSIX-source) extensions are vendored** — on pull/sync, extensions whose pool
  entry has `metadata.source == "vsix"` are copied into the app home's
  `vendor/extensions/` (folder + a sidecar of the catalog entry) so the config is
  portable. With explicit `--config` and no `--app-dir`, vendored extensions live under
  `.code-pm/vendor/extensions/` next to that config. On push they are restored from
  there onto machines that don't have them installed (the path is rewritten to the local
  pool). Verified by vendoring from VSCodium and restoring onto a fresh extensions
  directory.
- **App home + aliases** — the default config/state location is the per-user app home;
  `--app-dir` relocates the whole home, and `--config` remains an exact editor-config
  override. `config.toml` stores `default_editor`, known editor aliases, and verified
  path overrides. Editor selectors are case-insensitive, punctuation-normalized, and can
  match product IDs, application names, discovered launcher names, or user aliases.
- **Consolidation** (`Config::consolidate`) hoists shared config into `[global]`,
  behavior-preservingly (`resolve()` unchanged): a settings key present in *every*
  profile is hoisted using its most-common value (when shared by ≥2 profiles) with
  dissenting profiles keeping an override; extensions present in every profile are hoisted
  by intersection. Default-on at `init`; also an interactive menu action.
- **Interactive menu** can switch the target editor ("Choose a different editor") without
  restarting.
- **JSON Schema + docs** — `schema/config.schema.json` is hand-maintained (not
  `schemars`-derived; the format is small and a curated schema gives better hover
  text/enums/regex constraints) and `docs/config.md` is the standalone reference.
  Generated configs carry a `#:schema` directive on the first line pointing at the schema
  raw URL on `main`. A drift-guard unit test validates an emitted config against the
  committed schema. Pinning the URL to release tags is future work (nothing is tagged yet).
- **Keybindings / snippets / tasks / MCP** are not synced yet.
- **Shared extension pool caveat**: the Default profile's extension list is the shared
  pool's own `extensions.json`, which Code - OSS and VSCodium share — `sync` on the
  Default profile's extensions can therefore affect both editors.

---

## Remaining work / roadmap

Near-term polish:
- ⏳ **VSIX vendoring + `[extension_sources]` + version pinning** — switch vendored
  artifact from unpacked folder to `.vsix`, `vendor/{vsix,extensions}` split, live external
  sources, manifest-driven discovery, and first-class `id@version` pins/freeze. Designed in
  [§4.1](#41-vsix-vendoring--extension_sources--version-pinning-designed-not-yet-implemented).
  Design fully decided; ready to implement in one branch.
- ✅ **JSON Schema + `docs/config.md`** — hand-maintained `schema/config.schema.json` +
  standalone `docs/config.md`; generated configs carry a `#:schema` directive (raw URL on
  `main`); a unit test guards schema/struct drift.
- ✅ **Authoritative push/pull (mirror) + profile deletion + `delete = true`** — `push`/
  `pull` are mirrors with a confirmed destructive plan; `[options] managed` and repeatable
  `--profile` give overlay mode; tombstones delete specific profiles. Subsumes the former
  `--prune` item.
- **Resources beyond settings + extensions** — keybindings, snippets, tasks, MCP
  (honoring `useDefaultFlags` per resource, §1.4).
- **`gc`** for pool extension folders no longer referenced by any profile *across all
  editors sharing the pool* (§1.2 collision).
- **Comment/formatting preservation** in settings files (CST-level edits, §5).
- **Deep object merge** for settings layering (today: per-top-level-key replace, §2.3).

Cross-platform & quality:
- **Broader fixture tests + manual real-editor smoke** (below) — initial engine fixtures
  and a manual Ubuntu/VSCodium smoke workflow are in place; keep expanding fixture
  coverage as new resources are added.
- **Data-driven `editor/paths.rs`** — inject platform + env instead of `cfg!`, so the
  Windows/macOS path rules are unit-testable on a single Linux runner. ✅
- **More editors** — the `product.json` layer makes new forks mostly free; verify VS Code
  and Cursor when available.

Larger:
- **GUI** (§7) — a desktop front-end over the same engine library: browse a marketplace
  to pick extensions, assign to groups/profiles, visualize drift/conflicts. A stable
  storage dir (below) matters most here.
- **Per-editor scope/defaults registry** — a versioned *data* file (community-maintainable,
  decoupled from the binary) describing setting scopes and defaults, to handle
  application-scoped settings and default-value awareness (§8). Degrades gracefully to
  today's behavior when absent.

## Config & state storage location

Default: a per-user application directory —

- Linux: `$XDG_CONFIG_HOME/code-profile-manager/` (fallback
  `~/.config/code-profile-manager/`)
- macOS: `~/Library/Application Support/code-profile-manager/`
- Windows: `%LOCALAPPDATA%\code-profile-manager\`

Layout:

```text
config.toml              # app preferences, default editor, known aliases/paths
editors/
  vscodium.toml          # per-editor managed profile state
snapshots/
  vscodium.snapshot.json
backups/
  <timestamp>/
vendor/
  extensions/
```

`--app-dir <dir>` relocates the whole app home, useful for sandboxes, remote-mounted
systems, or managed/shared profile baselines. `--config <path>` remains an exact
editor-config override. With `--config` alone, derived state stays next to that config:

```text
.code-pm/
  snapshots/<editor>.snapshot.json
  backups/<timestamp>/
  vendor/extensions/
```

With `--config` and `--app-dir`, the explicit config is used but state goes under the
supplied app home.

## Testing & CI

Most of the cross-platform surface (discovery, `product.json` parsing, path derivation,
config layering/consolidation, the sync engine) is pure logic over files + env vars, so
it can be tested **without a real editor**: build fixture install trees
(`…/resources/app/product.json`) and fake `User/` + extensions directories, and point the
tool at them via env (`$HOME` / `$APPDATA` / `$LOCALAPPDATA` / `$XDG_CONFIG_HOME`) and the
`user_dir`/`extensions_dir` overrides (already exercised with scratch dirs).

- **CI:** GitHub Actions runs formatting, clippy with warnings denied, and tests with
  `matrix.os: [ubuntu-latest, windows-latest, macos-latest]`.
- **Current fixture coverage:** data-driven path derivation for Linux/macOS/Windows,
  fake install-tree discovery through `product.json`, fake `storage.json` profile
  registries including stringified `userDataProfiles`, and temp-backed engine tests for
  `push`, `pull`, and `sync` over fake `User/` and extensions directories.
- **Real-editor smoke:** `.github/workflows/vscodium-smoke.yml` is manual
  (`workflow_dispatch`), Ubuntu only, VSCodium stable only. It installs VSCodium, builds
  the CLI, runs `push` against temp `HOME`/`XDG_CONFIG_HOME` directories, forces the
  editor CLI fallback to install `editorconfig.editorconfig`, and verifies profile
  membership plus the shared pool folder. It is intentionally not scheduled and not a PR
  gate yet, because package-manager, display, and Open VSX/network failures would add
  noise to normal development.
- **Future real-editor coverage:** add running-process detection to the manual smoke
  workflow when that path next changes; consider VS Code/Cursor or PR-gated smoke only
  after the manual VSCodium path proves stable.
- **Code - OSS** has no clean Windows/macOS prebuilt (Linux distro only), so its coverage
  stays local; **VSCodium** is its cross-platform stand-in (same `dataFolderName`).

---

## 0. Editor discovery — detect binaries, identify via `product.json`

**Detect installed editors by their binaries, not their config directories** (stale
config dirs from uninstalled editors must be ignored). Crucially, the command name is
ambiguous: on this machine `code` is a symlink to `code-oss` (Code - OSS), but on a
machine with upstream VS Code `code` is VS Code instead. So the command name alone
cannot identify the product.

Discovery algorithm:

1. Enumerate candidate launchers on `PATH`: `code`, `code-oss`, `codium`, `vscodium`,
   `code-insiders`, `cursor`, `windsurf`, plus any user-configured paths.
2. For each, `readlink -f` to the real target, then locate `resources/app/product.json`
   (standard layout: launcher at `<root>/bin/<app>` or `<root>/<app>`, manifest at
   `<root>/resources/app/product.json`; also probe distro paths like
   `/usr/lib/<app>/product.json`).
3. Read identity from `product.json` — **this is the source of truth**, so forks work
   without a hardcoded table:
   - `nameShort` → user-data dir name (see §1.1)
   - `dataFolderName` → default extensions dir (see §1.2)
   - `applicationName`, `quality`, `commit` → CLI binary name / channel / version
4. De-dupe by resolved install path (so `code` and `code-oss` collapse to one editor).

Verified on this machine:

| Launcher(s)        | Real target               | `nameShort`  | `applicationName` | `dataFolderName` |
|--------------------|---------------------------|--------------|-------------------|------------------|
| `code`, `code-oss` | `/usr/bin/code-oss`       | `Code - OSS` | `code-oss`        | `.vscode-oss`    |
| `codium`,`vscodium`| `/opt/vscodium-bin/…`     | `VSCodium`   | `codium`          | `.vscode-oss`    |

`--editor` selects among discovered editors by `nameShort`/`applicationName`; with no
flag the tool lists what it found and asks (or uses a configured default). Each editor
gets its own self-contained config file (§2).

## 1. How VS Code–family editors store profiles (researched + verified locally)

### 1.1 User-data directory (`User/`)

The default profile's data lives at the top level of `User/`. Named profiles live in
`User/profiles/<location>/`, where `<location>` is an opaque id (e.g. `-72fdf191`) that
is **both** the directory name and the `location` field in `storage.json`.

The user-data dir is derived from `product.json` `nameShort`, per OS:

| OS      | User-data dir                                         | This machine (Code - OSS / VSCodium)                            |
|---------|-------------------------------------------------------|-----------------------------------------------------------------|
| Linux   | `$XDG_CONFIG_HOME`(`~/.config`)`/<nameShort>/User`    | `~/.config/Code - OSS/User`, `~/.config/VSCodium/User`          |
| macOS   | `~/Library/Application Support/<nameShort>/User`       | —                                                               |
| Windows | `%APPDATA%\<nameShort>\User`                           | —                                                               |

(e.g. `nameShort` is `Code` for upstream VS Code → `~/.config/Code`.) The launcher may
also override the user-data dir at runtime via `--user-data-dir` or a wrapper script;
the path layer must accept an explicit override. Portable installs relocate this to
`<install>/data/user-data/User` (deferred, but the override path covers it).

Per-profile (and default) on-disk contents (verified on this machine):

```
User/                          # the Default profile
  settings.json
  keybindings.json
  snippets/<lang>.json
  globalStorage/storage.json   # profile registry (see 1.3)
  profiles/
    -72fdf191/                 # a named profile (location id)
      settings.json
      extensions.json          # extension MEMBERSHIP list (see 1.2)
      snippets/
      globalStorage/
      chatLanguageModels.json  # present only for some profiles
```

### 1.2 Extensions (`extensions.json` + shared extension dir)

Extensions are **installed once** into a shared install dir; each profile's
`extensions.json` is only a **membership list** that points at entries in that shared
dir. The dir defaults to `~/<dataFolderName>/extensions` (from `product.json`), and is
overridable at runtime via `--extensions-dir`.

| Editor     | `dataFolderName` | Default extensions dir       |
|------------|------------------|------------------------------|
| Code - OSS | `.vscode-oss`    | `~/.vscode-oss/extensions`   |
| VSCodium   | `.vscode-oss`    | `~/.vscode-oss/extensions`   |
| VS Code    | `.vscode`        | `~/.vscode/extensions`       |

> **Collision (verified):** Code - OSS and VSCodium **share the same default
> `dataFolderName` (`.vscode-oss`)**, so by default they share one physical extension
> pool while keeping *separate* per-profile membership lists (under their separate
> user-data dirs). Implications: (1) detection must not assume "one extensions dir = one
> editor"; (2) any future GC must consider membership across *all* editors that resolve
> to that dir before pruning a folder; (3) installing for one editor can satisfy the
> other. The tool stores the resolved extensions dir per editor and **warns when two
> managed editors resolve to the same one**. `~/.vscode-oss-shared` on this machine is
> unrelated (holds `sharedStorage`, not extensions).

A verified entry in a profile's `extensions.json`:

```json
{
  "identifier": { "id": "brunnerh.insert-unicode", "uuid": "4a8209b8-…" },
  "version": "0.15.1",
  "location": { "$mid": 1, "path": "/home/viell/.vscode-oss/extensions/brunnerh.insert-unicode-0.15.1-universal", "scheme": "file" },
  "relativeLocation": "brunnerh.insert-unicode-0.15.1-universal",
  "metadata": { "id": "4a8209b8-…", "publisherId": "…", "publisherDisplayName": "brunnerh", "targetPlatform": "universal", "source": "gallery", "installedTimestamp": 1741948167475, "pinned": false, "updated": false, "isPreReleaseVersion": false, "hasPreReleaseVersion": false }
}
```

**Portable vs machine-specific fields:** `location.path` is absolute and
machine-specific. The portable identity is `identifier.id` + `version` (+ pin /
pre-release intent). `relativeLocation`, `uuid`, `publisherId`, `targetPlatform` must
be reconstructed per machine. **Consequence:** you cannot safely hand-write an
`extensions.json` entry for an extension whose folder is not already present in the
shared dir — the entry would dangle and the editor would show a broken extension.
This drives the hybrid write strategy (§4).

> Marketplace note: VSCodium/Code-OSS resolve from **Open VSX**; VS Code from the **MS
> Marketplace**. IDs mostly overlap but some MS-only extensions are absent on Open VSX.
> The repo stores IDs; resolution/availability is per-editor.

### 1.3 Profile registry — `User/globalStorage/storage.json`

Two relevant keys (verified locally):

`userDataProfiles` — the array of named profiles (the Default profile is implicit and
**not** listed):

```json
[
  { "location": "-72fdf191", "name": "Rust", "icon": "package",
    "useDefaultFlags": { "keybindings": true } },
  { "location": "-6b465c31", "name": "TaqsWeb V2", "icon": "briefcase",
    "useDefaultFlags": { "keybindings": true } }
]
```

`profileAssociations` — `{ "workspaces": {…}, "emptyWindows": {…} }` mapping a
workspace/window to a profile id. Empty on this machine. v1 reads it but does not
manage it (out of scope; preserve verbatim).

### 1.4 "Use default" flags — first-class, already in use

`useDefaultFlags` records, per profile, which resource types are **inherited from the
Default profile** instead of being profile-local. On this machine **all 7 profiles set
`{ "keybindings": true }`** — so the tool must treat this as a core concept, not an
edge case.

Resource keys (from VS Code source `ProfileResourceType`): `settings`, `keybindings`,
`snippets`, `tasks`, `extensions`, `globalState`, `mcp`, `prompts`, `languageModels`.
Stored lowercased in `storage.json`.

Rules the tool must obey:
- If a profile inherits a resource (flag = true), it has **no** profile-local file for
  it; the editor reads the Default profile's copy. The tool must **not** write a
  profile-local `settings.json`/`extensions.json` for an inherited resource — it would
  be ignored and misleading.
- The config can **declare** desired inheritance; applying it means writing the flag
  into `storage.json` (and removing/creating the profile-local file accordingly).
- Pull must surface inheritance so it round-trips faithfully.

---

## 2. The repo as source of truth — config model

A declarative TOML file describes the **desired state**. One self-contained file per
editor is supported (e.g. `vscodium.toml`, `code.toml`); `--editor`/`--config` selects
which. Layering keeps common things DRY:

- **`[options]`** — behavior flags. `managed` (default false) selects overlay mode:
  manage only the profiles defined in this config and never create/delete undefined ones.
- **`[global]`** — settings + extensions applied to *every* managed profile.
- **`[groups.<name>]`** — reusable bundles (settings + extensions) a profile can
  include. Models "common across most" without repeating.
- **`[default]`** — the built-in Default profile (always present, cannot be renamed).
  Takes the same fields as a named profile *except* `icon` and `use_default` (which
  don't apply to it). It is **not** put under `[profiles.*]`; a config with
  `[profiles.Default]` is rejected on load.
- **`[profiles.<name>]`** — per named profile: which groups it includes, profile-specific
  settings/extensions, an optional exclude list (to drop a global/group item for this
  profile), and desired `use_default` flags. `delete = true` makes it a tombstone (delete
  the profile; valid only in overlay mode; no other fields allowed).

### 2.1 Documentation & optional authoring schema

**Primary requirement: the config is fully documented** — every field, every enum
value, and every value constraint (icon = Codicon ID; extension ID shape; `use_default`
keys; merge precedence) is explained in a `docs/config.md` reference and in doc comments
on the Rust config structs. The docs are the source of truth and must stand alone
without any editor tooling.

**Implemented: a JSON Schema** for in-editor completion/validation via **Taplo** (the LSP
behind "Even Better TOML"). It is **hand-maintained** at `schema/config.schema.json`
rather than `schemars`-derived: the config format is small and stable, a curated schema
gives better hover text/enums/regex constraints, and it avoids a runtime dependency. It is
kept in lockstep with the config structs by a drift-guard unit test that validates an
emitted config against the committed schema (failing if the binary emits/accepts something
the schema rejects). It's wired self-contained via a first-line directive pointing at the
raw URL on `main` (pinning to release tags is future work — nothing is tagged yet):

```toml
#:schema https://raw.githubusercontent.com/viell-dev/code-profile-manager/main/schema/config.schema.json
```

The binary validates structurally via `serde` at load — the schema only adds authoring
ergonomics (and needs network in-editor to fetch the raw URL). Constraints worth encoding when generated: `icon` → Codicon-ID
`enum` (from the upstream codicon manifest); `[editor].name` examples; `use_default`
keys = `ProfileResourceType` (§1.4); extension IDs →
`^[a-z0-9][a-z0-9-]*\.[a-z0-9][a-z0-9-]*(@.+)?$`; `vsix` → `.vsix` paths.

### 2.2 Sketch

```toml
# code-profile-manager editor config. See README.md for the format.

[editor]
# Match a discovered editor (§0) by product.json nameShort or applicationName.
name = "VSCodium"                 # e.g. "VSCodium", "Code - OSS"
# binary = "/usr/bin/codium"      # optional: pin the launcher instead of PATH discovery
# user_dir = "…"                  # optional override (wrapper/--user-data-dir/portable)
# extensions_dir = "…"            # optional override (--extensions-dir; note .vscode-oss is shared)

[global]
extensions = ["editorconfig.editorconfig", "usernamehw.errorlens"]
[global.settings]
"editor.formatOnSave" = true
"files.trimTrailingWhitespace" = true

[groups.web]
extensions = ["dbaeumer.vscode-eslint", "esbenp.prettier-vscode"]
[groups.web.settings]
"editor.defaultFormatter" = "esbenp.prettier-vscode"

# The built-in Default profile (no icon / use_default).
[default]
extensions = ["brunnerh.insert-unicode"]

[profiles.Rust]
icon = "package"
extensions = ["rust-lang.rust-analyzer", "tamasfe.even-better-toml"]
use_default = { keybindings = true }   # inherits Default keybindings

[profiles."TaqsWeb V2"]
icon = "briefcase"
groups = ["web"]
exclude_extensions = ["usernamehw.errorlens"]  # opt out of a global ext here
use_default = { keybindings = true }
```

> Local (VSIX-source) extensions need no special config field: they are vendored
> automatically into `vendor/extensions/` and restored on push (see the status section
> and §4).

### 2.3 Resolution (merge) semantics

Effective per-profile desired state is computed deterministically:

- **settings** = `global.settings` → each included `group.settings` (in listed order) →
  profile/`[default]` settings. Later wins, per **top-level key** (the MVP replaces a
  key's value wholesale; recursive object deep-merge is future work).
- **extensions** = `union(global, groups…, profile)` minus `exclude_extensions`. A
  set of IDs; versions are ignored for membership in the MVP (`id@version` pins are
  parsed but not enforced).
- **use_default** flags are taken from the profile (not inherited from global/groups);
  `[default]` has none.
- A profile that inherits a resource ignores resolved settings/extensions for that
  resource.
- **Consolidation** is the inverse refactor (status section): hoist what every profile
  shares into `[global]` while keeping `resolve()` identical.

The Default profile is addressable as the pseudo-profile **`Default`** (maps to
`User/` root) so global keybindings/settings it owns can be managed too.

---

## 3. Sync engine & conflict resolution

### 3.1 Three states, 3-way merge

To tell "changed in the editor" from "changed in the repo" we keep a **snapshot** of
the last-synced, fully-resolved per-profile state (a lockfile, `.profile-sync/state/<editor>.json`,
committed). The three inputs:

- **Repo desired** (resolved from the TOML, §2.3)
- **Editor actual** (parsed from disk, §1)
- **Snapshot** (last applied)

For each item (a settings key; an extension id) classify:

| repo vs snapshot | editor vs snapshot | result                          |
|------------------|--------------------|---------------------------------|
| same             | same               | no-op                           |
| changed          | same               | repo wins (push)                |
| same             | changed            | editor wins (pull)              |
| changed          | changed, same value| converged, update snapshot      |
| changed          | changed, differ    | **conflict** → resolve          |

This granularity (per-key / per-extension, not whole-file) means edits to unrelated
keys never collide. v1 honors the user's "keep editor or keep repo" requirement at the
item level.

### 3.2 Conflict resolution (v1)

Interactive prompt per conflict: **keep editor** / **keep repo** / show diff / skip.
Non-interactive flags: `--prefer editor|repo`, `--yes`, `--dry-run` (always show the
plan first). Pulled-side changes that win land at the **profile** level in the TOML
(group attribution is ambiguous and is a job for the GUI later); a warning notes when
a pulled value shadows a group/global value so the user can refactor upward by hand.

### 3.3 Directions / commands

The bare invocation (`code-profile-manager` with no subcommand) launches the **interactive
flow** (§3.5) — the intended default for v1. Each action is also a direct subcommand for
scripting / non-interactive use:

- `status` — parse editor + resolve repo + snapshot; print drift, no writes.
- `pull` — editor → repo (+ snapshot). Makes the config mirror the editor; in mirror mode
  removes config-only profiles (overlay/`--profile` keeps them). (Menu: "Pull".)
- `push` — repo → editor (+ snapshot). Makes the editor mirror the config; in mirror mode
  deletes editor-only profiles and prunes extra settings/extensions (overlay/`--profile`
  scopes to defined profiles; `delete = true` tombstones delete specific ones). (Menu:
  "Push".)
- `sync` — full 3-way (§3.1) with conflict resolution, then update snapshot (non-destructive
  merge; honors `delete = true` tombstones).
- `list-profiles` — enumerate editor profiles incl. `useDefaultFlags`.
- `detect` — list discovered editors (§0).
- `init` — scaffold a config by importing the current editor state (an honest first
  snapshot, so the repo starts converged).

Common flags: `--editor <name>`, `--config <path>`, `--app-dir <path>`, `--profile
<name>` (limit scope), `--dry-run`, `--yes`, `--prefer editor|repo`, `--force`
(override safety checks), `--non-interactive` (never prompt; for scripts/CI).

### 3.4 Safety

- **Editor-must-be-closed.** Writing `storage.json`/`extensions.json` while the editor
  runs risks the editor overwriting on exit. Detect a running process for the target
  editor (and/or its lock files); refuse writes without `--force`. Reads are always
  allowed.
- **Atomic writes** — write temp + rename; never partially overwrite a JSON file.
- **Backups** — before the first write in a run, copy touched files to
  `.profile-sync/backups/<timestamp>/`.
- **Dry-run first** — every mutating command can print the full plan without writing.

### 3.5 Interactive flow (first-run wizard + menu) — v1 default

Running the CLI with no subcommand walks the user through everything. No prior config or
state is assumed; missing pieces are offered, not errored.

1. **Select editor.** Show the editors discovered in §0 (e.g. "Code - OSS", "VSCodium").
   The user picks one, or chooses **"Enter a custom path…"** to point at a launcher that
   wasn't auto-found (then identified via its `product.json`). Selection can be
   remembered as the config's `[editor]`.
2. **Ensure a config.** If no config exists for the chosen editor — or it's missing/was
   deleted — offer **"Create a config from the editor's current profiles"** (`init`):
   read every profile (settings, extensions, `useDefaultFlags`, icon) and write the
   TOML + an initial snapshot, so the repo starts converged. Decline → continue with an
   empty/partial config.
3. **Main menu** (loop until Exit):
   - **Sync** — bidirectional 3-way reconcile with conflict prompts (§3.1–3.2).
   - **Push** — make the editor match the config (config → editor; REPLACES editor profiles).
   - **Pull** — make the config match the editor (editor → config; REPLACES config profiles).
   - **Manage a single profile…** — show an overview of every profile (config and/or
     editor, with in-sync state), pick one, then run a **scoped** action on just that
     profile: Status / Sync / Push / Pull / Delete. Scoped actions are overlay (like
     `--profile`), so other profiles are never created or deleted. Enables manual workflows
     like "pull one profile, then push the full config."
   - **Consolidate** — hoist settings/extensions shared by all profiles into `[global]`.
   - **Choose a different editor** — re-select the target editor without restarting.
   - **Exit.**
4. **Close-the-editor gate.** Before *any* action that writes (Sync / either Overwrite),
   check whether the selected editor is running (§3.4). If it is, **prompt the user to
   close it** and re-check; refuse to proceed while it's open (with a `--force` escape
   hatch for the non-interactive path). Read-only steps (selection, preview/diff) never
   gate. Always show the planned changes and confirm before writing.

The same operations are available non-interactively via the subcommands (§3.3) for
scripting; `--non-interactive` disables all prompts.

---

## 4. Extension writes — hybrid (chosen)

> **Scope note (v1):** the tool **never queries extension marketplaces / Open VSX
> itself**. Installing happens only by shelling out to the *editor's* CLI (which does
> its own fetch). Extension IDs get into a config two ways: the user types them in, or
> `pull` captures what's installed. *Browsing/searching a marketplace to pick extensions
> from within the tool* is a later GUI feature, not v1.

Reads always parse `extensions.json` + the shared dir directly. For writes:

- **Add to a profile**
  - If a matching folder already exists in the shared extensions dir (by id; pick the
    requested or newest compatible version): **direct-edit** the profile's
    `extensions.json`, reconstructing `relativeLocation`/`location.path` from the
    folder and copying `uuid`/`publisherId`/`targetPlatform` metadata from any existing
    profile entry for the same id (or from the folder's `package.json`/`.vsixmanifest`).
  - Else: **CLI fallback** — invoke the editor binary to fetch + register it
    (`codium --profile "<name>" --install-extension <id>`), then re-parse. Requires the
    editor binary + network; respects the editor-closed rule.
- **Add from a local `.vsix`** (`vsix = [...]`) — installed via the editor CLI
  (`codium --profile "<name>" --install-extension <path>.vsix`), which unpacks it into
  the shared dir; then re-parse to capture the resulting entry. VSIX files are vendored
  in the repo (e.g. `vendor/extensions/*.vsix`) so they sync across machines without a
  marketplace. Their id/version come from the manifest; recorded `metadata.source` is
  `"vsix"` (vs `"gallery"`) — verified, the dev's VSCodium profiles already contain 10
  such entries — so they are excluded from any "update from marketplace" logic and
  re-resolved by file content (hash) when the vendored file changes.
- **Remove from a profile** — drop the entry from that profile's `extensions.json`. Do
  **not** delete the shared folder (other profiles may use it). Optional `gc` later to
  prune unreferenced folders.
- **Pinning/versions** — `id@version` pins; otherwise newest compatible is acceptable
  and recorded into the snapshot so it doesn't churn every run.
- Binary resolution per editor: PATH lookup (`codium`/`code`/`cursor`) with a config
  override.

### 4.1 VSIX vendoring + `[extension_sources]` + version pinning (designed, NOT yet implemented)

Design agreed in discussion (2026-06-27); to be implemented next. Today's code vendors the
**unpacked install folder** into `vendor/extensions/<rel>/` + a `.entry.json` sidecar,
which drags `node_modules` trees into the config repo (e.g. `hansu.git-graph-2`). The plan
below switches the canonical artifact to the `.vsix` and adds first-class non-marketplace
sourcing. Some of it (version pinning) is a larger membership-model change with open
questions — flagged below.

**Decided:**

- **Vendor the `.vsix`, not the unpacked folder.** Small, single-blob, content-hashable,
  the canonical distributable. Restore via the editor CLI
  (`<editor> --install-extension <vsix> --force`), which unpacks and writes correct
  `relativeLocation`/`uuid`/`targetPlatform` metadata so we don't hand-reconstruct it. Keep
  the existing folder-copy + hand-written-entry path as an **offline fallback** (no binary
  / no vsix).
- **Split the vendor store under the existing app-home `vendor/`:**
  - `vendor/vsix/` — `.vsix` artifacts, **auto-discovered**, portable, the managed cache.
    Filenames/discovery keyed by **`id + version + targetPlatform`** (read from the vsix
    manifest `extension/package.json` inside the zip, *not* the filename — files may omit
    the publisher, e.g. `git-graph-2-1.31.7.vsix`). Needs a `zip` dependency.
  - `vendor/extensions/` — unpacked **fallback** folders (today's behavior; pull-captured
    installs where no source vsix exists). `--config`-only layout mirrors this as
    `.code-pm/vendor/{vsix,extensions}/`.
- **`[extension_sources]` — top-level flat map for non-marketplace sources:**
  ```toml
  [extension_sources]
  "hansu.git-graph-2" = "vendor/vsix/hansu.git-graph-2-1.31.7.vsix"
  "corp.internal-tool" = "/mnt/corp/extensions/internal-tool.vsix"   # custom location
  ```
  Keyed by extension id so profiles keep referencing extensions by plain id in
  `extensions`/`groups`/`[global]` (resolve() union/exclude logic unchanged — no parallel
  membership set). Schema constrains values to `*.vsix`. **Semantics: a custom path here is
  a _live external reference_** — re-read each run, **never copied** into the vendor dir
  (the corporate-share case). Anything dropped in `vendor/vsix/` is auto-available without a
  config entry. The two are orthogonal: `vendor/vsix/` = portable store that travels with
  the repo; `extension_sources` = explicit "fetch from here, don't manage it."
- **Restore resolution order for an id:** pool (installed → direct catalog edit) →
  `extension_sources[id]` (live external vsix, explicit wins over implicit cache) →
  `vendor/vsix/` (auto-discovered by id+version+targetPlatform) → `vendor/extensions/<rel>/`
  (unpacked fallback) → editor CLI (Open VSX).
- **Pull/sync workflow:** always copy the unpacked folder fallback (never blocks). When a
  matching vsix (**same id+version+targetPlatform**) exists, skip the folder copy and prune
  any existing fallback folder for that exact id+version. Same id but a *different* version
  is an independent artifact — never pruned against another version.
- **End-of-run report, two buckets (nudge, never force):** (1) folder-fallback packages →
  "supply a `.vsix` to slim the repo"; (2) live-external packages → "not portable — sourced
  from `<path>`", so the user knows the repo isn't self-contained for those (by choice). A
  config leaning on `extension_sources` external paths is not reproducible on a machine that
  lacks them; restore falls through to the next tier (CLI/Open VSX) and reports the miss
  rather than failing.

**Fork compatibility (some extensions only run in some forks).** No new config surface:
- Per-editor config files (`vscodium.toml`, `code-oss.toml`) already separate fork-specific
  sets; there is no cross-editor `[global]`. So an extension that only works in one fork is
  simply listed in that fork's config. **Do not** add a per-extension "compatible forks"
  field — it duplicates the per-editor file and we have no compat DB to validate it (that
  data belongs in the deferred per-editor scope registry, §8).
- Shared-pool isolation (§1.2): a vsix installed for one editor lands in the shared
  `.vscode-oss` pool, but membership lists are separate and the tool only edits the target's
  — so a fork-gated extension in the pool is inert for the other fork unless its config
  lists the id. Failed installs are already reported and skipped (§4), covering genuine
  engine/product rejections.
- **Add a manifest pre-flight:** since we already crack `package.json` for
  id+version+targetPlatform, also read `engines.vscode`; if the target editor's version
  doesn't satisfy it, warn ("won't run on this editor") instead of surfacing a raw CLI
  failure. The vendor cache key **must** include `targetPlatform` (native-binary extensions
  ship `linux-x64` etc. builds) or two platforms/forks would collide on one entry.

**Version pinning / freeze (DECIDED — ships in the same branch).** Today pins/freeze are
*not* exposed: `normalize_id` (config.rs) drops `@version` and lowercases, the snapshot
stores no extension version, and `metadata.pinned` is never read. Two distinct editor
concepts — **exact-version install** (`id@1.2.3`) and **freeze / no-auto-update**
(`metadata.pinned`). Decision:
- **Collapse both into `id@version`** = "install and hold this exact version,"
  round-tripping to `pinned: true` + that version. **Pinning is optional** — bare `id` =
  latest/newest compatible, resolved version recorded in the snapshot so it doesn't churn.
  Pull: `pinned` → `id@version`; unpinned → bare `id`. No "frozen but floating" concept
  (rare; skipped).
- **"Keep multiple versions" lives in the vendor store and _across_ profiles, not within
  one profile** — the editor loads one version of an id per profile, but `vendor/vsix/` (keyed
  id+version+targetPlatform) holds e.g. `1.31.2` and `1.31.7` side by side so different
  profiles/editors can pin different ones.
- **Membership-model impact (the reason this is its own step):** resolved membership can no
  longer be a `BTreeSet<String>` of bare ids — it becomes id → optional version. Touches
  `resolve()` precedence (profile > group > global picks the pin; `exclude_extensions` still
  matches by id), pull (capture version+pinned), push (`--install-extension id@version` +
  set the pinned flag), and the snapshot (record resolved version).
- **Sequencing (DECIDED):** one branch — vsix vendoring + version pinning together.

---

## 5. Settings writes

VS Code settings/keybindings files are **JSONC** (comments + trailing commas). v1:
parse JSONC tolerantly; apply per-key merges; serialize back as valid JSON. Comment and
formatting preservation is a known v1 limitation (call it out in output) and a v2
improvement via CST-level edits. Deep-merge objects; replace arrays/scalars wholesale,
matching §2.3.

---

## 6. Architecture (crate layout)

```
src/
  main.rs            # thin: parse CLI, dispatch, set exit codes
  cli.rs             # clap derive: commands + flags
  config/            # TOML desired-state: parse + resolve (§2)
  editor/            # editor abstraction
    mod.rs           #   Editor: paths, profile registry, ext dir, binary
    discover.rs      #   PATH scan + product.json identity (§0); de-dupe; --editor select
    product.rs       #   product.json model (nameShort, dataFolderName, applicationName…)
  paths.rs           # OS base dirs + product.json-derived paths (§1.1) + overrides
  profile.rs         # storage.json read/write: userDataProfiles, useDefaultFlags
  jsonc.rs           # tolerant JSONC read, merged write (§5)
  extensions.rs      # parse extensions.json + shared dir; hybrid writes (§4)
  sync/              # 3-way engine, snapshot, conflict resolution (§3)
  safety.rs          # running-editor detection, atomic write, backups
```

### Candidate dependencies
`clap` (derive), `serde`, `toml`, `serde_json`, `jsonc-parser` (tolerant JSONC),
`sysinfo` (running-process detection), `thiserror`, plus a TUI prompt helper for
interactive conflicts (e.g. `dialoguer`/`inquire`). Avoid `rusqlite` — `state.vscdb`
(globalState) is out of scope for v1.

> Repo lints are strict (`unwrap_used`, `expect_used`, `panic`, `indexing_slicing`,
> `arithmetic_side_effects`, `as_conversions`, `print_stdout/stderr` all warn; `unsafe`
> denied). Use `thiserror` + `?`, explicit error types, and a logging/output layer
> instead of bare `println!`. Follow the `rust-code-style` skill when coding.

---

## 7. Milestones

1. ✅ **Read-only foundation** — binary discovery + `product.json` identity (§0), derived
   path layer, parse `storage.json` (profiles + flags), parse
   `settings.json`/`extensions.json`. `status`, `list-profiles`, `detect` work end to end
   against Code - OSS and VSCodium.
2. ✅ **Config + resolve** — config structs (documented via doc comments), layering/merge
   (§2.3), `init` (import current state → first snapshot, repo starts converged),
   behavior-preserving consolidation, the standalone `docs/config.md` reference, and a
   hand-maintained `schema/config.schema.json` wired via a `#:schema` directive.
3. ✅ **Push** — authoritative mirror: applies settings (JSONC merge), tiered extension
   adds, prunes extras, and deletes editor-only profiles (overlay/`--profile`/`delete =
   true` scope it); safety gate, atomic writes, backups, `--dry-run`, confirmed destructive
   plan. Honors `useDefaultFlags`.
4. ✅ **Pull** — authoritative mirror: editor → repo at profile level (removes config-only
   profiles in mirror mode) + snapshot update + VSIX vendoring.
5. ✅ **Sync + interactive flow** — 3-way engine + interactive/`--prefer` conflict
   resolution, first-run wizard + main menu (§3.5), close-the-editor gate, and switching
   editors from the menu. The default entry point.
6. ⏳ **Hardening** — portable-install overrides, shared-extensions-dir-aware `gc`,
   keybindings/snippets/tasks/MCP, more fixture tests as resources are added, more
   editors.
   *(See "Remaining work / roadmap".)*
7. ⏳ **GUI (later)** — select extensions from the editor's marketplace, assign to
   groups/profiles, visualize drift/conflicts. Core engine is a library the GUI calls;
   needs the stable storage dir ("Config & state storage location").

---

## 8. Open questions / deferred

- **Pull attribution**: pulled changes land at profile level; consolidation hoists shared
  items into `[global]`, but promoting into named `[groups.*]` stays manual / GUI-assisted.
- **Comment preservation** in settings files: deferred (CST edits).
- **globalState (`state.vscdb`)** and **workspaceStorage**: out of scope.
- **`profileAssociations`** (workspace→profile pinning): preserved, not managed.
- **Open VSX vs MS Marketplace** availability gaps for the same ID across editors.
- **Application-scoped settings** (e.g. `window.titleBarStyle`, `update.mode`,
  `telemetry.*`): the editor only honors these in the Default profile and ignores copies
  in named profiles. In the normal pull-driven flow this self-corrects (they're captured
  only into `[default]`); the only untidy case is hand-authoring one into `[global]`/a
  named profile, where push writes a harmless ignored key. A precise fix needs the
  per-editor scope registry (roadmap), not a hardcoded prefix list (`window.*` would
  wrongly catch the many genuinely per-profile window settings).
