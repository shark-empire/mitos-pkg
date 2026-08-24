# mitos-pkg

The official package manager for [MITOS](https://github.com/shark-empire).

## Layout

```
src/
  main.rs            thin CLI dispatch → service::PackageService
  cli.rs              clap argument/subcommand definitions

  package/            what a .mpkg archive *is*
    manifest.rs        Manifest struct embedded in every archive
    format.rs          archive layout constants (manifest.json, payload/)
    archive.rs          tar+gzip read/extract
    signature.rs         checksum + signature trust chain for one archive

  repository/         where packages come from
    metadata.rs         lightweight per-version metadata (index entry)
    index.rs             merged local index cache, search/best-match
    download.rs           checksum-verified HTTP fetch (ureq, blocking)

  dependency/          what needs to happen, and in what order
    version.rs           Dependency (name + semver requirement)
    graph.rs               DAG + topological install order
    resolver.rs              walks a package's dependency tree into a plan

  database/            what's actually installed, on disk
    packages.rs         installed package records
    files.rs               file → owning-package reverse index

  install/              applying a plan to the filesystem
    extractor.rs          archive → install-root
    transaction.rs          atomic install/remove with backup-based rollback
    rollback.rs               undo helpers used on failure

  security/             crypto primitives (no package/repo knowledge)
    checksum.rs            SHA-256
    signature.rs            Ed25519 verify
    keys.rs                  trusted signer keystore

  config/                on-disk config → resolved paths
  service/               orchestration layer main.rs calls into
  error.rs                one PkgError enum for the whole crate
```

## Design notes

- **No async runtime.** Networking uses `ureq` (blocking) instead of
  `reqwest`/`tokio` — installs happen one package at a time anyway, so an
  async I/O driver would only add resident memory without buying anything.
- **Trust before extraction.** `package::signature::verify_package` checks
  the archive's SHA-256 against the manifest, and (if signed) the
  signature against a local `KeyStore`, before `install::extractor` ever
  unpacks a single file.
- **Transactional install/remove.** `install::transaction::Transaction`
  either fully applies or fully rolls back — installs clean up written
  files on conflict, removals back files up before deleting and restore
  on failure.
- **Chroot-friendly by construction.** Every filesystem path flows through
  `config::Config`, so the whole tool can run against a non-`/` root for
  testing or image builds via `mitos-pkg --root <path> ...`.

## Status

Core install/remove/list/search/update flow is wired end-to-end. Not yet
implemented: package *building* (there's no `mitos-pkg build` counterpart
to produce a `.mpkg` from source yet), upgrades, and cascading removal.
