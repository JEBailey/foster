//! Test-only implementations of the program-specific string hooks.
use super::*;

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
    assert_eq!(foster_rt_v4_host_ok(handle), 1);
    // The handle was allocated immediately above and remains live until release.
    assert_eq!(
        unsafe { &foster_host_response_ref(handle).bytes },
        &[0, 255, 65]
    );
    foster_rt_v4_host_release(handle);
}
