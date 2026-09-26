use super::Value;
use super::value::PlaceHandle;

#[derive(Debug, Clone)]
pub enum Capture {
    Value(Value),
    Place(PlaceHandle),
}

impl PartialEq for Capture {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Value(left), Self::Value(right)) => left == right,
            (Self::Place(left), Self::Place(right)) => left == right,
            _ => false,
        }
    }
}
