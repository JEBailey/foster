use std::rc::Rc;

use super::Value;
use super::value::Slot;

#[derive(Debug, Clone)]
pub enum Capture {
    Value(Value),
    Slot(Rc<Slot>),
}

impl PartialEq for Capture {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Value(left), Self::Value(right)) => left == right,
            (Self::Slot(left), Self::Slot(right)) => Rc::ptr_eq(left, right),
            _ => false,
        }
    }
}
