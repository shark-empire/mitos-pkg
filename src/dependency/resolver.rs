use crate::database::packages::InstalledDb;
use crate::dependency::graph::DependencyGraph;
use crate::error::{PkgError, Result};
use crate::repository::index::RepositoryIndex;
use crate::repository::metadata::PackageMetadata;
use std::collections::{HashMap, HashSet};

/// The result of resolving one package's dependency tree: every package
/// that still needs installing, in the order they must be installed so
/// each one's dependencies are already in place.
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

    /// Resolves `root_name` and its full (transitive) dependency tree.
    /// Packages already installed at any version are treated as already
    /// satisfied and left out of the returned plan — mitos-pkg does not
    /// currently upgrade transitively pulled-in dependencies on install,
    /// only on an explicit `upgrade` command.
    pub fn resolve_install(&self, root_name: &str) -> Result<InstallPlan> {
        let mut graph = DependencyGraph::new();
        let mut resolved: HashMap<String, PackageMetadata> = HashMap::new();
        let mut visiting: HashSet<String> = HashSet::new();

        self.visit(root_name, None, &mut graph, &mut resolved, &mut visiting)?;

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

        if resolved.contains_key(name) || self.installed.get(name).is_some() {
            return Ok(());
        }
        if !visiting.insert(name.to_string()) {
            return Err(PkgError::CircularDependency(name.to_string()));
        }

        let candidate = self
            .index
            .latest(name)
            .ok_or_else(|| PkgError::PackageNotFound(name.to_string()))?
            .clone();

        for dep in &candidate.dependencies {
            // A dependency can be satisfied either by a real package at a
            // matching version, or — if no version of that exact name
            // matches — by whatever package declares it as a virtual
            // `provides`. Either way we recurse on the *real* package
            // name, since that's what actually needs installing.
            let resolved_name = match self.index.best_match(&dep.name, &dep.version_req) {
                Some(_) => dep.name.clone(),
                None => self
                    .index
                    .find_provider(&dep.name)
                    .map(|provider| provider.name.clone())
                    .ok_or_else(|| {
                        PkgError::DependencyConflict(format!(
                            "no version of '{}' satisfies '{}' required by '{}'",
                            dep.name, dep.version_req, name
                        ))
                    })?,
            };
            self.visit(&resolved_name, Some(name), graph, resolved, visiting)?;
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

    /// Installed packages that directly depend on `name` — i.e. what would
    /// break if `name` were removed right now.
    ///
    /// Known limitation: if a package depends on `name` only via a virtual
    /// `provides` (rather than naming it directly), this won't catch it —
    /// `InstalledPackage::dependencies` stores the dependency exactly as
    /// declared, not the concrete package it resolved to.
    pub fn dependents_of(&self, name: &str) -> Vec<String> {
        self.installed
            .all()
            .iter()
            .filter(|(_, pkg)| pkg.dependencies.iter().any(|d| d.name == name))
            .map(|(n, _)| n.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::packages::InstalledPackage;
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

    #[test]
    fn virtual_dependency_resolves_via_provides() {
        use crate::dependency::version::Dependency;
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
        installed.insert(InstalledPackage {
            name: "legacy-app".to_string(),
            version: Version::parse("0.9.0").unwrap(),
            description: String::new(),
            dependencies: Vec::new(),
            provides: Vec::new(),
            conflicts: Vec::new(),
            installed_files: Vec::new(),
            explicit: true,
        });

        let err = Resolver::new(&index, &installed)
            .resolve_install("app")
            .unwrap_err();
        assert!(matches!(err, PkgError::DependencyConflict(_)));
    }
}
