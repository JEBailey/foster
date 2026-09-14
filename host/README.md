# Shared host services

`HostContext` captures a working directory and forwards requests to an installed
`HostProvider`. Both Foster execution backends use this boundary. The default
`SystemHost` supplies operating-system services, a monotonic clock origin, and a
TCP handle registry. Contexts sharing a provider also share its state and handles;
resources are released when closed or when the provider is finally dropped.

The system provider releases network registry locks before blocking accept
operations; individual stream operations lock only the selected connection.
Host operations return errors rather than interpreting Foster values; each backend
adapts those results to its own representation. See the
[host-provider guide](../docs/host-providers.md) for injection and readiness contracts.
