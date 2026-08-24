use crate::error::{PkgError, Result};
use std::collections::{HashMap, HashSet, VecDeque};

/// A directed graph of "depends on" edges between package names, used only
/// to compute a safe install order. It does not itself know about versions
/// or metadata — that's `Resolver`'s job; this type just orders whatever
/// node names it's given.
#[derive(Debug, Default)]
pub struct DependencyGraph {
    /// node -> set of nodes it directly depends on
    edges: HashMap<String, HashSet<String>>,
}

impl DependencyGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_node(&mut self, name: &str) {
        self.edges.entry(name.to_string()).or_default();
    }

    pub fn add_edge(&mut self, from: &str, depends_on: &str) {
        self.add_node(from);
        self.add_node(depends_on);
        self.edges
            .get_mut(from)
            .expect("node just inserted")
            .insert(depends_on.to_string());
    }

    /// Topologically sorts the graph via Kahn's algorithm so that every
    /// dependency appears before everything that depends on it. Returns
    /// `CircularDependency` if the graph isn't a DAG.
    pub fn install_order(&self) -> Result<Vec<String>> {
        // in_degree[n] = number of not-yet-resolved dependencies of n.
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        // dependents[d] = nodes that directly depend on d.
        let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();

        for node in self.edges.keys() {
            in_degree.entry(node.as_str()).or_insert(0);
        }
        for (node, deps) in &self.edges {
            for dep in deps {
                *in_degree.entry(node.as_str()).or_insert(0) += 1;
                dependents.entry(dep.as_str()).or_default().push(node.as_str());
            }
        }

        let mut queue: VecDeque<&str> = in_degree
            .iter()
            .filter(|&(_, &deg)| deg == 0)
            .map(|(&n, _)| n)
            .collect();
        let mut order = Vec::with_capacity(in_degree.len());

        while let Some(node) = queue.pop_front() {
            order.push(node.to_string());
            if let Some(deps) = dependents.get(node) {
                for &dependent in deps {
                    let deg = in_degree.get_mut(dependent).expect("known node");
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(dependent);
                    }
                }
            }
        }

        if order.len() != in_degree.len() {
            let stuck = in_degree
                .into_iter()
                .find(|&(_, deg)| deg > 0)
                .map(|(n, _)| n.to_string())
                .unwrap_or_default();
            return Err(PkgError::CircularDependency(stuck));
        }

        Ok(order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_chain_orders_dependencies_first() {
        let mut graph = DependencyGraph::new();
        graph.add_edge("app", "libc");
        graph.add_edge("libc", "libcore");

        let order = graph.install_order().unwrap();
        let pos = |n: &str| order.iter().position(|x| x == n).unwrap();

        assert!(pos("libcore") < pos("libc"));
        assert!(pos("libc") < pos("app"));
    }

    #[test]
    fn diamond_dependency_orders_shared_dep_once_before_both_parents() {
        let mut graph = DependencyGraph::new();
        graph.add_edge("app", "net");
        graph.add_edge("app", "fs");
        graph.add_edge("net", "libc");
        graph.add_edge("fs", "libc");

        let order = graph.install_order().unwrap();
        assert_eq!(order.iter().filter(|n| n.as_str() == "libc").count(), 1);

        let pos = |n: &str| order.iter().position(|x| x == n).unwrap();
        assert!(pos("libc") < pos("net"));
        assert!(pos("libc") < pos("fs"));
    }

    #[test]
    fn cycle_is_rejected() {
        let mut graph = DependencyGraph::new();
        graph.add_edge("a", "b");
        graph.add_edge("b", "c");
        graph.add_edge("c", "a");

        assert!(matches!(
            graph.install_order(),
            Err(PkgError::CircularDependency(_))
        ));
    }
}
