# mitos-pkg

The official package manager for [MITOS](https://github.com/shark-empire).

## Commands

```
mitos-pkg install <name>              install a package + its unmet deps
mitos-pkg remove <name>                remove (fails if something depends on it)
mitos-pkg upgrade [name]                upgrade one package, or all if none given
mitos-pkg autoremove                     remove orphaned (non-explicit) deps
mitos-pkg list                            list installed packages
mitos-pkg search <query>                  search the local repository index
mitos-pkg info <name>                      show details, installed or available
mitos-pkg update                            refresh the repository index
mitos-pkg build <source> [-o out] [--sign-with seed]
                                              package source/ into a .mpkg
```

`--root <path>` (global) installs/removes relative to a chroot-style path
instead of `/`. `--config <path>` overrides the config file location.

## Layout

```
src/
  main.rs            thin CLI dispatch → service::PackageService
  cli.rs              clap argument/subcommand definitions

  package/            what a .mpkg archive *is*, and how to make one
    manifest.rs        Manifest struct embedded in every archive
    spec.rs             PackageSpec — the hand-authored pkg.json a build reads
    build.rs             packages a source dir into a signed, checksummed .mpkg
    format.rs           archive layout constants (manifest.json, payload/)
    archive.rs           tar+gzip read/extract
    signature.rs          checksum + signature trust chain for one archive

  repository/         where packages come from
    metadata.rs         lightweight per-version metadata (index entry)
    index.rs             merged local index cache, search/best-match/provides
    download.rs           checksum-verified HTTP fetch (ureq, blocking)

  dependency/          what needs to happen, and in what order
    version.rs           Dependency (name + semver requirement)
    graph.rs               DAG + topological install order
    resolver.rs              dependency tree → install plan; provides
                               fallback resolution; conflict checking

  database/            what's actually installed, on disk
    packages.rs         installed package records (full manifest snapshot)
    files.rs               file → owning-package reverse index

  install/              applying a plan to the filesystem
    extractor.rs          archive → install-root
    transaction.rs          atomic install/remove, payload-hash re-check,
                              backup-based rollback
    rollback.rs               undo helpers used on failure

  security/             crypto primitives (no package/repo knowledge)
    checksum.rs            SHA-256, incl. aggregate payload-directory hash
    signature.rs            Ed25519 sign + verify
    keys.rs                  trusted signer keystore

  config/                on-disk config → resolved paths
  service/               orchestration layer main.rs calls into
    lock.rs                exclusive lock over the package database
  error.rs                one PkgError enum for the whole crate
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
- **Provides & conflicts are enforced, not just parsed.** A dependency that
  doesn't match any real package by name falls back to searching for a
  `provides` entry (`repository::index::find_provider`); a resolved install
  plan is checked against both directions of `conflicts` before anything is
  downloaded (`dependency::resolver::check_conflicts`).
- **Transactional install/remove/upgrade.** `install::transaction::Transaction`
  either fully applies or fully rolls back. `upgrade` fetches and verifies
  the replacement archive *before* removing the old one, so a bad download
  never leaves a package removed with nothing to replace it.
- **Locked like a real package database.** `service::lock::Lock` takes an
  exclusive lock file (à la dpkg/pacman) around every install/remove/
  upgrade/autoremove, so two concurrent runs can't corrupt each other's
  writes.
- **Chroot-friendly by construction.** Every filesystem path flows through
  `config::Config`, so the whole tool can run against a non-`/` root for
  testing or image builds via `mitos-pkg --root <path> ...`.

## Building a package

A build source directory looks like:

```
my-package/
  pkg.json     # PackageSpec: name, version, description, dependencies,
               #  provides, conflicts, optional signer
  payload/     # files exactly as they should land under the install root
```

`mitos-pkg build my-package/` writes `<name>-<version>.mpkg` to the current
directory (or `-o <dir>`). With `--sign-with <seed-file>` (a file holding a
hex-encoded 32-byte Ed25519 seed — e.g. `openssl rand -hex 32`), it also
prints a signature to paste into your repository index alongside the
package's entry; there's no `keygen` command since signing needs no RNG,
only a valid seed.

## Status

Known, deliberate gaps rather than oversights:
- `upgrade` doesn't check whether upgrading one package breaks another
  installed package's version requirement on it.
- `dependents_of` (used by `remove`'s safety check) matches on the
  dependency name as declared, not the concrete package a virtual
  `provides` resolved to — a package depending on a virtual capability
  won't currently block removal of the concrete provider satisfying it.
- No cascading `remove --cascade`; use `autoremove` after a plain `remove`
  instead.
