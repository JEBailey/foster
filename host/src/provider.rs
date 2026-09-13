use super::*;
use std::io;

/// Runtime callers supply context-resolved paths. A provider owns its namespace and policy.
/// Read/ReadRange return Bytes, Write/Append/CreateDirectory/Remove/Rename return Unit,
/// Metadata returns Metadata, List returns Names, Copy returns Count, and Canonicalize
/// returns Path. Writes and appends must accept the whole input or return an error.
pub enum FileRequest<'a> {
    Read(&'a Path),
    Write(&'a Path, &'a [u8]),
    ReadRange(&'a Path, u64, usize),
    Append(&'a Path, &'a [u8]),
    Metadata(&'a Path),
    List(&'a Path),
    CreateDirectory(&'a Path, bool),
    Remove(&'a Path, bool),
    Rename(&'a Path, &'a Path),
    Copy(&'a Path, &'a Path),
    Canonicalize(&'a Path),
}

pub enum FileResponse {
    Bytes(Vec<u8>),
    Unit,
    Metadata(FileMetadata),
    Names(Vec<String>),
    Count(u64),
    Path(PathBuf),
}

#[derive(Clone, Copy, Debug)]
pub struct FileMetadata {
    pub length: u64,
    pub file: bool,
    pub directory: bool,
}
impl FileMetadata {
    pub fn len(&self) -> u64 {
        self.length
    }
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
    pub fn is_file(&self) -> bool {
        self.file
    }
    pub fn is_dir(&self) -> bool {
        self.directory
    }
}

pub enum NetworkRequest<'a> {
    Listen(&'a str, i64),
    Connect(&'a str, i64),
    Accept(i64),
    Read(i64, i64),
    Write(i64, &'a [u8]),
    SetTimeout(i64, i64),
    CloseListener(i64),
    CloseConnection(i64),
    WaitReadable(i64, i64),
    WaitWritable(i64, i64),
    WaitAccept(i64, i64),
}
/// Listen/Connect/Accept return a positive, nonzero Handle; Read returns Bytes (empty
/// at EOF). Write must transfer all input and returns Unit, as do timeout and close.
/// Wait operations return Ready(false) only on timeout. Handles must not be recycled
/// while an outstanding operation could still refer to them.
pub enum NetworkResponse {
    Handle(i64),
    Bytes(Vec<u8>),
    Unit,
    Ready(bool),
}

/// Shared provider boundary for VM machines and native programs. Unimplemented
/// capabilities fail closed; implementations may delegate selectively to SystemHost.
pub trait HostProvider: Send + Sync {
    fn filesystem(&self, _request: FileRequest<'_>) -> io::Result<FileResponse> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "filesystem capability is unavailable",
        ))
    }
    fn network(&self, _request: NetworkRequest<'_>) -> Result<NetworkResponse, String> {
        Err("network capability is unavailable".into())
    }
    /// Normalized Unix seconds and nanoseconds in 0..1_000_000_000.
    fn wall_now(&self) -> Result<(i64, i64), String> {
        Err("wall clock capability is unavailable".into())
    }
    fn monotonic_nanoseconds(&self) -> Result<i64, String> {
        Err("monotonic clock capability is unavailable".into())
    }
}

/// An invocation's provider and working directory. Remote objects share this context.
pub struct HostContext {
    directory: PathBuf,
    provider: Arc<dyn HostProvider>,
}
impl HostContext {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        let host = SystemHost::new(directory);
        Self {
            directory: host.working_directory().to_path_buf(),
            provider: Arc::new(host),
        }
    }
    pub fn current() -> io::Result<Self> {
        std::env::current_dir().map(Self::new)
    }
    pub fn with_provider(directory: impl Into<PathBuf>, provider: Arc<dyn HostProvider>) -> Self {
        let directory = directory.into();
        let directory = if directory.is_absolute() {
            directory
        } else {
            std::env::current_dir().unwrap_or_default().join(directory)
        };
        Self {
            directory,
            provider,
        }
    }
    pub fn working_directory(&self) -> &Path {
        &self.directory
    }
    pub fn resolve_path(&self, path: &str) -> PathBuf {
        self.directory.join(path)
    }
    pub fn provider(&self) -> &dyn HostProvider {
        self.provider.as_ref()
    }
    pub fn files(&self) -> FileHost<'_> {
        FileHost(self.provider.as_ref())
    }
    pub fn wall_now(&self) -> Result<(i64, i64), String> {
        let value = self.provider.wall_now()?;
        if !(0..1_000_000_000).contains(&value.1) {
            return Err("provider returned invalid wall clock nanoseconds".into());
        }
        Ok(value)
    }
    pub fn monotonic_nanoseconds(&self) -> Result<i64, String> {
        self.provider.monotonic_nanoseconds()
    }
    pub fn listen(&self, address: &str, port: i64) -> Result<i64, String> {
        match self
            .provider
            .network(NetworkRequest::Listen(address, port))?
        {
            NetworkResponse::Handle(value) if value > 0 => Ok(value),
            _ => Err(invalid_network()),
        }
    }
    pub fn connect(&self, address: &str, port: i64) -> Result<i64, String> {
        match self
            .provider
            .network(NetworkRequest::Connect(address, port))?
        {
            NetworkResponse::Handle(value) if value > 0 => Ok(value),
            _ => Err(invalid_network()),
        }
    }
    pub fn accept(&self, handle: i64) -> Result<i64, String> {
        match self.provider.network(NetworkRequest::Accept(handle))? {
            NetworkResponse::Handle(value) if value > 0 => Ok(value),
            _ => Err(invalid_network()),
        }
    }
    pub fn read_bytes(&self, handle: i64, maximum: i64) -> Result<Vec<u8>, String> {
        match self
            .provider
            .network(NetworkRequest::Read(handle, maximum))?
        {
            NetworkResponse::Bytes(value) => Ok(value),
            _ => Err(invalid_network()),
        }
    }
    pub fn read(&self, handle: i64, maximum: i64) -> Result<String, String> {
        String::from_utf8(self.read_bytes(handle, maximum)?)
            .map_err(|_| "TCP input is not valid UTF-8".into())
    }
    pub fn write_bytes(&self, handle: i64, bytes: &[u8]) -> Result<(), String> {
        self.unit(NetworkRequest::Write(handle, bytes))
    }
    pub fn write(&self, handle: i64, text: &str) -> Result<(), String> {
        self.write_bytes(handle, text.as_bytes())
    }
    pub fn set_timeout(&self, handle: i64, milliseconds: i64) -> Result<(), String> {
        self.unit(NetworkRequest::SetTimeout(handle, milliseconds))
    }
    pub fn close_listener(&self, handle: i64) -> Result<(), String> {
        self.unit(NetworkRequest::CloseListener(handle))
    }
    pub fn close_connection(&self, handle: i64) -> Result<(), String> {
        self.unit(NetworkRequest::CloseConnection(handle))
    }
    pub fn wait_readable(&self, handle: i64, milliseconds: i64) -> Result<bool, String> {
        self.ready(NetworkRequest::WaitReadable(handle, milliseconds))
    }
    pub fn wait_writable(&self, handle: i64, milliseconds: i64) -> Result<bool, String> {
        self.ready(NetworkRequest::WaitWritable(handle, milliseconds))
    }
    pub fn wait_accept(&self, handle: i64, milliseconds: i64) -> Result<bool, String> {
        self.ready(NetworkRequest::WaitAccept(handle, milliseconds))
    }
    fn unit(&self, request: NetworkRequest<'_>) -> Result<(), String> {
        match self.provider.network(request)? {
            NetworkResponse::Unit => Ok(()),
            _ => Err(invalid_network()),
        }
    }
    fn ready(&self, request: NetworkRequest<'_>) -> Result<bool, String> {
        match self.provider.network(request)? {
            NetworkResponse::Ready(value) => Ok(value),
            _ => Err(invalid_network()),
        }
    }
}
fn invalid_network() -> String {
    "provider returned an invalid network response".into()
}
fn invalid_file() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "provider returned an invalid filesystem response",
    )
}

pub struct FileHost<'a>(&'a dyn HostProvider);
impl FileHost<'_> {
    pub fn read(&self, path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
        match self.0.filesystem(FileRequest::Read(path.as_ref()))? {
            FileResponse::Bytes(bytes) => Ok(bytes),
            _ => Err(invalid_file()),
        }
    }
    pub fn read_to_string(&self, path: impl AsRef<Path>) -> io::Result<String> {
        String::from_utf8(self.read(path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }
    pub fn read_range(
        &self,
        path: impl AsRef<Path>,
        offset: u64,
        maximum: usize,
    ) -> io::Result<Vec<u8>> {
        match self
            .0
            .filesystem(FileRequest::ReadRange(path.as_ref(), offset, maximum))?
        {
            FileResponse::Bytes(bytes) => Ok(bytes),
            _ => Err(invalid_file()),
        }
    }
    pub fn write(&self, path: impl AsRef<Path>, bytes: &[u8]) -> io::Result<()> {
        self.unit(FileRequest::Write(path.as_ref(), bytes))
    }
    pub fn append(&self, path: impl AsRef<Path>, bytes: &[u8]) -> io::Result<()> {
        self.unit(FileRequest::Append(path.as_ref(), bytes))
    }
    pub fn metadata(&self, path: impl AsRef<Path>) -> io::Result<FileMetadata> {
        match self.0.filesystem(FileRequest::Metadata(path.as_ref()))? {
            FileResponse::Metadata(value) => Ok(value),
            _ => Err(invalid_file()),
        }
    }
    pub fn list(&self, path: impl AsRef<Path>) -> io::Result<Vec<String>> {
        match self.0.filesystem(FileRequest::List(path.as_ref()))? {
            FileResponse::Names(mut names) => {
                names.sort();
                Ok(names)
            }
            _ => Err(invalid_file()),
        }
    }
    pub fn exists(&self, path: impl AsRef<Path>) -> bool {
        self.metadata(path).is_ok()
    }
    pub fn is_file(&self, path: impl AsRef<Path>) -> bool {
        self.metadata(path).is_ok_and(|value| value.file)
    }
    pub fn is_dir(&self, path: impl AsRef<Path>) -> bool {
        self.metadata(path).is_ok_and(|value| value.directory)
    }
    pub fn create_dir(&self, path: impl AsRef<Path>) -> io::Result<()> {
        self.unit(FileRequest::CreateDirectory(path.as_ref(), false))
    }
    pub fn create_dir_all(&self, path: impl AsRef<Path>) -> io::Result<()> {
        self.unit(FileRequest::CreateDirectory(path.as_ref(), true))
    }
    pub fn remove_file(&self, path: impl AsRef<Path>) -> io::Result<()> {
        self.unit(FileRequest::Remove(path.as_ref(), false))
    }
    pub fn remove_dir(&self, path: impl AsRef<Path>) -> io::Result<()> {
        self.unit(FileRequest::Remove(path.as_ref(), true))
    }
    pub fn rename(&self, from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
        self.unit(FileRequest::Rename(from.as_ref(), to.as_ref()))
    }
    pub fn copy(&self, from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<u64> {
        match self
            .0
            .filesystem(FileRequest::Copy(from.as_ref(), to.as_ref()))?
        {
            FileResponse::Count(count) => Ok(count),
            _ => Err(invalid_file()),
        }
    }
    pub fn canonicalize(&self, path: impl AsRef<Path>) -> io::Result<PathBuf> {
        match self
            .0
            .filesystem(FileRequest::Canonicalize(path.as_ref()))?
        {
            FileResponse::Path(path) => Ok(path),
            _ => Err(invalid_file()),
        }
    }
    fn unit(&self, request: FileRequest<'_>) -> io::Result<()> {
        match self.0.filesystem(request)? {
            FileResponse::Unit => Ok(()),
            _ => Err(invalid_file()),
        }
    }
}

impl HostProvider for SystemHost {
    fn filesystem(&self, request: FileRequest<'_>) -> io::Result<FileResponse> {
        use std::io::{Seek, SeekFrom};
        match request {
            FileRequest::Read(path) => std::fs::read(path).map(FileResponse::Bytes),
            FileRequest::Write(path, bytes) => {
                std::fs::write(path, bytes).map(|()| FileResponse::Unit)
            }
            FileRequest::Append(path, bytes) => std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut file| file.write_all(bytes))
                .map(|()| FileResponse::Unit),
            FileRequest::ReadRange(path, offset, maximum) => {
                let mut file = std::fs::File::open(path)?;
                file.seek(SeekFrom::Start(offset))?;
                let mut bytes = vec![0; maximum];
                let read = file.read(&mut bytes)?;
                bytes.truncate(read);
                Ok(FileResponse::Bytes(bytes))
            }
            FileRequest::Metadata(path) => std::fs::metadata(path).map(|m| {
                FileResponse::Metadata(FileMetadata {
                    length: m.len(),
                    file: m.is_file(),
                    directory: m.is_dir(),
                })
            }),
            FileRequest::List(path) => std::fs::read_dir(path)?
                .map(|entry| {
                    entry?.file_name().into_string().map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "directory entry name is not valid UTF-8",
                        )
                    })
                })
                .collect::<io::Result<Vec<_>>>()
                .map(FileResponse::Names),
            FileRequest::CreateDirectory(path, recursive) => if recursive {
                std::fs::create_dir_all(path)
            } else {
                std::fs::create_dir(path)
            }
            .map(|()| FileResponse::Unit),
            FileRequest::Remove(path, directory) => if directory {
                std::fs::remove_dir(path)
            } else {
                std::fs::remove_file(path)
            }
            .map(|()| FileResponse::Unit),
            FileRequest::Rename(from, to) => std::fs::rename(from, to).map(|()| FileResponse::Unit),
            FileRequest::Copy(from, to) => std::fs::copy(from, to).map(FileResponse::Count),
            FileRequest::Canonicalize(path) => std::fs::canonicalize(path).map(FileResponse::Path),
        }
    }
    fn network(&self, request: NetworkRequest<'_>) -> Result<NetworkResponse, String> {
        match request {
            NetworkRequest::Listen(address, port) => {
                self.listen(address, port).map(NetworkResponse::Handle)
            }
            NetworkRequest::Connect(address, port) => {
                self.connect(address, port).map(NetworkResponse::Handle)
            }
            NetworkRequest::Accept(handle) => self.accept(handle).map(NetworkResponse::Handle),
            NetworkRequest::Read(handle, maximum) => {
                self.read_bytes(handle, maximum).map(NetworkResponse::Bytes)
            }
            NetworkRequest::Write(handle, bytes) => self
                .write_bytes(handle, bytes)
                .map(|()| NetworkResponse::Unit),
            NetworkRequest::SetTimeout(handle, timeout) => self
                .set_timeout(handle, timeout)
                .map(|()| NetworkResponse::Unit),
            NetworkRequest::CloseListener(handle) => {
                self.close_listener(handle).map(|()| NetworkResponse::Unit)
            }
            NetworkRequest::CloseConnection(handle) => self
                .close_connection(handle)
                .map(|()| NetworkResponse::Unit),
            NetworkRequest::WaitReadable(handle, timeout) => self
                .wait_socket(handle, timeout, false, false)
                .map(NetworkResponse::Ready),
            NetworkRequest::WaitWritable(handle, timeout) => self
                .wait_socket(handle, timeout, false, true)
                .map(NetworkResponse::Ready),
            NetworkRequest::WaitAccept(handle, timeout) => self
                .wait_socket(handle, timeout, true, false)
                .map(NetworkResponse::Ready),
        }
    }
    fn monotonic_nanoseconds(&self) -> Result<i64, String> {
        SystemHost::monotonic_nanoseconds(self)
    }
    fn wall_now(&self) -> Result<(i64, i64), String> {
        use std::time::{SystemTime, UNIX_EPOCH};
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(value) => Ok((
                i64::try_from(value.as_secs()).map_err(|_| "wall clock seconds exceed Int")?,
                i64::from(value.subsec_nanos()),
            )),
            Err(error) => {
                let value = error.duration();
                let seconds =
                    i64::try_from(value.as_secs()).map_err(|_| "wall clock seconds exceed Int")?;
                let nanos = i64::from(value.subsec_nanos());
                Ok(if nanos == 0 {
                    (-seconds, 0)
                } else {
                    (-seconds - 1, 1_000_000_000 - nanos)
                })
            }
        }
    }
}
