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
}

impl<'a> Resolver<'a> {
    pub fn new(index: &'a RepositoryIndex, installed: &'a InstalledDb) -> Self {
        Self { index, installed }
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
    /// dependency tree. Packages already installed are treated as
    /// satisfied *only if their installed version actually matches the
    /// requirement that pulled them in* — mitos-pkg does not silently
    /// accept an installed version that doesn't meet a fresh requirement,
    /// nor does it auto-upgrade to fix that; it reports the conflict and
    /// leaves the decision to the caller (run `upgrade` first).
    ///
    /// Resolution is greedy, not backtracking: each package name is
    /// assigned a candidate version the first time it's reached, and
    /// every later requirement on that same name is checked against that
    /// one choice rather than being solved jointly (the way a SAT-based
    /// resolver like apt's or a PubGrub-based one like cargo's would). A
    /// diamond where two branches need genuinely incompatible ranges of
    /// the same package is reported as a conflict rather than resolved by
    /// trying a different candidate order — a deliberate, documented
    /// trade-off for a tool this size (see README "Status").
    pub fn resolve_install_req(
        &self,
        root_name: &str,
        root_req: &VersionReq,
    ) -> Result<InstallPlan> {
        let mut graph = DependencyGraph::new();
        let mut resolved: HashMap<String, PackageMetadata> = HashMap::new();
        let mut visiting: HashSet<String> = HashSet::new();

        self.visit(root_name, root_req, None, &mut graph, &mut resolved, &mut visiting)?;

        let order: Vec<PackageMetadata> = graph
            .install_order()?
            .into_iter()
            .filter_map(|name| resolved.remove(&name))
            .collect();

        self.check_conflicts(&order)?;

        Ok(InstallPlan { order })
    }

    fn visit(
        &self,
        name: &str,
        req: &VersionReq,
        required_by: Option<&str>,
        graph: &mut DependencyGraph,
        resolved: &mut HashMap<String, PackageMetadata>,
        visiting: &mut HashSet<String>,
    ) -> Result<()> {
        // Record the node/edge unconditionally, even for already-satisfied
        // or already-visited packages, so ordering among siblings that
        // share a dependency (diamonds) still comes out right.
        graph.add_node(name);
        if let Some(parent) = required_by {
            graph.add_edge(parent, name);
        }

        // Already picked a candidate for this name in this resolution —
        // the requirement that brought us back here must still hold
        // against that one choice (see `resolve_install_req` docs on why
        // this doesn't backtrack to try a different version instead).
        if let Some(existing) = resolved.get(name) {
            if !req.matches(&existing.version) {
                return Err(PkgError::DependencyConflict(format!(
                    "'{}' requires '{name} {req}', but {name} {} was already selected to satisfy another dependency in this install",
                    required_by.unwrap_or(name), existing.version
                )));
            }
            return Ok(());
        }

        // Already on the system — fine, *provided* the installed version
        // actually meets this requirement. mitos-pkg intentionally does
        // not auto-upgrade transitively pulled-in dependencies just
        // because a new requirement showed up (see module docs); it
        // surfaces the mismatch instead.
        if let Some(installed_pkg) = self.installed.get(name) {
            if !req.matches(&installed_pkg.version) {
                return Err(PkgError::DependencyConflict(format!(
                    "installed '{name} {}' does not satisfy '{name} {req}' required by '{}' — upgrade it first",
                    installed_pkg.version, required_by.unwrap_or(name)
                )));
            }
            return Ok(());
        }

        if !visiting.insert(name.to_string()) {
            return Err(PkgError::CircularDependency(name.to_string()));
        }

        let candidate = self.index.best_match(name, req).cloned().ok_or_else(|| {
            if self.index.packages.contains_key(name) {
                PkgError::NoMatchingVersion {
                    name: name.to_string(),
                    requirement: req.to_string(),
                }
            } else {
                PkgError::PackageNotFound(name.to_string())
            }
        })?;

        for dep in &candidate.dependencies {
            // A dependency can be satisfied either by a real package at a
            // matching version, or — if no version of that exact name
            // matches — by whatever package declares it as a virtual
            // `provides`. Either way we recurse on the *real* package
            // name, since that's what actually needs installing.
            match self.index.best_match(&dep.name, &dep.version_req) {
                Some(_) => {
                    self.visit(&dep.name, &dep.version_req, Some(name), graph, resolved, visiting)?;
                }
                None => {
                    let provider = self
                        .index
                        .find_provider(&dep.name)
                        .ok_or_else(|| {
                            PkgError::DependencyConflict(format!(
                                "no version of '{}' satisfies '{}' required by '{}'",
                                dep.name, dep.version_req, name
                            ))
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
                    || other_pkg.provides.iter().any(|provided| provided == dep_name))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::packages::InstalledPackage;
    use crate::dependency::version::Dependency;
    use semver::Version;

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
        let plan = Resolver::new(&index, &installed)
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

        let err = Resolver::new(&index, &installed)
            .resolve_install("app")
            .unwrap_err();
        assert!(matches!(err, PkgError::DependencyConflict(_)));
    }

    #[test]
    fn dependency_resolves_to_a_version_that_actually_satisfies_the_requirement() {
        use semver::VersionReq;

        // Two versions of the same dependency exist; only the older one
        // satisfies the requirement. A resolver that greedily grabs
        // whatever's newest (the bug this test guards against) would
        // pick 0.5.0 and silently violate the requirement it just
        // confirmed *some* version could satisfy.
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
        let plan = Resolver::new(&index, &installed)
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
        let plan = Resolver::new(&index, &installed)
            .resolve_install_req("app", &req)
            .unwrap();

        assert_eq!(plan.order.len(), 1);
        assert_eq!(plan.order[0].version, Version::parse("1.0.0").unwrap());
    }

    #[test]
    fn diamond_with_incompatible_requirements_is_a_conflict_not_a_silent_pick() {
        use semver::VersionReq;

        // app depends on both `net` and `old-client`; `net` wants a
        // recent `libc`, `old-client` needs an older major. Both
        // requirements are independently satisfiable (1.5.0 and 2.5.0
        // both exist), but not by the *same* version — since this
        // resolver commits to one version per name the first time it's
        // reached rather than backtracking, that must surface as an
        // error rather than silently keeping whichever version was
        // picked first.
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
        let err = Resolver::new(&index, &installed)
            .resolve_install("app")
            .unwrap_err();
        assert!(matches!(err, PkgError::DependencyConflict(_)));
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

        let resolver = Resolver::new(&index, &installed);
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

        let resolver = Resolver::new(&index, &installed);
        // Either provider alone still leaves the other satisfying `app`.
        assert!(resolver.dependents_of("mitos-libc-musl").is_empty());
        assert!(resolver.dependents_of("mitos-libc-glibc").is_empty());
    }
}
