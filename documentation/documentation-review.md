# Documentation review — 2026-09-14

## Scope and assessment

This pass inventoried all 48 repository Markdown files, scanned their local links and
Foster code fences, and reviewed consistency between the guides, current source, tests,
and generated API reference. Semantic checks focused on syntax, ownership, time,
iteration, host services, compilation, and artifact formats. This is a repository-wide
documentation audit, not a proof of every implementation or API claim.

| Documentation area | Review emphasis |
| --- | --- |
| Root README, AGENTS, writing guide, examples catalogs | Discoverability, commands, source syntax, complete examples |
| Language, semantics, ownership, closures, effects, remote rules | Contract consistency, supported analysis, conservative limits |
| Library guide, reference, documentation standard, contract audit, library review | Ownership, failures, public contracts, source-comment coverage |
| Time, Unicode, randomness, hash collections | Units, bounds, examples, provider availability |
| VM, native, binary format, packages, compiled libraries, symbolic modules | Current pipeline, format versions, runtime behavior |
| Diagnostics, interactive checking, testing, coverage, ownership verification, analysis precision | Current implementation and verification boundaries |
| Host/runtime READMEs, Unicode/tzdata tools, tzdata package | Embedding, regeneration, dependency setup |
| Editor README/changelog, benchmarking guide and three result reports | Instructions versus historical measurements and release notes |
| Documentation index, development policy, roadmap | Reader routing and implemented versus proposed work |

The overall organization and level of API detail are appropriate. Keep beginner
examples short, explain ownership and failure behavior where they affect callers,
and keep compiler representation details in contributor references. Public/private
labels remain necessary for fields as well as methods. Source signatures, rather
than generated resolved reference notation, remain the syntax to copy into programs.

## Findings corrected

- The time guide contained invalid bare `assert` branch arms, unsupported line breaks
  after call-opening parentheses, and missing `move` markers for consuming arguments.
  Corrected the examples, stated their shared import/function context, and added
  [an executable guide check](../tests/documentation_examples.rs).
- Twelve required methods on `Int` lacked documentation even though their implementations
  were documented. Added matching summaries, bounds, and ownership-relevant details;
  the existing library documentation coverage test now passes.
- The README built `recursion.fbc` but instructed readers to run `fibonacci.fbc`.
  Both commands now name the same artifact.
- Compiled-library and package guides named bytecode version 26 instead of 27.
  The bytecode tag inventory also omitted TCP readiness tags 62–64.
- The VM guide repeated obsolete register-inliner limits. It now describes the shared
  SSA inliner and its current bounds. The native guide now identifies neutral layout
  metadata and avoids naming an older bytecode version as current.
- The README and roadmap described native `await` as blocking without distinguishing
  ordinary threads from actor coroutines. Actor waits already suspend their coroutine.
- The host README attributed provider-owned state to each context. It now explains
  that contexts sharing a provider also share its state and handles.
- Time and library guides described regional IANA data as unavailable or future work.
  They now point to the optional tzdata package. The core reference also recognizes
  implemented timed TCP readiness and active owner-shutdown outcomes.
- Analysis-precision and semantic-gap descriptions lagged behind bounded callable-target
  sets, factory/aggregate borrower summaries, and computed/saved integer conditions.
  Updated those descriptions without claiming general theorem proving.
- Corrected the public-type inventory count to 130, clarified iterator mutation permission
  and resource construction side effects, and updated the benchmark guide's recovery description.

## Validation

- Inventoried 150 `foster` Markdown code blocks. Nine self-contained programs with
  `main` compiled and ran on the VM. The remaining complete tzdata program requires
  a separately built dependency and was not run in this pass.
- The time guide's 30 code blocks compile together with their documented imports;
  its 26 statement snippets run in separate functions and their assertions pass.
  Three helper function examples are type-checked; the import block supplies context.
- Standard-library documentation coverage passed, checking 995 implementation functions
  and attached module, public-type, and required-method documentation.
- All eight documentation-renderer tests passed, including overloads, visibility,
  Markdown rendering, and generated type links.
- Regenerated the library site: 1,230 declarations in 60 modules. All 8,697 local links
  across its 61 HTML pages resolve to existing files and, where applicable, anchors.
- Repository Markdown local file links and heading links resolve. Rust formatting and
  whitespace checks were run on the changed test and patch.

## Limits and maintenance

Code-fence parsing is a triage tool: type fragments, abbreviated examples, and deliberately
rejected programs are not standalone applications. Complete-program execution and the new
time-guide test provide stronger evidence for their specific examples. This pass did not
execute every API comment example, every example-directory program, or the full backend
conformance suite. It did not fetch external URLs or remeasure historical benchmarks.

Documentation presence and link checks cannot prove behavioral accuracy. Preserve explicit
bounds, units, consumption, and failure descriptions in authoritative `.fos` comments, and
keep required-method comments consistent with implementation comments. Field-level comments
remain unsupported; document field meaning in the owning type's comment. Rebuild installed
compilers and language servers when distributing embedded comment changes.
