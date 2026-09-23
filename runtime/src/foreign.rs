#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_c_exchange(
    path: usize,
    schema: usize,
    operation: i64,
    token: i64,
    create: u8,
    payload: usize,
) -> usize {
    if may::coroutine::is_coroutine() {
        return owned_string(&c_bridge::remote_error());
    }
    owned_string(&c_bridge::exchange(
        unsafe { string_value(path) },
        unsafe { string_value(schema) },
        operation,
        token,
        create != 0,
        unsafe { string_value(payload) },
    ))
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_c_close(token: i64) -> usize {
    owned_string(&c_bridge::close(token))
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_c_release(token: i64) -> u8 {
    if let Err(error) = c_bridge::release(token) {
        foster_execution_failure(error);
    }
    0
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_c_encode_int(value: i64) -> usize {
    owned_string(&c_bridge::encode(&value.to_le_bytes()))
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_c_encode_float(value: f64) -> usize {
    owned_string(&c_bridge::encode(&value.to_le_bytes()))
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_c_decode_int(value: usize) -> i64 {
    c_bridge::int(unsafe { string_value(value) }).unwrap_or_else(|error| {
        foster_execution_failure(error);
        0
    })
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v4_c_decode_float(value: usize) -> f64 {
    f64::from_bits(foster_rt_v4_c_decode_int(value) as u64)
}
