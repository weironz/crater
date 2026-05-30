//! Dependency ordering for components (M3).
//!
//! Components declare `requires: [..]`. We topologically sort the set so each
//! component is deployed after everything it depends on. Detects cycles and
//! missing dependencies (within the selected set).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// A node: a component name plus the names it depends on.
#[derive(Debug, Clone)]
pub struct DepNode {
    pub name: String,
    pub requires: Vec<String>,
}

/// Topologically sort `nodes` so dependencies come first.
///
/// - A `requires` on a component not in `nodes` is an error (fail loud).
/// - Ties are broken alphabetically for deterministic output.
/// - Returns an error on cycles.
pub fn topo_sort(nodes: &[DepNode]) -> crate::Result<Vec<String>> {
    let names: BTreeSet<&str> = nodes.iter().map(|n| n.name.as_str()).collect();

    for n in nodes {
        for dep in &n.requires {
            if !names.contains(dep.as_str()) {
                anyhow::bail!(
                    "component '{}' requires '{}', which is not in this deployment",
                    n.name,
                    dep
                );
            }
        }
    }

    let mut indegree: BTreeMap<String, usize> = BTreeMap::new();
    let mut dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for n in nodes {
        indegree.entry(n.name.clone()).or_insert(0);
        for dep in &n.requires {
            *indegree.entry(n.name.clone()).or_insert(0) += 1;
            dependents.entry(dep.clone()).or_default().push(n.name.clone());
        }
    }

    let mut ready_vec: Vec<String> = indegree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(k, _)| k.clone())
        .collect();
    ready_vec.sort();
    let mut ready: VecDeque<String> = ready_vec.into();

    let mut order = Vec::with_capacity(nodes.len());
    while let Some(name) = ready.pop_front() {
        order.push(name.clone());
        if let Some(deps) = dependents.get(&name) {
            let mut newly: Vec<String> = Vec::new();
            for d in deps {
                if let Some(v) = indegree.get_mut(d) {
                    *v -= 1;
                    if *v == 0 {
                        newly.push(d.clone());
                    }
                }
            }
            newly.sort();
            for d in newly {
                ready.push_back(d);
            }
        }
    }

    if order.len() != nodes.len() {
        let remaining: Vec<String> = indegree
            .iter()
            .filter(|(_, &d)| d > 0)
            .map(|(k, _)| k.clone())
            .collect();
        anyhow::bail!("dependency cycle detected among: {}", remaining.join(", "));
    }

    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(name: &str, reqs: &[&str]) -> DepNode {
        DepNode {
            name: name.into(),
            requires: reqs.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn linear_chain() {
        let order = topo_sort(&[
            n("app", &["mysql"]),
            n("mysql", &["docker"]),
            n("docker", &[]),
        ])
        .unwrap();
        assert_eq!(order, vec!["docker", "mysql", "app"]);
    }

    #[test]
    fn independent_sorted_alphabetically() {
        let order = topo_sort(&[n("redis", &[]), n("docker", &[]), n("nginx", &[])]).unwrap();
        assert_eq!(order, vec!["docker", "nginx", "redis"]);
    }

    #[test]
    fn detects_cycle() {
        assert!(topo_sort(&[n("a", &["b"]), n("b", &["a"])]).is_err());
    }

    #[test]
    fn detects_missing_dep() {
        assert!(topo_sort(&[n("app", &["ghost"])]).is_err());
    }
}
