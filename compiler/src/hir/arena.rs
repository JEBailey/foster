//! Compiler storage indexed by executable identities rather than declaration types.
use la_arena::{Arena, Idx};
use std::{
    marker::PhantomData,
    ops::{Index, IndexMut},
};
#[derive(Debug)]
pub struct IdentityArena<T, I> {
    values: Arena<T>,
    identity: PhantomData<fn() -> I>,
}
impl<T, I> Default for IdentityArena<T, I> {
    fn default() -> Self {
        Self {
            values: Arena::new(),
            identity: PhantomData,
        }
    }
}
impl<T, I> IdentityArena<T, I> {
    pub fn alloc(&mut self, value: T) -> Idx<I> {
        Idx::from_raw(self.values.alloc(value).into_raw())
    }
    pub fn len(&self) -> usize {
        self.values.len()
    }
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (Idx<I>, &T)> + ExactSizeIterator {
        self.values
            .iter()
            .map(|(id, value)| (Idx::from_raw(id.into_raw()), value))
    }
    pub fn iter_mut(
        &mut self,
    ) -> impl DoubleEndedIterator<Item = (Idx<I>, &mut T)> + ExactSizeIterator {
        self.values
            .iter_mut()
            .map(|(id, value)| (Idx::from_raw(id.into_raw()), value))
    }
}
impl<T, I> Index<Idx<I>> for IdentityArena<T, I> {
    type Output = T;
    fn index(&self, id: Idx<I>) -> &T {
        &self.values[Idx::from_raw(id.into_raw())]
    }
}
impl<T, I> IndexMut<Idx<I>> for IdentityArena<T, I> {
    fn index_mut(&mut self, id: Idx<I>) -> &mut T {
        &mut self.values[Idx::from_raw(id.into_raw())]
    }
}
