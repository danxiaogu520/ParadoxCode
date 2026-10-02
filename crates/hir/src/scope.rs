//! Structural child indexes shared by IR lowering.
use super::{HirProperty, range_within};
pub(crate) fn property_children(properties: &[HirProperty]) -> Vec<Vec<usize>> {
    let mut children = vec![Vec::new(); properties.len()];
    let mut ancestors = Vec::<usize>::new();
    for (index, property) in properties.iter().enumerate() {
        while ancestors.last().is_some_and(|ancestor_index| {
            let ancestor = &properties[*ancestor_index];
            property.path.len() <= ancestor.path.len()
                || !property.path.starts_with(&ancestor.path)
                || !range_within(property.range, ancestor.range)
        }) {
            ancestors.pop();
        }
        if let Some(&parent_index) = ancestors.last()
            && property.path.len() == properties[parent_index].path.len().saturating_add(1)
        {
            children[parent_index].push(index);
        }
        ancestors.push(index);
    }
    children
}
