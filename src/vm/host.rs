use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

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

fn network() -> &'static Mutex<NetworkHost> {
    static NETWORK: OnceLock<Mutex<NetworkHost>> = OnceLock::new();
    NETWORK.get_or_init(|| Mutex::new(NetworkHost::default()))
}

fn next_handle(host: &mut NetworkHost) -> i64 {
    let handle = host.next_handle;
    host.next_handle += 1;
    handle
}

pub(super) fn listen(address: &str, port: i64) -> Result<i64, String> {
    let port = u16::try_from(port).map_err(|_| "port must be between 0 and 65535".to_owned())?;
    let listener = TcpListener::bind((address, port))
        .map_err(|error| format!("could not listen on {address}:{port}: {error}"))?;
    let mut host = network()
        .lock()
        .map_err(|_| "network host lock was poisoned".to_owned())?;
    let handle = next_handle(&mut host);
    host.listeners.insert(handle, Arc::new(listener));
    Ok(handle)
}

pub(super) fn accept(listener: i64) -> Result<i64, String> {
    let listener = network()
        .lock()
        .map_err(|_| "network host lock was poisoned".to_owned())?
        .listeners
        .get(&listener)
        .cloned()
        .ok_or_else(|| "TCP listener is closed or invalid".to_owned())?;
    let (connection, _) = listener
        .accept()
        .map_err(|error| format!("could not accept TCP connection: {error}"))?;
    let mut host = network()
        .lock()
        .map_err(|_| "network host lock was poisoned".to_owned())?;
    let handle = next_handle(&mut host);
    host.connections
        .insert(handle, Arc::new(Mutex::new(connection)));
    Ok(handle)
}

pub(super) fn connect(address: &str, port: i64) -> Result<i64, String> {
    let port = u16::try_from(port).map_err(|_| "port must be between 0 and 65535".to_owned())?;
    let connection = TcpStream::connect((address, port))
        .map_err(|error| format!("could not connect to {address}:{port}: {error}"))?;
    let mut host = network()
        .lock()
        .map_err(|_| "network host lock was poisoned".to_owned())?;
    let handle = next_handle(&mut host);
    host.connections
        .insert(handle, Arc::new(Mutex::new(connection)));
    Ok(handle)
}

pub(super) fn read(connection: i64, maximum: i64) -> Result<String, String> {
    String::from_utf8(read_bytes(connection, maximum)?)
        .map_err(|_| "TCP input is not valid UTF-8".to_owned())
}

pub(super) fn read_bytes(connection: i64, maximum: i64) -> Result<Vec<u8>, String> {
    let maximum = usize::try_from(maximum)
        .ok()
        .filter(|maximum| (1..=1024 * 1024).contains(maximum))
        .ok_or_else(|| "read maximum must be between 1 and 1048576".to_owned())?;
    let connection = connection_for(connection)?;
    let mut bytes = vec![0; maximum];
    let read = connection
        .lock()
        .map_err(|_| "TCP connection lock was poisoned".to_owned())?
        .read(&mut bytes)
        .map_err(|error| format!("could not read TCP connection: {error}"))?;
    bytes.truncate(read);
    Ok(bytes)
}

pub(super) fn write(connection: i64, text: &str) -> Result<(), String> {
    write_bytes(connection, text.as_bytes())
}

pub(super) fn write_bytes(connection: i64, bytes: &[u8]) -> Result<(), String> {
    connection_for(connection)?
        .lock()
        .map_err(|_| "TCP connection lock was poisoned".to_owned())?
        .write_all(bytes)
        .map_err(|error| format!("could not write TCP connection: {error}"))
}

pub(super) fn set_timeout(connection: i64, milliseconds: i64) -> Result<(), String> {
    let milliseconds =
        u64::try_from(milliseconds).map_err(|_| "TCP timeout cannot be negative".to_owned())?;
    let duration = Some(Duration::from_millis(milliseconds));
    let connection = connection_for(connection)?;
    let connection = connection
        .lock()
        .map_err(|_| "TCP connection lock was poisoned".to_owned())?;
    connection
        .set_read_timeout(duration)
        .and_then(|()| connection.set_write_timeout(duration))
        .map_err(|error| format!("could not set TCP timeout: {error}"))
}

pub(super) fn close_listener(listener: i64) -> Result<(), String> {
    network()
        .lock()
        .map_err(|_| "network host lock was poisoned".to_owned())?
        .listeners
        .remove(&listener)
        .map(|_| ())
        .ok_or_else(|| "TCP listener is already closed or invalid".to_owned())
}

pub(super) fn close_connection(connection: i64) -> Result<(), String> {
    network()
        .lock()
        .map_err(|_| "network host lock was poisoned".to_owned())?
        .connections
        .remove(&connection)
        .map(|_| ())
        .ok_or_else(|| "TCP connection is already closed or invalid".to_owned())
}

fn connection_for(handle: i64) -> Result<Arc<Mutex<TcpStream>>, String> {
    network()
        .lock()
        .map_err(|_| "network host lock was poisoned".to_owned())?
        .connections
        .get(&handle)
        .cloned()
        .ok_or_else(|| "TCP connection is closed or invalid".to_owned())
}
