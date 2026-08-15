# HTTP and file servers

> Historical design note. The root-level `file_server*.foster` and
> `http_server_lib.foster` files are the authoritative capability-neutral ports.

The Pima file and HTTP server examples exercise capabilities, byte streams, errors, concurrency,
actors/remote blocks, maps, records, and filesystem security. Their Foster entry point should make
capabilities and effects explicit:

```foster
type Request {
    method: Method
    target: String
    headers: Map[String, String]
    body: Bytes
}

type Response {
    status: Int
    reason: String
    headers: Map[String, String]
    body: Bytes
}

func create_file_server(fs: ref FileSystem, root: Path) -> Handler throws IoError {
    // Canonicalize root once and reject targets that escape it.
}

func listen(tcp: ref Tcp, address: IpAddress, port: Int, handler: Handler)
    -> never
    throws NetworkError
{
    // ...
}
```

The port should wait for records, maps, byte strings, typed errors, interfaces/traits, filesystem
capabilities, and the concurrency model. Preserving the path-traversal tests is mandatory.

