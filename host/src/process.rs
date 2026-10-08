//! OS child-process primitives shared by the VM and native runtime.
//! Foster owns the public API, result decoding, and await loop.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

type Capture = JoinHandle<Result<Vec<u8>, String>>;
struct Process {
    child: Child,
    stdout: Option<Capture>,
    stderr: Option<Capture>,
    captured: Option<(Vec<u8>, Vec<u8>)>,
}

impl Drop for Process {
    fn drop(&mut self) {
        // Always reap the direct child. Pipe readers may outlive it when a
        // descendant inherited a pipe; cleanup must not wait for that descendant.
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[derive(Default)]
struct Processes {
    next: i64,
    entries: HashMap<i64, Arc<Mutex<Process>>>,
}

fn processes() -> &'static Mutex<Processes> {
    static PROCESSES: OnceLock<Mutex<Processes>> = OnceLock::new();
    PROCESSES.get_or_init(Mutex::default)
}

fn capture(mut pipe: impl Read + Send + 'static, limit: usize) -> Result<Capture, String> {
    std::thread::Builder::new()
        .name("foster-process-output".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let mut buffer = [0; 8192];
            let mut overflow = false;
            loop {
                let count = pipe.read(&mut buffer).map_err(|e| e.to_string())?;
                if count == 0 {
                    break;
                }
                let keep = count.min(limit.saturating_sub(bytes.len()));
                bytes.extend_from_slice(&buffer[..keep]);
                overflow |= keep < count;
            }
            if overflow {
                Err(format!("process output exceeded {limit} bytes per stream"))
            } else {
                Ok(bytes)
            }
        })
        .map_err(|e| e.to_string())
}

fn encode(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 15) as usize] as char);
    }
    result
}

fn argument(value: &str) -> Result<String, String> {
    let (pairs, remainder) = value.as_bytes().as_chunks::<2>();
    if !remainder.is_empty() {
        return Err("invalid process argument encoding".into());
    }
    let bytes = pairs
        .iter()
        .map(|pair| {
            let digit = |byte: u8| {
                (byte as char)
                    .to_digit(16)
                    .ok_or("invalid process argument encoding".to_owned())
            };
            Ok((digit(pair[0])? * 16 + digit(pair[1])?) as u8)
        })
        .collect::<Result<Vec<_>, String>>()?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

fn status(status: ExitStatus) -> String {
    format!(
        "{}\n{}",
        u8::from(status.success()),
        status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_default()
    )
}

/// Reserve identity before spawning, so Foster can install its cleanup owner first.
pub fn reserve() -> i64 {
    let mut table = processes().lock().unwrap_or_else(|e| e.into_inner());
    let Some(token) = table.next.checked_add(1) else {
        return 0;
    };
    table.next = token;
    token
}

fn start(
    token: i64,
    executable: &str,
    arguments: &str,
    directory: &str,
    limit: i64,
) -> Result<String, String> {
    {
        let table = processes().lock().unwrap_or_else(|e| e.into_inner());
        if token <= 0 || token > table.next || table.entries.contains_key(&token) {
            return Err("invalid process handle".into());
        }
    }
    if !(1..=64 * 1024 * 1024).contains(&limit) {
        return Err("output limit must be between 1 and 67108864 bytes".into());
    }
    let mut command = Command::new(executable);
    // Each argument has a trailing newline. Empty strings remain distinct from no arguments.
    for value in arguments.split_terminator('\n') {
        command.arg(argument(value)?);
    }
    if !directory.is_empty() {
        command.current_dir(directory);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let child = command.spawn().map_err(|e| e.to_string())?;
    let mut process = Process {
        child,
        stdout: None,
        stderr: None,
        captured: None,
    };
    let pid = process.child.id();
    process.stdout = Some(capture(
        process.child.stdout.take().expect("piped stdout"),
        limit as usize,
    )?);
    process.stderr = Some(capture(
        process.child.stderr.take().expect("piped stderr"),
        limit as usize,
    )?);
    let mut table = processes().lock().unwrap_or_else(|e| e.into_inner());
    table.entries.insert(token, Arc::new(Mutex::new(process)));
    Ok(format!("S\n{pid}"))
}

fn operate(
    operation: i64,
    token: i64,
    executable: &str,
    arguments: &str,
    directory: &str,
    limit: i64,
) -> Result<String, String> {
    if operation == 0 {
        return start(token, executable, arguments, directory, limit);
    }
    let process = processes()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entries
        .get(&token)
        .cloned()
        .ok_or("process handle is closed")?;
    let mut process = process.lock().unwrap_or_else(|e| e.into_inner());
    if operation == 4 || operation == 5 {
        let (stdout, stderr) = process
            .captured
            .as_ref()
            .ok_or("process output is not complete")?;
        return Ok(encode(if operation == 4 { stdout } else { stderr }));
    }
    if operation == 3 {
        if process
            .child
            .try_wait()
            .map_err(|e| e.to_string())?
            .is_none()
        {
            process.child.kill().map_err(|e| e.to_string())?;
        }
        return Ok("K".into());
    }
    if operation != 1 && operation != 2 {
        return Err("invalid process operation".into());
    }
    if let Some(exit) = process.child.try_wait().map_err(|e| e.to_string())? {
        if operation == 1 {
            return Ok(format!("X\n{}", status(exit)));
        }
        if process.stdout.as_ref().is_some_and(JoinHandle::is_finished)
            && process.stderr.as_ref().is_some_and(JoinHandle::is_finished)
        {
            let stdout = process
                .stdout
                .take()
                .unwrap()
                .join()
                .map_err(|_| "stdout reader failed")??;
            let stderr = process
                .stderr
                .take()
                .unwrap()
                .join()
                .map_err(|_| "stderr reader failed")??;
            process.captured = Some((stdout, stderr));
            return Ok(format!("D\n{}", status(exit)));
        }
    }
    drop(process);
    if operation == 2 {
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok("R".into())
}

/// Executes one bounded wait step; the caller owns scheduling/cancellation.
pub fn exchange(
    operation: i64,
    token: i64,
    executable: &str,
    arguments: &str,
    directory: &str,
    limit: i64,
) -> String {
    operate(operation, token, executable, arguments, directory, limit)
        .unwrap_or_else(|message| format!("E\n{message}"))
}

/// Removes the handle before cleanup; repeated release is harmless.
pub fn release(token: i64) {
    let process = processes()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entries
        .remove(&token);
    drop(process);
}
