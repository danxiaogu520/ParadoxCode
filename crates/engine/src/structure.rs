//! Game-independent dependency graphs and deterministic structured writes.
use std::collections::HashMap;
use text::TextRange;

/// Name-resolved dependency graph; node and outgoing-edge order follow input order.
/// Duplicate nodes use their last declaration, matching ordinary name resolution.
pub struct DependencyGraph<'a> {
    names: Vec<&'a str>,
    ids: HashMap<&'a str, usize>,
    edges: Vec<Vec<usize>>,
}
impl<'a> DependencyGraph<'a> {
    /// Assembles a graph from named nodes and their dependency names.
    #[must_use]
    pub fn new(nodes: &[(&'a str, Vec<&'a str>)]) -> Self {
        let mut ids = HashMap::new();
        let mut names = Vec::new();
        for (name, _) in nodes {
            if !ids.contains_key(name) {
                ids.insert(*name, names.len());
                names.push(*name);
            }
        }
        let mut edges = vec![Vec::new(); names.len()];
        for (name, dependencies) in nodes {
            edges[ids[name]] = dependencies
                .iter()
                .filter_map(|name| ids.get(name).copied())
                .collect();
        }
        Self { names, ids, edges }
    }
    /// Whether a local node exists; callers can supplement this with a workspace universe.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.ids.contains_key(name)
    }

    /// Finds DFS back-edge cycles in stable declaration order, including the closing node.
    /// Uses an explicit stack so large acyclic graphs do not overflow the process stack.
    #[must_use]
    pub fn cycles(&self) -> Vec<Vec<&'a str>> {
        let mut marks = vec![0u8; self.names.len()];
        let mut path = Vec::new();
        let mut cycles = Vec::new();
        for root in 0..self.names.len() {
            if marks[root] != 0 {
                continue;
            }
            marks[root] = 1;
            path.push(root);
            let mut stack = vec![(root, 0)];
            while let Some((node, edge)) = stack.last_mut() {
                let Some(&next) = self.edges[*node].get(*edge) else {
                    marks[*node] = 2;
                    stack.pop();
                    path.pop();
                    continue;
                };
                *edge += 1;
                match marks[next] {
                    0 => {
                        marks[next] = 1;
                        path.push(next);
                        stack.push((next, 0));
                    }
                    1 => {
                        let start = path
                            .iter()
                            .position(|&node| node == next)
                            .expect("visiting node is on path");
                        let mut cycle = path[start..]
                            .iter()
                            .map(|&id| self.names[id])
                            .collect::<Vec<_>>();
                        cycle.push(self.names[next]);
                        cycles.push(cycle);
                    }
                    _ => {}
                }
            }
        }
        cycles
    }
}

/// Appends rendered fields in declared role order, retaining source order for unknown roles.
pub fn write_ordered_fields(
    output: &mut String,
    fields: &mut [(String, String)],
    order: &[String],
) {
    fields.sort_by_key(|(role, _)| {
        order
            .iter()
            .position(|name| name == role)
            .unwrap_or(usize::MAX)
    });
    for (_, text) in fields {
        output.push_str(text);
    }
}

/// Replaces one structured block without rewriting any surrounding source bytes.
#[must_use]
pub fn replace_block(source: &str, span: TextRange, rendered: &str) -> String {
    let start = span.start() as usize;
    let end = span.end() as usize;
    let mut output = String::with_capacity(source.len() + rendered.len());
    output.push_str(&source[..start]);
    output.push_str(rendered);
    output.push_str(&source[end..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cycles_use_declared_order_and_long_chains_do_not_recurse() {
        let graph = DependencyGraph::new(&[
            ("z", vec!["b"]),
            ("b", vec!["z", "external"]),
            ("a", vec!["a"]),
        ]);
        assert_eq!(graph.cycles(), [vec!["z", "b", "z"], vec!["a", "a"]]);
        assert!(!graph.contains("external"));
        let names = (0..20_000).map(|i| i.to_string()).collect::<Vec<_>>();
        let nodes = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                (
                    name.as_str(),
                    names
                        .get(i + 1)
                        .map(|name| vec![name.as_str()])
                        .unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();
        assert!(DependencyGraph::new(&nodes).cycles().is_empty());
    }
    #[test]
    fn declared_field_order_preserves_unknown_fields_and_surrounding_bytes() {
        let mut fields = vec![
            ("tail".to_owned(), "tail ".to_owned()),
            ("b".to_owned(), "b ".to_owned()),
            ("unknown".to_owned(), "unknown ".to_owned()),
            ("a".to_owned(), "a ".to_owned()),
        ];
        let mut output = String::new();
        write_ordered_fields(&mut output, &mut fields, &["a".to_owned(), "b".to_owned()]);
        assert_eq!(output, "a b tail unknown ");
        assert_eq!(
            replace_block("before {} after", TextRange::new(7, 9).unwrap(), &output),
            "before a b tail unknown  after"
        );
    }
}
