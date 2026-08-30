use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

struct TestServer {
    child: Child,
    root: PathBuf,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn request(port: u16, head: &str, body: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    stream.set_write_timeout(Some(Duration::from_secs(20)))?;
    stream.write_all(head.as_bytes())?;
    for chunk in body.chunks(997) {
        stream.write_all(chunk)?;
    }
    stream.shutdown(Shutdown::Write)?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    Ok(response)
}

fn response_body(response: &[u8]) -> &[u8] {
    let marker = b"\r\n\r\n";
    let start = response
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("HTTP response did not contain a header terminator")
        + marker.len();
    &response[start..]
}

fn wait_until_ready(child: &mut Child, port: u16) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(
            child.try_wait().unwrap().is_none(),
            "artifact service exited during startup"
        );
        if let Ok(response) = request(
            port,
            "GET /health HTTP/1.1\r\nConnection: close\r\n\r\n",
            &[],
        ) && response.starts_with(b"HTTP/1.1 200 OK")
        {
            return;
        }
        assert!(Instant::now() < deadline, "artifact service did not start");
        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn remote_artifact_service_streams_binary_uploads_and_downloads() {
    let port = TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let root = std::env::temp_dir().join(format!(
        "foster-artifact-service-{}-{port}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_foster"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "run",
            "service",
            "--",
            "127.0.0.1",
            &port.to_string(),
            root.to_str().unwrap(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until_ready(&mut child, port);
    let lease_marker = root.join("leased.bin.upload");
    let _server = TestServer { child, root };

    let payload = (0..200_000)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let upload_head = format!(
        "PUT /artifacts/binary-test.bin HTTP/1.1\r\ncontent-length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    let uploaded = request(port, &upload_head, &payload).unwrap();
    assert!(uploaded.starts_with(b"HTTP/1.1 201 Created"));

    let downloaded = request(
        port,
        "GET /artifacts/binary-test.bin HTTP/1.1\r\nConnection: close\r\n\r\n",
        &[],
    )
    .unwrap();
    assert!(downloaded.starts_with(b"HTTP/1.1 200 OK"));
    assert_eq!(response_body(&downloaded), payload);

    let listed = request(
        port,
        "GET /artifacts HTTP/1.1\r\nConnection: close\r\n\r\n",
        &[],
    )
    .unwrap();
    assert_eq!(response_body(&listed), b"binary-test.bin");

    let conflict = request(port, &upload_head, &payload).unwrap();
    assert!(conflict.starts_with(b"HTTP/1.1 409 Conflict"));

    let leased_payload = vec![0xa5; 4096];
    let leased_head = format!(
        "PUT /artifacts/leased.bin HTTP/1.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        leased_payload.len()
    );
    let mut first = TcpStream::connect(("127.0.0.1", port)).unwrap();
    first
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    first.write_all(leased_head.as_bytes()).unwrap();
    first.write_all(&leased_payload[..17]).unwrap();
    let lease_deadline = Instant::now() + Duration::from_secs(5);
    while !lease_marker.exists() {
        assert!(
            Instant::now() < lease_deadline,
            "upload lease was not created"
        );
        thread::sleep(Duration::from_millis(10));
    }
    thread::sleep(Duration::from_millis(50));

    let concurrent = request(port, &leased_head, &[]).unwrap();
    assert!(concurrent.starts_with(b"HTTP/1.1 409 Conflict"));

    first.write_all(&leased_payload[17..]).unwrap();
    first.shutdown(Shutdown::Write).unwrap();
    let mut first_response = Vec::new();
    first.read_to_end(&mut first_response).unwrap();
    assert!(first_response.starts_with(b"HTTP/1.1 201 Created"));

    let leased_download = request(
        port,
        "GET /artifacts/leased.bin HTTP/1.1\r\nConnection: close\r\n\r\n",
        &[],
    )
    .unwrap();
    assert_eq!(response_body(&leased_download), leased_payload);
}
