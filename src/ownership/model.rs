//! Small executable reference model used to validate the optimized CFG
//! implementation. It intentionally models one linear loan at a time; CFG
//! paths are compared by running each path independently.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Event {
    Issue,
    Use,
    Invalidate,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Decision {
    pub accepted: bool,
    pub failing_event: Option<usize>,
}

pub(super) fn evaluate(events: &[Event]) -> Decision {
    let mut live = false;
    let mut valid = false;
    for (index, event) in events.iter().enumerate() {
        match event {
            Event::Issue => {
                live = true;
                valid = true;
            }
            Event::Use if live && !valid => {
                return Decision {
                    accepted: false,
                    failing_event: Some(index),
                };
            }
            Event::Use => {}
            Event::Invalidate if live => valid = false,
            Event::Invalidate => {}
            Event::End => live = false,
        }
    }
    Decision {
        accepted: true,
        failing_event: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_model_observes_last_use_and_reissuance() {
        assert!(evaluate(&[Event::Issue, Event::Use, Event::Invalidate]).accepted);
        assert!(!evaluate(&[Event::Issue, Event::Invalidate, Event::Use]).accepted);
        assert!(evaluate(&[Event::Issue, Event::Invalidate, Event::Issue, Event::Use,]).accepted);
        assert!(evaluate(&[Event::Issue, Event::End, Event::Invalidate, Event::Use]).accepted);
    }
}
