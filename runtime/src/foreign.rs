#[unsafe(no_mangle)]
extern "C" fn foster_rt_v5_c_exchange(
    path: usize,
    schema: usize,
    operation: i64,
    token: i64,
    create: u8,
    payload: usize,
) -> usize {
    if may::coroutine::is_coroutine() {
        return owned_bytes(&c_bridge::remote_error());
    }
    owned_bytes(&c_bridge::exchange(
        unsafe { string_value(path) },
        unsafe { string_value(schema) },
        operation,
        token,
        create != 0,
        unsafe { bytes_value(payload) },
    ))
}

struct NativeCallback {
    code: unsafe extern "C" fn(usize, usize) -> usize,
    environment: usize,
    owner: usize,
    release: unsafe extern "C" fn(usize) -> u8,
    release_packet: unsafe extern "C" fn(usize) -> u8,
}
impl Drop for NativeCallback {
    fn drop(&mut self) {
        unsafe {
            (self.release)(self.owner);
        }
    }
}
impl NativeCallback {
    fn call(&self, payload: &[u8]) -> Result<Vec<u8>, String> {
        let previous = FOSTER_EXECUTION.with(|failure| failure.borrow_mut().take());
        let input = owned_bytes(payload);
        // Generated Foster entry points take one owned ABI reference for each argument.
        let output = unsafe { (self.code)(self.environment, input) };
        let failure = FOSTER_EXECUTION.with(|failure| failure.replace(previous));
        if let Some(failure) = failure {
            return Err(failure);
        }
        if output == 0 {
            return Err("callback returned a null packet".into());
        }
        let result = unsafe { bytes_value(output) }.to_vec();
        unsafe {
            (self.release_packet)(output);
        }
        Ok(result)
    }
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v5_c_callback_new(
    code: usize,
    environment: usize,
    owner: usize,
    release: usize,
    release_packet: usize,
    signature: usize,
    mode: i64,
    owned: u8,
) -> usize {
    let callback = NativeCallback {
        code: unsafe {
            std::mem::transmute::<usize, unsafe extern "C" fn(usize, usize) -> usize>(code)
        },
        environment,
        owner,
        release: unsafe { std::mem::transmute::<usize, unsafe extern "C" fn(usize) -> u8>(release) },
        release_packet: unsafe {
            std::mem::transmute::<usize, unsafe extern "C" fn(usize) -> u8>(release_packet)
        },
    };
    let result = if may::coroutine::is_coroutine() {
        Err("C callbacks cannot be registered in remote tasks".into())
    } else if !(0..=2).contains(&mode) {
        Err("invalid callback mode".into())
    } else if mode != 0 && owned == 0 {
        Err("retained callbacks cannot capture borrowed references or erased callables".into())
    } else {
        c_bridge::callbacks::register(
            unsafe { string_value(signature) },
            mode == 2,
            Box::new(move |payload| callback.call(payload)),
        )
    };
    owned_bytes(&c_bridge::response(
        result.map(|token| token.to_le_bytes().to_vec()),
    ))
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v5_c_callback_release(token: i64) -> u8 {
    if let Err(error) = c_bridge::callbacks::release(token) {
        foster_execution_failure(error);
    }
    0
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v5_c_callback_poll(token: i64) -> usize {
    owned_bytes(&c_bridge::response(
        c_bridge::callbacks::poll(token).map(|()| vec![]),
    ))
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v5_c_callback_error(token: i64) -> usize {
    owned_bytes(&c_bridge::response(
        c_bridge::callbacks::take_error(token).map(|()| vec![]),
    ))
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v5_c_close(token: i64) -> usize {
    owned_bytes(&c_bridge::close(token))
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v5_c_release(token: i64) -> u8 {
    if let Err(error) = c_bridge::release(token) {
        foster_execution_failure(error);
    }
    0
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v5_c_encode_int(value: i64) -> usize {
    owned_bytes(&value.to_le_bytes())
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v5_c_encode_float(value: f64) -> usize {
    owned_bytes(&value.to_le_bytes())
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v5_c_decode_int(value: usize) -> i64 {
    c_bridge::int(unsafe { bytes_value(value) }).unwrap_or_else(|error| {
        foster_execution_failure(error);
        0
    })
}
#[unsafe(no_mangle)]
extern "C" fn foster_rt_v5_c_decode_float(value: usize) -> f64 {
    f64::from_bits(foster_rt_v5_c_decode_int(value) as u64)
}
