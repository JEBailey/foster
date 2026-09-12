# Remote ownership, requests, and failure

This document encapsulates the remote lifecycle decisions accompanying
[S-19 and S-20 of the semantic specification](semantics.md#9-remote-execution).
It specifies required behavior. The implementation details below document cancellation points and
the completion proofs supported by the compiler.

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
result contract. A remote call returns `Future<Result<T, RemoteError>>`; its awaited value is
`Result<T, RemoteError>`, where `T` is the method's declared result type. `RemoteError.Failed(String)` describes an execution failure. `RemoteError` is declared in
`core.remote_error`; `Result` is declared in `core.result`. `RemoteError.Shutdown` is
the distinct owner-cancellation outcome.

If a method already returns `Result<T, E>`, the outer remote outcome remains separate:
`Result<Result<T, E>, RemoteError>`. Remote execution failure must not be silently converted into
the method's domain error type. Ordinary Result handling, rather than an unrelated sometimes-returned
error object, determines how callers inspect the outcome.

Use ordinary Result matching, transformations, or `try await` to handle remote outcomes.
Successful method results are wrapped even when the method already returns a Result.

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
the future does not finish the remote invocation. The diagnostic code is `E0730`.

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

## Implementation and conformance

Both backends connect owner release to the [shared lifecycle controller](../src/remote.rs).
Shutdown publishes outstanding errors before reclaiming queued arguments. Futures retain their
outcomes, not worker ownership. VM instruction dispatch and native block entry provide cooperative
cancellation points; waits for futures also check cancellation. Cleanup continues to run `deinit`
while preserving the original failure. Remote owners remain live until their semantic scope exit,
rather than being reclaimed at a last use before an await.

Running workers retain receiver storage and read leases until execution has stopped safely.
Idle workers finish receiver cleanup at owner release; active workers can finish physical cleanup
later. A blocked host call is not forcibly terminated. Per-worker FIFO and exactly-once terminal
outcomes remain unchanged. Cross-worker scheduling, fairness, deadlock freedom, exact host
interruption latency, and process-wide shutdown ordering remain separate design work.

### Supported static proofs

The ownership CFG tracks remote owners and request identities separately from borrow origins.
`E0730` rejects a reachable normal destruction boundary with outstanding work, including discarded
futures, future moves and record/list storage, owner replacement, early returns, branch exits,
loop exits, and incomplete awaits across branches. Awaiting a later known request on the same
worker also proves earlier non-repeated requests complete through FIFO ordering. Failure exits
use runtime cancellation; they do not masquerade as successful completion witnesses.

Owner and future moves within a function preserve identities. A direct future returned by a
helper using a borrowed remote parameter carries an obligation at its caller. Concrete remote
factory results and remote fields of concrete record factory results receive owner identities.
Completed futures may escape, and an owner with no pending requests may transfer across a function
boundary. Pending owner/aggregate transfers across function boundaries are conservatively rejected:
a plain record type does not encode the needed interprocedural ownership relationship. Await the
requests before that transfer. Awaiting through arbitrary helpers, indirect call provenance,
dynamically sized owner collections returned by opaque calls, and richer loop proofs are not
currently completion proofs. Runtime shutdown remains the backstop for unmodeled dynamic cases.

Branch states remain separate so that an owner from one path cannot satisfy another path's
obligation. Repeated request sites are conservative; the analysis bounds distinct states at a CFG
point to 256 and reports a diagnostic if that limit prevents a proof.

Conformance tests cover accepted completion and moves; rejected discarded, escaping, partially
awaited, and replaced-owner requests; running and queued cancellation; discarded futures;
completed outcomes surviving shutdown; receiver cleanup; failure containment; and optimization
parity. A runtime parity fixture removes a completion witness after semantic checking to test the
runtime backstop independently of the compiler rejection. No source-language compatibility mode
bypasses the checks.
