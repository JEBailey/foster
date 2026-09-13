use std::io::{Read, Write};

#[test]
fn socket_readiness_matches_in_vm_and_native_with_optimization() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let source =
        include_str!("fixtures/programs/socket_readiness.fos").replace("PORT", &port.to_string());
    let compilation = foster::compile(&source).unwrap();
    let server = std::thread::spawn(move || {
        for _ in 0..8 {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = [0; 1];
            peer.read_exact(&mut request).unwrap();
            assert_eq!(&request, b"x");
            peer.write_all(b"y").unwrap();
        }
    });
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap(),
            foster::vm::Value::Integer(42)
        );
        let path = std::env::temp_dir().join(format!(
            "foster-readiness-{}-{optimize}{}",
            std::process::id(),
            std::env::consts::EXE_SUFFIX
        ));
        foster::native::build_executable(
            &compilation,
            &path,
            foster::native::CompileOptions { optimize },
        )
        .unwrap();
        let output = std::process::Command::new(&path).output().unwrap();
        let _ = std::fs::remove_file(path);
        assert!(
            output.status.success(),
            "optimize={optimize}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
    }
    server.join().unwrap();
}
