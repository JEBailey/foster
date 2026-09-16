# Hash collections

`HashMap` and `HashSet` are implemented in Foster, over ordinary lists. They compose the
storage-free `Map` and `Set` contracts, respectively. `ListMap` and `ListSet` provide insertion-ordered
implementations of those same contracts. Hash collection iteration order is unspecified.

`Map.empty()`, `Set.empty()`, and `Set.from(values)` remain available as factories returning
`ListMap` and `ListSet`. A `Map<K, V>` or `Set<T>` annotation accepts either implementation;
consuming updates through these contracts preserve the concrete implementation. Use `ListMap`
or `ListSet` annotations when callers specifically require the list-backed representation.

```foster
import core.option
import std.collections.hash_map
import std.collections.hash_set
import std.collections.hashing

func main() -> Int {
    let scores = HashMap.empty(hashing::text).put("Ada", 42).put("Lin", 7)
    assert(scores.contains_key?("Ada"))
    assert(scores.remove("Lin") == Option.Some(7))
    let languages = HashSet.from(["Foster", "Rust", "Foster"], hashing::text)
    assert(languages.length() == 2)
    scores.get("Ada").unwrap_or(0)
}
```

Pass a stable `func(K) -> Int` to `empty`, or to `HashSet.from(values, hasher)`. Equal keys
under Foster's `==` must produce equal hashes. Any signed hash is accepted, including the
minimum Int. Custom record keys can use a closure that hashes their equality-relevant fields.
Hasher behavior and key equality must remain stable while keys are stored. The supplied
hashing helpers are deterministic and are not designed to resist adversarial collision attacks.

Both contracts compose `Collection`, exposing `length()`, `empty?()`, and `iterator()`.
HashMap additionally provides `contains_key?`, `put`, `remove`, `get`, `keys`, and `values`.
HashSet provides `contains?`, `insert`, `remove`, and `values`. Map `put` and set updates consume
and return the collection. Map `get` borrows the map and returns `Option<V>` containing an
independent Copy; a present noncopyable value raises a runtime error. `None` means only that
the key is absent. Map `remove` mutates the map and returns the original owned value in
`Option<V>`, preserving all other entries and supporting noncopyable values. Keys and values extraction
consume the collection. Iterators retain independent snapshots, so later updates preserve
the iterator's original contents.

The table starts with eight buckets and doubles above a 75% load factor. Hash collisions are
resolved by equality checks within a bucket. Key search and updates have expected constant cost
with well-distributed hashes, and linear worst-case cost when hashes collide. Growth rehashes
all entries. Live snapshots can add copy-on-write costs to updates. Removal rebuilds its
collision chain and does not shrink the table. Table loops are iterative rather than recursive.
The cost of `get` includes the selected value's Copy operation. HashMap iterator
creation retains bucket storage; HashSet iterator creation collects a snapshot of its values
in linear time and space.
