# Host providers and socket readiness

Foster applications can select implementations explicitly through `std.host.FileProvider<F>`,
`std.host.NetworkProvider<C, L>`, and the existing `std.time.Clock<T>`. Resource types remain
independent: an in-memory network provider may return its own stream and listener types implementing
`Reader`, `Writer`, `Accepting`, or readiness contracts. `RuntimeHost.new()` supplies the standard
`File`, `Connection`, and `Listener` adapters. Constructing a file resource does not perform I/O.

At the embedding boundary, `foster_host::HostProvider` supplies filesystem, TCP, wall-clock, and
monotonic-clock operations. Every existing filesystem and TCP intrinsic in both execution modes
uses this boundary, including metadata, directory mutations, canonicalization, readiness, and close.
`SystemHost` implements the default operating-system services. Unimplemented provider methods return
errors instead of falling back to the OS. Existing path predicates still return false on host errors;
fallible file operations retain `IoError`. The existing infallible wall-clock and monotonic
intrinsics report provider errors as language execution failures.

VM embedders create a `HostContext::with_provider(directory, Arc::new(provider))` and pass it to
`Machine::with_host_context`. Remote objects inherit that context. Filesystem requests receive paths
resolved against its captured directory; absolute paths remain absolute. Providers enforce their own
namespace and access policy. Sharing one provider between contexts also shares its state and handles.

Native embedders call `foster_runtime_install_host(context)` before `foster_runtime_initialize` or
any host operation. Installation is process-wide and succeeds only once; a rejected installation
returns the supplied context to its caller. The generated CLI executable uses `SystemHost` by default.
A custom native entry shim is needed to install an embedding provider; there is no CLI plugin loader.
Providers are Rust implementations of the shared trait, while explicit application providers and
resource adapters can be written in Foster.

This boundary does not replace process arguments, standard output, or entropy services, and is not
a complete security sandbox. Rust provider implementations must be thread-safe, return the response
variant appropriate to each request, and release their resources when closed or dropped. Context
adapters reject mismatched response variants and invalid wall-clock nanoseconds. Automatic TCP Drop
and explicit close both dispatch to the installed provider; a successful explicit close suppresses
the later automatic close attempt through the existing handle ownership protocol.

## Timed readiness

`Listener.wait_readable(milliseconds)` waits for an incoming connection. Connections implement both
`wait_readable(milliseconds)` and `wait_writable(milliseconds)` through the `ReadReady<NetworkError>`
and `WriteReady<NetworkError>` contracts in `std.io`.

- `Result.Ok(true)` means ready. Read readiness includes EOF and pending socket errors; perform the
  I/O operation to discover its outcome.
- `Result.Ok(false)` means the timeout elapsed. Zero performs an immediate poll.
- `Result.Error(error)` means the wait failed, including a negative timeout or an invalid/closed handle.

Readiness is advisory. Another consumer can drain the socket before I/O starts; write readiness does
not guarantee that a complete write will fit. The existing blocking I/O and timeout APIs retain their
semantics. OS scheduling can extend the observed timeout.

The system provider uses `WSAPoll` on Windows and `poll` on Linux, Android, macOS, iOS, and BSD targets,
checking for concurrent close between
poll slices of at most 50 ms. Closing a handle makes a pending readiness wait return an error without
waiting for its full timeout. Other blocking operations such as accept/read retain their existing
interruption limits. Runtime actor host offloading keeps readiness waits off scheduler workers, but
currently uses an OS helper thread per pending host operation. This is not yet a shared event reactor
or a general task-cancellation facility. Other target families return an unsupported-operation error.

Platform references: [WSAPoll](https://learn.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-wsapoll)
and [poll](https://man7.org/linux/man-pages/man2/poll.2.html).
