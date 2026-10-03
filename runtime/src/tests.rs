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
