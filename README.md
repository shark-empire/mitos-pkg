# mitos-pkg

The official package manager for [MITOS](https://github.com/shark-empire).

## Commands

```
mitos-pkg install <name>[@version] install a package (+ its unmet deps);
[--signature <hex>] pin an exact version with @version, or
install a local .mpkg path directly
(--signature verifies a signed local file)
mitos-pkg remove <name> [--cascade] remove (fails if something depends on
[--force] it, unless --cascade; fails on an
essential package unless --force)
mitos-pkg upgrade [name] [--ignore-hold] upgrade one package, or all if none
given; refuses a held package unless
--ignore-hold
mitos-pkg hold <name> pin a package so upgrade skips it
mitos-pkg unhold <name> reverse hold
mitos-pkg autoremove remove orphaned, non-essential,
non-explicit deps
mitos-pkg list list installed packages ([held] shown)
mitos-pkg search <query> search the local repository index
mitos-pkg info <name> show details, installed or available
mitos-pkg verify [name] re-check installed files' checksums
against what was recorded at install
mitos-pkg history [--limit N] show recent install/remove/upgrade ops
mitos-pkg update refresh the repository index
mitos-pkg clean delete cached downloaded archives
mitos-pkg build <source> [-o out] [--sign-with seed]
package source/ into a .mpkg
```

`--root <path>` (global) installs/removes relative to a chroot-style path
instead of `/`. `--config <path>` overrides the config file location.
`--dry-run` (global, install/remove/upgrade only) reports what would
happen — still running every real check — without changing anything.
`--json` (global) prints machine-readable JSON instead of formatted text.
See [`docs/INTEGRATION.md`](docs/INTEGRATION.md) for the full CLI/library
reference other MITOS components can build against.

## mitos-pkgd (daemon)

The CLI above talks to the package database directly — fine for a
terminal, but it means every install needs both a terminal *and* root.
`mitos-pkgd` is a second binary built from this same crate: a background
service, normally started once at boot by `mitos-services`, that any
number of unprivileged clients (a GUI software panel, `mitos-settings`,
scripts) can connect to over a local Unix socket instead — the daemon
does the actual privileged filesystem writes, the client just asks.

```
mitos-pkgd [--config <path>] [--socket <path>]
```

Rust components link `mitos_pkg::daemon::client::DaemonClient` rather
than hand-rolling the socket protocol:

```rust,no_run
use mitos_pkg::daemon::client::DaemonClient;
use std::path::Path;

# fn main() -> std::io::Result<()> {
let mut client = DaemonClient::connect(Path::new("/run/mitos-pkg/pkgd.sock"))?;
client.install("mitos-shell", |event| println!("{event:?}"))
    .expect("install failed");
# Ok(())
# }
```

`install`/`install_local`/`remove`/`upgrade`/`update`/`autoremove` take a
progress callback (`ProgressEvent`: resolving/fetching/verifying/installing/
removing/updating) streamed live from the daemon — this is what lets a
GUI show real progress instead of a spinner over a blocked call.
`install`/`remove`/`upgrade` also have `*_dry_run` variants for a
"preview before you commit" confirmation dialog, and `verify`/`history`
round out read-only diagnostics alongside `list`/`search`/`info`. See
[`docs/INTEGRATION.md`](docs/INTEGRATION.md#mitos-pkgd-daemon) for the
wire protocol, if you're connecting from something that isn't Rust.

## Layout

```
src/
main.rs thin CLI dispatch → service::PackageService
cli.rs clap argument/subcommand definitions
bin/mitos-pkgd.rs thin daemon entry point → daemon::server::run

daemon/ mitos-pkgd: a non-CLI way to drive mitos-pkg
protocol.rs NDJSON wire format (Request/Message/ResponsePayload)
server.rs Unix-socket listener, thread-per-connection,
RwLock<PackageService> dispatch, SIGTERM/SIGINT handling
client.rs DaemonClient — the Rust side for mitos-gui etc.

package/ what a .mpkg archive *is*, and how to make one
manifest.rs Manifest struct embedded in every archive
spec.rs PackageSpec — the hand-authored pkg.json a build reads
build.rs packages a source dir into a signed, checksummed .mpkg
format.rs archive layout constants (manifest.json, payload/)
archive.rs tar+gzip read/extract
signature.rs checksum + signature trust chain for one archive,
and for a repository index (verify_index)
arch.rs architecture targeting (package::arch::is_compatible,
host_arch) — MITOS ships on x86_64 and AArch64
hooks.rs Hooks — declared pre/post-install/remove script
paths, payload-relative (execution: install::hooks)

repository/ where packages come from
metadata.rs lightweight per-version metadata (index entry)
index.rs merged local index cache, search/best-match/provides,
arch- and priority-aware lookups
download.rs checksum-verified HTTP fetch (ureq, blocking),
retry + multi-mirror fallback

dependency/ what needs to happen, and in what order
version.rs Dependency (name + semver requirement);
`name`/`name@version` CLI-argument parsing
graph.rs DAG + topological install order
resolver.rs dependency tree → install plan; backtracking,
requirement-narrowing search over diamond
dependencies; arch-aware candidate selection;
provides fallback; conflict checking;
dependents_of (provides-aware, for remove)

database/ what's actually installed, on disk
packages.rs installed package records (full manifest snapshot,
incl. held/arch/essential/hooks/payload_sha256)
files.rs file → owning-package reverse index
history.rs append-only JSON-lines operation audit log

install/ applying a plan to the filesystem
extractor.rs archive → a destination directory (install root,
or a staging directory — see transaction.rs)
diskspace.rs free-space precheck before a download/extract starts
hooks.rs runs a declared maintainer script, never via a shell
transaction.rs atomic install/remove: stage → verify → hook →
activate (rename into place) → hook → commit;
payload-hash re-check; backup-based rollback
rollback.rs undo helpers used on failure

security/ crypto primitives (no package/repo knowledge)
checksum.rs SHA-256, incl. aggregate payload-directory hash
signature.rs Ed25519 sign + verify
keys.rs trusted signer keystore

config/ on-disk config → resolved paths; RepoSource
(plain URL, or {url, signer, mirrors, priority}
for a signed/mirrored/prioritized repo); target_arch
service/ orchestration layer main.rs (and mitos-pkgd) call into
lock.rs exclusive lock over the package database
progress.rs ProgressEvent + the *_with_progress method
variants; what mitos-pkgd streams to clients
error.rs one PkgError enum for the whole crate

docs/
INTEGRATION.md CLI, library API, on-disk schemas, and file
layout — for other MITOS repos wiring up
against mitos-pkg
```

## Design notes

- **No async runtime.** Networking uses `ureq` (blocking) instead of
`reqwest`/`tokio` — installs happen one package at a time anyway, so an
async I/O driver would only add resident memory without buying anything.
- **Non-circular trust chain.** The whole-archive checksum lives in the
*repository index* (`PackageMetadata::sha256`), checked before an archive
is even opened. `Manifest::payload_sha256`, embedded in the archive, is a
hash of the *payload files* instead — an archive can't contain a valid
hash of its own complete bytes without being self-referential, so those
two checks are deliberately separate values with separate jobs.
`payload_sha256` is also what gets signed, gets re-verified again after
staged extraction (`install::transaction`) independent of the
pre-download check, and is what `verify` re-derives on demand afterward
to catch drift.
- **Repository indexes are signable, not just packages**, and every
configured repository can name fallback mirrors and a tie-breaking
priority. A repository configured with a `signer`
(`config::RepoSource::Detailed`) must serve a valid Ed25519 signature
over its index bytes at `<url>.sig` — tried across every mirror in order,
so one mirror failing to serve a valid signature doesn't fail the whole
refresh — checked with the same trust chain as package signatures
(`package::signature::verify_index`). Left unset, an index is accepted
unsigned, same opt-in-by-default posture as `Manifest::signer`. When more
than one configured repository lists the *same* version of a package,
`priority` breaks the tie (a newer version elsewhere still always wins on
its own); every fetch (index or archive) retries transient failures with
backoff (`repository::download::fetch_with_retry`).
- **Provides & conflicts are enforced, not just parsed.** A dependency that
doesn't match any real package by name falls back to searching for a
`provides` entry (`repository::index::find_provider`); a resolved install
plan is checked against both directions of `conflicts` before anything is
downloaded (`dependency::resolver::check_conflicts`). `remove`'s
dependents check (`dependency::resolver::dependents_of`) is also
`provides`-aware: removing the concrete package satisfying another
package's virtual dependency is blocked (or cascaded, with `--cascade`)
the same as removing something depended on by name.
- **Backtracking dependency resolution.** `Resolver` runs a restart-based,
requirement-narrowing search rather than pure greedy assignment: a
requirement conflict discovered against an already-resolved package
folds into that package's accumulated requirement (via `semver`
comparator-list concatenation — exactly requirement intersection, given
`VersionReq`'s AND-of-all-comparators matching) and the whole resolution
restarts with it in place, so a diamond needing overlapping-but-different
ranges of the same shared package is solved instead of rejected whenever
a solution actually exists. Still not a full SAT/PubGrub solver — see
"Status" for the remaining edge it doesn't cover. Every candidate is also
filtered by architecture (`package::arch`) against the effective target
(`Config::target_arch`, or the host's own architecture).
- **Staged, atomic install; maintainer hooks; essential-package guard.**
`install::transaction::Transaction` extracts into a scratch staging
directory under the install root first, re-checks the payload hash and
file-ownership conflicts *before* anything real is touched, runs a
declared `pre_install` hook against the staged files, then moves
(`rename`) everything into place and runs `post_install` — a failure at
any point before commit means the live install root was never touched at
all. `remove` runs `pre_remove` while files are still present and a
best-effort `post_remove` afterward. Hook scripts are ordinary payload
files, so they're covered by the same checksum/signature trust chain as
everything else — there's no separate mechanism to trust or bypass. A
package marked `essential` refuses `remove`/`autoremove` without
`--force`. Multi-package `install` is now whole-plan atomic: an
N-th-package failure rolls back everything the same call already
installed, not just that one package's own transaction.
- **Transactional install/remove/upgrade.** `upgrade` fetches and verifies
the replacement archive *before* removing the old one, and refuses a
version bump that would break another installed package's dependency on
it (`service::lifecycle::check_upgrade_safe`), so a bad download or a
breaking bump never leaves the system in a worse state than before. A
free-space check (`install::diskspace`) runs before any download or
extraction starts, using each package's `installed_size_bytes`.
- **Locked like a real package database.** `service::lock::Lock` takes an
exclusive lock file (à la dpkg/pacman) around every install/remove/
upgrade/hold/unhold/autoremove, so two concurrent runs can't corrupt
each other's writes.
- **Chroot-friendly by construction.** Every filesystem path flows through
`config::Config`, so the whole tool can run against a non-`/` root for
testing or image builds via `mitos-pkg --root <path> ...`.
- **`mitos-pkgd` is thread-per-connection, not async.** Connections are
rare (a handful of GUI panels, an occasional CLI call) and every
mutating request already serializes through one `RwLock` write lock, so
an async I/O driver would cost resident memory the same way `tokio`
would for the CLI's networking — see the `ureq` note in `Cargo.toml`.
Read-only requests (`list`/`search`/`info`/`verify`/`history`/`clean`)
take a read lock instead, so a GUI can keep refreshing its panel while
something else installs. `SIGTERM`/`SIGINT` are caught (a signal handler
sets a flag; an ordinary thread polls it and removes the socket before
exiting) rather than left to the default disposition — see Status for
what this graceful-shutdown pass doesn't cover.
- **The daemon's socket is the access-control boundary, not a
per-action policy engine.** `mitos-pkgd`'s socket is created `0660` —
root and one group can connect, everyone else is refused before a byte
of protocol is parsed. *Which* users are in that group is left to
whatever starts the daemon (`mitos-services` already owns runtime-
directory ownership and privilege dropping elsewhere in MITOS); there's
no PackageKit-style polkit layer distinguishing "may list" from "may
install" within a connection that's already allowed to connect at all.
That's a deliberate scope line for now, not an oversight — see Status.

## Building a package

A build source directory looks like:

```
my-package/
pkg.json # PackageSpec: name, version, description, dependencies,
# provides, conflicts, optional signer/arch/essential/hooks
payload/ # files exactly as they should land under the install root
```

`mitos-pkg build my-package/` writes `<name>-<version>.mpkg` to the current
directory (or `-o <dir>`). With `--sign-with <seed-file>` (a file holding a
hex-encoded 32-byte Ed25519 seed — e.g. `openssl rand -hex 32`), it also
prints a signature to paste into your repository index alongside the
package's entry; there's no `keygen` command since signing needs no RNG,
only a valid seed. The same seed can sign a repository index (see
`docs/INTEGRATION.md`) — there's no separate "repo key" concept.

`pkg.json` can also declare `"arch"` (defaults to `["any"]` — set this to
`["x86_64"]`/`["aarch64"]`/both for anything with compiled, architecture-
specific content), `"essential"` (defaults to `false` — refuses `remove`/
`autoremove` without `--force`), and `"hooks"` (payload-relative script
paths run at defined install/remove lifecycle points — see
`docs/INTEGRATION.md#maintainer-hooks`).

## Status

Known, deliberate gaps rather than oversights:
- **Not a full SAT/PubGrub solver.** `Resolver`'s backtracking search (see
"Design notes") solves the common diamond-dependency case — two branches
needing overlapping-but-different ranges of the same shared package —
by narrowing and restarting, but it doesn't do true conflict-directed
backjumping. Two acceptable *versions* of a shared package whose ranges
are individually fine but whose own transitive dependencies conflict is
still reported as a conflict rather than searched around. Closing that
gap fully is a meaningfully bigger undertaking (real backjumping, or a
PubGrub-style solver) than this tool currently warrants.
- **`mitos-pkgd` has no per-action authorization.** Anyone who can open
the socket can do anything a `PackageService` can do — there's no
polkit-style "may search, may not install" distinction between clients.
Coarser than that: connection-level (socket permissions), not
request-level.
- **Progress reporting is per-package-phase, not per-byte.** A GUI
watching `ProgressEvent::Fetching` sees "downloading mitos-shell" but
not a percentage — byte-level progress would mean threading a callback
into `repository::download`'s HTTP loop, not done here yet.
- **`mitos-pkgd`'s graceful shutdown doesn't drain in-flight requests.**
`SIGTERM`/`SIGINT` now remove the socket and exit promptly (see "Design
notes") instead of relying purely on the next start's stale-socket
check, but an already-accepted connection mid-request is simply cut
short by the process exiting, not given a chance to finish first. A
real drain needs connection tracking this crate doesn't have yet.
- **`history.jsonl` is read in full on every `history` call**, not
paginated on disk or rotated. Fine at the scale this tool targets (see
`database::history`'s doc comment); a system doing very high install
volume would want a rotation/summarization pass this doesn't have.
- **Maintainer hooks run unsandboxed**, with the same privileges as
`mitos-pkg`/`mitos-pkgd` itself — there's no seccomp/namespace isolation
around a hook script the way some modern package managers (e.g. nix's
build sandboxing) apply to build-time or install-time scripts. The
trust boundary is "this payload passed the same checksum/signature
check as everything else in the archive," not "this script's blast
radius is contained even if that check somehow passed something bad."
