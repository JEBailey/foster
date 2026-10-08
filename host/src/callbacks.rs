//! Backend-independent callback tokens. C sees integers, never Foster addresses.
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::ThreadId;

type Handler = dyn Fn(&[u8]) -> Result<Vec<u8>, String>;
struct Entry {
    handler: Box<Handler>,
    active: Cell<bool>,
    signal: Arc<Signal>,
}
struct Signal {
    owner: ThreadId,
    signature: u64,
    queued: bool,
    state: Mutex<Pending>,
}
#[derive(Default)]
struct Pending {
    closed: bool,
    bytes: usize,
    events: VecDeque<Vec<u8>>,
    error: Option<String>,
}
thread_local! {
    static HANDLERS: RefCell<HashMap<i64, Rc<Entry>>> = RefCell::new(HashMap::new());
}
static NEXT: AtomicI64 = AtomicI64::new(1);
static SIGNALS: OnceLock<Mutex<HashMap<i64, Arc<Signal>>>> = OnceLock::new();
fn signals() -> &'static Mutex<HashMap<i64, Arc<Signal>>> {
    SIGNALS.get_or_init(Default::default)
}

pub fn register(signature: &str, queued: bool, handler: Box<Handler>) -> Result<i64, String> {
    let signature = u64::from_str_radix(signature, 16).map_err(|_| "invalid callback signature")?;
    let token = NEXT
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map_err(|_| "callback token space exhausted")?;
    let signal = Arc::new(Signal {
        owner: std::thread::current().id(),
        signature,
        queued,
        state: Mutex::new(Pending::default()),
    });
    HANDLERS.with(|handlers| {
        handlers.borrow_mut().insert(
            token,
            Rc::new(Entry {
                handler,
                active: Cell::new(false),
                signal: signal.clone(),
            }),
        )
    });
    signals().lock().unwrap().insert(token, signal);
    Ok(token)
}

fn entry(token: i64) -> Result<Rc<Entry>, String> {
    HANDLERS
        .with(|handlers| handlers.borrow().get(&token).cloned())
        .ok_or_else(|| "callback is closed or belongs to another thread".into())
}
pub fn release(token: i64) -> Result<(), String> {
    if token == 0 {
        return Ok(());
    }
    let entry = entry(token)?;
    if entry.active.get() {
        return Err("cannot close a callback during its invocation".into());
    }
    let mut pending = entry.signal.state.lock().unwrap();
    pending.closed = true;
    pending.events.clear();
    pending.bytes = 0;
    drop(pending);
    signals().lock().unwrap().remove(&token);
    HANDLERS.with(|handlers| handlers.borrow_mut().remove(&token));
    Ok(())
}

fn call(entry: &Entry, payload: &[u8]) -> Result<Vec<u8>, String> {
    if entry.active.replace(true) {
        return Err("recursive invocation of the same callback is forbidden".into());
    }
    struct Active<'a>(&'a Cell<bool>);
    impl Drop for Active<'_> {
        fn drop(&mut self) {
            self.0.set(false);
        }
    }
    let _active = Active(&entry.active);
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (entry.handler)(payload)))
            .map_err(|_| "callback panicked at the native boundary".to_owned())??;
    if result.len() > super::LIMIT + 1 {
        return Err("invalid callback response length".into());
    }
    let bytes = result;
    match bytes.split_first() {
        Some((0, payload)) => Ok(payload.to_vec()),
        Some((1, error)) => Err(String::from_utf8_lossy(error).into_owned()),
        _ => Err("invalid callback response envelope".into()),
    }
}

/// Drains a bounded snapshot, so a producer cannot keep the owner inside poll forever.
pub fn poll(token: i64) -> Result<(), String> {
    let entry = entry(token)?;
    let events = {
        let mut pending = entry.signal.state.lock().unwrap();
        if let Some(error) = pending.error.take() {
            return Err(error);
        }
        pending.bytes = 0;
        std::mem::take(&mut pending.events)
    };
    let mut first_error = None;
    for event in events {
        if let Err(error) = call(&entry, &event) {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}
pub fn take_error(token: i64) -> Result<(), String> {
    let entry = entry(token)?;
    let error = entry.signal.state.lock().unwrap().error.take();
    error.map_or(Ok(()), Err)
}

/// Installed in a generated DLL. UINT64_MAX is a signature/liveness probe.
/// All real arguments and results are copied packets bounded by LIMIT.
///
/// # Safety
/// For non-probe calls, non-null `input` must be readable for `length` bytes,
/// `output` must be writable for `capacity` bytes when capacity is nonzero, and
/// `written` must be aligned and writable for one u64. These regions must remain
/// valid throughout the call and must not alias in a way that violates Rust's
/// slice or pointer access rules. Probe calls do not access the packet pointers.
pub unsafe extern "C" fn invoke(
    token: i64,
    signature: u64,
    input: *const u8,
    length: u64,
    output: *mut u8,
    capacity: u64,
    written: *mut u64,
) -> i32 {
    let Some(signal) = signals().lock().unwrap().get(&token).cloned() else {
        return 1;
    };
    let fail = |message: String| {
        let mut pending = signal.state.lock().unwrap();
        if pending.error.is_none() {
            pending.error = Some(message);
        }
        1
    };
    if signature != signal.signature {
        return fail("callback signature mismatch".into());
    }
    if signal.state.lock().unwrap().closed {
        return 1;
    }
    if length == u64::MAX {
        return 0;
    }
    if length == u64::MAX - 1 {
        return fail("callback result is outside its declared C type".into());
    }
    if length > super::LIMIT as u64
        || capacity > super::LIMIT as u64
        || (length != 0 && input.is_null())
        || written.is_null()
    {
        return fail("invalid callback packet".into());
    }
    unsafe {
        *written = 0;
    }
    let payload = if length == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(input, length as usize) }
    };
    if signal.queued {
        let mut pending = signal.state.lock().unwrap();
        if pending.closed {
            return 1;
        }
        if pending.events.len() >= 4096 || payload.len() > super::LIMIT - pending.bytes {
            if pending.error.is_none() {
                pending.error = Some("callback queue capacity exceeded".into());
            }
            return 1;
        }
        pending.bytes += payload.len();
        pending.events.push_back(payload.to_vec());
        return 0;
    }
    if signal.owner != std::thread::current().id() {
        return fail("direct callback invoked on a foreign thread".into());
    }
    let result = entry(token).and_then(|entry| call(&entry, payload));
    match result {
        Ok(bytes)
            if bytes.len() <= capacity as usize && (bytes.is_empty() || !output.is_null()) =>
        {
            if !bytes.is_empty() {
                unsafe {
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), output, bytes.len());
                }
            }
            unsafe {
                *written = bytes.len() as u64;
            }
            0
        }
        Ok(_) => fail("callback returned an invalid result size".into()),
        Err(error) => fail(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dispatch(token: i64, signature: u64, bytes: &[u8]) -> (i32, Vec<u8>) {
        let mut output = [0; 256];
        let mut written = 0;
        let status = unsafe {
            invoke(
                token,
                signature,
                bytes.as_ptr(),
                bytes.len() as u64,
                output.as_mut_ptr(),
                output.len() as u64,
                &mut written,
            )
        };
        (status, output[..written as usize].to_vec())
    }
    #[test]
    fn binary_callback_packets_preserve_every_octet() {
        let token = register(
            "1",
            false,
            Box::new(|packet| Ok(super::super::response(Ok(packet.to_vec())))),
        )
        .unwrap();
        let bytes: Vec<u8> = (0..=255).collect();
        assert_eq!(dispatch(token, 1, &bytes), (0, bytes));
        assert_eq!(dispatch(token, 1, &[]), (0, vec![]));
        release(token).unwrap();
    }

    #[test]
    fn direct_failures_and_stale_tokens_are_contained() {
        let calls = Rc::new(Cell::new(0));
        let captured = calls.clone();
        let token = register(
            "42",
            false,
            Box::new(move |packet| {
                captured.set(captured.get() + 1);
                Ok(super::super::response(Ok(packet.to_vec())))
            }),
        )
        .unwrap();
        assert_eq!(dispatch(token, 0x42, &[7]), (0, vec![7]));
        assert_eq!(calls.get(), 1);
        assert_eq!(
            std::thread::spawn(move || dispatch(token, 0x42, &[]).0)
                .join()
                .unwrap(),
            1
        );
        assert!(take_error(token).unwrap_err().contains("foreign thread"));
        assert_eq!(dispatch(token, 0x43, &[]).0, 1);
        assert!(take_error(token).unwrap_err().contains("signature"));
        release(token).unwrap();
        assert_eq!(dispatch(token, 0x42, &[]).0, 1);
        assert_eq!(calls.get(), 1);
    }
    #[test]
    fn queued_events_are_bounded_copied_and_run_on_owner() {
        let calls = Rc::new(Cell::new(0));
        let captured = calls.clone();
        let token = register(
            "1",
            true,
            Box::new(move |packet| {
                assert_eq!(packet, &[7]);
                captured.set(captured.get() + 1);
                Ok(vec![0])
            }),
        )
        .unwrap();
        std::thread::spawn(move || {
            for _ in 0..4096 {
                assert_eq!(dispatch(token, 1, &[7]).0, 0);
            }
            assert_eq!(dispatch(token, 1, &[8]).0, 1);
        })
        .join()
        .unwrap();
        assert_eq!(calls.get(), 0);
        assert!(poll(token).unwrap_err().contains("capacity"));
        poll(token).unwrap();
        assert_eq!(calls.get(), 4096);
        release(token).unwrap();
    }
    #[test]
    fn panic_and_reentry_do_not_unwind_across_c() {
        let token = register("1", false, Box::new(|_| panic!("contained"))).unwrap();
        assert_eq!(dispatch(token, 1, &[]).0, 1);
        assert!(take_error(token).unwrap_err().contains("panicked"));
        release(token).unwrap();
        let identity = Rc::new(Cell::new(0));
        let captured = identity.clone();
        let token = register(
            "1",
            false,
            Box::new(move |_| {
                assert!(release(captured.get()).is_err());
                assert_eq!(dispatch(captured.get(), 1, &[]).0, 1);
                Ok(vec![0])
            }),
        )
        .unwrap();
        identity.set(token);
        assert_eq!(dispatch(token, 1, &[]).0, 0);
        assert!(take_error(token).unwrap_err().contains("recursive"));
        release(token).unwrap();
    }
}
