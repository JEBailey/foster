use std::rc::Rc;

use super::Ty;

const PAGE_SIZE: usize = 64;
type Page = [Option<Ty>; PAGE_SIZE];

/// Inference variables have dense numeric IDs. Backtracking snapshots share their pages;
/// binding a variable detaches only its page, rather than cloning every inferred type.
#[derive(Clone, Default)]
pub(super) struct Substitutions {
    pages: Vec<Rc<Page>>,
}

impl Substitutions {
    pub(super) fn get(&self, variable: &u32) -> Option<&Ty> {
        let index = *variable as usize;
        self.pages.get(index / PAGE_SIZE)?[index % PAGE_SIZE].as_ref()
    }

    pub(super) fn insert(&mut self, variable: u32, ty: Ty) {
        let index = variable as usize;
        let page = index / PAGE_SIZE;
        while self.pages.len() <= page {
            self.pages.push(Rc::new(std::array::from_fn(|_| None)));
        }
        Rc::make_mut(&mut self.pages[page])[index % PAGE_SIZE] = Some(ty);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn backtracking_snapshots_match_independent_maps() {
        let mut actual = Substitutions::default();
        let mut expected = HashMap::new();
        let mut snapshots = Vec::new();
        for step in 0..512u32 {
            let variable = (step * 73) % 257;
            let ty = Ty::RawList(Box::new(Ty::Variable(step)));
            actual.insert(variable, ty.clone());
            expected.insert(variable, ty);
            if step % 7 == 0 {
                snapshots.push((actual.clone(), expected.clone()));
            }
            if step % 11 == 0 && snapshots.len() > 1 {
                // Revisit an older branch, then overwrite variables and grow it independently.
                let branch = step as usize % snapshots.len();
                (actual, expected) = snapshots[branch].clone();
            }
            for variable in 0..320 {
                assert_eq!(actual.get(&variable), expected.get(&variable));
            }
        }
        for (actual, expected) in snapshots {
            for variable in 0..320 {
                assert_eq!(actual.get(&variable), expected.get(&variable));
            }
        }
    }
}
