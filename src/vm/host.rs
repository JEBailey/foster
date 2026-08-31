use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Host-owned state shared by a Foster machine and the remote objects it creates.
///
/// Relative filesystem paths are resolved against the captured working directory;
/// absolute paths remain absolute. Network handles are private to this context.
/// Dropping the last context owner releases every listener and connection that
/// remains open.
pub struct HostContext {
    working_directory: PathBuf,
    network: Mutex<NetworkHost>,
}

impl HostContext {
    /// Creates an isolated host context based at `working_directory`.
    pub fn new(working_directory: impl Into<PathBuf>) -> Self {
        let working_directory = working_directory.into();
        let working_directory = if working_directory.is_absolute() {
            working_directory
        } else {
            std::env::current_dir()
                .map(|current| current.join(&working_directory))
                .unwrap_or(working_directory)
        };
        Self {
            working_directory,
            network: Mutex::new(NetworkHost::default()),
        }
    }

    /// Captures the process working directory for a new standalone machine.
    pub fn current() -> std::io::Result<Self> {
        std::env::current_dir().map(Self::new)
    }

    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    pub(super) fn resolve_path(&self, path: &str) -> PathBuf {
        let path = Path::new(path);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.working_directory.join(path)
        }
    }

    pub(super) fn listen(&self, address: &str, port: i64) -> Result<i64, String> {
        let port =
            u16::try_from(port).map_err(|_| "port must be between 0 and 65535".to_owned())?;
        let listener = TcpListener::bind((address, port))
            .map_err(|error| format!("could not listen on {address}:{port}: {error}"))?;
        let mut network = self.network()?;
        let handle = network.next_handle();
        network.listeners.insert(handle, Arc::new(listener));
        Ok(handle)
    }

    pub(super) fn accept(&self, listener: i64) -> Result<i64, String> {
        let listener = self
            .network()?
            .listeners
            .get(&listener)
            .cloned()
            .ok_or_else(|| "TCP listener is closed or invalid".to_owned())?;
        let (connection, _) = listener
            .accept()
            .map_err(|error| format!("could not accept TCP connection: {error}"))?;
        let mut network = self.network()?;
        let handle = network.next_handle();
        network
            .connections
            .insert(handle, Arc::new(Mutex::new(connection)));
        Ok(handle)
    }

    pub(super) fn connect(&self, address: &str, port: i64) -> Result<i64, String> {
        let port =
            u16::try_from(port).map_err(|_| "port must be between 0 and 65535".to_owned())?;
        let connection = TcpStream::connect((address, port))
            .map_err(|error| format!("could not connect to {address}:{port}: {error}"))?;
        let mut network = self.network()?;
        let handle = network.next_handle();
        network
            .connections
            .insert(handle, Arc::new(Mutex::new(connection)));
        Ok(handle)
    }

    pub(super) fn read(&self, connection: i64, maximum: i64) -> Result<String, String> {
        String::from_utf8(self.read_bytes(connection, maximum)?)
            .map_err(|_| "TCP input is not valid UTF-8".to_owned())
    }

    pub(super) fn read_bytes(&self, connection: i64, maximum: i64) -> Result<Vec<u8>, String> {
        let maximum = usize::try_from(maximum)
            .ok()
            .filter(|maximum| (1..=1024 * 1024).contains(maximum))
            .ok_or_else(|| "read maximum must be between 1 and 1048576".to_owned())?;
        let connection = self.connection(connection)?;
        let mut bytes = vec![0; maximum];
        let read = connection
            .lock()
            .map_err(|_| "TCP connection lock was poisoned".to_owned())?
            .read(&mut bytes)
            .map_err(|error| format!("could not read TCP connection: {error}"))?;
        bytes.truncate(read);
        Ok(bytes)
    }

    pub(super) fn write(&self, connection: i64, text: &str) -> Result<(), String> {
        self.write_bytes(connection, text.as_bytes())
    }

    pub(super) fn write_bytes(&self, connection: i64, bytes: &[u8]) -> Result<(), String> {
        self.connection(connection)?
            .lock()
            .map_err(|_| "TCP connection lock was poisoned".to_owned())?
            .write_all(bytes)
            .map_err(|error| format!("could not write TCP connection: {error}"))
    }

    pub(super) fn set_timeout(&self, connection: i64, milliseconds: i64) -> Result<(), String> {
        let milliseconds =
            u64::try_from(milliseconds).map_err(|_| "TCP timeout cannot be negative".to_owned())?;
        let duration = Some(Duration::from_millis(milliseconds));
        let connection = self.connection(connection)?;
        let connection = connection
            .lock()
            .map_err(|_| "TCP connection lock was poisoned".to_owned())?;
        connection
            .set_read_timeout(duration)
            .and_then(|()| connection.set_write_timeout(duration))
            .map_err(|error| format!("could not set TCP timeout: {error}"))
    }

    pub(super) fn close_listener(&self, listener: i64) -> Result<(), String> {
        self.network()?
            .listeners
            .remove(&listener)
            .map(|_| ())
            .ok_or_else(|| "TCP listener is already closed or invalid".to_owned())
    }

    pub(super) fn close_connection(&self, connection: i64) -> Result<(), String> {
        self.network()?
            .connections
            .remove(&connection)
            .map(|_| ())
            .ok_or_else(|| "TCP connection is already closed or invalid".to_owned())
    }

    fn network(&self) -> Result<std::sync::MutexGuard<'_, NetworkHost>, String> {
        self.network
            .lock()
            .map_err(|_| "network host lock was poisoned".to_owned())
    }

    fn connection(&self, handle: i64) -> Result<Arc<Mutex<TcpStream>>, String> {
        self.network()?
            .connections
            .get(&handle)
            .cloned()
            .ok_or_else(|| "TCP connection is closed or invalid".to_owned())
    }
}

struct NetworkHost {
    next_handle: i64,
    listeners: HashMap<i64, Arc<TcpListener>>,
    connections: HashMap<i64, Arc<Mutex<TcpStream>>>,
}

impl Default for NetworkHost {
    fn default() -> Self {
        Self {
            next_handle: 1,
            listeners: HashMap::new(),
            connections: HashMap::new(),
        }
    }
}

impl NetworkHost {
    fn next_handle(&mut self) -> i64 {
        let handle = self.next_handle;
        self.next_handle += 1;
        handle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contexts_isolate_network_handles() {
        let first = HostContext::new("first");
        let second = HostContext::new("second");

        let first_listener = first.listen("127.0.0.1", 0).unwrap();
        let second_listener = second.listen("127.0.0.1", 0).unwrap();

        assert_eq!(first_listener, 1);
        assert_eq!(second_listener, 1);
        first.close_listener(first_listener).unwrap();
        second.close_listener(second_listener).unwrap();
    }

    #[test]
    fn relative_paths_resolve_from_the_context_directory() {
        let context = HostContext::new(PathBuf::from("runtime-root"));
        assert_eq!(
            context.resolve_path("resources/config.toml"),
            std::env::current_dir()
                .unwrap()
                .join("runtime-root/resources/config.toml")
        );
    }
}
