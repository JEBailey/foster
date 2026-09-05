//! Backend-independent terminal state and exactly-once request completion.
//!
//! Worker execution may retain `Control`, but only language owners retain `Owner`.
//! Futures retain neither. Cancellation callbacks publish outcomes outside the state lock.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteError {
    Shutdown,
    Failed(String),
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shutdown => {
                output.write_str("remote owner shut down before the request completed")
            }
            Self::Failed(message) => write!(output, "remote execution failed: {message}"),
        }
    }
}

type Cancel = Box<dyn FnOnce(RemoteError) + Send>;

#[derive(Default)]
struct State {
    terminal: Option<RemoteError>,
    next: u64,
    pending: BTreeMap<u64, Cancel>,
}

#[derive(Default)]
pub struct Control(Mutex<State>);

impl Control {
    pub fn register(&self, cancelled: impl FnOnce(RemoteError) + Send + 'static) -> Option<u64> {
        let mut state = self.0.lock().expect("remote lifecycle lock poisoned");
        if let Some(error) = state.terminal.clone() {
            drop(state);
            cancelled(error);
            return None;
        }
        let id = state.next;
        state.next = id
            .checked_add(1)
            .expect("remote request identity exhausted");
        state.pending.insert(id, Box::new(cancelled));
        Some(id)
    }

    /// Reserves the sole successful completion before publishing its value.
    pub fn complete(&self, id: u64) -> bool {
        let cancelled = {
            self.0
                .lock()
                .expect("remote lifecycle lock poisoned")
                .pending
                .remove(&id)
        };
        // Dropping the callback may release user-owned captures. Do not run their
        // destructors under the lifecycle lock either.
        cancelled.is_some()
    }

    pub fn error(&self) -> Option<RemoteError> {
        self.0
            .lock()
            .expect("remote lifecycle lock poisoned")
            .terminal
            .clone()
    }

    pub fn terminate(&self, error: RemoteError) {
        let pending = {
            let mut state = self.0.lock().expect("remote lifecycle lock poisoned");
            if state.terminal.is_some() {
                return;
            }
            state.terminal = Some(error.clone());
            std::mem::take(&mut state.pending)
        };
        for cancel in pending.into_values() {
            cancel(error.clone());
        }
    }
}

pub struct Owner(pub Arc<Control>);

impl Drop for Owner {
    fn drop(&mut self) {
        self.0.terminate(RemoteError::Shutdown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn dropping_owner_cancels_requests_even_when_worker_retains_control() {
        let control = Arc::new(Control::default());
        let owner = Owner(control.clone());
        let (send, receive) = mpsc::channel();
        let request = control
            .register(move |error| send.send(error).unwrap())
            .unwrap();
        drop(owner);
        assert_eq!(receive.recv().unwrap(), RemoteError::Shutdown);
        assert!(!control.complete(request));
    }

    #[test]
    fn failure_is_sticky_and_later_requests_resolve_without_execution() {
        let control = Control::default();
        let (send, receive) = mpsc::channel();
        control.register(move |error| send.send(error).unwrap());
        let failure = RemoteError::Failed("assertion failed".into());
        control.terminate(failure.clone());
        control.terminate(RemoteError::Shutdown);
        assert_eq!(receive.recv().unwrap(), failure);
        let (send, receive) = mpsc::channel();
        assert!(
            control
                .register(move |error| send.send(error).unwrap())
                .is_none()
        );
        assert_eq!(receive.recv().unwrap(), failure);
    }

    #[test]
    fn completed_requests_are_not_cancelled_or_completed_twice() {
        let control = Control::default();
        let request = control
            .register(|_| panic!("completed request cancelled"))
            .unwrap();
        assert!(control.complete(request));
        assert!(!control.complete(request));
        control.terminate(RemoteError::Shutdown);
    }

    #[test]
    fn completion_racing_shutdown_has_exactly_one_winner() {
        for _ in 0..64 {
            let control = Arc::new(Control::default());
            let (send, receive) = mpsc::channel();
            let request = control.register(move |_| send.send(()).unwrap()).unwrap();
            let cancelled = control.clone();
            let shutdown = std::thread::spawn(move || cancelled.terminate(RemoteError::Shutdown));
            let completed = control.complete(request);
            shutdown.join().unwrap();
            assert_eq!(receive.try_recv().is_ok(), !completed);
        }
    }
}
