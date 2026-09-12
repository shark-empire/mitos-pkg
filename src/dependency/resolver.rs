use crate::database::packages::InstalledDb;
use crate::dependency::graph::DependencyGraph;
use crate::error::{PkgError, Result};
use crate::repository::index::RepositoryIndex;
use crate::repository::metadata::PackageMetadata;
use semver::VersionReq;
use std::collections::{HashMap, HashSet};

/// The result of resolving one package's dependency tree: every package
/// that still needs installing, in the order they must be installed so
/// each one's dependencies are already in place.
#[derive(Debug)]
pub struct InstallPlan {
    pub order: Vec<PackageMetadata>,
}

pub struct Resolver<'a> {
    index: &'a RepositoryIndex,
    installed: &'a InstalledDb,
    /// Architecture every candidate must be installable on (see
    /// `package::arch`). Not `Config::target_arch` itself — callers pass
    /// the already-resolved effective value (`PackageService::effective_arch`)
    /// so this module doesn't need to know `Config` exists.
    target_arch: &'a str,
}

/// Internal control-flow signal used only inside backtracking resolution
/// (never surfaces outside this module): either a genuinely unrecoverable
/// failure, or a request to retry a specific package's candidate choice
/// with an additionally-learned requirement folded in. See
/// `Resolver::resolve_install_req`'s doc comment for how `Narrow` drives
/// the restart loop, and why that's sound.
enum VisitError {
    Fatal(PkgError),
    Narrow(String, VersionReq),
}

/// Cap on how many times resolution restarts with a newly-learned
/// requirement before giving up. Each restart strictly narrows (never
/// widens) some package's accumulated requirement, so termination is
/// guaranteed well before this in any real dependency graph; it exists
/// purely as a safety valve against a pathological/malicious index.
const MAX_RESTARTS: usize = 200;

impl<'a> Resolver<'a> {
    pub fn new(
        index: &'a RepositoryIndex,
        installed: &'a InstalledDb,
        target_arch: &'a str,
    ) -> Self {
        Self {
            index,
            installed,
            target_arch,
        }
    }

    /// Resolves `root_name` — at whatever version `latest()` would pick —
    /// and its full (transitive) dependency tree. Thin wrapper over
    /// `resolve_install_req` for callers (and existing tests) that don't
    /// need to pin an exact version.
    pub fn resolve_install(&self, root_name: &str) -> Result<InstallPlan> {
        self.resolve_install_req(root_name, &VersionReq::STAR)
    }

    /// Resolves `root_name` against `root_req` (e.g. an exact-version pin
    /// from `mitos-pkg install name@1.2.3`) and its full transitive
    /// dependency tree, on packages installable on `target_arch`.
    ///
    /// This is a **restart-based, requirement-narrowing search**: each
    /// attempt runs the same single-pass, first-candidate-wins visit a
    /// purely greedy resolver would, but a requirement conflict
    /// discovered against an already-resolved name no longer fails
    /// immediately. Instead it's folded — via `semver` comparator-list
    /// concatenation, which is exactly requirement intersection given
    /// `VersionReq`'s AND-of-all-comparators semantics (there is no OR in
    /// Cargo's version-requirement syntax to worry about colliding with)
    /// — into that name's accumulated requirement in `extra_reqs`, and
    /// the *entire* resolution restarts from the root with that extra
    /// constraint in place, so the shared package is picked correctly
    /// from the very first visit next time. `extra_reqs` only ever grows
    /// more restrictive across restarts, which is what guarantees
    /// termination (bounded by `MAX_RESTARTS`) without needing true
    /// conflict-directed backjumping.
    ///
    /// What this *doesn't* solve: two acceptable versions of a shared
    /// package whose version ranges are individually compatible but whose
    /// own *transitive dependencies* conflict — that's still reported as
    /// `DependencyConflict` rather than searched around. Closing that gap
    /// fully needs real backjumping (or a PubGrub-style solver), a larger
    /// undertaking than this tool currently warrants — see README
    /// "Status".
    pub fn resolve_install_req(
        &self,
        root_name: &str,
        root_req: &VersionReq,
    ) -> Result<InstallPlan> {
        let mut extra_reqs: HashMap<String, VersionReq> = HashMap::new();

        for _ in 0..MAX_RESTARTS {
            let mut graph = DependencyGraph::new();
            let mut resolved: HashMap<String, PackageMetadata> = HashMap::new();
            let mut visiting: HashSet<String> = HashSet::new();

            match self.visit(
                root_name,
                root_req,
                None,
                &extra_reqs,
                &mut graph,
                &mut resolved,
                &mut visiting,
            ) {
                Ok(()) => {
                    let order: Vec<PackageMetadata> = graph
                        .install_order()?
                        .into_iter()
                        .filter_map(|name| resolved.remove(&name))
                        .collect();
                    self.check_conflicts(&order)?;
                    return Ok(InstallPlan { order });
                }
                Err(VisitError::Narrow(name, req)) => {
                    let combined = match extra_reqs.get(&name) {
                        Some(existing) => intersect_req(existing, &req),
                        None => req,
                    };
                    extra_reqs.insert(name, combined);
                    continue;
                }
                Err(VisitError::Fatal(e)) => return Err(e),
            }
        }

        Err(PkgError::DependencyConflict(format!(
            "could not find a mutually compatible set of versions for '{root_name}' after {MAX_RESTARTS} attempts — the dependency graph likely has no valid solution"
        )))
    }

    #[allow(clippy::too_many_arguments)]
    fn visit(
        &self,
        name: &str,
        req: &VersionReq,
        required_by: Option<&str>,
        extra_reqs: &HashMap<String, VersionReq>,
        graph: &mut DependencyGraph,
        resolved: &mut HashMap<String, PackageMetadata>,
        visiting: &mut HashSet<String>,
    ) -> std::result::Result<(), VisitError> {
        // Record the node/edge unconditionally, even for already-satisfied
        // or already-visited packages, so ordering among siblings that
        // share a dependency (diamonds) still comes out right.
        graph.add_node(name);
        if let Some(parent) = required_by {
            graph.add_edge(parent, name);
        }

        let effective_req = match extra_reqs.get(name) {
            Some(extra) => intersect_req(req, extra),
            None => req.clone(),
        };

        // Already picked a candidate for this name in this attempt — the
        // requirement that brought us back here must still hold against
        // that one choice. If it doesn't, this is exactly the diamond
        // case `resolve_install_req`'s doc comment describes: signal a
        // restart rather than failing outright.
        if let Some(existing) = resolved.get(name) {
            if !effective_req.matches(&existing.version) {
                return Err(VisitError::Narrow(name.to_string(), effective_req));
            }
            return Ok(());
        }

        // Already on the system — fine, *provided* the installed version
        // actually meets this requirement. mitos-pkg intentionally does
        // not auto-upgrade transitively pulled-in dependencies just
        // because a new requirement showed up; it surfaces the mismatch
        // instead. Unlike a not-yet-resolved candidate, there's no
        // "different version to try" here — the installed version is
        // fixed truth — so this is always `Fatal`, never `Narrow`.
        if let Some(installed_pkg) = self.installed.get(name) {
            if !effective_req.matches(&installed_pkg.version) {
                return Err(VisitError::Fatal(PkgError::DependencyConflict(format!(
                    "installed '{name} {}' does not satisfy '{name} {effective_req}' required by '{}' — upgrade it first",
                    installed_pkg.version,
                    required_by.unwrap_or(name)
                ))));
            }
            return Ok(());
        }

        if !visiting.insert(name.to_string()) {
            return Err(VisitError::Fatal(PkgError::CircularDependency(
                name.to_string(),
            )));
        }

        let candidate = match self
            .index
            .best_match_for_arch(name, &effective_req, self.target_arch)
        {
            Some(c) => c.clone(),
            None => {
                // Distinguish "nothing installable here" from "nothing at
                // all matches" so the error actually points at the real
                // cause rather than looking like a missing package.
                if let Some(wrong_arch) = self.index.best_match(name, &effective_req) {
                    return Err(VisitError::Fatal(PkgError::ArchMismatch {
                        name: name.to_string(),
                        package_arch: wrong_arch.arch.clone(),
                        host_arch: self.target_arch.to_string(),
                    }));
                }
                let err = if self.index.packages.contains_key(name) {
                    if extra_reqs.contains_key(name) {
                        PkgError::DependencyConflict(format!(
                            "no version of '{name}' satisfies every requirement placed on it across this install (combined: {effective_req})"
                        ))
                    } else {
                        PkgError::NoMatchingVersion {
                            name: name.to_string(),
                            requirement: effective_req.to_string(),
                        }
                    }
                } else {
                    PkgError::PackageNotFound(name.to_string())
                };
                return Err(VisitError::Fatal(err));
            }
        };

        for dep in &candidate.dependencies {
            // A dependency can be satisfied either by a real package at a
            // matching version, or — if no version of that exact name
            // matches — by whatever package declares it as a virtual
            // `provides`. Either way we recurse on the *real* package
            // name, since that's what actually needs installing.
            match self
                .index
                .best_match_for_arch(&dep.name, &dep.version_req, self.target_arch)
            {
                Some(_) => {
                    self.visit(
                        &dep.name,
                        &dep.version_req,
                        Some(name),
                        extra_reqs,
                        graph,
                        resolved,
                        visiting,
                    )?;
                }
                None => {
                    let provider = self
                        .index
                        .find_provider_for_arch(&dep.name, self.target_arch)
                        .ok_or_else(|| {
                            VisitError::Fatal(PkgError::DependencyConflict(format!(
                                "no version of '{}' satisfies '{}' required by '{}'",
                                dep.name, dep.version_req, name
                            )))
                        })?
                        .name
                        .clone();
                    // `provides` is unversioned by design (see
                    // `RepositoryIndex::find_provider`) — once we know
                    // which concrete package provides it, recurse
                    // unconstrained rather than re-applying `dep.version_req`
                    // to a package it was never written against.
                    self.visit(
                        &provider,
                        &VersionReq::STAR,
                        Some(name),
                        extra_reqs,
                        graph,
                        resolved,
                        visiting,
                    )?;
                }
            }
        }

        visiting.remove(name);
        resolved.insert(name.to_string(), candidate);
        Ok(())
    }

    /// Fails fast if anything in `plan` conflicts with an already-installed
    /// package, with another package in the same plan, or vice versa (an
    /// installed package that conflicts with something we're about to
    /// add). Real package managers (dpkg, pacman) check this before
    /// touching disk — a conflict discovered mid-transaction is much
    /// harder to walk back cleanly.
    fn check_conflicts(&self, plan: &[PackageMetadata]) -> Result<()> {
        for candidate in plan {
            for conflict in &candidate.conflicts {
                if self.installed.is_installed(conflict) {
                    return Err(PkgError::DependencyConflict(format!(
                        "'{}' conflicts with already-installed '{}'",
                        candidate.name, conflict
                    )));
                }
                if plan.iter().any(|p| &p.name == conflict) {
                    return Err(PkgError::DependencyConflict(format!(
                        "'{}' conflicts with '{}', both required by this install",
                        candidate.name, conflict
                    )));
                }
            }
            for (installed_name, installed_pkg) in self.installed.all() {
                if installed_pkg.conflicts.iter().any(|c| c == &candidate.name) {
                    return Err(PkgError::DependencyConflict(format!(
                        "installed package '{}' conflicts with '{}'",
                        installed_name, candidate.name
                    )));
                }
            }
        }
        Ok(())
    }

    /// Installed packages that would be left with an unsatisfied
    /// dependency if `name` were removed right now — i.e. what `remove`
    /// must refuse to break (or, with `--cascade`, must remove alongside
    /// it).
    ///
    /// Accounts for virtual `provides`: if `name` is the concrete package
    /// satisfying another package's dependency on a capability (not on
    /// `name` itself), that other package is still counted as a
    /// dependent — unless some *other* installed package also provides
    /// the same capability and would keep satisfying it after `name` is
    /// gone.
    pub fn dependents_of(&self, name: &str) -> Vec<String> {
        self.installed
            .all()
            .iter()
            .filter(|(pkg_name, pkg)| {
                pkg_name.as_str() != name
                    && pkg
                        .dependencies
                        .iter()
                        .any(|d| self.only_satisfied_by(&d.name, name))
            })
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// True if `candidate` is currently what satisfies dependency name
    /// `dep_name` among installed packages (by literal name or by
    /// `provides`), and no *other* installed package would still satisfy
    /// it if `candidate` were removed.
    fn only_satisfied_by(&self, dep_name: &str, candidate: &str) -> bool {
        let candidate_satisfies = dep_name == candidate
            || self
                .installed
                .get(candidate)
                .map(|p| p.provides.iter().any(|provided| provided == dep_name))
                .unwrap_or(false);
        if !candidate_satisfies {
            return false;
        }

        !self.installed.all().iter().any(|(other_name, other_pkg)| {
            other_name.as_str() != candidate
                && (other_name.as_str() == dep_name
                    || other_pkg
                        .provides
                        .iter()
                        .any(|provided| provided == dep_name))
        })
    }
}

/// Concatenates two `VersionReq`s' comparator lists. Correct as an
/// intersection specifically because of how `semver::VersionReq` matching
/// works: a version matches a `VersionReq` only if it satisfies *every*
/// comparator in it, so the union of two comparator lists matches exactly
/// the versions that would have matched both original requirements.
/// Implemented via `Display`/`parse` round-trip (`a.to_string()` +
/// `b.to_string()`, comma-joined, re-parsed) rather than constructing a
/// `VersionReq` from its `comparators` field directly — both are public
/// API this crate already relies on elsewhere, but going through the
/// string form sidesteps ever needing to assume anything about the
/// struct's exact internal representation.
fn intersect_req(a: &VersionReq, b: &VersionReq) -> VersionReq {
    if a.comparators.is_empty() {
        return b.clone();
    }
    if b.comparators.is_empty() {
        return a.clone();
    }
    let combined = format!("{a}, {b}");
    VersionReq::parse(&combined).unwrap_or_else(|_| a.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::packages::InstalledPackage;
    use crate::dependency::version::Dependency;
    use crate::package::hooks::Hooks;
    use semver::Version;

    const HOST: &str = "x86_64";

    fn meta(name: &str, version: &str) -> PackageMetadata {
        PackageMetadata {
            name: name.to_string(),
            version: Version::parse(version).unwrap(),
            description: String::new(),
            dependencies: Vec::new(),
            provides: Vec::new(),
            conflicts: Vec::new(),
            url: String::new(),
            sha256: String::new(),
            signature: None,
            size_bytes: 0,
            arch: vec!["any".to_string()],
            essential: false,
            installed_size_bytes: 0,
            priority: 0,
        }
    }

    fn installed_pkg(name: &str, version: &str) -> InstalledPackage {
        InstalledPackage {
            name: name.to_string(),
            version: Version::parse(version).unwrap(),
            description: String::new(),
            dependencies: Vec::new(),
            provides: Vec::new(),
            conflicts: Vec::new(),
            installed_files: Vec::new(),
            explicit: true,
            held: false,
            arch: vec!["any".to_string()],
            essential: false,
            hooks: Hooks::default(),
            payload_sha256: String::new(),
        }
    }

    #[test]
    fn virtual_dependency_resolves_via_provides() {
        use semver::VersionReq;

        let mut app = meta("app", "1.0.0");
        app.dependencies.push(Dependency {
            name: "mitos-libc".to_string(),
            version_req: VersionReq::parse("*").unwrap(),
        });

        let mut libc_impl = meta("mitos-libc-musl", "0.3.0");
        libc_impl.provides.push("mitos-libc".to_string());

        let mut index = RepositoryIndex::default();
        index.packages.insert("app".to_string(), vec![app]);
        index
            .packages
            .insert("mitos-libc-musl".to_string(), vec![libc_impl]);

        let installed = InstalledDb::default();
        let plan = Resolver::new(&index, &installed, HOST)
            .resolve_install("app")
            .unwrap();

        let names: Vec<&str> = plan.order.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"mitos-libc-musl"));
        assert!(names.contains(&"app"));
    }

    #[test]
    fn conflicting_package_is_rejected() {
        let mut app = meta("app", "1.0.0");
        app.conflicts.push("legacy-app".to_string());

        let mut index = RepositoryIndex::default();
        index.packages.insert("app".to_string(), vec![app]);

        let mut installed = InstalledDb::default();
        installed.insert(installed_pkg("legacy-app", "0.9.0"));

        let err = Resolver::new(&index, &installed, HOST)
            .resolve_install("app")
            .unwrap_err();
        assert!(matches!(err, PkgError::DependencyConflict(_)));
    }

    #[test]
    fn dependency_resolves_to_a_version_that_actually_satisfies_the_requirement() {
        use semver::VersionReq;

        let mut app = meta("app", "1.0.0");
        app.dependencies.push(Dependency {
            name: "mitos-libc".to_string(),
            version_req: VersionReq::parse(">=0.3.0, <0.4.0").unwrap(),
        });

        let mut index = RepositoryIndex::default();
        index.packages.insert("app".to_string(), vec![app]);
        index.packages.insert(
            "mitos-libc".to_string(),
            vec![meta("mitos-libc", "0.3.5"), meta("mitos-libc", "0.5.0")],
        );

        let installed = InstalledDb::default();
        let plan = Resolver::new(&index, &installed, HOST)
            .resolve_install("app")
            .unwrap();

        let libc = plan
            .order
            .iter()
            .find(|p| p.name == "mitos-libc")
            .expect("mitos-libc should be in the plan");
        assert_eq!(libc.version, Version::parse("0.3.5").unwrap());
    }

    #[test]
    fn install_pins_to_the_exact_requested_version() {
        let mut index = RepositoryIndex::default();
        index.packages.insert(
            "app".to_string(),
            vec![meta("app", "1.0.0"), meta("app", "2.0.0")],
        );

        let installed = InstalledDb::default();
        let req = VersionReq::parse("=1.0.0").unwrap();
        let plan = Resolver::new(&index, &installed, HOST)
            .resolve_install_req("app", &req)
            .unwrap();

        assert_eq!(plan.order.len(), 1);
        assert_eq!(plan.order[0].version, Version::parse("1.0.0").unwrap());
    }

    #[test]
    fn diamond_with_incompatible_requirements_is_a_conflict_not_a_silent_pick() {
        use semver::VersionReq;

        // app depends on both `net` and `old-client`; `net` wants a
        // recent `libc`, `old-client` needs an older major, and no
        // single version of `libc` can satisfy both — this must surface
        // as a conflict even from a resolver that backtracks, since no
        // amount of retrying finds a version that doesn't exist.
        let mut app = meta("app", "1.0.0");
        app.dependencies.push(Dependency {
            name: "net".to_string(),
            version_req: VersionReq::parse("*").unwrap(),
        });
        app.dependencies.push(Dependency {
            name: "old-client".to_string(),
            version_req: VersionReq::parse("*").unwrap(),
        });

        let mut net = meta("net", "1.0.0");
        net.dependencies.push(Dependency {
            name: "libc".to_string(),
            version_req: VersionReq::parse(">=2.0.0").unwrap(),
        });

        let mut old_client = meta("old-client", "1.0.0");
        old_client.dependencies.push(Dependency {
            name: "libc".to_string(),
            version_req: VersionReq::parse("<2.0.0").unwrap(),
        });

        let mut index = RepositoryIndex::default();
        index.packages.insert("app".to_string(), vec![app]);
        index.packages.insert("net".to_string(), vec![net]);
        index
            .packages
            .insert("old-client".to_string(), vec![old_client]);
        index.packages.insert(
            "libc".to_string(),
            vec![meta("libc", "1.5.0"), meta("libc", "2.5.0")],
        );

        let installed = InstalledDb::default();
        let err = Resolver::new(&index, &installed, HOST)
            .resolve_install("app")
            .unwrap_err();
        assert!(matches!(err, PkgError::DependencyConflict(_)));
    }

    #[test]
    fn diamond_with_a_real_solution_is_backtracked_into_instead_of_rejected() {
        use semver::VersionReq;

        // Same shape as the test above, but this time a version of
        // `libc` genuinely satisfies both branches (1.5.0 is both
        // `>=1.0.0,<3.0.0` *and* `<2.0.0`) — just not the newest one a
        // purely greedy, non-backtracking resolver would grab for `net`
        // on its first visit. This is the exact case
        // `resolve_install_req`'s restart-based search exists for.
        let mut app = meta("app", "1.0.0");
        app.dependencies.push(Dependency {
            name: "net".to_string(),
            version_req: VersionReq::parse("*").unwrap(),
        });
        app.dependencies.push(Dependency {
            name: "old-client".to_string(),
            version_req: VersionReq::parse("*").unwrap(),
        });

        let mut net = meta("net", "1.0.0");
        net.dependencies.push(Dependency {
            name: "libc".to_string(),
            version_req: VersionReq::parse(">=1.0.0, <3.0.0").unwrap(),
        });

        let mut old_client = meta("old-client", "1.0.0");
        old_client.dependencies.push(Dependency {
            name: "libc".to_string(),
            version_req: VersionReq::parse("<2.0.0").unwrap(),
        });

        let mut index = RepositoryIndex::default();
        index.packages.insert("app".to_string(), vec![app]);
        index.packages.insert("net".to_string(), vec![net]);
        index
            .packages
            .insert("old-client".to_string(), vec![old_client]);
        index.packages.insert(
            "libc".to_string(),
            vec![meta("libc", "1.5.0"), meta("libc", "2.5.0")],
        );

        let installed = InstalledDb::default();
        let plan = Resolver::new(&index, &installed, HOST)
            .resolve_install("app")
            .expect("a mutually compatible version (1.5.0) exists and should be found");

        let libc = plan
            .order
            .iter()
            .find(|p| p.name == "libc")
            .expect("libc should be in the plan");
        assert_eq!(libc.version, Version::parse("1.5.0").unwrap());
    }

    #[test]
    fn arch_mismatched_candidate_is_reported_distinctly() {
        let mut app = meta("app", "1.0.0");
        app.arch = vec!["aarch64".to_string()];

        let mut index = RepositoryIndex::default();
        index.packages.insert("app".to_string(), vec![app]);

        let installed = InstalledDb::default();
        let err = Resolver::new(&index, &installed, "x86_64")
            .resolve_install("app")
            .unwrap_err();
        assert!(matches!(err, PkgError::ArchMismatch { .. }));
    }

    #[test]
    fn arch_compatible_candidate_resolves_normally() {
        let mut app = meta("app", "1.0.0");
        app.arch = vec!["x86_64".to_string(), "aarch64".to_string()];

        let mut index = RepositoryIndex::default();
        index.packages.insert("app".to_string(), vec![app]);

        let installed = InstalledDb::default();
        let plan = Resolver::new(&index, &installed, "aarch64")
            .resolve_install("app")
            .unwrap();
        assert_eq!(plan.order.len(), 1);
    }

    #[test]
    fn dependents_of_catches_provides_based_dependents() {
        let index = RepositoryIndex::default();
        let mut installed = InstalledDb::default();

        let mut app = installed_pkg("app", "1.0.0");
        app.dependencies.push(Dependency {
            name: "mitos-libc".to_string(),
            version_req: VersionReq::parse("*").unwrap(),
        });
        installed.insert(app);

        let mut libc_impl = installed_pkg("mitos-libc-musl", "0.3.0");
        libc_impl.provides.push("mitos-libc".to_string());
        installed.insert(libc_impl);

        let resolver = Resolver::new(&index, &installed, HOST);
        let dependents = resolver.dependents_of("mitos-libc-musl");
        assert_eq!(dependents, vec!["app".to_string()]);
    }

    #[test]
    fn dependents_of_ignores_a_provider_with_a_redundant_alternative() {
        let index = RepositoryIndex::default();
        let mut installed = InstalledDb::default();

        let mut app = installed_pkg("app", "1.0.0");
        app.dependencies.push(Dependency {
            name: "mitos-libc".to_string(),
            version_req: VersionReq::parse("*").unwrap(),
        });
        installed.insert(app);

        let mut musl = installed_pkg("mitos-libc-musl", "0.3.0");
        musl.provides.push("mitos-libc".to_string());
        installed.insert(musl);

        let mut glibc = installed_pkg("mitos-libc-glibc", "0.3.0");
        glibc.provides.push("mitos-libc".to_string());
        installed.insert(glibc);

        let resolver = Resolver::new(&index, &installed, HOST);
        // Either provider alone still leaves the other satisfying `app`.
        assert!(resolver.dependents_of("mitos-libc-musl").is_empty());
        assert!(resolver.dependents_of("mitos-libc-glibc").is_empty());
    }
}
