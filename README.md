# mitos-pkg

The official package manager for [MITOS](https://github.com/shark-empire).

## Commands

```
mitos-pkg install <name>[@version] install a package (+ its unmet deps);
pin an exact version with @version
mitos-pkg remove <name> [--cascade] remove (fails if something depends on
it, unless --cascade)
mitos-pkg upgrade [name] [--ignore-hold] upgrade one package, or all if none
given; refuses a held package unless
--ignore-hold
mitos-pkg hold <name> pin a package so upgrade skips it
mitos-pkg unhold <name> reverse hold
mitos-pkg autoremove remove orphaned (non-explicit) deps
mitos-pkg list list installed packages ([held] shown)
mitos-pkg search <query> search the local repository index
mitos-pkg info <name> show details, installed or available
mitos-pkg update refresh the repository index
mitos-pkg clean delete cached downloaded archives
mitos-pkg build <source> [-o out] [--sign-with seed]
package source/ into a .mpkg
```

`--root <path>` (global) installs/removes relative to a chroot-style path
instead of `/`. `--config <path>` overrides the config file location. See
[`docs/INTEGRATION.md`](docs/INTEGRATION.md) for the full CLI/library
reference other MITOS components can build against.

## Layout

```
src/
main.rs thin CLI dispatch → service::PackageService
cli.rs clap argument/subcommand definitions

package/ what a .mpkg archive *is*, and how to make one
manifest.rs Manifest struct embedded in every archive
spec.rs PackageSpec — the hand-authored pkg.json a build reads
build.rs packages a source dir into a signed, checksummed .mpkg
format.rs archive layout constants (manifest.json, payload/)
archive.rs tar+gzip read/extract
signature.rs checksum + signature trust chain for one archive,
and for a repository index (verify_index)

repository/ where packages come from
metadata.rs lightweight per-version metadata (index entry)
index.rs merged local index cache, search/best-match/provides
download.rs checksum-verified HTTP fetch (ureq, blocking)

dependency/ what needs to happen, and in what order
version.rs Dependency (name + semver requirement);
`name`/`name@version` CLI-argument parsing
graph.rs DAG + topological install order
resolver.rs dependency tree → install plan; provides
fallback resolution; conflict checking;
dependents_of (provides-aware, for remove)

database/ what's actually installed, on disk
packages.rs installed package records (full manifest snapshot,
incl. held)
files.rs file → owning-package reverse index

install/ applying a plan to the filesystem
extractor.rs archive → install-root
transaction.rs atomic install/remove, payload-hash re-check,
backup-based rollback
rollback.rs undo helpers used on failure

security/ crypto primitives (no package/repo knowledge)
checksum.rs SHA-256, incl. aggregate payload-directory hash
signature.rs Ed25519 sign + verify
keys.rs trusted signer keystore

config/ on-disk config → resolved paths; RepoSource
(plain URL, or {url, signer} for a signed repo)
service/ orchestration layer main.rs calls into
lock.rs exclusive lock over the package database
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
`payload_sha256` is also what gets signed, and gets re-verified again
after extraction (`install::transaction`), independent of the pre-download
check.
- **Repository indexes are signable, not just packages.** Per-package
signing alone leaves a gap: whoever can replace a repository's index can
point an unsigned package at a malicious archive and just publish that
archive's own checksum alongside it. A repository configured with a
`signer` (`config::RepoSource::Detailed`) must serve a valid Ed25519
signature over its index bytes at `<url>.sig`, checked with the same
trust chain as package signatures (`package::signature::verify_index`) —
the same model apt's Release/InRelease, pacman's sync-db `.sig`, and
dnf's `repomd.xml.asc` use. Left unset, an index is accepted unsigned,
same opt-in-by-default posture as `Manifest::signer`.
- **Provides & conflicts are enforced, not just parsed.** A dependency that
doesn't match any real package by name falls back to searching for a
`provides` entry (`repository::index::find_provider`); a resolved install
plan is checked against both directions of `conflicts` before anything is
downloaded (`dependency::resolver::check_conflicts`). `remove`'s
dependents check (`dependency::resolver::dependents_of`) is also
`provides`-aware: removing the concrete package satisfying another
package's virtual dependency is blocked (or cascaded, with `--cascade`)
the same as removing something depended on by name.
- **Version resolution actually enforces the requirement it resolved
against.** `best_match` is used consistently for the *specific* version
a requirement needs, never `latest` as a stand-in — the resolver is
greedy (each package name is assigned one candidate the first time it's
reached) rather than backtracking, so a diamond needing genuinely
incompatible ranges of the same package is reported as a conflict
instead of silently picked one way or the other (see "Status" below).
- **Transactional install/remove/upgrade.** `install::transaction::Transaction`
either fully applies or fully rolls back. `upgrade` fetches and verifies
the replacement archive *before* removing the old one, and refuses a
version bump that would break another installed package's dependency on
it (`service::lifecycle::check_upgrade_safe`), so a bad download or a
breaking bump never leaves the system in a worse state than before.
- **Locked like a real package database.** `service::lock::Lock` takes an
exclusive lock file (à la dpkg/pacman) around every install/remove/
upgrade/hold/unhold/autoremove, so two concurrent runs can't corrupt
each other's writes.
- **Chroot-friendly by construction.** Every filesystem path flows through
`config::Config`, so the whole tool can run against a non-`/` root for
testing or image builds via `mitos-pkg --root <path> ...`.

## Building a package

A build source directory looks like:

```
my-package/
pkg.json # PackageSpec: name, version, description, dependencies,
# provides, conflicts, optional signer
payload/ # files exactly as they should land under the install root
```

`mitos-pkg build my-package/` writes `<name>-<version>.mpkg` to the current
directory (or `-o <dir>`). With `--sign-with <seed-file>` (a file holding a
hex-encoded 32-byte Ed25519 seed — e.g. `openssl rand -hex 32`), it also
prints a signature to paste into your repository index alongside the
package's entry; there's no `keygen` command since signing needs no RNG,
only a valid seed. The same seed can sign a repository index (see
`docs/INTEGRATION.md`) — there's no separate "repo key" concept.

## Status

Known, deliberate gaps rather than oversights:
- **Greedy, non-backtracking dependency resolution.** Each package name is
assigned a candidate version the first time the resolver reaches it;
every later requirement on that name is checked against that one choice
rather than solved jointly. A diamond where two branches each have a
version requirement that's independently satisfiable, but not by the
*same* version, is reported as a conflict rather than resolved by trying
a different candidate order — the kind of case a SAT-based resolver
(apt) or a PubGrub-based one (cargo) would actually solve. Fixing this
properly means backtracking search, which is a meaningfully bigger
undertaking than a tool this size currently warrants.
- **No multi-package rollback.** If a dependency chain's Nth package fails
to verify or install, the first N-1 already committed stay installed —
each package's own transaction is atomic (`install::transaction`), but
`install`'s overall plan is not. Re-running `install` will pick up where
it left off, since already-installed packages in the plan are skipped.
- **No maintainer scripts.** There's no pre/post-install or pre/post-remove
hook mechanism (unlike dpkg's `postinst`, pacman's `.install`, rpm's
`%post`) — a package is exactly its payload files, nothing executes on
install/remove beyond mitos-pkg's own bookkeeping. This is a deliberate
attack-surface tradeoff as much as a scope one.
- **No repository priority/pinning across multiple configured repos.**
When more than one repository lists the same package name, the highest
version wins regardless of which repo it came from — there's no
apt-pinning or dnf-priority equivalent yet.
