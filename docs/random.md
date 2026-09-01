# Randomness in Foster

Foster separates randomness into sources, deterministic generators, distributions, security
operations, and sequence algorithms. The separation makes the caller's intent visible: tests can
inject a repeatable stream, applications can request fresh operating-system randomness, and code
handling secrets can stay within the security-oriented API.

## Choosing an API

| Need | API |
| --- | --- |
| One integer from `0` through `count - 1` | `random::below(count)` |
| One integer from `start` through `end - 1` | `random::between(start, end)` |
| Repeatable values across Foster releases and targets | `LehmerRandom.from_seed(seed)` |
| A fast repeatable stream whose algorithm may evolve | `FastRandom.from_seed(seed)` |
| A fast stream initialized from the operating system | `FastRandom.from_system()` |
| A reusable validated probability model | `UniformInt`, `UniformFloat`, `Bernoulli`, or `WeightedIndex` |
| Cryptographic entropy or a secret token | `random_bytes`, `token_hex`, or `token` |
| Choice, shuffling, or sampling without replacement | `std.random.sequence` |
| A custom, injectable source | Implement the `RandomSource` structural contract |

All operations which can consume host entropy or a fallible source return
`Result<..., RandomError>`. `RandomError` identifies the operation, offending integer value when
one exists, and a human-readable message.

## Half-open integer ranges

Integer range APIs use half-open bounds. `[start, end)` contains `start` but does not contain
`end`. Consequently, `random::below(20)` has exactly 20 possible results: `0` through `19`.

```foster
import core.result
import std.random

func number_below_twenty() -> Result<Int, RandomError> {
    random::below(20)
}
```

Half-open ranges make a count usable as the upper bound, agree with zero-based list indices, and
compose without overlapping endpoints. `below(0)`, an empty `between(4, 4)`, a reversed range, or
a range wider than the source can represent returns `Result.Error`.

Foster does not implement a bounded draw as `value.modulo(count)`. Unless `count` evenly divides
the source capacity, that shortcut makes some results more likely. The common range operations use
rejection sampling: they discard the small portion of source values which would introduce that
modulo bias and draw again.

## Sources and dependency injection

`RandomSource` is a structural contract:

```foster
pub type RandomSource = {
    pub func next(self) -> Result<Int, RandomError> [mut self]
    pub func maximum(self) -> Int
}
```

`next` must be uniform over the inclusive range `[0, maximum]`. `maximum` must remain stable while
the source is used and must be from zero through `9223372036854775806`; the upper limit keeps the
source capacity representable as a Foster `Int`. A source which returns a value outside its
advertised range is rejected by the common bounded operations.

Algorithms accept the contract rather than a particular generator:

```foster
import core.result
import std.random

func roll_die(source: RandomSource) -> Result<Int, RandomError> [mut source] {
    Result.Ok(1 + try random::below_with(source, 6))
}
```

This form is the normal choice for reusable code. A production caller can pass `SystemRandom`; a
test can pass a fixed-seed `LehmerRandom` without changing the algorithm under test.

`SeedableRandom` extends `RandomSource` with `reseed`. `SplittableRandom` adds `split`, which
advances the parent and returns a separately owned derived stream. Splitting provides convenient
state ownership; it does not promise cryptographic independence.

## System and deterministic generators

`SystemRandom` obtains every draw from the operating system's secure entropy facility. It has no
user-supplied seed and makes environmental failure explicit. It is appropriate for occasional
unpredictable draws. For many draws, initialize a generator once and pass it through the algorithm.

The runtime boundary is intentionally tiny and adds no Rust package dependency. On Windows it
calls the operating system's CNG system-preferred generator; on Unix it reads `/dev/urandom`.
Targets without either provider compile with the same API and return `RandomError` until a platform
provider is added. Random policy and transformations do not live in this platform shim.

`LehmerRandom` is the named portable generator. Its multiplier, modulus, seed normalization, and
zero-based output mapping are compatibility promises. For example, seed `42` begins with
`2027381`, then `1226992406`. It is useful for tests, procedural generation, and modest simulations.
It is not cryptographically secure and must not generate passwords, session identifiers, keys, or
other secrets.

```foster
import core.result
import std.random
import std.random.generator

func repeatable_index() -> Result<Int, RandomError> {
    let source = LehmerRandom.from_seed(42)
    random::below_with(source, 20)
}
```

`FastRandom` is the default deterministic generator. Its algorithm may change between Foster
releases, so use it when release-to-release replay is not a persistence requirement. Its current
implementation shares the compact Lehmer core. `FastRandom.from_system()` combines unpredictable
initialization with efficient subsequent deterministic draws.

## Distributions

`Distribution<T>` transforms a `RandomSource` into values from a validated probability model:

- `UniformInt.from(start, end)` samples integers from `[start, end)`.
- `UniformFloat.from(start, end)` samples finite floats from `[start, end)`.
- `Bernoulli.from(probability)` samples `Bool`, with probability from `0.0` through `1.0`.
- `WeightedIndex.from(weights)` returns an index in proportion to nonnegative integer weights.

Constructors return `Result` so invalid models cannot be sampled. Weighted distributions require
at least one positive weight, reject negative weights, and cap the total at the portable
generator's capacity.

```foster
import core.result
import std.random
import std.random.distribution
import std.random.generator

func weighted_choice() -> Result<Int, RandomError> {
    let source = LehmerRandom.from_seed(9)
    let distribution = try WeightedIndex.from([1, 3, 6])
    distribution.sample(source)
}
```

`UniformFloat` maps one discrete source draw into a binary64 interval. It guarantees the documented
bounds, not a continuous mathematical distribution or every representable float with equal
probability.

## Security-oriented randomness

Import `std.random.secure` when unpredictability is a correctness requirement:

```foster
import core.result
import std.random
import std.random.secure

func session_token() -> Result<String, RandomError> {
    token(32)
}
```

`EntropySource` is the byte-oriented structural contract. `SecureRandom` implements both
`EntropySource` and `RandomSource` using operating-system entropy. `random_bytes(count)` returns
exactly the requested byte count, `token_hex(byte_count)` returns two lowercase hexadecimal
characters per byte, and `token(length)` returns exactly that many characters from a uniform
64-character URL-safe alphabet.

Raw byte requests are limited to 1,048,576 bytes per call. URL-safe tokens are limited to 4,096
characters. A zero length is valid. These APIs may fail if the host cannot provide secure entropy;
security-sensitive code should propagate the error rather than substitute a deterministic seed.

## Lists: choose, shuffle, and sample

The sequence module provides operating-system-backed convenience functions and explicit-source
variants:

| Convenience | Explicit source | Result |
| --- | --- | --- |
| `choose(values)` | `choose_with(source, values)` | `Option.None` for an empty list, otherwise one member |
| `shuffle(values)` | `shuffle_with(source, values)` | Every input value in uniformly randomized order |
| `sample(values, count)` | `sample_with(source, values, count)` | `count` values without replacement |

```foster
import core.result
import std.random
import std.random.generator
import std.random.sequence

func test_order() -> Result<List<Int>, RandomError> {
    shuffle_with(LehmerRandom.from_seed(7), [1, 2, 3, 4])
}
```

`shuffle` and `sample` consume their input list because they return owned elements in a new list.
`sample` rejects negative counts and counts larger than the list. The explicit-source forms are
recommended in tests so a failure can be reproduced from its seed.

## Reproducibility and security rules

- Persist the generator name and seed, not just a bare seed. A seed has meaning only with an
  algorithm and its versioned mapping.
- Use `LehmerRandom` when output must reproduce across Foster versions or targets.
- Use `FastRandom` for ordinary deterministic work where the implementation may improve over time.
- Use `SystemRandom` or `std.random.secure` when results must be unpredictable.
- Never turn a timestamp, process identifier, or portable generator seed into a security token.
- Test exact portable sequences, but test random ranges and invariants rather than expecting a
  particular operating-system result.
