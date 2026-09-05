//! Rust source linked beside generated Cranelift objects for native platform services.
//!
//! The response object is deliberately opaque to generated code. Accessor functions form the
//! stable ABI; the Rust layout below may change without changing compiled Foster object files.

pub(super) const SOURCE: &str = r#"
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Default)]
struct FosterHostResponse {
    ok: bool,
    integers: [i64; 2],
    text: String,
    bytes: Vec<u8>,
    strings: Vec<String>,
    error_operation: String,
    error_path: String,
    error_message: String,
    error_value: i64,
}

enum FosterHostValue {
    Unit,
    Integer(i64),
    Pair(i64, i64),
    Text(String),
    Bytes(Vec<u8>),
    Strings(Vec<String>),
}

impl FosterHostResponse {
    fn success(value: FosterHostValue) -> Self {
        let mut response = Self { ok: true, ..Self::default() };
        match value {
            FosterHostValue::Unit => {}
            FosterHostValue::Integer(value) => response.integers[0] = value,
            FosterHostValue::Pair(first, second) => response.integers = [first, second],
            FosterHostValue::Text(value) => response.text = value,
            FosterHostValue::Bytes(value) => response.bytes = value,
            FosterHostValue::Strings(value) => response.strings = value,
        }
        response
    }

    fn error(operation: &str, path: &str, value: i64, message: impl Into<String>) -> Self {
        Self {
            error_operation: operation.to_owned(),
            error_path: path.to_owned(),
            error_message: message.into(),
            error_value: value,
            ..Self::default()
        }
    }
}

fn foster_host_response(response: FosterHostResponse) -> usize {
    Box::into_raw(Box::new(response)) as usize
}

unsafe fn foster_host_response_ref<'a>(response: usize) -> &'a FosterHostResponse {
    unsafe { &*(response as *const FosterHostResponse) }
}

fn foster_host_io(
    operation: &str,
    path: &str,
    result: std::io::Result<FosterHostValue>,
) -> FosterHostResponse {
    match result {
        Ok(value) => FosterHostResponse::success(value),
        Err(error) => FosterHostResponse::error(operation, path, 0, error.to_string()),
    }
}

fn foster_host_network(
    operation: &str,
    result: Result<FosterHostValue, String>,
) -> FosterHostResponse {
    match result {
        Ok(value) => FosterHostResponse::success(value),
        Err(error) => FosterHostResponse::error(operation, "", 0, error),
    }
}

static FOSTER_HOST_DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
static FOSTER_MONOTONIC_ORIGIN: OnceLock<Instant> = OnceLock::new();
static FOSTER_NETWORK: OnceLock<Mutex<FosterNetwork>> = OnceLock::new();

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_host_initialize() -> u8 {
    FOSTER_HOST_DIRECTORY.get_or_init(|| std::env::current_dir().unwrap_or_default());
    FOSTER_MONOTONIC_ORIGIN.get_or_init(Instant::now);
    FOSTER_NETWORK.get_or_init(|| Mutex::new(FosterNetwork::default()));
    0
}

fn foster_host_directory() -> &'static PathBuf {
    FOSTER_HOST_DIRECTORY.get_or_init(|| std::env::current_dir().unwrap_or_default())
}

fn foster_host_resolve(path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        foster_host_directory().join(path)
    }
}

fn foster_host_path_text(path: PathBuf) -> std::io::Result<String> {
    path.into_os_string().into_string().map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "path is not valid UTF-8")
    })
}

fn foster_host_component(value: Option<&std::ffi::OsStr>) -> Result<String, String> {
    match value {
        Some(value) => value
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| "path component is not valid UTF-8".to_owned()),
        None => Ok(String::new()),
    }
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_host_call_nullary(operation: i64) -> usize {
    let response = match operation {
        45 => foster_host_io(
            "current_directory",
            "",
            foster_host_path_text(foster_host_directory().clone()).map(FosterHostValue::Text),
        ),
        59 => match foster_host_wall_now() {
            Ok((seconds, nanos)) => FosterHostResponse::success(FosterHostValue::Pair(seconds, nanos)),
            Err(error) => FosterHostResponse::error("wall_now", "", 0, error),
        },
        60 => match i64::try_from(
            FOSTER_MONOTONIC_ORIGIN.get_or_init(Instant::now).elapsed().as_nanos(),
        ) {
            Ok(value) => FosterHostResponse::success(FosterHostValue::Integer(value)),
            Err(_) => FosterHostResponse::error(
                "monotonic_now",
                "",
                0,
                "monotonic clock reading exceeds Int",
            ),
        },
        _ => FosterHostResponse::error("host", "", operation, "unknown nullary host operation"),
    };
    foster_host_response(response)
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_host_call_string(operation: i64, value: usize) -> usize {
    let value = unsafe { string_value(value) };
    let response = match operation {
        26 => foster_host_io(
            "read_text",
            value,
            std::fs::read_to_string(foster_host_resolve(value)).map(FosterHostValue::Text),
        ),
        28 => foster_host_io(
            "read_bytes",
            value,
            std::fs::read(foster_host_resolve(value)).map(FosterHostValue::Bytes),
        ),
        30 => foster_host_list_directory(value),
        31 => FosterHostResponse::success(FosterHostValue::Integer(i64::from(
            foster_host_resolve(value).exists(),
        ))),
        32 => FosterHostResponse::success(FosterHostValue::Integer(i64::from(
            foster_host_resolve(value).is_file(),
        ))),
        33 => FosterHostResponse::success(FosterHostValue::Integer(i64::from(
            foster_host_resolve(value).is_dir(),
        ))),
        34 => foster_host_io(
            "create_directory",
            value,
            std::fs::create_dir(foster_host_resolve(value)).map(|()| FosterHostValue::Unit),
        ),
        35 => foster_host_io(
            "create_directory_all",
            value,
            std::fs::create_dir_all(foster_host_resolve(value)).map(|()| FosterHostValue::Unit),
        ),
        36 => foster_host_io(
            "remove_file",
            value,
            std::fs::remove_file(foster_host_resolve(value)).map(|()| FosterHostValue::Unit),
        ),
        37 => foster_host_io(
            "remove_directory",
            value,
            std::fs::remove_dir(foster_host_resolve(value)).map(|()| FosterHostValue::Unit),
        ),
        41 => match Path::new(value).parent().map(Path::to_path_buf) {
            Some(path) => match foster_host_path_text(path) {
                Ok(path) => FosterHostResponse::success(FosterHostValue::Text(path)),
                Err(error) => FosterHostResponse::error("parent", value, 0, error.to_string()),
            },
            None => FosterHostResponse::success(FosterHostValue::Text(String::new())),
        },
        42 => match foster_host_component(Path::new(value).file_name()) {
            Ok(component) => FosterHostResponse::success(FosterHostValue::Text(component)),
            Err(error) => FosterHostResponse::error("file_name", value, 0, error),
        },
        43 => match foster_host_component(Path::new(value).extension()) {
            Ok(component) => FosterHostResponse::success(FosterHostValue::Text(component)),
            Err(error) => FosterHostResponse::error("extension", value, 0, error),
        },
        44 => foster_host_io(
            "canonicalize",
            value,
            std::fs::canonicalize(foster_host_resolve(value))
                .and_then(foster_host_path_text)
                .map(FosterHostValue::Text),
        ),
        58 => foster_host_file_length(value),
        _ => FosterHostResponse::error("host", value, operation, "unknown string host operation"),
    };
    foster_host_response(response)
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_host_call_strings(
    operation: i64,
    first: usize,
    second: usize,
) -> usize {
    let first = unsafe { string_value(first) };
    let second = unsafe { string_value(second) };
    let response = match operation {
        27 => foster_host_io(
            "write_text",
            first,
            std::fs::write(foster_host_resolve(first), second.as_bytes())
                .map(|()| FosterHostValue::Unit),
        ),
        38 => foster_host_io(
            "rename",
            first,
            std::fs::rename(foster_host_resolve(first), foster_host_resolve(second))
                .map(|()| FosterHostValue::Unit),
        ),
        39 => foster_host_io(
            "copy_file",
            first,
            std::fs::copy(foster_host_resolve(first), foster_host_resolve(second)).and_then(|size| {
                i64::try_from(size)
                    .map(FosterHostValue::Integer)
                    .map_err(|_| std::io::Error::other("copied byte count exceeds Int"))
            }),
        ),
        40 => match foster_host_path_text(Path::new(first).join(second)) {
            Ok(path) => FosterHostResponse::success(FosterHostValue::Text(path)),
            Err(error) => FosterHostResponse::error("join", first, 0, error.to_string()),
        },
        _ => FosterHostResponse::error("host", first, operation, "unknown string-pair host operation"),
    };
    foster_host_response(response)
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_host_call_string_ints(
    operation: i64,
    text: usize,
    first: i64,
    second: i64,
) -> usize {
    let text = unsafe { string_value(text) };
    let response = match operation {
        46 => foster_host_network(
            "listen",
            foster_network_listen(text, first).map(FosterHostValue::Integer),
        ),
        47 => foster_host_network(
            "connect",
            foster_network_connect(text, first).map(FosterHostValue::Integer),
        ),
        56 => foster_host_read_range(text, first, second),
        _ => FosterHostResponse::error("host", text, operation, "unknown string/integer host operation"),
    };
    foster_host_response(response)
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_host_call_int(operation: i64, value: i64) -> usize {
    let response = match operation {
        48 => foster_host_network(
            "accept",
            foster_network_accept(value).map(FosterHostValue::Integer),
        ),
        54 => foster_host_network(
            "close_listener",
            foster_network_close_listener(value).map(|()| FosterHostValue::Unit),
        ),
        55 => foster_host_network(
            "close_connection",
            foster_network_close_connection(value).map(|()| FosterHostValue::Unit),
        ),
        61 => match foster_random_bytes(value) {
            Ok(bytes) => FosterHostResponse::success(FosterHostValue::Bytes(bytes)),
            Err(error) => FosterHostResponse::error("bytes", "", value, error),
        },
        _ => FosterHostResponse::error("host", "", operation, "unknown integer host operation"),
    };
    foster_host_response(response)
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_host_call_ints(operation: i64, first: i64, second: i64) -> usize {
    let response = match operation {
        49 => foster_host_network(
            "read",
            foster_network_read(first, second).map(FosterHostValue::Text),
        ),
        51 => foster_host_network(
            "read_bytes",
            foster_network_read_bytes(first, second).map(FosterHostValue::Bytes),
        ),
        53 => foster_host_network(
            "set_timeout",
            foster_network_set_timeout(first, second).map(|()| FosterHostValue::Unit),
        ),
        _ => FosterHostResponse::error("host", "", operation, "unknown integer-pair host operation"),
    };
    foster_host_response(response)
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_host_call_string_bytes(
    operation: i64,
    text: usize,
    data: usize,
    length: i64,
) -> usize {
    let text = unsafe { string_value(text) };
    let bytes = unsafe { foster_host_input_bytes(data, length) };
    let response = match operation {
        29 => foster_host_io(
            "write_bytes",
            text,
            std::fs::write(foster_host_resolve(text), bytes).map(|()| FosterHostValue::Unit),
        ),
        57 => foster_host_append_bytes(text, bytes),
        _ => FosterHostResponse::error("host", text, operation, "unknown string/bytes host operation"),
    };
    foster_host_response(response)
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_host_call_int_bytes(
    operation: i64,
    handle: i64,
    data: usize,
    length: i64,
) -> usize {
    let bytes = unsafe { foster_host_input_bytes(data, length) };
    let response = match operation {
        52 => foster_host_network(
            "write_bytes",
            foster_network_write_bytes(handle, bytes).map(|()| FosterHostValue::Unit),
        ),
        _ => FosterHostResponse::error("host", "", operation, "unknown integer/bytes host operation"),
    };
    foster_host_response(response)
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_host_call_int_string(
    operation: i64,
    handle: i64,
    text: usize,
) -> usize {
    let text = unsafe { string_value(text) };
    let response = match operation {
        50 => foster_host_network(
            "write",
            foster_network_write_bytes(handle, text.as_bytes()).map(|()| FosterHostValue::Unit),
        ),
        _ => FosterHostResponse::error("host", "", operation, "unknown integer/string host operation"),
    };
    foster_host_response(response)
}

unsafe fn foster_host_input_bytes<'a>(data: usize, length: i64) -> &'a [u8] {
    let length = usize::try_from(length).unwrap_or_else(|_| std::process::abort());
    if length == 0 {
        return &[];
    }
    if data == 0 {
        std::process::abort();
    }
    unsafe { std::slice::from_raw_parts(data as *const u8, length) }
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_host_require_ok(response: usize) -> u8 {
    let response = unsafe { foster_host_response_ref(response) };
    if !response.ok {
        eprintln!("error: {}", response.error_message);
        std::process::exit(2);
    }
    0
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_host_ok(response: usize) -> u8 {
    u8::from(unsafe { foster_host_response_ref(response).ok })
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_host_integer(response: usize, index: i64) -> i64 {
    let response = unsafe { foster_host_response_ref(response) };
    usize::try_from(index)
        .ok()
        .and_then(|index| response.integers.get(index))
        .copied()
        .unwrap_or_else(|| std::process::abort())
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_host_error_value(response: usize) -> i64 {
    unsafe { foster_host_response_ref(response).error_value }
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_host_string(response: usize, field: i64, index: i64) -> usize {
    let response = unsafe { foster_host_response_ref(response) };
    let value = match field {
        0 => &response.text,
        1 => &response.error_operation,
        2 => &response.error_path,
        3 => &response.error_message,
        4 => usize::try_from(index)
            .ok()
            .and_then(|index| response.strings.get(index))
            .unwrap_or_else(|| std::process::abort()),
        _ => std::process::abort(),
    };
    owned_string(value)
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_host_bytes_length(response: usize) -> i64 {
    i64::try_from(unsafe { foster_host_response_ref(response).bytes.len() })
        .unwrap_or_else(|_| std::process::abort())
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_host_copy_bytes(response: usize, destination: usize) -> u8 {
    let bytes = unsafe { &foster_host_response_ref(response).bytes };
    if !bytes.is_empty() {
        if destination == 0 {
            std::process::abort();
        }
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), destination as *mut u8, bytes.len()) };
    }
    0
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_host_strings_length(response: usize) -> i64 {
    i64::try_from(unsafe { foster_host_response_ref(response).strings.len() })
        .unwrap_or_else(|_| std::process::abort())
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_host_release(response: usize) -> u8 {
    unsafe { drop(Box::from_raw(response as *mut FosterHostResponse)) };
    0
}

type FosterRemoteCallback = unsafe extern "C" fn(u64, usize) -> u64;
type FosterReleaseCallback = unsafe extern "C" fn(usize) -> u8;

fn foster_remote_abort(message: &str) -> ! {
    eprintln!("error: {message}");
    std::process::exit(2);
}

unsafe fn foster_release_word(value: u64, release: usize) {
    if release == 0 {
        return;
    }
    let value = usize::try_from(value).unwrap_or_else(|_| std::process::abort());
    let release: FosterReleaseCallback = unsafe { std::mem::transmute(release) };
    unsafe { release(value) };
}

struct FosterRemoteCompletion {
    value: u64,
    release: usize,
}

impl Drop for FosterRemoteCompletion {
    fn drop(&mut self) {
        unsafe { foster_release_word(self.value, self.release) };
    }
}

struct FosterRemoteMessage {
    callback: FosterRemoteCallback,
    arguments: Vec<u64>,
    result_release: usize,
    response: mpsc::Sender<FosterRemoteCompletion>,
}

struct FosterRemote {
    sender: mpsc::Sender<FosterRemoteMessage>,
    borrowed: bool,
    worker: Option<thread::JoinHandle<()>>,
}

struct FosterFuture {
    receiver: Mutex<Option<mpsc::Receiver<FosterRemoteCompletion>>>,
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_remote_spawn(state: u64, release: usize, borrowed: u8) -> usize {
    let (sender, receiver) = mpsc::channel::<FosterRemoteMessage>();
    let worker = thread::spawn(move || {
        while let Ok(message) = receiver.recv() {
            let arguments = message.arguments.as_ptr() as usize;
            let value = unsafe { (message.callback)(state, arguments) };
            let _ = message.response.send(FosterRemoteCompletion {
                value,
                release: message.result_release,
            });
        }
        unsafe { foster_release_word(state, release) };
    });
    Box::into_raw(Box::new(FosterRemote {
        sender,
        borrowed: borrowed != 0,
        worker: Some(worker),
    })) as usize
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_remote_call(
    remote: usize,
    callback: usize,
    arguments: usize,
    argument_count: i64,
    blocking: u8,
    result_release: usize,
) -> usize {
    let remote = unsafe { &*(remote as *const FosterRemote) };
    let callback: FosterRemoteCallback = unsafe { std::mem::transmute(callback) };
    let argument_count = usize::try_from(argument_count).unwrap_or_else(|_| std::process::abort());
    let arguments = if argument_count == 0 {
        Vec::new()
    } else {
        if arguments == 0 {
            std::process::abort();
        }
        unsafe { std::slice::from_raw_parts(arguments as *const u64, argument_count) }.to_vec()
    };
    let (response, receiver) = mpsc::channel();
    remote
        .sender
        .send(FosterRemoteMessage {
            callback,
            arguments,
            result_release,
            response,
        })
        .unwrap_or_else(|_| foster_remote_abort("remote object is closed"));
    let receiver = if blocking != 0 || remote.borrowed {
        let completion = receiver
            .recv()
            .unwrap_or_else(|_| foster_remote_abort("remote object terminated before replying"));
        let (ready, receiver) = mpsc::channel();
        ready
            .send(completion)
            .unwrap_or_else(|_| std::process::abort());
        receiver
    } else {
        receiver
    };
    Box::into_raw(Box::new(FosterFuture {
        receiver: Mutex::new(Some(receiver)),
    })) as usize
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_future_await(future: usize) -> u64 {
    let future = unsafe { &*(future as *const FosterFuture) };
    let receiver = future
        .receiver
        .lock()
        .unwrap_or_else(|_| foster_remote_abort("future lock was poisoned"))
        .take()
        .unwrap_or_else(|| foster_remote_abort("future has already been awaited"));
    let mut completion = receiver
        .recv()
        .unwrap_or_else(|_| foster_remote_abort("remote object terminated before replying"));
    completion.release = 0;
    completion.value
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_remote_release(remote: usize) -> u8 {
    let remote = unsafe { Box::from_raw(remote as *mut FosterRemote) };
    let FosterRemote {
        sender,
        worker,
        borrowed: _,
    } = *remote;
    drop(sender);
    if let Some(worker) = worker
        && worker.thread().id() != thread::current().id()
    {
        let _ = worker.join();
    }
    0
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_future_release(future: usize) -> u8 {
    unsafe { drop(Box::from_raw(future as *mut FosterFuture)) };
    0
}

fn foster_host_list_directory(path: &str) -> FosterHostResponse {
    let result = (|| {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(foster_host_resolve(path))? {
            let name = entry?.file_name().into_string().map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "directory entry name is not valid UTF-8",
                )
            })?;
            names.push(name);
        }
        names.sort();
        Ok(FosterHostValue::Strings(names))
    })();
    foster_host_io("list_directory", path, result)
}

fn foster_host_read_range(path: &str, offset: i64, maximum: i64) -> FosterHostResponse {
    let result = (|| {
        let offset = u64::try_from(offset).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "file offset cannot be negative")
        })?;
        let maximum = usize::try_from(maximum)
            .ok()
            .filter(|maximum| (1..=1024 * 1024).contains(maximum))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "read maximum must be between 1 and 1048576",
                )
            })?;
        let mut file = std::fs::File::open(foster_host_resolve(path))?;
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0; maximum];
        let read = file.read(&mut bytes)?;
        bytes.truncate(read);
        Ok(FosterHostValue::Bytes(bytes))
    })();
    foster_host_io("read_range", path, result)
}

fn foster_host_append_bytes(path: &str, bytes: &[u8]) -> FosterHostResponse {
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(foster_host_resolve(path))
        .and_then(|mut file| file.write_all(bytes))
        .map(|()| FosterHostValue::Integer(i64::try_from(bytes.len()).unwrap_or(i64::MAX)));
    foster_host_io("append_bytes", path, result)
}

fn foster_host_file_length(path: &str) -> FosterHostResponse {
    let result = std::fs::metadata(foster_host_resolve(path)).and_then(|metadata| {
        if !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path is not a regular file",
            ));
        }
        i64::try_from(metadata.len())
            .map(FosterHostValue::Integer)
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "file length exceeds Foster Int",
                )
            })
    });
    foster_host_io("file_length", path, result)
}

fn foster_host_wall_now() -> Result<(i64, i64), String> {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => Ok((
            i64::try_from(duration.as_secs())
                .map_err(|_| "wall clock seconds exceed Int".to_owned())?,
            i64::from(duration.subsec_nanos()),
        )),
        Err(error) => {
            let duration = error.duration();
            let seconds = i64::try_from(duration.as_secs())
                .map_err(|_| "wall clock seconds exceed Int".to_owned())?;
            let nanosecond = i64::from(duration.subsec_nanos());
            if nanosecond == 0 {
                Ok((-seconds, 0))
            } else {
                Ok((-seconds - 1, 1_000_000_000 - nanosecond))
            }
        }
    }
}

#[cfg(unix)]
fn foster_random_fill(output: &mut [u8]) -> Result<(), String> {
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(output))
        .map_err(|error| format!("operating-system entropy is unavailable: {error}"))
}

#[cfg(windows)]
#[link(name = "bcrypt")]
unsafe extern "system" {
    #[link_name = "BCryptGenRandom"]
    fn foster_bcrypt_gen_random(
        algorithm: *mut std::ffi::c_void,
        output: *mut u8,
        output_length: u32,
        flags: u32,
    ) -> i32;
}

#[cfg(windows)]
fn foster_random_fill(output: &mut [u8]) -> Result<(), String> {
    const USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;
    if output.is_empty() {
        return Ok(());
    }
    let output_length = u32::try_from(output.len())
        .map_err(|_| "operating-system entropy request exceeds the Windows API limit".to_owned())?;
    let status = unsafe {
        foster_bcrypt_gen_random(
            std::ptr::null_mut(),
            output.as_mut_ptr(),
            output_length,
            USE_SYSTEM_PREFERRED_RNG,
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
fn foster_random_fill(_output: &mut [u8]) -> Result<(), String> {
    Err("operating-system entropy is unavailable on this target".to_owned())
}

fn foster_random_bytes(count: i64) -> Result<Vec<u8>, String> {
    let count = usize::try_from(count)
        .ok()
        .filter(|count| *count <= 1_048_576)
        .ok_or_else(|| "random byte count must be between 0 and 1048576".to_owned())?;
    let mut bytes = vec![0; count];
    foster_random_fill(&mut bytes)?;
    Ok(bytes)
}

#[derive(Default)]
struct FosterNetwork {
    next_handle: i64,
    listeners: HashMap<i64, Arc<TcpListener>>,
    connections: HashMap<i64, Arc<Mutex<TcpStream>>>,
}

impl FosterNetwork {
    fn allocate_handle(&mut self) -> Result<i64, String> {
        if self.next_handle == 0 {
            self.next_handle = 1;
        }
        let handle = self.next_handle;
        self.next_handle = self
            .next_handle
            .checked_add(1)
            .ok_or_else(|| "TCP handle space is exhausted".to_owned())?;
        Ok(handle)
    }
}

fn foster_network() -> Result<std::sync::MutexGuard<'static, FosterNetwork>, String> {
    FOSTER_NETWORK
        .get_or_init(|| Mutex::new(FosterNetwork::default()))
        .lock()
        .map_err(|_| "network host lock was poisoned".to_owned())
}

fn foster_network_listen(address: &str, port: i64) -> Result<i64, String> {
    let port = u16::try_from(port).map_err(|_| "port must be between 0 and 65535".to_owned())?;
    let listener = TcpListener::bind((address, port))
        .map_err(|error| format!("could not listen on {address}:{port}: {error}"))?;
    let mut network = foster_network()?;
    let handle = network.allocate_handle()?;
    network.listeners.insert(handle, Arc::new(listener));
    Ok(handle)
}

fn foster_network_connect(address: &str, port: i64) -> Result<i64, String> {
    let port = u16::try_from(port).map_err(|_| "port must be between 0 and 65535".to_owned())?;
    let connection = TcpStream::connect((address, port))
        .map_err(|error| format!("could not connect to {address}:{port}: {error}"))?;
    let mut network = foster_network()?;
    let handle = network.allocate_handle()?;
    network
        .connections
        .insert(handle, Arc::new(Mutex::new(connection)));
    Ok(handle)
}

fn foster_network_accept(listener: i64) -> Result<i64, String> {
    let listener = foster_network()?
        .listeners
        .get(&listener)
        .cloned()
        .ok_or_else(|| "TCP listener is closed or invalid".to_owned())?;
    let (connection, _) = listener
        .accept()
        .map_err(|error| format!("could not accept TCP connection: {error}"))?;
    let mut network = foster_network()?;
    let handle = network.allocate_handle()?;
    network
        .connections
        .insert(handle, Arc::new(Mutex::new(connection)));
    Ok(handle)
}

fn foster_network_connection(handle: i64) -> Result<Arc<Mutex<TcpStream>>, String> {
    foster_network()?
        .connections
        .get(&handle)
        .cloned()
        .ok_or_else(|| "TCP connection is closed or invalid".to_owned())
}

fn foster_network_read_bytes(handle: i64, maximum: i64) -> Result<Vec<u8>, String> {
    let maximum = usize::try_from(maximum)
        .ok()
        .filter(|maximum| (1..=1024 * 1024).contains(maximum))
        .ok_or_else(|| "read maximum must be between 1 and 1048576".to_owned())?;
    let connection = foster_network_connection(handle)?;
    let mut bytes = vec![0; maximum];
    let read = connection
        .lock()
        .map_err(|_| "TCP connection lock was poisoned".to_owned())?
        .read(&mut bytes)
        .map_err(|error| format!("could not read TCP connection: {error}"))?;
    bytes.truncate(read);
    Ok(bytes)
}

fn foster_network_read(handle: i64, maximum: i64) -> Result<String, String> {
    String::from_utf8(foster_network_read_bytes(handle, maximum)?)
        .map_err(|_| "TCP input is not valid UTF-8".to_owned())
}

fn foster_network_write_bytes(handle: i64, bytes: &[u8]) -> Result<(), String> {
    foster_network_connection(handle)?
        .lock()
        .map_err(|_| "TCP connection lock was poisoned".to_owned())?
        .write_all(bytes)
        .map_err(|error| format!("could not write TCP connection: {error}"))
}

fn foster_network_set_timeout(handle: i64, milliseconds: i64) -> Result<(), String> {
    let milliseconds =
        u64::try_from(milliseconds).map_err(|_| "TCP timeout cannot be negative".to_owned())?;
    let connection = foster_network_connection(handle)?;
    let connection = connection
        .lock()
        .map_err(|_| "TCP connection lock was poisoned".to_owned())?;
    let duration = Some(Duration::from_millis(milliseconds));
    connection
        .set_read_timeout(duration)
        .and_then(|()| connection.set_write_timeout(duration))
        .map_err(|error| format!("could not set TCP timeout: {error}"))
}

fn foster_network_close_listener(handle: i64) -> Result<(), String> {
    foster_network()?
        .listeners
        .remove(&handle)
        .map(|_| ())
        .ok_or_else(|| "TCP listener is already closed or invalid".to_owned())
}

fn foster_network_close_connection(handle: i64) -> Result<(), String> {
    foster_network()?
        .connections
        .remove(&handle)
        .map(|_| ())
        .ok_or_else(|| "TCP connection is already closed or invalid".to_owned())
}
"#;
