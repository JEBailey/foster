use foster_host::{HostContext, HostProvider, NetworkRequest, NetworkResponse, SystemHost};
use std::io::Write;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn readiness_reports_timeout_data_write_eof_and_closed_handles() {
    let server = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let host = HostContext::new(".");
    let handle = host
        .connect("127.0.0.1", i64::from(server.local_addr().unwrap().port()))
        .unwrap();
    let (mut peer, _) = server.accept().unwrap();
    assert!(!host.wait_readable(handle, 0).unwrap());
    assert!(!host.wait_readable(handle, 10).unwrap());
    assert!(host.wait_writable(handle, 1000).unwrap());
    assert!(host.wait_readable(handle, -1).is_err());
    peer.write_all(b"ready").unwrap();
    assert!(host.wait_readable(handle, 1000).unwrap());
    assert_eq!(host.read_bytes(handle, 5).unwrap(), b"ready");
    drop(peer);
    assert!(host.wait_readable(handle, 1000).unwrap());
    assert!(host.read_bytes(handle, 5).unwrap().is_empty());
    host.close_connection(handle).unwrap();
    assert!(host.wait_readable(handle, 0).is_err());
    assert!(host.wait_writable(handle, 0).is_err());
}

#[test]
fn close_interrupts_a_long_readiness_wait() {
    let server = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let host = Arc::new(HostContext::new("."));
    let handle = host
        .connect("127.0.0.1", i64::from(server.local_addr().unwrap().port()))
        .unwrap();
    let (_peer, _) = server.accept().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker_host = host.clone();
    let worker =
        std::thread::spawn(move || tx.send(worker_host.wait_readable(handle, 30_000)).unwrap());
    std::thread::sleep(Duration::from_millis(30));
    host.close_connection(handle).unwrap();
    assert!(rx.recv_timeout(Duration::from_secs(2)).unwrap().is_err());
    worker.join().unwrap();
}

#[test]
fn listeners_support_timed_accept_readiness() {
    // Get the actual ephemeral port from SystemHost's unit test below; here also
    // verify the provider dispatch and closed-listener errors without port races.
    let host = SystemHost::new(".");
    let handle = host.listen("127.0.0.1", 0).unwrap();
    assert!(matches!(
        host.network(NetworkRequest::WaitAccept(handle, 0)),
        Ok(NetworkResponse::Ready(false))
    ));
    host.close_listener(handle).unwrap();
    assert!(host.network(NetworkRequest::WaitAccept(handle, 0)).is_err());
}
