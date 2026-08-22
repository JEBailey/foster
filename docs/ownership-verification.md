# Ownership verification

Foster treats ownership soundness as a compiler contract rather than a collection of incidental
regression tests. Verification is layered so that a failure identifies which assumption changed.

## Place reasoning

All move and loan checks use the canonical place relations in `src/hir/queries.rs`. Named sibling
fields are disjoint. Integer-literal indices retain their constant key, so different constant
indices are disjoint; equal constants overlap even when they came from different expressions.
Dynamic indices conservatively overlap every index. Dereference and mixed projection shapes remain
conservative.

## Rule witnesses

`tests/ownership_soundness.rs` contains rule-indexed compile-pass and compile-fail programs. New or
changed normative rules in `docs/ownership.md` must add both a smallest accepted witness and a
smallest rejected witness where both outcomes are meaningful.

## Differential and mutation checks

Ownership unit tests compare linear and split CFGs representing the same event history. Source-level
tests also compare equivalent branch rewrites. Mutation-sensitivity cases remove an invalidation or
end a loan before its later use and require the decision to change. The normal CI command is:

```text
cargo test
```

Long-running release qualification should additionally run a Rust mutation-testing tool against
`src/ownership/regions.rs`, `src/ownership/lower.rs`, and `src/hir/queries.rs`. Surviving mutations
in place overlap, requirement transfer, invalidation, return escape, suspension, or destruction are
release blockers unless documented as equivalent transformations.

## Executable reference model

`src/ownership/model.rs` is a deliberately small operational oracle for a single loan on one CFG
path. It models issuance, invalidation, use, and region end independently of the optimized dataflow
implementation. The test suite exhaustively enumerates all six-event histories after issuance and
requires the CFG checker and oracle to agree. CFG behavior is the union of independently valid
paths, with joins handled by the production worklist solver.

The model should stay smaller than the compiler. Features such as parent reborrows should be added
as separate model state rather than by calling production analysis helpers.

## Bounded fuzzing

Normal CI deterministically generates malformed token streams and runs them through the complete
compiler under panic capture. This covers lexer, parser, HIR lowering, type inference, ownership-MIR
lowering, and ownership validation with a stable seed. The exhaustive reference-model comparison
fuzzes the MIR region boundary without requiring malformed internal IDs.

Release qualification should increase input counts and run coverage-guided fuzzers over:

1. Arbitrary source bytes passed to `foster::compile`.
2. Syntactically valid generated functions emphasizing branches, loops, projections, calls,
   reborrows, moves, `await`, and returns.
3. Valid ownership MIR generated from typed HIR, including unreachable blocks and loop backedges.
4. Binary bytecode decoding and the VM verifier.

Every campaign must enforce time and memory limits. Panics, hangs, nondeterministic diagnostics, and
oracle disagreements are failures. Raw malformed MIR with invalid arena IDs is outside the language
input boundary and belongs to defensive verifier tests rather than the semantic oracle.
