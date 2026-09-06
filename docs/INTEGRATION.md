# Integration guide

Every way another MITOS component can connect to `mitos-pkg` — as a
subprocess, as a linked Rust library, or by reading/producing the files it
reads and writes. If you're building `mitos-installer`, `mitos-gui`,
`mitos-settings`, `mitos-update`, `mitos-recovery`, `mitos-build`, or
`mitos-repo`, this is the doc to check first.

`mitos-pkg` is at version `0.3.0` — nothing here is under a stability
guarantee yet. Treat file/CLI formats as likely-stable and the exact Rust
API (function signatures, struct fields) as more likely to shift.

## Two ways to connect

**Shell out to the `mitos-pkg` binary** if your component isn't Rust, or
runs in its own process anyway (a shell script, `mitos-installer`'s
first-boot sequence, a cron-style trigger for `mitos-update`). Parse stdout
as line-oriented plain text — see [CLI reference](#cli-reference) below.
There is currently **no `--json` output mode**; every command prints
human-readable text meant for a terminal. If you're building something
that needs structured data (a list widget in `mitos-gui`, a progress
report in `mitos-installer`), either parse conservatively (formats are
documented below and used in tests) or use the library instead — parsing
stdout is inherently the more fragile of the two options.

**Link `mitos-pkg` as a library** if your component is Rust. As of this
change the crate builds both a library (`mitos_pkg`) and the `mitos-pkg`
binary from the same source — the binary is a thin wrapper over
`mitos_pkg::service::PackageService`, the same type you'd call directly.
This is the more robust option: typed errors (`PkgError`), typed results
(`Vec<(String, Version, Version)>` from `upgrade`, not a string to parse),
no subprocess overhead. See [Library API](#library-api) below.

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
mitos-pkg [--config <path>] [--root <path>] <command>
```

`--config <path>` — config file location, default `/etc/mitos-pkg/config.json`.
`--root <path>` — install/remove relative to this root instead of `/`
(chroot-style). **This is the flag `mitos-installer` wants**: run
`mitos-pkg --root /mnt/target install <base-set...>` against a freshly
partitioned/mounted target during install, and recovery/image-build
tooling can do the same against a mounted image.

| Command | Notes |
|---|---|
| `install <name>[@version]` | Exit non-zero + stderr message on failure. `@version` pins an exact version. |
| `remove <name> [--cascade]` | Fails (exit non-zero) if other installed packages depend on it, unless `--cascade`. |
| `upgrade [name] [--ignore-hold]` | No name = upgrade everything upgradable. Prints `nothing to upgrade` (exit 0) if nothing was. |
| `hold <name>` / `unhold <name>` | Pin/unpin a package against `upgrade`. |
| `autoremove` | Removes orphaned non-explicit deps, to a fixed point. |
| `list` | One line per installed package: `name version` or `name version [held]`. |
| `search <query>` | One line per match: `name version - description`. |
| `info <name>` | `key: value` lines — see `main.rs` for the exact field order/set. |
| `update` | Refreshes the local index cache from configured repositories. |
| `clean` | Deletes cached downloaded archives; prints bytes freed. |
| `build <source> [-o dir] [--sign-with seed-file]` | Packages `source/` into a `.mpkg`. See [Publishing a package](#publishing-a-package--repository). |

**Exit codes**: `0` on success, non-zero on any error (no differentiated
exit codes per error type yet — if `mitos-installer` or `mitos-update`
need to distinguish failure modes programmatically, that's a reason to
link the library instead of parsing stderr text).

## Library API

Everything below is `pub` and reachable as `mitos_pkg::...`.

```rust,no_run
use mitos_pkg::config::Config;
use mitos_pkg::service::PackageService;

# fn main() -> mitos_pkg::error::Result<()> {
let config = Config::load_or_default(std::path::Path::new(Config::DEFAULT_PATH))?;
let mut service = PackageService::open(config)?;

service.update()?; // refresh index from repositories
service.install("mitos-shell")?; // or "mitos-shell@1.2.0" to pin
service.hold("mitos-shell")?;
let upgraded = service.upgrade(None, false)?; // Vec<(String, Version, Version)>
let installed = service.list(); // Vec<(&String, &InstalledPackage)>
let info = service.info("mitos-shell")?; // InfoView — installed and/or available
# Ok(())
# }
```

`PackageService` methods, all on `&mut self` unless noted (`list`,
`search`, `info` are `&self` — safe to call from a read-only context like
a GUI refresh):

- `open(Config) -> Result<Self>` — loads local state; **never touches the
network**. Call `update()` explicitly.
- `update() -> Result<()>`
- `install(spec: &str) -> Result<()>` — `spec` is `"name"` or `"name@version"`.
- `remove(name: &str, cascade: bool) -> Result<Vec<String>>` — returns
every package actually removed.
- `upgrade(name: Option<&str>, ignore_hold: bool) -> Result<Vec<(String, Version, Version)>>`
- `hold(name: &str) -> Result<()>` / `unhold(name: &str) -> Result<()>`
- `autoremove() -> Result<Vec<String>>`
- `clean() -> Result<u64>` — bytes freed.
- `list() -> Vec<(&String, &InstalledPackage)>` — `&self`.
- `search(query: &str) -> Vec<&PackageMetadata>` — `&self`.
- `info(name: &str) -> Result<InfoView>` — `&self`.

`Result<T>` is `Result<T, PkgError>`; match on `PkgError`'s variants
(`error.rs`) instead of formatting-and-parsing the CLI's error text.

`Config` fields you're likely to set directly rather than load from disk
(e.g. `mitos-installer` pointing at a mounted target): `install_root`,
`db_dir`, `cache_dir`, `trusted_keys_dir`, `repositories: Vec<RepoSource>`.
`RepoSource::Url(String)` for a plain repo, or
`RepoSource::Detailed { url, signer: Option<String> }` for one whose index
must be signed (see [Repository index](#repository-index-indexjson)).

## On-disk layout

| Path (default) | What | Format |
|---|---|---|
| `/etc/mitos-pkg/config.json` | `Config` | JSON, see below |
| `/etc/mitos-pkg/trusted-keys/<signer>.pub` | one file per trusted signer | hex-encoded 32-byte Ed25519 public key, no wrapping |
| `/var/lib/mitos-pkg/packages.json` | `InstalledDb` | JSON, see below |
| `/var/lib/mitos-pkg/files.json` | file → owning-package reverse index | JSON, `{path: owner_name}` |
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
"repositories": [
"https://packages.mitos-os.org/index.json",
{ "url": "https://packages.mitos-os.org/index.json", "signer": "mitos-repo-signer" }
]
}
```
Each entry in `repositories` is either a bare URL string, or an object
with `url` and an optional `signer`. Old configs with only bare-string
URLs keep working unmodified.

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
"held": false
}
}
}
```

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
"size_bytes": 123456
}
]
}
}
```
Every package name maps to an array of *every known version* — `mitos-pkg`
picks whichever one satisfies a given requirement, it doesn't assume the
array is pre-filtered.

**Signing the index itself** (recommended for anything `mitos-repo` serves
publicly): compute the SHA-256 of the raw `index.json` bytes, sign that
hex digest with the same Ed25519 seed used for package signing (there's
no separate "repo key"), and publish the hex signature at
`index.json.sig` — a sibling file, same directory. Configure the
repository with a `signer` in `config.json` (or `RepoSource::Detailed` if
setting it up from Rust) and `mitos-pkg update` will refuse the index
if the signature is missing or doesn't verify. Without this, an index
served over a compromised channel can point *unsigned* packages at
substituted archives with a matching (attacker-controlled) checksum —
signing the index closes that gap the same way apt's Release.gpg or
pacman's sync-db `.sig` do.

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
"signer": "mitos-repo-signer"
}
```
(`signer` is optional — omit for an unsigned package.)
2. `mitos-pkg build my-package/ -o out/ --sign-with seed.hex` writes
`out/mitos-shell-1.2.0.mpkg` (gzip-compressed tar; `manifest.json` at
the archive root, payload files under `payload/`) and prints the
payload SHA-256 and, if `--sign-with` was given, a signature.
3. Host the `.mpkg`, and add an entry for it to `index.json` under its
package name with `url`, `sha256` (**of the whole `.mpkg` file** — a
separate value from the payload hash `mitos-pkg build` printed) and,
if signed, `signature`.
4. If the repository itself is signed (recommended), re-sign
`index.json`'s bytes after every edit and republish `index.json.sig`.

## Per-component notes

- **`mitos-installer`**: use `--root` (or `Config.install_root` from the
library) pointed at the mounted target filesystem; point `db_dir` /
`cache_dir` / `trusted_keys_dir` under the mount too (see "On-disk
layout" above — they don't follow `--root` automatically). Link the
library rather than shelling out if you want typed progress/error
reporting during a first-boot install sequence.
- **`mitos-gui` / `mitos-settings`** (a software-center-style panel):
either read `packages.json` directly for a fast listing, or call
`PackageService::list`/`search`/`info` for the same data with less
risk of the on-disk format shifting under you. Installs/removes/
upgrades should go through the library (typed errors) or the CLI with
care taken around its plain-text output — not direct file writes.
- **`mitos-update`**: `service.update()` then `service.upgrade(None, false)`
on whatever schedule/trigger you use; check `upgrade`'s returned
`Vec` to know what changed instead of parsing CLI stdout.
- **`mitos-recovery`**: `--root`/`install_root` pointed at a mounted
broken install, same considerations as `mitos-installer`.
- **`mitos-build`, `mitos-repo`**: see
[Publishing a package + repository](#publishing-a-package--repository).
