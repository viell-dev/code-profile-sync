# Code Profile Sync — Plan

A CLI (GUI later) that keeps profiles in a VS Code OSS–based editor in sync with a
declarative TOML config held in this repo. It can push the config into the editor,
pull editor state back into the config, and reconcile both directions with conflict
resolution.

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
stripping, id normalization, TOML round-tripping, and the 3-way classify table.

Deviations from the design below, kept deliberately simple for the MVP:

- **`push` is non-destructive** — it sets the config's settings keys and installs missing
  extensions but never deletes editor-only settings or uninstalls extras. Removals flow
  only through `sync` (snapshot-gated). A destructive `--prune` push is future work.
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
  entry has `metadata.source == "vsix"` are copied into `<config_dir>/vendor/extensions/`
  (folder + a sidecar of the catalog entry) so the config is portable. On push they are
  restored from there onto machines that don't have them installed (the path is rewritten
  to the local pool). Verified by vendoring from VSCodium and restoring onto a fresh
  extensions directory.
- **Consolidation** (`Config::consolidate`) hoists settings/extensions common to every
  profile into `[global]`, behavior-preservingly. Default-on at `init`; also an
  interactive menu action.
- **Interactive menu** can switch the target editor ("Choose a different editor") without
  restarting.
- **No JSON Schema yet** — generated configs carry a plain header instead of a `#:schema`
  directive. A `schemars`-derived schema (PLAN §2.1) is still optional/future.
- **Keybindings / snippets / tasks / MCP** are not synced yet.
- **Shared extension pool caveat**: the Default profile's extension list is the shared
  pool's own `extensions.json`, which Code - OSS and VSCodium share — `sync` on the
  Default profile's extensions can therefore affect both editors.

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

- **`[global]`** — settings + extensions applied to *every* managed profile.
- **`[groups.<name>]`** — reusable bundles (settings + extensions) a profile can
  include. Models "common across most" without repeating.
- **`[profiles.<name>]`** — per-profile: which groups it includes, profile-specific
  settings/extensions, an optional exclude list (to drop a global/group item for this
  profile), and desired `use_default` flags.

### 2.1 Documentation & optional authoring schema

**Primary requirement: the config is fully documented** — every field, every enum
value, and every value constraint (icon = Codicon ID; extension ID shape; `use_default`
keys; merge precedence) is explained in a `docs/config.md` reference and in doc comments
on the Rust config structs. The docs are the source of truth and must stand alone
without any editor tooling.

**Nice-to-have: a JSON Schema** for in-editor completion/validation via **Taplo** (the
LSP behind "Even Better TOML"). It's not a deliverable to gate on and not hand-
maintained — `schemars` derives it from the same config structs (and their doc
comments) basically for free, so it stays in lockstep with the code. When present it's
wired self-contained via a first-line directive, with the doc comments becoming hover
text:

```toml
#:schema ./schema/config.schema.json
```

Optionally a `.taplo.toml` maps the config glob to the schema for `taplo check` in CI.
Either way the binary validates structurally via `serde` at load — the schema only adds
authoring ergonomics. Constraints worth encoding when generated: `icon` → Codicon-ID
`enum` (from the upstream codicon manifest); `[editor].name` examples; `use_default`
keys = `ProfileResourceType` (§1.4); extension IDs →
`^[a-z0-9][a-z0-9-]*\.[a-z0-9][a-z0-9-]*(@.+)?$`; `vsix` → `.vsix` paths.

### 2.2 Sketch

```toml
#:schema ./schema/config.schema.json

[editor]
# Match a discovered editor (§0) by product.json nameShort or applicationName.
name = "VSCodium"                 # e.g. "VSCodium", "Code - OSS"
# binary = "/usr/bin/codium"      # optional: pin the launcher instead of PATH discovery
# user_dir = "…"                  # optional override (wrapper/--user-data-dir/portable)
# extensions_dir = "…"            # optional override (--extensions-dir; note .vscode-oss is shared)

[global]
settings = { "editor.formatOnSave" = true, "files.trimTrailingWhitespace" = true }
extensions = ["editorconfig.editorconfig", "usernamehw.errorlens"]

[groups.web]
extensions = ["dbaeumer.vscode-eslint", "esbenp.prettier-vscode"]
settings = { "editor.defaultFormatter" = "esbenp.prettier-vscode" }

[groups.work]
extensions = ["eamodio.gitlens"]

[profiles.Rust]
icon = "package"
groups = []
extensions = ["rust-lang.rust-analyzer", "tamasfe.even-better-toml"]
# Local .vsix files (not on any marketplace), vendored in the repo for portability:
vsix = ["vendor/extensions/my-internal-tool-1.2.0.vsix"]
use_default = { keybindings = true }   # inherits Default keybindings

[profiles."TaqsWeb V2"]
icon = "briefcase"
groups = ["web", "work"]
exclude_extensions = ["usernamehw.errorlens"]  # opt out of a global ext here
use_default = { keybindings = true }
```

### 2.3 Resolution (merge) semantics

Effective per-profile desired state is computed deterministically:

- **settings** = deep-merge of `global.settings` → each included `group.settings` (in
  listed order) → `profile.settings`. Later wins per JSON key. Deep-merge objects;
  replace arrays/scalars wholesale.
- **extensions** = `union(global, groups…, profile)` minus `exclude_extensions`. A
  set of IDs; version is "latest compatible" unless pinned (`id@version`).
- **use_default** flags are taken from the profile (not inherited from global/groups).
- A profile that inherits a resource ignores resolved settings/extensions for that
  resource (with a warning if the config also specifies them).

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

The bare invocation (`code-profile-sync` with no subcommand) launches the **interactive
flow** (§3.5) — the intended default for v1. Each action is also a direct subcommand for
scripting / non-interactive use:

- `status` — parse editor + resolve repo + snapshot; print drift, no writes.
- `pull` — editor → repo (+ snapshot). Writes profile-level TOML and updates snapshot.
  (Menu: "overwrite config from profiles".)
- `push` — repo → editor (+ snapshot). Applies desired state to the editor.
  (Menu: "overwrite profiles from config".)
- `sync` — full 3-way (§3.1) with conflict resolution, then update snapshot.
- `list-profiles` — enumerate editor profiles incl. `useDefaultFlags`.
- `detect` — list discovered editors (§0).
- `init` — scaffold a config by importing the current editor state (an honest first
  snapshot, so the repo starts converged).

Common flags: `--editor <name>`, `--config <path>`, `--profile <name>` (limit scope),
`--dry-run`, `--yes`, `--prefer editor|repo`, `--force` (override safety checks),
`--non-interactive` (never prompt; for scripts/CI).

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
   - **Overwrite profiles from config** — push (config → editor).
   - **Overwrite config from profiles** — pull (editor → config).
   - *(reserved)* — placeholder for a later action (e.g. manage groups / per-profile
     extension picking). Listed but inert in v1.
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

1. **Read-only foundation** — binary discovery + `product.json` identity (§0), derived
   path layer, parse `storage.json` (profiles + flags), parse
   `settings.json`/`extensions.json`. `status`, `list-profiles`, and a `detect` command
   (show discovered editors) work end to end against Code - OSS and VSCodium.
2. **Config + resolve** — config structs (documented via doc comments) + `docs/config.md`
   reference, layering/merge (§2.3), `init` (import current state → first snapshot, repo
   starts converged). Optional `schemars`-derived JSON Schema for editor tooling.
3. **Push** — apply settings (JSONC merge) + extensions (hybrid) with safety,
   atomic writes, backups, `--dry-run`. Honor `useDefaultFlags`.
4. **Pull** — editor → repo at profile level + snapshot update.
5. **Sync + interactive flow** — 3-way engine + interactive/`--prefer` conflict
   resolution, and the first-run wizard + main menu (§3.5: select editor / custom path,
   create-config-from-profiles, the Sync / Overwrite-either-way / Exit menu, and the
   close-the-editor gate before any write). This is the v1 default entry point.
6. **Hardening** — portable-install overrides, shared-extensions-dir-aware `gc`,
   keybindings/snippets, more editors as discovered (the product.json layer makes new
   forks mostly free), tests against fixture user-dirs.
7. **GUI (later)** — select extensions from the editor's marketplace, assign to
   groups/profiles, visualize drift/conflicts. Core engine is a library the GUI calls.

---

## 8. Open questions / deferred

- **Pull attribution**: pulled changes land at profile level; promoting into
  groups/global is manual (v1) / GUI-assisted (later).
- **Comment preservation** in settings files: deferred to v2 (CST edits).
- **globalState (`state.vscdb`)** and **workspaceStorage**: out of scope.
- **`profileAssociations`** (workspace→profile pinning): preserved, not managed in v1.
- **Open VSX vs MS Marketplace** availability gaps for the same ID across editors.
