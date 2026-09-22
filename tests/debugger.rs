use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

struct Client {
    child: Child,
    stream: BufReader<TcpStream>,
    events: Vec<Value>,
    sequence: usize,
    root: PathBuf,
    file: PathBuf,
}
impl Client {
    fn new(source: &str) -> Self {
        Self::start(source, None)
    }

    fn start(source: &str, helper: Option<&str>) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "debugger-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        let file = root.join("main.fos");
        fs::write(&file, source).unwrap();
        if let Some(helper) = helper {
            fs::write(root.join("helper.fos"), helper).unwrap();
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_foster"))
            .arg("debug")
            .arg(if helper.is_some() { &root } else { &file })
            .args([
                "--port",
                &listener.local_addr().unwrap().port().to_string(),
                "--token",
                "test-token",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("{error}"),
            }
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                panic!("debugger failed to connect")
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let mut stream = BufReader::new(socket);
        let mut token = String::new();
        stream.read_line(&mut token).unwrap();
        assert_eq!(token.trim(), "test-token");
        Self {
            child,
            stream,
            events: Vec::new(),
            sequence: 0,
            root,
            file,
        }
    }
    fn read(&mut self) -> Value {
        let mut length = 0;
        loop {
            let mut header = String::new();
            assert!(
                self.stream.read_line(&mut header).unwrap() > 0,
                "unexpected debugger EOF"
            );
            if header == "\r\n" {
                break;
            }
            if let Some(value) = header.strip_prefix("Content-Length:") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut bytes = vec![0; length];
        self.stream.read_exact(&mut bytes).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
    fn request_raw(&mut self, command: &str, args: Value) -> Value {
        self.sequence += 1;
        let seq = self.sequence;
        let body = serde_json::to_vec(
            &json!({"seq":seq,"type":"request","command":command,"arguments":args}),
        )
        .unwrap();
        write!(
            self.stream.get_mut(),
            "Content-Length: {}\r\n\r\n",
            body.len()
        )
        .unwrap();
        self.stream.get_mut().write_all(&body).unwrap();
        loop {
            let message = self.read();
            if message["type"] == "response" {
                assert_eq!(message["request_seq"], seq);
                return message;
            }
            self.events.push(message);
        }
    }
    fn request(&mut self, command: &str, args: Value) -> Value {
        let response = self.request_raw(command, args);
        assert_eq!(response["success"], true, "{response}");
        response["body"].clone()
    }
    fn event(&mut self, event: &str) -> Value {
        loop {
            if let Some(index) = self
                .events
                .iter()
                .position(|message| message["event"] == event)
            {
                return self.events.remove(index)["body"].clone();
            }
            let message = self.read();
            self.events.push(message);
        }
    }
    fn initialize(&mut self, stop: bool) {
        let capabilities = self.request(
            "initialize",
            json!({"linesStartAt1":true,"columnsStartAt1":true,"pathFormat":"path"}),
        );
        assert_eq!(capabilities["supportsConfigurationDoneRequest"], true);
        self.event("initialized");
        self.request("launch", json!({"stopOnEntry":stop}));
    }
    fn breakpoints(&mut self, lines: &[usize]) -> Value {
        self.request("setBreakpoints",json!({"source":{"path":self.file},"breakpoints":lines.iter().map(|line|json!({"line":line})).collect::<Vec<_>>()}))
    }
    fn frame(&mut self) -> Value {
        self.request("stackTrace", json!({"threadId":1}))["stackFrames"][0].clone()
    }
    fn step(&mut self, command: &str) -> Value {
        self.request(command, json!({"threadId":1}));
        self.event("stopped");
        self.frame()
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let target = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .canonicalize()
            .unwrap();
        if let Ok(root) = self.root.canonicalize() {
            assert_eq!(root.parent(), Some(target.as_path()));
            let _ = fs::remove_dir_all(root);
        }
    }
}

#[test]
fn source_breakpoints_stepping_locals_and_expired_handles() {
    let mut client = Client::new(
        "func double(value: Int) -> Int {\n    let result = value * 2\n    result\n}\n\nfunc main() -> Int {\n    let input = 21\n    let answer = double(input)\n    answer\n}\n",
    );
    client.initialize(true);
    assert_eq!(client.breakpoints(&[8])["breakpoints"][0]["line"], 8);
    client.request("configurationDone", json!({}));
    assert_eq!(client.event("stopped")["reason"], "entry");
    assert_eq!(client.frame()["line"], 7);
    let frame = client.step("continue");
    assert_eq!(frame["line"], 8);
    assert_eq!(
        client.request(
            "evaluate",
            json!({"frameId":frame["id"],"expression":"input"})
        )["result"],
        "21"
    );
    client.breakpoints(&[]);
    let frame = client.step("stepIn");
    assert_eq!(frame["line"], 2);
    let stack = client.request("stackTrace", json!({"threadId":1}));
    assert_eq!(stack["totalFrames"], 2);
    assert_eq!(
        client.request(
            "evaluate",
            json!({"frameId":frame["id"],"expression":"value"})
        )["result"],
        "21"
    );
    let old = frame["id"].clone();
    let frame = client.step("next");
    assert_eq!(frame["line"], 3);
    assert_eq!(
        client.request_raw("scopes", json!({"frameId":old}))["success"],
        false
    );
    assert_eq!(
        client.request(
            "evaluate",
            json!({"frameId":frame["id"],"expression":"result"})
        )["result"],
        "42"
    );
    assert_eq!(
        client.request(
            "evaluate",
            json!({"frameId":frame["id"],"expression":"value"})
        )["result"],
        "<unavailable>"
    );
    assert_eq!(
        client.request_raw("evaluate", json!({"expression":"result + 1"}))["success"],
        false
    );
    let frame = client.step("stepOut");
    assert!(frame["name"].as_str().unwrap().ends_with("::main"));
    let frame = client.step("next");
    assert_eq!(frame["line"], 9);
    assert_eq!(
        client.request(
            "evaluate",
            json!({"frameId":frame["id"],"expression":"answer"})
        )["result"],
        "42"
    );
    client.request("continue", json!({"threadId":1}));
    assert_eq!(client.event("exited")["exitCode"], 0);
    client.event("terminated");
    assert!(client.child.wait().unwrap().success());
    let mut output = String::new();
    client
        .child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    assert_eq!(output.trim(), "42");
}

#[test]
fn loop_breakpoints_update_and_disconnect_while_paused() {
    let mut client = Client::new(
        "func main() -> Int {\n    let count = 0\n    while count < 3 {\n        count = count + 1\n    }\n    count\n}\n",
    );
    client.initialize(false);
    assert_eq!(
        client.breakpoints(&[4, 1000])["breakpoints"][1]["verified"],
        false
    );
    client.request("configurationDone", json!({}));
    client.event("stopped");
    let frame = client.frame();
    assert_eq!(frame["line"], 4);
    assert_eq!(
        client.request(
            "evaluate",
            json!({"frameId":frame["id"],"expression":"count"})
        )["result"],
        "0"
    );
    let frame = client.step("continue");
    assert_eq!(frame["line"], 4);
    assert_eq!(
        client.request(
            "evaluate",
            json!({"frameId":frame["id"],"expression":"count"})
        )["result"],
        "1"
    );
    client.request("disconnect", json!({}));
    client.event("terminated");
}

#[test]
fn runtime_failure_reports_nonzero_exit_and_stderr() {
    let mut client = Client::new("func main() {\n    assert(false, \"debug failure\")\n}\n");
    client.initialize(false);
    client.request("configurationDone", json!({}));
    assert_eq!(client.event("exited")["exitCode"], 1);
    client.event("terminated");
    client.child.wait().unwrap();
    let mut stderr = String::new();
    client
        .child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(stderr.contains("debug failure"), "{stderr}");
}

#[test]
fn pattern_bindings_are_visible_inside_their_arm() {
    let mut client = Client::new(
        "enum Pick = Some(Int) | None\nfunc main() -> Int {\n    branch Pick.Some(42) {\n        Pick.Some(value) -> {\n            let result = value + 0\n            result\n        }\n        _ -> 0\n    }\n}\n",
    );
    client.initialize(false);
    client.breakpoints(&[5]);
    client.request("configurationDone", json!({}));
    client.event("stopped");
    let frame = client.frame();
    assert_eq!(frame["line"], 5);
    assert_eq!(
        client.request(
            "evaluate",
            json!({"frameId":frame["id"],"expression":"value"})
        )["result"],
        "42"
    );
    client.request("disconnect", json!({}));
    client.event("terminated");
}

#[test]
fn package_breakpoints_navigate_to_the_called_module() {
    let mut client = Client::start(
        "import helper\nfunc main() -> Int { helper::answer() }\n",
        Some("pub func answer() -> Int {\n    let value = 42\n    value\n}\n"),
    );
    client.initialize(false);
    let file = client.root.join("helper.fos");
    let result = client.request(
        "setBreakpoints",
        json!({"source":{"path":file},"breakpoints":[{"line":2}]}),
    );
    assert_eq!(result["breakpoints"][0]["verified"], true);
    client.request("configurationDone", json!({}));
    client.event("stopped");
    let frame = client.frame();
    assert_eq!(frame["line"], 2);
    assert!(
        frame["source"]["path"]
            .as_str()
            .unwrap()
            .ends_with("helper.fos")
    );
    assert_eq!(
        client.request("stackTrace", json!({"threadId":1}))["totalFrames"],
        2
    );
    client.request("disconnect", json!({}));
    client.event("terminated");
}

#[test]
fn pause_interrupts_a_running_loop_at_a_source_location() {
    let mut client = Client::new(
        "func main() -> Int {\n    let count = 0\n    while count < 100000000 {\n        count = count + 1\n    }\n    count\n}\n",
    );
    client.initialize(false);
    client.request("configurationDone", json!({}));
    client.request("pause", json!({"threadId":1}));
    assert_eq!(client.event("stopped")["reason"], "pause");
    client.request("disconnect", json!({}));
    client.event("terminated");
}
