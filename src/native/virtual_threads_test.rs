// Appended to the assembled runtime and compiled as a standalone test executable.
static SEEN_THREADS: Mutex<Vec<std::thread::ThreadId>> = Mutex::new(Vec::new());
static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static GO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RELEASES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

unsafe extern "C" fn release_actor(_: usize) -> u8 {
    RELEASES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    0
}

fn call(actor: usize, argument: u64, blocking: u8) -> usize {
    foster_rt_v4_remote_call(
        actor,
        request as *const () as usize,
        &argument as *const u64 as usize,
        1,
        blocking,
        0,
    )
}

fn value(future: usize) -> u64 {
    let result = foster_rt_v4_future_await(future);
    assert_eq!(foster_rt_v4_future_error(future), 0);
    foster_rt_v4_future_release(future);
    result
}

unsafe extern "C" fn request(state: u64, arguments: usize, execute: u8) -> u64 {
    if execute == 0 {
        return 0;
    }
    assert!(may::coroutine::is_coroutine());
    SEEN_THREADS
        .lock()
        .unwrap()
        .push(std::thread::current().id());
    let argument = unsafe { *(arguments as *const u64) };
    match state {
        0 => {
            // All actors reach a wait with only one scheduler worker.
            while !GO.load(std::sync::atomic::Ordering::SeqCst) {
                may::coroutine::sleep(std::time::Duration::from_millis(1));
            }
            assert_eq!(foster_rt_v4_failure_pending(), 0);
            argument
        }
        1 => {
            // Await and borrowed dispatch preserve a nested native stack.
            let child = foster_rt_v4_remote_spawn(0, release_actor as *const () as usize, 0);
            let result = value(call(child, argument, 0)) + value(call(child, 1, 1));
            foster_rt_v4_remote_release(child);
            result
        }
        2 => {
            // A pending error and a cleanup frame must stay local across yield.
            foster_execution_failure("isolated failure".into());
            foster_rt_v4_begin_cleanup();
            may::coroutine::sleep(std::time::Duration::from_millis(30));
            assert_eq!(foster_rt_v4_failure_pending(), 0);
            foster_rt_v4_end_cleanup();
            assert_eq!(foster_rt_v4_failure_pending(), 1);
            0
        }
        3 => {
            STARTED.store(true, std::sync::atomic::Ordering::SeqCst);
            while foster_rt_v4_cancellation_point() == 0 {}
            0
        }
        4 => {
            // A blocking host call must let a second actor produce its input.
            let listener = argument as i64;
            STARTED.store(true, std::sync::atomic::Ordering::SeqCst);
            let response = foster_rt_v4_host_call_int(48, listener);
            assert_eq!(foster_rt_v4_host_ok(response), 1);
            let connection = foster_rt_v4_host_integer(response, 0);
            foster_rt_v4_host_release(response);
            foster_network_close_connection(connection).unwrap();
            42
        }
        5 => {
            let connection =
                foster_host_blocking(move || foster_network_connect("127.0.0.1", argument as i64))
                    .unwrap();
            foster_network_close_connection(connection).unwrap();
            42
        }
        _ => unreachable!(),
    }
}

fn main() {
    may::config().set_workers(1);
    // Bound deadlock failures, including regressions to OS-blocking waits.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(20));
        eprintln!("virtual-thread test timed out");
        std::process::abort();
    });
    foster_runtime_initialize(&[]);
    unsafe extern "C" {
        fn foster_native_entry() -> i64;
    }
    assert_eq!(unsafe { foster_native_entry() }, 42);
    foster_runtime_check_execution();
    let spawn = |state| foster_rt_v4_remote_spawn(state, release_actor as *const () as usize, 0);
    let actors: Vec<_> = (0..128).map(|_| spawn(0)).collect();
    let futures: Vec<_> = actors
        .iter()
        .enumerate()
        .map(|(i, actor)| call(*actor, i as u64, 0))
        .collect();
    while SEEN_THREADS.lock().unwrap().len() != actors.len() {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    GO.store(true, std::sync::atomic::Ordering::SeqCst);
    for (i, future) in futures.into_iter().enumerate() {
        assert_eq!(value(future), i as u64);
    }
    let nested = spawn(1);
    let failing = spawn(2);
    let failed = call(failing, 0, 0);
    assert_eq!(value(call(nested, 41, 0)), 42);
    foster_rt_v4_future_await(failed);
    let error = unsafe { &*(failed as *const FosterFuture) }
        .error
        .lock()
        .unwrap()
        .take();
    assert_eq!(
        error,
        Some(remote_lifecycle::RemoteError::Failed(
            "isolated failure".into()
        ))
    );
    foster_rt_v4_future_release(failed);
    assert_eq!(foster_rt_v4_failure_pending(), 0);
    let busy = spawn(3);
    let pending = call(busy, 0, 0);
    while !STARTED.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::yield_now();
    }
    assert_eq!(value(call(nested, 41, 0)), 42);
    foster_rt_v4_remote_release(busy);
    foster_rt_v4_future_await(pending);
    assert_eq!(foster_rt_v4_future_error(pending), 1);
    foster_rt_v4_future_release(pending);

    // Allocate an ephemeral listening socket through the shared host context.
    let (listener, port) = (0..32)
        .find_map(|_| {
            let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = reservation.local_addr().unwrap().port();
            drop(reservation);
            foster_network_listen("127.0.0.1", i64::from(port))
                .ok()
                .map(|listener| (listener, port))
        })
        .expect("could not reserve a test port");
    let server = spawn(4);
    let client = spawn(5);
    STARTED.store(false, std::sync::atomic::Ordering::SeqCst);
    let accepted = call(server, listener as u64, 0);
    while !STARTED.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::yield_now();
    }
    assert_eq!(value(call(client, port as u64, 0)), 42);
    assert_eq!(value(accepted), 42);
    foster_network_close_listener(listener).unwrap();
    for actor in actors.into_iter().chain([nested, failing, server, client]) {
        foster_rt_v4_remote_release(actor);
    }
    while RELEASES.load(std::sync::atomic::Ordering::SeqCst) != 135 {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let threads = SEEN_THREADS.lock().unwrap();
    assert!(threads.iter().all(|id| *id == threads[0]));
    assert_ne!(threads[0], std::thread::current().id());
    println!("virtual threads passed");
}
