use super::*;

impl SystemHost {
    pub(super) fn wait_socket(
        &self,
        handle: i64,
        milliseconds: i64,
        listener: bool,
        writable: bool,
    ) -> Result<bool, String> {
        let milliseconds =
            u64::try_from(milliseconds).map_err(|_| "readiness timeout cannot be negative")?;
        let timeout = Duration::from_millis(milliseconds);
        let started = Instant::now();
        loop {
            // Short bounded polls let close cancel the wait. Never hold the
            // handle-table or stream I/O lock while waiting in the OS.
            let remaining = timeout.saturating_sub(started.elapsed());
            let slice = remaining.min(Duration::from_millis(50));
            let millis = i32::try_from(slice.as_millis())
                .unwrap()
                .max(i32::from(!slice.is_zero()));
            let ready = if listener {
                let socket = self
                    .network()?
                    .listeners
                    .get(&handle)
                    .cloned()
                    .ok_or_else(|| "TCP listener is closed or invalid".to_owned())?;
                poll_socket(&*socket, writable, millis)
            } else {
                let socket = self.connection(handle)?;
                poll_socket(&socket.stream, writable, millis)
            }
            .map_err(|error| format!("TCP readiness failed: {error}"))?;
            let open = if listener {
                self.network()?.listeners.contains_key(&handle)
            } else {
                self.network()?.connections.contains_key(&handle)
            };
            if !open {
                return Err("TCP socket closed while waiting for readiness".into());
            }
            if ready {
                return Ok(true);
            }
            if started.elapsed() >= timeout {
                return Ok(false);
            }
        }
    }
}

#[cfg(windows)]
fn poll_socket(
    socket: &impl std::os::windows::io::AsRawSocket,
    writable: bool,
    timeout: i32,
) -> std::io::Result<bool> {
    #[repr(C)]
    struct PollFd {
        fd: usize,
        events: i16,
        revents: i16,
    }
    #[link(name = "ws2_32")]
    unsafe extern "system" {
        fn WSAPoll(fds: *mut PollFd, count: u32, timeout: i32) -> i32;
        fn WSAGetLastError() -> i32;
    }
    let mut fd = PollFd {
        fd: socket.as_raw_socket() as usize,
        events: if writable { 0x10 } else { 0x100 },
        revents: 0,
    };
    // The borrowed socket remains owned throughout this single-descriptor poll.
    let result = unsafe { WSAPoll(&mut fd, 1, timeout) };
    if result < 0 {
        return Err(std::io::Error::from_raw_os_error(unsafe {
            WSAGetLastError()
        }));
    }
    if fd.revents & 4 != 0 {
        return Err(std::io::Error::other("invalid socket"));
    }
    // EOF and connection errors are ready: the next I/O reports their outcome.
    Ok(result > 0 && fd.revents & (fd.events | 1 | 2) != 0)
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
fn poll_socket(
    socket: &impl std::os::fd::AsRawFd,
    writable: bool,
    timeout: i32,
) -> std::io::Result<bool> {
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    type Count = usize;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    type Count = u32;
    unsafe extern "C" {
        fn poll(fds: *mut PollFd, count: Count, timeout: i32) -> i32;
    }
    let mut fd = PollFd {
        fd: socket.as_raw_fd(),
        events: if writable { 4 } else { 1 },
        revents: 0,
    };
    let result = unsafe { poll(&mut fd, 1, timeout) };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(error);
    }
    if fd.revents & 32 != 0 {
        return Err(std::io::Error::other("invalid socket"));
    }
    Ok(result > 0 && fd.revents & (fd.events | 8 | 16) != 0)
}

#[cfg(not(any(
    windows,
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
fn poll_socket<T>(_socket: &T, _writable: bool, _timeout: i32) -> std::io::Result<bool> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "socket readiness is unavailable on this target",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listener_becomes_ready_when_a_connection_arrives() {
        let host = SystemHost::new(".");
        let handle = host.listen("127.0.0.1", 0).unwrap();
        let address = host.network().unwrap().listeners[&handle]
            .local_addr()
            .unwrap();
        assert!(!host.wait_socket(handle, 0, true, false).unwrap());
        let _peer = TcpStream::connect(address).unwrap();
        assert!(host.wait_socket(handle, 1000, true, false).unwrap());
        let connection = host.accept(handle).unwrap();
        host.close_connection(connection).unwrap();
        host.close_listener(handle).unwrap();
    }
}
