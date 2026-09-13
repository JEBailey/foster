use foster_host::{
    FileRequest, FileResponse, HostContext, HostProvider, NetworkRequest, NetworkResponse,
};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct DeterministicHost {
    calls: Mutex<Vec<String>>,
}
impl HostProvider for DeterministicHost {
    fn filesystem(&self, request: FileRequest<'_>) -> std::io::Result<FileResponse> {
        match request {
            FileRequest::Read(path) => {
                assert!(path.is_absolute());
                self.calls.lock().unwrap().push("read".into());
                Ok(FileResponse::Bytes(b"virtual".to_vec()))
            }
            _ => Err(std::io::Error::other("denied")),
        }
    }
    fn wall_now(&self) -> Result<(i64, i64), String> {
        Ok((123, 456))
    }
    fn monotonic_nanoseconds(&self) -> Result<i64, String> {
        Ok(789)
    }
    fn network(&self, request: NetworkRequest<'_>) -> Result<NetworkResponse, String> {
        match request {
            NetworkRequest::Connect(_, _) => Ok(NetworkResponse::Handle(1)),
            NetworkRequest::WaitReadable(1, 0) => Ok(NetworkResponse::Ready(false)),
            NetworkRequest::WaitWritable(1, 0) => Ok(NetworkResponse::Ready(true)),
            NetworkRequest::CloseConnection(1) => {
                self.calls.lock().unwrap().push("close".into());
                Ok(NetworkResponse::Unit)
            }
            _ => Err("denied".into()),
        }
    }
}

#[test]
fn vm_uses_installed_provider_for_files_clocks_network_and_drop() {
    let compilation = foster::compile(
        r#"
import core.result
import std.host
import std.fs
import std.io
import std.time
import std.net.tcp

func readiness(socket: Connection) -> Int [consume socket] {
    branch socket.wait_writable(0) {
        Result.Ok(true) -> 1
        _ -> 0
    }
}
func main() -> Int {
    let file = File.from("virtual.txt")
    let text = branch file.read_text() { Result.Ok(value) -> value
        Result.Error(_) -> "failed" }
    let socket = RuntimeHost.new().connect("virtual", 80)
    let ready = branch move socket { Result.Ok(value) -> readiness(move value)
        Result.Error(_) -> 0 }
    let clock = SystemClock.new().now()
    assert(ContinuousClock.new().now().ticks() == 789, "provider monotonic clock")
    clock.epoch_seconds() + clock.nanosecond() + text.length + ready
}

"#,
    )
    .unwrap();
    for optimize in [false, true] {
        let provider = Arc::new(DeterministicHost::default());
        let program =
            foster::vm::compile_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap();
        foster::vm::verify(&program).unwrap();
        let machine = foster::vm::Machine::with_host_context(
            &program,
            HostContext::with_provider(".", provider.clone()),
        );
        assert_eq!(machine.run_main().unwrap(), foster::vm::Value::Integer(587));
        assert_eq!(*provider.calls.lock().unwrap(), ["read", "close"]);
    }
}

#[test]
fn application_providers_can_be_implemented_in_foster() {
    let compilation = foster::compile(r#"
import core.bytes
import core.result
import std.host
import std.io
import std.net.tcp
import std.path as paths
type Memory = & FileProvider<Bytes> & NetworkProvider<Int, Int> & {}
impl Memory {
    func file(self, location: paths::Path) -> Result<Bytes, IoError> [read self, consume location] {
        Result.Ok(location.as_string().bytes)
    }
    func connect(self, address: String, port: Int) -> Result<Int, NetworkError> [read self, consume address] {
        Result.Ok(port)
    }
    func listen(self, address: String, port: Int) -> Result<Int, NetworkError> [read self, consume address] {
        Result.Ok(port + 1)
    }
}
func length(provider: FileProvider<Bytes>) -> Int {
    branch provider.file(paths::Path.from("memory")) {
        Result.Ok(contents) -> contents.length
        Result.Error(_) -> -1
    }
}
func port(provider: NetworkProvider<Int, Int>) -> Int {
    provider.connect("memory", 35).unwrap_or(0) + provider.listen("memory", 0).unwrap_or(0)
}
func main() -> Int {
    let provider = Memory {}
    length(provider) + port(provider)
}
"#).unwrap();
    assert_eq!(
        foster::vm::run(&compilation).unwrap(),
        foster::vm::Value::Integer(42)
    );
}
