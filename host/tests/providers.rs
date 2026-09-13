use foster_host::{
    FileRequest, FileResponse, HostContext, HostProvider, NetworkRequest, NetworkResponse,
};
use std::sync::Arc;

struct Denied;
impl HostProvider for Denied {}

#[test]
fn omitted_capabilities_never_fall_back_to_the_os() {
    let host = HostContext::with_provider(".", Arc::new(Denied));
    assert!(host.files().read(host.resolve_path("Cargo.toml")).is_err());
    assert!(!host.files().exists(host.resolve_path("Cargo.toml")));
    assert!(host.connect("127.0.0.1", 80).is_err());
    assert!(host.wall_now().is_err());
    assert!(host.monotonic_nanoseconds().is_err());
    assert!(host.wait_readable(1, 0).is_err());
}

struct Malformed;
impl HostProvider for Malformed {
    fn filesystem(&self, _: FileRequest<'_>) -> std::io::Result<FileResponse> {
        Ok(FileResponse::Unit)
    }
    fn network(&self, _: NetworkRequest<'_>) -> Result<NetworkResponse, String> {
        Ok(NetworkResponse::Unit)
    }
    fn wall_now(&self) -> Result<(i64, i64), String> {
        Ok((42, -1))
    }
}

#[test]
fn malformed_provider_responses_are_errors() {
    let host = HostContext::with_provider(".", Arc::new(Malformed));
    assert!(host.files().read("x").is_err());
    assert!(host.connect("x", 80).is_err());
    assert!(host.wait_readable(1, 0).is_err());
    assert!(host.wall_now().is_err());
}
