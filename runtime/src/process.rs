#[unsafe(no_mangle)]
extern "C" fn foster_rt_v5_process_exchange(
    operation: i64,
    token: i64,
    executable: usize,
    arguments: usize,
    directory: usize,
    limit: i64,
) -> usize {
    let executable = unsafe { string_value(executable) }.to_owned();
    let arguments = unsafe { string_value(arguments) }.to_owned();
    let directory = unsafe { string_value(directory) }.to_owned();
    let response = foster_host_blocking(move || {
        process_runtime::exchange(operation, token, &executable, &arguments, &directory, limit)
    });
    owned_string(&response)
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v5_process_reserve() -> i64 {
    process_runtime::reserve()
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v5_process_wait(token: i64) -> usize {
    let response = foster_host_blocking(move || process_runtime::exchange(2, token, "", "", "", 0));
    owned_string(&response)
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v5_process_release(token: i64) -> u8 {
    foster_host_blocking(move || process_runtime::release(token));
    0
}
