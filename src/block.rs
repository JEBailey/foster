use std::ops::{Index, Range};

fn value_ref<T>(item: &Spanned<T>) -> &T {
    &item.value
}

#[derive(Debug, Clone, PartialEq)]
pub struct Spanned<T> {
    pub value: T,
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block<T> {
    items: Vec<Spanned<T>>,
}

impl<T> Block<T> {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn single(value: T, span: Range<usize>) -> Self {
        Self {
            items: vec![Spanned { value, span }],
        }
    }

    pub fn push(&mut self, value: T, span: Range<usize>) {
        self.items.push(Spanned { value, span });
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> {
        self.items.iter().map(|item| &item.value)
    }

    pub fn iter_spanned(&self) -> impl DoubleEndedIterator<Item = (&T, &Range<usize>)> {
        self.items.iter().map(|item| (&item.value, &item.span))
    }

    pub fn last(&self) -> Option<&T> {
        self.items.last().map(|item| &item.value)
    }

    pub fn span(&self, index: usize) -> Option<&Range<usize>> {
        self.items.get(index).map(|item| &item.span)
    }

    pub fn last_span(&self) -> Option<&Range<usize>> {
        self.items.last().map(|item| &item.span)
    }

    pub fn first_span(&self) -> Option<&Range<usize>> {
        self.items.first().map(|item| &item.span)
    }
}

impl<T> Default for Block<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Index<usize> for Block<T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        &self.items[index].value
    }
}

impl<'a, T> IntoIterator for &'a Block<T> {
    type Item = &'a T;
    type IntoIter = std::iter::Map<std::slice::Iter<'a, Spanned<T>>, fn(&'a Spanned<T>) -> &'a T>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.iter().map(value_ref::<T>)
    }
}
