# Remote ownership, requests, and failure

Status: **accepted semantic direction; implementation pending**, 2026-09-05.

This document encapsulates the remote lifecycle decisions accompanying
[S-19 and S-20 of the semantic specification](semantics.md#9-remote-execution).
It specifies required behavior, not a claim that the current compiler or either runtime already
enforces it. No runtime, syntax, or language-version change is made by documenting these decisions.

## Ownership and request lifetime

**R-01 — Scoped ownership.** An owning remote value controls the lifetime of its remote worker
and receiver state. Destruction of that owner, including scope exit, shuts the worker down.
Moving the owner transfers that responsibility; leaving the moved-from scope does not shut down
the worker. A borrowed view is not an additional owner.

**R-02 — Requests.** Calling a method through a remote handle immediately creates a future for
that individual request. A future is not ownership of the worker and cannot keep it alive.
Requests accepted by a live worker retain the existing per-worker FIFO ordering contract.

**R-03 — Completion.** A request is outstanding until it has a terminal outcome, whether or not
its future is retained or awaited. Dropping a future does not establish completion and does not
remove the request's lifetime obligation. A completed future may outlive the worker: its outcome
must be independently owned and must not borrow destroyed worker state.

## Worker state and failure containment

The lifecycle has the following externally relevant transitions:

| State/event | Required outcome |
| --- | --- |
| Live worker receives a call | Accept the request and return its future. |
| Method completes normally | Resolve that request once; the worker remains live. |
| Remote execution fails | Enter a terminal failed state; resolve the failing and other outstanding requests with remote failure errors. |
| Call reaches a failed worker | Return a future already resolved with a remote failure error; do not invoke the method. |
| Owner is destroyed | Shut down; cancel running and queued requests and resolve outstanding futures with shutdown errors. |
| Completion races with failure or shutdown | Publish exactly one terminal outcome per request. An outcome already published is not replaced. |

**R-04 — Containment.** A remote execution failure must be contained within the remote, rather
than terminating the application. Failure is sticky: later requests do not restart the receiver
or continue processing its possibly inconsistent state. Already completed requests keep their
outcomes. The failed worker remains unusable until its owner is destroyed; automatic restart is
not part of this contract.

Returning an ordinary domain error, such as a method's `Result.Error`, is a normal method return.
It does not by itself fail the worker. A remote execution failure and a method's recoverable
application error must remain distinguishable.

**R-05 — Cancellation, not draining.** Owner destruction does not wait for accepted work to
finish successfully. Running and queued requests are cancelled and their outstanding futures
resolve as errors; no accepted future may remain permanently pending because its owner disappeared.
Queued methods must not start after shutdown. Cancelled execution must not resume ordinary Foster
execution or mutate receiver state after that state has been destroyed.

This is not transaction rollback: externally visible effects already performed are not undone.
It also does not authorize unsafe termination of host threads. Safe cancellation points, waking
blocked operations, and retaining internal storage until executing host code can no longer access
it are runtime implementation obligations. Future error delivery and physical resource reclamation
need not be the same instant. A general bounded cancellation latency for arbitrary host calls is
not established by this decision.

## Typed outcomes

**R-06 — One outcome type.** Success and remote failure must be expressible through one static
result contract. The planned representation is a future whose awaited value is
`Result<T, RemoteError>`, where `T` is the method's declared result type. `RemoteError.Shutdown`
denotes owner-driven cancellation; remote execution failure needs a separate error category.
These names describe the intended API and are not declarations currently available in the library.

If a method already returns `Result<T, E>`, the outer remote outcome remains separate:
`Result<Result<T, E>, RemoteError>`. Remote execution failure must not be silently converted into
the method's domain error type. Ordinary Result handling, rather than an unrelated sometimes-returned
error object, determines how callers inspect the outcome.

The current `Future<T>`/`await` behavior must be migrated deliberately when this representation is
implemented. This document does not claim that existing examples already type-check with that API.

## Compile-time lifetime requirement

**R-07 — Owner outlives requests.** Reject a program when analysis establishes a reachable owner
destruction boundary with an outstanding request that has not been shown complete. Do not treat
elapsed time, a fast method body, or dropping its future as proof of completion. Awaiting the request
before that boundary is the ordinary completion witness, including an await that yields an error.

Track the obligation by owner and request identity, not just a local variable's spelling. Moves,
future storage, early returns, branch joins, and loop exits must preserve it. Awaiting on only one
reachable branch is insufficient for an unconditional owner exit. An owner transfer must transfer
the lifetime responsibility rather than prematurely discharge it.

Returning a pending future while destroying its local remote owner is invalid. Transferring both
the owner and its pending work to a longer-lived owner is a valid semantic direction only where
the language can express and preserve that ownership relationship; no new tuple or task syntax
is introduced here.

Suggested diagnostic:

```text
error: remote owner leaves scope while a request may still be pending

  request created here
  remote owner leaves scope here

help: await the request before leaving this scope, or transfer the remote owner
      to a longer-lived scope
```

This wording refers to request completion, not to “returning” a future: returning or discarding
the future does not finish the remote invocation. A diagnostic code has not yet been assigned.

**R-08 — Runtime backstop.** Static checking does not replace runtime shutdown. Exceptional exits
and cases beyond the analysis must still cancel safely and resolve futures as errors. The compiler
must document its supported completion proofs and remaining conservative or dynamic cases; the
existence of a runtime backstop is not justification for accepting a statically established violation.

## Borrowed remote views

**R-09 — Preserve loan boundaries.** The existing read-only rules for `remote ref value` and
borrow-mode remote arguments remain in force. Shutting down a read-view worker does not destroy
the separately owned object it observes. Pending invocations must release their read capabilities
on completion, failure, or safe cancellation. Publishing a cancellation result must not release
storage or synchronization still needed by executing code.

The view's worker lifetime and the origin object's loan lifetime are distinct obligations; neither
may be erased by retaining a future or adapting the receiver to another contract.

## Implementation and conformance work

The [shared lifecycle controller](../src/remote.rs) now implements terminal-state arbitration,
owner-triggered cancellation callbacks, and exactly-once request completion independently of an
execution engine. Its unit tests cover sticky failure, later request rejection, owner destruction,
and completion/shutdown races. It is not yet connected to the VM or native workers and does not
by itself enforce Foster owner lifetimes or provide Foster `RemoteError` values.

The current VM has remote failure delivery, but that alone does not establish terminal worker
failure, scoped cancellation, or the new typed outcome API. Native remote execution failures are
currently process-fatal. The owner/request completion analysis and proposed diagnostic are not
implemented. See [native runtime gaps](native.md#known-runtime-correctness-gaps).

Implementation must include witnesses for:

- accepting calls awaited before owner exit and owner moves to a longer-lived scope;
- rejecting returned pending futures, discarded-but-outstanding requests, and partially awaited
  branches whose owners leave scope;
- cancelling both queued and running work, with every pending future receiving a shutdown error;
- keeping completed outcomes usable after worker shutdown;
- containing failure and rejecting further execution on the failed worker;
- distinguishing domain errors, remote execution failure, and owner shutdown;
- resolving completion/shutdown races once without resurrecting cancelled work;
- releasing remote read loans safely during cancellation; and
- equivalent VM/native behavior with optimization enabled and disabled.

Implement enforcement and the awaited result type directly, without retaining the prior behavior
or adding a compatibility mode. Cross-worker scheduling, fairness, deadlock freedom, exact host interruption latency,
and process-wide shutdown ordering remain separate design work.
