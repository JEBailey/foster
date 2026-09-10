# Shared host services

`HostContext` captures a working directory and monotonic clock and owns an isolated
TCP handle registry. Both Foster execution backends use this implementation.
Network locks are released before blocking accept operations; individual stream
operations lock only the selected connection. Dropping the context releases its
remaining resources. Host operations return errors rather than interpreting
Foster values; each backend adapts those results to its own representation.
