# Futures and subprocesses

`Future<T>` is the composable contract in `core.future`. Its public `resolve()`
method consumes the receiver, may suspend, and returns `T`. `await value` calls
that method. A custom future can compose other contracts and stored fields;
generic code can accept `Future<T>` without knowing its implementation.

Remote requests implement this contract as `RemoteFuture<Result<T, RemoteError>>`.
Their existing remote-owner lifetime obligations still apply. Process handles
implement `Future<Result<ProcessOutput, ProcessError>>` and own their OS child.

This complete program runs the executable and arguments supplied to it:

```foster
import std.process
import std.process.Arguments
import std.process.ProcessError
import std.process.ExitStatus
import std.process.ProcessStatus
import std.process.ProcessOutput
import std.process.SpawnOptions
import std.process.Process
import static std.process.*
import core.result
import core.result.Result

import core.option


import core.string
import core.string.String


func main(arguments: Arguments) -> Result<Int, ProcessError> {
    return Result.Error(ProcessError { message: "pass an executable and its arguments" }) if arguments.values.empty?
    let child = try process::spawn(arguments.values[0], arguments.values.rest)
    println("Started", child.pid())
    let output = try await child
    println(String.from_utf8(move output.stdout).unwrap_or("[binary stdout]"))
    println(String.from_utf8(move output.stderr).unwrap_or("[binary stderr]"))
    Result.Ok(output.status.code.unwrap_or(-1))
}
```

`spawn` launches immediately. Pass the executable and individual arguments;
quotes, whitespace, empty arguments, and Unicode are preserved. A shell is used
only if explicitly launched as the executable. The environment is inherited and
stdin is closed. Windows children launch without a new console window.

`spawn_with` accepts `SpawnOptions { directory, output_limit }`. An empty directory
inherits the current directory. The default capture limit is 16 MiB **per stream**;
valid limits are 1 byte through 64 MiB. Both streams drain concurrently. Bytes
beyond the limit are discarded while draining, then await reports `ProcessError`.
An output limit does not terminate the child or impose a timeout.

`poll()` returns `Result<ProcessStatus, ProcessError>` with `Running` or
`Exited(ExitStatus)`. `terminate()` requests termination and leaves the handle
available for awaiting its final output. A nonzero exit code is a successful
collection with `status.success == false`, not a `ProcessError`. `status.code` is
`None` when the OS does not supply an exit code, such as signal termination on Unix.

Await consumes the process handle, waits for exit and both output streams, and
reaps the child. Dropping an unawaited handle kills and reaps the **direct child**,
including during unwinding. Cleanup does not manage descendants. Descendants
that inherit stdout/stderr can delay await until they close those pipes; dropping
the handle does not wait for their pipe readers. There is no timeout, environment
override, interactive stdin, or streaming-output API in this implementation.

The API, argument encoding, result decoding, and completion loop live in
`library/std/process.fos`. The shared Rust runtime supplies OS spawning, polling,
termination, concurrent pipe drainage, and reaping for both executable backends.
