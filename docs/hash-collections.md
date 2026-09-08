# Hash collections

`HashMap` and `HashSet` are implemented in Foster, over ordinary lists. They work alongside
the insertion-ordered `Map` and `Set`. Hash collection iteration order is unspecified.

```foster
import core.option
import std.collections.hash_map
import std.collections.hash_set
import std.collections.hashing

func main() -> Int {
    let scores = HashMap.empty(hashing::text).put("Ada", 42).put("Lin", 7)
    assert(scores.contains_key?("Ada"))
    scores = (move scores).remove("Lin")
    let languages = HashSet.from(["Foster", "Rust", "Foster"], hashing::text)
    assert(languages.length() == 2)
    (move scores).get("Ada").unwrap_or(0)
}
```

Pass a stable `func(K) -> Int` to `empty`, or to `HashSet.from(values, hasher)`. Equal keys
under Foster's `==` must produce equal hashes. Any signed hash is accepted, including the
minimum Int. Custom record keys can use a closure that hashes their equality-relevant fields.
Hasher behavior and key equality must remain stable while keys are stored. The supplied
hashing helpers are deterministic and are not designed to resist adversarial collision attacks.

Both types implement `Collection`, exposing `length()`, `empty?()`, and `iterator()`.
HashMap additionally provides `contains_key?`, `put`, `remove`, `get`, `keys`, and `values`.
HashSet provides `contains?`, `insert`, `remove`, and `values`. Updates consume and return the
collection, following the existing Map/Set API. HashMap's `get` also consumes the map and returns
`Option<V>`; use `contains_key?` for a borrowed membership check. Keys and values extraction
consume the collection. Iterators retain independent snapshots, so later updates preserve
the iterator's original contents.

The table starts with eight buckets and doubles above a 75% load factor. Hash collisions are
resolved by equality checks within a bucket. Key search and updates have expected constant cost
with well-distributed hashes, and linear worst-case cost when hashes collide. Growth rehashes
all entries. Live snapshots can add copy-on-write costs to updates. Removal rebuilds its
collision chain and does not shrink the table. Table loops are iterative rather than recursive.
Consuming `get` also releases the remaining table, which can take linear time. HashMap iterator
creation retains bucket storage; HashSet iterator creation collects a snapshot of its values
in linear time and space.
