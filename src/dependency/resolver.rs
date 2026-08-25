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
    /// only on an explicit future `upgrade` command.
    pub fn resolve_install(&self, root_name: &str) -> Result<InstallPlan> {
        let mut graph = DependencyGraph::new();
        let mut resolved: HashMap<String, PackageMetadata> = HashMap::new();
        let mut visiting: HashSet<String> = HashSet::new();

        self.visit(root_name, None, &mut graph, &mut resolved, &mut visiting)?;

        let order = graph
            .install_order()?
            .into_iter()
            .filter_map(|name| resolved.remove(&name))
            .collect();

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
            self.index
                .best_match(&dep.name, &dep.version_req)
                .ok_or_else(|| {
                    PkgError::DependencyConflict(format!(
                        "no version of '{}' satisfies '{}' required by '{}'",
                        dep.name, dep.version_req, name
                    ))
                })?;
            self.visit(&dep.name, Some(name), graph, resolved, visiting)?;
        }

        visiting.remove(name);
        resolved.insert(name.to_string(), candidate);
        Ok(())
    }

    /// Installed packages that directly depend on `name` — i.e. what would
    /// break if `name` were removed right now.
    pub fn dependents_of(&self, name: &str) -> Vec<String> {
        self.installed
            .all()
            .iter()
            .filter(|(_, pkg)| pkg.dependencies.iter().any(|d| d.name == name))
            .map(|(n, _)| n.clone())
            .collect()
    }
}
