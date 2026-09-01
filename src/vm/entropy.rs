//! Minimal platform entropy boundary used by `std.random`.
//!
//! Random transformations and policy remain Foster code. This module only fills a caller-owned
//! byte slice from the operating system and deliberately avoids a Rust package dependency.

#[cfg(unix)]
pub(super) fn fill(output: &mut [u8]) -> Result<(), String> {
    use std::io::Read;

    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(output))
        .map_err(|error| format!("operating-system entropy is unavailable: {error}"))
}

#[cfg(windows)]
pub(super) fn fill(output: &mut [u8]) -> Result<(), String> {
    use std::ffi::c_void;

    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;

    #[link(name = "bcrypt")]
    unsafe extern "system" {
        #[link_name = "BCryptGenRandom"]
        fn bcrypt_gen_random(
            algorithm: *mut c_void,
            output: *mut u8,
            output_length: u32,
            flags: u32,
        ) -> i32;
    }

    if output.is_empty() {
        return Ok(());
    }
    let output_length = u32::try_from(output.len())
        .map_err(|_| "operating-system entropy request exceeds the Windows API limit".to_owned())?;
    // SAFETY: `output` is writable for exactly `output_length` bytes, the algorithm handle is null
    // as required with BCRYPT_USE_SYSTEM_PREFERRED_RNG, and the call does not retain the buffer.
    let status = unsafe {
        bcrypt_gen_random(
            std::ptr::null_mut(),
            output.as_mut_ptr(),
            output_length,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status >= 0 {
        Ok(())
    } else {
        Err(format!(
            "operating-system entropy is unavailable: BCryptGenRandom returned NTSTATUS 0x{:08x}",
            status as u32
        ))
    }
}

#[cfg(not(any(unix, windows)))]
pub(super) fn fill(_output: &mut [u8]) -> Result<(), String> {
    Err("operating-system entropy is unavailable on this target".to_owned())
}
