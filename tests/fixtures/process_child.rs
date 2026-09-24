// Portable external program used to test the subprocess boundary, not Foster's API.
use std::io::{Read, Write};
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args[0].as_str() {
        "arguments" => {
            let mut stdin = Vec::new();
            std::io::stdin().read_to_end(&mut stdin).unwrap();
            assert!(stdin.is_empty());
            assert_eq!(&args[1..], &["", "two words", "a\"b\\c", "λ\nvalue"]);
            assert!(std::path::Path::new("cwd-marker").exists());
            std::io::stdout().write_all(&[0, 255, 10]).unwrap();
            std::io::stderr().write_all(b"diagnostic").unwrap();
            std::process::exit(7);
        }
        "pipes" => {
            let writer = std::thread::spawn(|| std::io::stderr().write_all(&vec![b'e'; 131072]).unwrap());
            std::io::stdout().write_all(&vec![b'o'; 131072]).unwrap();
            writer.join().unwrap();
        }
        "sleep" => {
            std::fs::write(&args[1], "ready").unwrap();
            std::thread::sleep(std::time::Duration::from_secs(2));
            std::fs::write(&args[2], "survived").unwrap();
        }
        "ok" => (),
        _ => panic!("unknown mode"),
    }
}
