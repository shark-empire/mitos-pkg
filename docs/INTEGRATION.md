# Integration guide

Every way another MITOS component can connect to `mitos-pkg` — as a
subprocess, as a linked Rust library, or by reading/producing the files it
reads and writes. If you're building `mitos-installer`, `mitos-gui`,
`mitos-settings`, `mitos-update`, `mitos-recovery`, `mitos-build`, or
`mitos-repo`, this is the doc to check first.

`mitos-pkg` is at version `0.4.0` — nothing here is under a stability
guarantee yet. Treat file/CLI formats as likely-stable and the exact Rust
API (function signatures, struct fields) as more likely to shift.

## Three ways to connect

**Shell out to the `mitos-pkg` binary** if your component isn't Rust, or
runs in its own process anyway (a shell script, `mitos-installer`'s
first-boot sequence, a cron-style trigger for `mitos-update`). Pass the
global `--json` flag and parse stdout as one JSON value per invocation
instead of scraping human-readable text — see [CLI reference](#cli-reference)
below for each command's JSON shape. Without `--json`, output is
line-oriented plain text meant for a terminal; that format is documented
below and exercised in tests, but `--json` is the more robust choice for
anything that needs structured data (a list widget in `mitos-gui`, a
progress report in `mitos-installer`).

**Link `mitos-pkg` as a library** if your component is Rust and either
already runs privileged, or is fine calling `PackageService` in its own
process (a first-boot installer, a build/CI tool). This is the most
direct option: typed errors (`PkgError`), typed results
(`Vec<(String, Version, Version)>` from `upgrade`, not a string to parse),
no subprocess overhead. See [Library API](#library-api) below.

**Connect to `mitos-pkgd`** — the daemon — if your component is a GUI, or
anything else that runs as the logged-in user and shouldn't itself run
privileged just to install a package. `mitos-pkgd` is a background
service (its own binary, same crate) that holds one `PackageService` and
serves it to unprivileged local clients over a Unix socket, streaming
typed progress events back as an operation runs. See
[mitos-pkgd (daemon)](#mitos-pkgd-daemon) below.

In your `Cargo.toml`:

```toml
[dependencies]
mitos-pkg = { git = "https://github.com/shark-empire/mitos-pkg" }
```

(or a `path = "../mitos-pkg"` dependency if you ever bring these repos
into one Cargo workspace — nothing about the crate's structure requires
that, it works standalone either way.)

## CLI reference

```
mitos-pkg [--config <path>] [--root <path>] [--dry-run] [--json] <command>
```

`--config <path>` — config file location, default `/etc/mitos-pkg/config.json`.
`--root <path>` — install/remove relative to this root instead of `/`
(chroot-style). **This is the flag `mitos-installer` wants**: run
`mitos-pkg --root /mnt/target install <base-set...>` against a freshly
partitioned/mounted target during install, and recovery/image-build
tooling can do the same against a mounted image.
`--dry-run` — for `install`/`remove`/`upgrade` only: report what would
happen without changing anything (still runs every check a real call
would — dependents in the way, an essential package without `--force`,
an unsatisfiable version — so a `--dry-run` success is a real guarantee,
not just a resolve). Ignored by read-only and non-transactional commands.
`--json` — print one JSON value to stdout instead of formatted text, for
every command. Errors become `{"error": "..."}` on stderr's-worth of
detail but still on stdout, matching the human-text path's "message goes
where you'd expect for this command" behavior — check the process exit
code (below) to know whether to read it as success or failure.

| Command | Notes |
|---|---|
| `install <name>[@version]` | `@version` pins an exact version. Also accepts a path to an already-built `.mpkg` file — installs it directly, no repository/dependency resolution involved. `--signature <hex>` verifies a local file's signature (only meaningful with a path, only required if that package declares a `signer`). |
| `remove <name> [--cascade] [--force]` | Fails if other installed packages depend on it, unless `--cascade` (removes them too, deepest first). Fails on an essential package unless `--force`. |
| `upgrade [name] [--ignore-hold]` | No name = upgrade everything upgradable. Prints `nothing to upgrade` (exit 0) if nothing was. |
| `hold <name>` / `unhold <name>` | Pin/unpin a package against `upgrade`. |
| `autoremove` | Removes orphaned non-explicit, non-essential deps, to a fixed point. |
| `list` | One line per installed package: `name version` or `name version [held]`. |
| `search <query>` | One line per match: `name version - description`. |
| `info <name>` | `key: value` lines, now including `arch` and (when true) `essential`. |
| `verify [name]` | Re-checks installed files' checksums against what was recorded at install time; no name checks everything. Reports missing/unreadable files and whole-package checksum drift. Exit 0 either way — check `--json`'s `issues` array (empty = clean) if you need a pass/fail signal, since "found problems" isn't itself a `mitos-pkg` failure. |
| `history [--limit N]` | Most recent completed operations, oldest first (default 20). |
| `update` | Refreshes the local index cache from configured repositories. |
| `clean` | Deletes cached downloaded archives; prints bytes freed. |
| `build <source> [-o dir] [--sign-with seed-file]` | Packages `source/` into a `.mpkg`. See [Publishing a package](#publishing-a-package--repository). |

**Exit codes**: `0` on success, non-zero on any error (no differentiated
exit codes per error type yet — if `mitos-installer` or `mitos-update`
need to distinguish failure modes programmatically, that's a reason to
link the library instead of parsing stderr text, `--json` or not).

**`--json` shapes** (all objects, all `#[derive(Serialize)]` structs
private to `main.rs` except where noted): `install`/`install <path>` →
`{"dry_run": bool, "packages": [{"name","version"}, ...]}`; `remove` →
`{"dry_run": bool, "removed": ["name", ...]}`; `upgrade` →
`{"dry_run": bool, "upgraded": [{"name","from","to"}, ...]}`; `list` →
`[{"name","version","explicit","held"}, ...]`; `search` →
`[{"name","version","description"}, ...]`; `info` → the library's
`InfoView` directly (see [Library API](#library-api)); `verify` → the
library's `Vec<VerifyIssue>` directly (`{"package","path","problem"}`,
`path` null for a whole-package issue); `history` → the library's
`Vec<HistoryEntry>` directly; `clean` → `{"freed_bytes": u64}`; `build` →
`{"archive_path","payload_sha256","signature_hex"}`; `hold`/`unhold`/
`update` → `{"status":"ok","package": "name" or null}`; any failing
command → `{"error": "message"}`.

## Library API

Everything below is `pub` and reachable as `mitos_pkg::...`.

```rust,no_run
use mitos_pkg::config::Config;
use mitos_pkg::service::PackageService;

# fn main() -> mitos_pkg::error::Result<()> {
let config = Config::load_or_default(std::path::Path::new(Config::DEFAULT_PATH))?;
let mut service = PackageService::open(config)?;

service.update()?; // refresh index from repositories
service.install("mitos-shell", None, false)?; // or "mitos-shell@1.2.0" to pin
service.hold("mitos-shell")?;
let upgraded = service.upgrade(None, false, false)?; // Vec<(String, Version, Version)>
let installed = service.list(); // Vec<(&String, &InstalledPackage)>
let info = service.info("mitos-shell")?; // InfoView — installed and/or available
# Ok(())
# }
```

`PackageService` methods, all on `&mut self` unless noted (`list`,
`search`, `info`, `verify`, `history` are `&self` — safe to call from a
read-only context like a GUI refresh):

- `open(Config) -> Result<Self>` — loads local state; **never touches the
network**. Call `update()` explicitly.
- `update() -> Result<()>` — refreshes from every configured repository,
trying each `RepoSource`'s mirrors in order and retrying transient
failures (see `repository::download`).
- `install(spec: &str, signature_hex: Option<&str>, dry_run: bool) -> Result<Vec<(String, Version)>>`
— `spec` is `"name"`, `"name@version"`, or a filesystem path to an
already-built `.mpkg` (see `install_local` below, which this calls into
automatically for a path). `signature_hex` is only used in that last
case. Returns every package installed (or, under `dry_run`, every
package that *would* be installed), in resolution/install order.
Whole-plan atomic: if package N of a multi-package resolve fails, every
package 1..N-1 this call already installed is rolled back before the
error returns.
- `install_local(archive_path: &Path, signature_hex: Option<&str>, dry_run: bool) -> Result<Vec<(String, Version)>>`
— installs one `.mpkg` directly, no dependency resolution (there's no
repository context to resolve against). `mitos-pkg`'s equivalent of
`dpkg -i`/`rpm -i`.
- `remove(name: &str, cascade: bool, force: bool, dry_run: bool) -> Result<Vec<String>>`
— `force` bypasses the essential-package guard (`PkgError::EssentialPackage`).
Returns every package actually removed (or planned, under `dry_run`).
- `upgrade(name: Option<&str>, ignore_hold: bool, dry_run: bool) -> Result<Vec<(String, Version, Version)>>`
- `hold(name: &str) -> Result<()>` / `unhold(name: &str) -> Result<()>`
- `autoremove() -> Result<Vec<String>>`
- `clean() -> Result<u64>` — bytes freed.
- `list() -> Vec<(&String, &InstalledPackage)>` — `&self`.
- `search(query: &str) -> Vec<&PackageMetadata>` — `&self`.
- `info(name: &str) -> Result<InfoView>` — `&self`.
- `verify(name: Option<&str>) -> Result<Vec<VerifyIssue>>` — `&self`. Empty
`Vec` means clean.
- `history(limit: usize) -> Result<Vec<HistoryEntry>>` — `&self`. Oldest
first.

`install`, `install_local`, `remove`, `upgrade`, `update`, and
`autoremove` each have an `*_with_progress` twin (`install_with_progress`,
etc.) taking one extra `&mut service::Progress` argument, built from
`Progress::new(&mut |event| { ... })` — same behavior, plus a
`ProgressEvent` (resolving/fetching/verifying/installing/removing/
updating) reported at each step. The plain methods are exactly
`*_with_progress` called with `Progress::none()` — use them directly if
you don't need progress; this is what `mitos-pkgd` uses to stream events
to daemon clients (see below).

`Result<T>` is `Result<T, PkgError>`; match on `PkgError`'s variants
(`error.rs`) instead of formatting-and-parsing the CLI's error text. New
since `0.3.0`: `ArchMismatch`, `InsufficientDiskSpace`, `EssentialPackage`,
`HookFailed`.

`Config` fields you're likely to set directly rather than load from disk
(e.g. `mitos-installer` pointing at a mounted target): `install_root`,
`db_dir`, `cache_dir`, `trusted_keys_dir`, `repositories: Vec<RepoSource>`,
`target_arch: Option<String>` (see [Architecture targeting](#architecture-targeting)),
`daemon_socket` (only relevant if you're also running/connecting to
`mitos-pkgd` against this same config).
`RepoSource::Url(String)` for a plain repo, or
`RepoSource::Detailed { url, signer: Option<String>, mirrors: Vec<String>, priority: i32 }`
for one with a required signature, fallback mirror URLs, and/or an
explicit tie-breaking priority (see [Repository index](#repository-index-indexjson)).

## Architecture targeting

MITOS ships on more than one CPU architecture (x86_64 and AArch64), so
every package, index entry, and installed-package record carries an
`arch: Vec<String>` list (`["any"]` by default — arch-independent). Every
resolve/install/upgrade call checks candidates against an *effective*
target architecture: `Config::target_arch` if set, otherwise whatever
architecture the running `mitos-pkg`/`mitos-pkgd` binary itself is on
(`package::arch::host_arch()`). Set `target_arch` explicitly whenever
that default is wrong for what you're doing — the main case is
cross-building: `mitos-installer` or `mitos-build` producing an AArch64
target image from an x86_64 CI runner should set
`Config { target_arch: Some("aarch64".to_string()), .. }` so resolution
and the arch-mismatch guard (`PkgError::ArchMismatch`) check against the
image's architecture, not the builder's.

## mitos-pkgd (daemon)

Everything above runs `PackageService` in your own process. `mitos-pkgd`
instead runs it once, in a long-lived (typically root) process, and
serves it to any number of unprivileged clients over a Unix socket at
`Config::daemon_socket` (default `/run/mitos-pkg/pkgd.sock`) — this is
what `mitos-gui` / `mitos-settings` should use for anything user-facing:
no polkit prompt or running the whole GUI as root just to install a
package, and progress streams back live instead of the call blocking
silently. `mitos-pkgd` catches `SIGTERM`/`SIGINT` to remove its socket
and exit promptly rather than relying purely on the next start's
stale-socket detection — see Status for what that graceful shutdown
does and doesn't cover.

**Rust clients**: use `mitos_pkg::daemon::client::DaemonClient` rather
than speaking the wire protocol directly.

```rust,no_run
use mitos_pkg::daemon::client::DaemonClient;
use std::path::Path;

# fn main() -> std::io::Result<()> {
let mut client = DaemonClient::connect(Path::new("/run/mitos-pkg/pkgd.sock"))?;

client.install("mitos-shell", |event| {
    println!("{event:?}"); // ProgressEvent, streamed live
})?; // ClientResult<Vec<(String, Version)>> — ClientError::{Io, Daemon(String), UnexpectedResponse}

let installed = client.list()?; // Vec<(String, InstalledPackage)>
let issues = client.verify(None)?; // Vec<VerifyIssue> — every installed package
# Ok(())
# }
```

`DaemonClient` methods mirror `PackageService`'s: `install`,
`install_local`, `remove`, `upgrade`, `autoremove`, `update` each take a
progress closure the same shape as `Progress::new`'s and always run for
real; `install_dry_run`, `remove_dry_run`, `upgrade_dry_run` are the
dry-run equivalents (no progress — a dry run never touches the
filesystem, so there's nothing to report). `hold`, `unhold`, `list`,
`search`, `info`, `verify`, `history`, `clean` don't stream progress
either (nothing to report — they're either instant or a single
filesystem/database sweep); `ping()` is a liveness check for detecting
"daemon not running" up front. Errors from the daemon arrive as
`ClientError::Daemon(String)` — the same text `PkgError::to_string()`
produces, **not** a typed `PkgError` (see below for why).

**Non-Rust clients**: connect to the socket and speak newline-delimited
JSON (NDJSON) directly — one `Request` object per line, replied to with
zero or more `{"type":"progress", "phase": ..., ...}` lines followed by
exactly one terminal `{"type":"ok", "kind": ..., ...}` or
`{"type":"err","message":"..."}` line. A connection may be reused for
further requests.

```json
{"op":"install","spec":"mitos-shell","dry_run":false}
```
```json
{"type":"progress","phase":"resolving","spec":"mitos-shell"}
{"type":"progress","phase":"fetching","name":"mitos-shell","version":"1.2.0"}
{"type":"progress","phase":"verifying","name":"mitos-shell","version":"1.2.0"}
{"type":"progress","phase":"installing","name":"mitos-shell","version":"1.2.0"}
{"type":"ok","kind":"installed","packages":[["mitos-shell","1.2.0"]],"dry_run":false}
```

Every `Request` variant (`src/daemon/protocol.rs`), tagged by `"op"`:
`ping`, `install {spec, dry_run}`,
`install_local {path, signature, dry_run}` (installs a `.mpkg` the
daemon can read at `path` on its own filesystem — meaningful because
client and daemon are both local to one machine), `remove {name,
cascade, force, dry_run}`, `upgrade {name, ignore_hold, dry_run}`
(`name` nullable), `hold {name}`, `unhold {name}`, `autoremove`, `list`,
`search {query}`, `info {name}`, `verify {name}` (`name` nullable —
null checks everything installed), `history {limit}`, `update`, `clean`
— the same operations as the CLI/library, minus `build` (a pure local
transform with nothing for a persistent daemon to add). Every mutating
request's `dry_run` field is `#[serde(default)]`, so an older client that
doesn't send it gets real execution — the same default the library's
plain (non-`_with_progress`) methods use.

`ResponsePayload` (tagged by `"kind"`): `pong`, `unit`,
`installed {packages, dry_run}` (`[name, version]` pairs),
`removed {names, dry_run}`, `upgraded {changes, dry_run}` (`[name, old,
new]` triples), `listed {packages}` (`[name, InstalledPackage]` pairs),
`searched {matches}` (`PackageMetadata[]`), `info {...}` (an `InfoView`,
fields merged in directly), `verified {issues}` (`VerifyIssue[]`),
`history {entries}` (`HistoryEntry[]`), `freed {bytes}`.

Errors cross the wire as `{"type":"err","message":"..."}` — formatted
text, not a serialized `PkgError` — deliberately: `PkgError`'s variant
set can keep growing without that being a protocol-breaking wire change.
If you need to branch on *which* error happened rather than just
surface it, link the library and call `PackageService` directly instead
of going through the daemon.

**Access control** is at the socket level, not per-request: the socket
is created `0660`, so only root (who owns it) and members of whatever
group owns it can connect at all — there's no distinction, once
connected, between "may list" and "may install". Which users are in
that group is a deployment decision (set by whatever starts
`mitos-pkgd`, typically a `mitos-services` unit — see that project for
runtime-directory ownership), not something `mitos-pkgd` decides for
itself.

**Running it**: `mitos-pkgd [--config <path>] [--socket <path>]`, same
flag shape as the CLI. Meant to be started once at boot (by
`mitos-services`) and left running, not invoked per-command.

## On-disk layout

| Path (default) | What | Format |
|---|---|---|
| `/etc/mitos-pkg/config.json` | `Config` | JSON, see below |
| `/etc/mitos-pkg/trusted-keys/<signer>.pub` | one file per trusted signer | hex-encoded 32-byte Ed25519 public key, no wrapping |
| `/var/lib/mitos-pkg/packages.json` | `InstalledDb` | JSON, see below |
| `/var/lib/mitos-pkg/files.json` | file → owning-package reverse index | JSON, `{path: owner_name}` |
| `/var/lib/mitos-pkg/history.jsonl` | operation audit log | one JSON object per line, oldest first, see below |
| `/var/cache/mitos-pkg/index.json` | cached merged `RepositoryIndex` | JSON, see below |
| `/var/cache/mitos-pkg/downloads/` | cached `.mpkg` downloads | filename `<name>-<version>.mpkg` |

All of these are relative to `--root` for the DB/cache/trusted-keys paths
*only if* `Config` itself is loaded from within that root — `Config`'s own
paths are absolute by default and not automatically rebased under
`--root`. If you're driving an install against a mounted target
(`mitos-installer`), point `Config.db_dir` / `cache_dir` /
`trusted_keys_dir` at paths under the mount yourself rather than relying
on `--root` to do it — `--root` only affects where package *payload
files* land (`install_root`).

**`config.json`** (`Config`):
```json
{
"install_root": "/",
"db_dir": "/var/lib/mitos-pkg",
"cache_dir": "/var/cache/mitos-pkg",
"trusted_keys_dir": "/etc/mitos-pkg/trusted-keys",
"target_arch": null,
"daemon_socket": "/run/mitos-pkg/pkgd.sock",
"repositories": [
"https://packages.mitos-os.org/index.json",
{
  "url": "https://packages.mitos-os.org/index.json",
  "signer": "mitos-repo-signer",
  "mirrors": ["https://mirror.example.org/mitos/index.json"],
  "priority": 0
}
]
}
```
Each entry in `repositories` is either a bare URL string, or an object
with `url` and optional `signer`/`mirrors`/`priority`. Old configs with
only bare-string URLs, or `{url, signer}` objects with no `mirrors`/
`priority`, keep working unmodified — every new field defaults to empty/
zero. `target_arch` and `daemon_socket` are likewise optional; omitting
them picks up the host architecture and the standard socket path
respectively.

**`packages.json`** (`InstalledDb` — read this directly if you just need
to *display* installed state, e.g. a `mitos-gui` software panel or
`mitos-system-monitor`; call into the library instead if you need to
*act* on it):
```json
{
"packages": {
"mitos-shell": {
"name": "mitos-shell",
"version": "1.2.0",
"description": "...",
"dependencies": [{ "name": "mitos-libc", "version": "*" }],
"provides": [],
"conflicts": [],
"installed_files": ["usr/bin/mitos-shell"],
"explicit": true,
"held": false,
"arch": ["x86_64"],
"essential": false,
"hooks": {},
"payload_sha256": "<hex sha256 of payload files at install time>"
}
}
}
```
`arch`, `essential`, `hooks`, `payload_sha256` are all new since `0.3.0`
and all `#[serde(default)]` — a `packages.json` written by an older
`mitos-pkg` still loads, with every existing entry treated as
arch-independent, non-essential, hook-free, and (for `payload_sha256`)
simply not checkable by `verify` until it's reinstalled once under the
current version.

**`history.jsonl`** (`Vec<HistoryEntry>`, one object per line, not a
JSON array — see `database::history` for why): each line is
`{"timestamp": "<RFC 3339 UTC>", "operation": "install"|"install-local"|
"remove"|"upgrade"|"autoremove", "package": "name", "from_version":
"1.0.0" | null, "to_version": "1.2.0" | null}`.

**`index.json`** (`RepositoryIndex` — same shape both cached locally and
served remotely, see next section).

## Repository index (`index.json`)

What `mitos-repo` serves and `mitos-pkg update` fetches:
```json
{
"packages": {
"mitos-shell": [
{
"name": "mitos-shell",
"version": "1.2.0",
"description": "...",
"dependencies": [{ "name": "mitos-libc", "version": ">=0.3.0, <0.4.0" }],
"provides": [],
"conflicts": [],
"url": "https://packages.mitos-os.org/mitos-shell-1.2.0.mpkg",
"sha256": "<hex sha256 of the whole .mpkg file>",
"signature": "<hex ed25519 sig over payload_sha256, or omit if unsigned>",
"size_bytes": 123456,
"arch": ["x86_64"],
"essential": false,
"installed_size_bytes": 456789
}
]
}
}
```
Every package name maps to an array of *every known version* — `mitos-pkg`
picks whichever one satisfies a given requirement, it doesn't assume the
array is pre-filtered. `arch` (default `["any"]`), `essential` (default
`false`), and `installed_size_bytes` (default `0`, which disables the
pre-download free-space check for that entry rather than falsely
refusing it) are all new since `0.3.0` and safe to omit from an
older-style index. There's a `priority` field too, but don't publish
it: `mitos-pkg update` overwrites it locally from whichever configured
`RepoSource`'s `priority` served this entry, regardless of what a served
index claims — it exists to break ties between *your own* configured
repositories, not as something a repository publishes about itself.

**Signing the index itself** (recommended for anything `mitos-repo` serves
publicly): compute the SHA-256 of the raw `index.json` bytes, sign that
hex digest with the same Ed25519 seed used for package signing (there's
no separate "repo key"), and publish the hex signature at
`index.json.sig` — a sibling file, same directory. Configure the
repository with a `signer` in `config.json` (or `RepoSource::Detailed` if
setting it up from Rust) and `mitos-pkg update` will refuse the index
if the signature is missing or doesn't verify — it tries every
configured mirror before giving up, so one mirror failing to serve a
valid signature doesn't fail the whole refresh if another has it.
Without index signing, an index served over a compromised channel can
point *unsigned* packages at substituted archives with a matching
(attacker-controlled) checksum — signing the index closes that gap the
same way apt's Release.gpg or pacman's sync-db `.sig` do.

## Publishing a package + repository

For `mitos-build`/`mitos-repo` tooling producing what `mitos-pkg` consumes:

1. Author `pkg.json` next to a `payload/` directory:
```json
{
"name": "mitos-shell",
"version": "1.2.0",
"description": "...",
"dependencies": [{ "name": "mitos-libc", "version": ">=0.3.0, <0.4.0" }],
"provides": [],
"conflicts": [],
"signer": "mitos-repo-signer",
"arch": ["x86_64", "aarch64"],
"essential": false,
"hooks": {
  "post_install": "bin/register-shell.sh"
}
}
```
(`signer`, `arch`, `essential`, `hooks` are all optional — omit `signer`
for an unsigned package, `arch` defaults to `["any"]`, `essential`
defaults to `false`, `hooks` defaults to none. Every path inside `hooks`
is relative to `payload/` and must name an executable file that ends up
there — see [Maintainer hooks](#maintainer-hooks) below.)
2. `mitos-pkg build my-package/ -o out/ --sign-with seed.hex` writes
`out/mitos-shell-1.2.0.mpkg` (gzip-compressed tar; `manifest.json` at
the archive root, payload files under `payload/`) and prints the
payload SHA-256 and, if `--sign-with` was given, a signature.
3. Host the `.mpkg`, and add an entry for it to `index.json` under its
package name with `url`, `sha256` (**of the whole `.mpkg` file** — a
separate value from the payload hash `mitos-pkg build` printed),
`installed_size_bytes` (the build output's manifest carries this — pull
it from the built `manifest.json` rather than recomputing it) and, if
signed, `signature`.
4. If the repository itself is signed (recommended), re-sign
`index.json`'s bytes after every edit and republish `index.json.sig`.

### Maintainer hooks

A package can declare up to four scripts — `pre_install`, `post_install`,
`pre_remove`, `post_remove` — each a path relative to `payload/` naming
an executable file shipped inside the package itself. Run directly (never
through a shell), with `MITOS_PKG_ROOT`/`MITOS_PKG_NAME`/
`MITOS_PKG_VERSION`/`MITOS_PKG_HOOK` set in the environment. `pre_install`/
`post_install`/`pre_remove` failures fail (and, for install, roll back)
the whole operation; `post_remove` is best-effort (logged, not fatal —
see `package::hooks::Hooks`). There's no separate hook-signing mechanism:
a hook script is just another payload file, so it's covered by the same
`payload_sha256` checksum and signature as everything else — tampering
with a hook is tampering with the payload.

## Per-component notes

- **`mitos-installer`**: use `--root` (or `Config.install_root` from the
library) pointed at the mounted target filesystem; point `db_dir` /
`cache_dir` / `trusted_keys_dir` under the mount too (see "On-disk
layout" above — they don't follow `--root` automatically), and set
`target_arch` explicitly if you're building for a different architecture
than the one `mitos-installer` itself runs on (see
[Architecture targeting](#architecture-targeting)). Link the library
rather than shelling out if you want typed progress/error reporting
during a first-boot install sequence.
- **`mitos-gui` / `mitos-settings`** (a software-center-style panel):
connect to `mitos-pkgd` (see [mitos-pkgd (daemon)](#mitos-pkgd-daemon))
rather than linking the library directly — a GUI runs as the logged-in
user, and installs/removes/upgrades need privileges that process
shouldn't have to carry itself. `DaemonClient::list`/`search`/`info`/
`verify`/`history` cover a fast panel refresh with no privilege concern
either way (read-only, and cheap enough to poll); reading `packages.json`
directly is still an option if you don't want even a socket round trip
for that part specifically, but installs/removes/upgrades should go
through the daemon so progress streams back live and the write happens
in the already-privileged process instead of yours. Use
`*_dry_run` first if you want a confirmation dialog showing exactly
what an install/remove/upgrade will do before committing to it.
- **`mitos-update`**: `service.update()` then
`service.upgrade(None, false, false)` on whatever schedule/trigger you
use; check `upgrade`'s returned `Vec` to know what changed instead of
parsing CLI stdout.
- **`mitos-recovery`**: `--root`/`install_root` pointed at a mounted
broken install, same considerations as `mitos-installer`; `verify(None)`
is a reasonable first diagnostic step before deciding what to repair.
- **`mitos-build`, `mitos-repo`**: see
[Publishing a package + repository](#publishing-a-package--repository).
