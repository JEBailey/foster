//! Test-only implementations of the program-specific string hooks.
use super::*;

#[test]
fn embedded_execution_recovers_failures_and_scopes_cancellation() {
    assert_eq!(
        embedded_execution(
            || true,
            || {
                foster_poll_cancellation();
                42
            }
        ),
        Err("execution cancelled".into())
    );
    assert_eq!(foster_rt_v5_failure_pending(), 0);
    assert_eq!(embedded_execution(|| false, || 42), Ok(42));
    assert!(EMBEDDING_CANCELLATION.with(|slot| slot.get().is_none()));
    assert_eq!(
        embedded_execution(
            || false,
            || {
                foster_execution_failure("formatter failed".into());
            }
        ),
        Err("formatter failed".into())
    );
    assert_eq!(foster_rt_v5_failure_pending(), 0);
}

#[test]
fn cancellation_is_discovered_at_cleanup_safe_polls() {
    let control = Arc::new(remote_lifecycle::Control::default());
    control.terminate(remote_lifecycle::RemoteError::Shutdown);
    FOSTER_CANCELLATION.with(|slot| *slot.borrow_mut() = Some(control));

    // A successful call's result is not yet registered in the caller's cleanup
    // set when this status check runs.
    assert_eq!(foster_rt_v5_failure_pending(), 0);
    foster_rt_v5_begin_cleanup();
    assert_eq!(foster_rt_v5_cancellation_point(), 0);
    foster_rt_v5_end_cleanup();
    assert_eq!(foster_rt_v5_cancellation_point(), 1);
    assert_eq!(foster_rt_v5_failure_pending(), 1);

    FOSTER_CANCELLATION.with(|slot| slot.borrow_mut().take());
    FOSTER_EXECUTION.with(|slot| slot.borrow_mut().take());
}

#[test]
fn nested_execution_restores_failure_state_after_errors_and_panics() {
    let result = embedded_execution(
        || false,
        || {
            foster_execution_failure("outer".into());
            assert_eq!(embedded_execution(|| false, || 7), Ok(7));
            assert_eq!(foster_rt_v5_failure_pending(), 1);
            assert_eq!(
                embedded_execution(|| false, || foster_execution_failure("inner".into())),
                Err("inner".into())
            );
            let panic = std::panic::catch_unwind(|| {
                embedded_execution(
                    || false,
                    || {
                        foster_execution_failure("unwound".into());
                        panic!("embedded work panicked");
                    },
                )
            });
            assert!(panic.is_err());
            FOSTER_EXECUTION.with(|slot| assert_eq!(slot.borrow().as_deref(), Some("outer")));
        },
    );
    assert_eq!(result, Err("outer".into()));
    assert_eq!(foster_rt_v5_failure_pending(), 0);
}

#[test]
fn cleanup_preserves_the_first_failure_through_nested_scopes() {
    let result = embedded_execution(
        || false,
        || {
            foster_execution_failure("first".into());
            foster_execution_failure("ignored".into());
            foster_rt_v5_begin_cleanup();
            assert_eq!(foster_rt_v5_failure_pending(), 0);
            foster_execution_failure("cleanup".into());
            foster_rt_v5_begin_cleanup();
            assert_eq!(foster_rt_v5_failure_pending(), 0);
            foster_execution_failure("nested cleanup".into());
            foster_rt_v5_end_cleanup();
            FOSTER_EXECUTION.with(|slot| assert_eq!(slot.borrow().as_deref(), Some("cleanup")));
            foster_rt_v5_end_cleanup();
        },
    );
    assert_eq!(result, Err("first".into()));
    assert_eq!(foster_rt_v5_failure_pending(), 0);
}

#[test]
fn ordinary_threads_have_independent_failure_state() {
    let result = embedded_execution(
        || false,
        || {
            foster_execution_failure("caller".into());
            let other = std::thread::spawn(|| {
                assert_eq!(foster_rt_v5_failure_pending(), 0);
                let result = embedded_execution(
                    || false,
                    || foster_execution_failure("other thread".into()),
                );
                assert_eq!(foster_rt_v5_failure_pending(), 0);
                result
            });
            assert_eq!(other.join().unwrap(), Err("other thread".into()));
        },
    );
    assert_eq!(result, Err("caller".into()));
}

#[test]
fn coroutine_failures_survive_yields_without_leaking_to_other_contexts() {
    let result = embedded_execution(
        || false,
        || {
            foster_execution_failure("ordinary thread".into());
            let workers: Vec<_> = (0..32)
                .map(|index| {
                    may::go!(move || {
                        assert!(may::coroutine::is_coroutine());
                        let state = foster_rt_v5_failure_state();
                        assert_eq!(foster_rt_v5_failure_pending(), 0);
                        assert_eq!(foster_rt_v5_failure_pending_in(state), 0);
                        let expected = format!("actor {index}");
                        foster_execution_failure(expected.clone());
                        for _ in 0..32 {
                            may::coroutine::yield_now();
                            assert_eq!(foster_rt_v5_failure_pending_in(state), 1);
                            assert_eq!(foster_rt_v5_failure_state(), state);
                            FOSTER_EXECUTION.with(|slot| {
                                assert_eq!(slot.borrow().as_deref(), Some(expected.as_str()));
                            });
                        }
                        foster_rt_v5_begin_cleanup();
                        may::coroutine::yield_now();
                        assert_eq!(foster_rt_v5_failure_pending(), 0);
                        assert_eq!(foster_rt_v5_failure_pending_in(state), 0);
                        foster_execution_failure("cleanup".into());
                        foster_rt_v5_end_cleanup();
                        assert_eq!(foster_rt_v5_failure_pending_in(state), 1);
                        assert_eq!(
                            FOSTER_EXECUTION.with(|slot| slot.borrow_mut().take()),
                            Some(expected)
                        );
                    })
                })
                .collect();
            for worker in workers {
                worker.join().unwrap();
            }
            may::go!(|| assert_eq!(foster_rt_v5_failure_pending(), 0))
                .join()
                .unwrap();
        },
    );
    assert_eq!(result, Err("ordinary thread".into()));
    assert_eq!(foster_rt_v5_failure_pending(), 0);
}

#[test]
fn cached_failure_state_survives_map_growth_and_nested_execution() {
    std::thread::spawn(|| {
        // Start with a fresh ordinary-thread local map. Adding the other keys
        // grows May's map while the acquired boxed slot must stay in place.
        let state = foster_rt_v5_failure_state();
        FOSTER_CANCELLATION.with(|_| ());
        FOSTER_CLEANUP_FAILURES.with(|_| ());
        FOSTER_SCHEDULING_BUDGET.with(|_| ());
        assert_eq!(foster_rt_v5_failure_state(), state);
        assert_eq!(foster_rt_v5_failure_pending_in(state), 0);
        let result = embedded_execution(
            || false,
            || {
                foster_execution_failure("outer".into());
                assert_eq!(foster_rt_v5_failure_pending_in(state), 1);
                let inner = embedded_execution(
                    || false,
                    || {
                        assert_eq!(foster_rt_v5_failure_pending_in(state), 0);
                        foster_execution_failure("inner".into());
                        assert_eq!(foster_rt_v5_failure_pending_in(state), 1);
                    },
                );
                assert_eq!(inner, Err("inner".into()));
                assert_eq!(foster_rt_v5_failure_pending_in(state), 1);
                foster_rt_v5_begin_cleanup();
                assert_eq!(foster_rt_v5_failure_pending_in(state), 0);
                foster_execution_failure("cleanup".into());
                foster_rt_v5_end_cleanup();
            },
        );
        assert_eq!(result, Err("outer".into()));
        assert_eq!(foster_rt_v5_failure_pending_in(state), 0);
    })
    .join()
    .unwrap();
}

#[unsafe(no_mangle)]
unsafe extern "C" fn foster_native_string(data: usize, length: i64) -> usize {
    // The runtime passes validated UTF-8 and an in-bounds byte count.
    let bytes = unsafe { std::slice::from_raw_parts(data as *const u8, length as usize) };
    Box::into_raw(Box::new(std::str::from_utf8(bytes).unwrap().to_owned())) as usize
}
#[unsafe(no_mangle)]
unsafe extern "C" fn foster_native_string_data(value: usize) -> usize {
    unsafe { (&*(value as *const String)).as_ptr() as usize }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn foster_native_string_length(value: usize) -> i64 {
    unsafe { (&*(value as *const String)).len() as i64 }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn foster_native_string_share(value: usize) -> usize {
    // The test stub has no managed storage to share; a clone is equivalent.
    let text = unsafe { &*(value as *const String) };
    Box::into_raw(Box::new(text.clone())) as usize
}

#[test]
fn text_adapter_preserves_utf8_through_program_hooks() {
    let value = owned_string("Aλ🙂");
    // The test owns this handle until the final release.
    assert_eq!(unsafe { string_value(value) }, "Aλ🙂");
    unsafe { drop(Box::from_raw(value as *mut String)) };
}

#[test]
fn response_handle_preserves_binary_payload_and_is_released() {
    let handle = foster_host_response(FosterHostResponse::success(FosterHostValue::Bytes(vec![
        0, 255, 65,
    ])));
    assert_eq!(foster_rt_v5_host_ok(handle), 1);
    // The handle was allocated immediately above and remains live until release.
    assert_eq!(
        unsafe { &foster_host_response_ref(handle).bytes },
        &[0, 255, 65]
    );
    foster_rt_v5_host_release(handle);
}

#[unsafe(no_mangle)]
unsafe extern "C" fn foster_native_bytes(data: usize, length: i64) -> usize {
    let bytes = unsafe { std::slice::from_raw_parts(data as *const u8, length as usize) };
    Box::into_raw(Box::new(bytes.to_vec())) as usize
}
#[unsafe(no_mangle)]
unsafe extern "C" fn foster_native_bytes_data(value: usize) -> usize {
    unsafe { (&*(value as *const Vec<u8>)).as_ptr() as usize }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn foster_native_bytes_length(value: usize) -> i64 {
    unsafe { (&*(value as *const Vec<u8>)).len() as i64 }
}
#[test]
fn binary_adapter_preserves_every_byte_without_utf8_validation() {
    let bytes: Vec<u8> = (0..=255).collect();
    let value = owned_bytes(&bytes);
    assert_eq!(unsafe { bytes_value(value) }, bytes);
    unsafe { drop(Box::from_raw(value as *mut Vec<u8>)) };
}
