# JSON

`std.json` implements JSON parsing and serialization in Foster. It preserves
object order and exact number spelling, and returns typed errors instead of
coercing mismatched values.

```foster
import std.json
import core.result
import core.option

func main() -> Result<Int, JsonError> {
    let tree = try json::parse("{\"answer\":42}")
    let number = tree.get("answer").unwrap_or(JsonValue.Null).as_number().unwrap_or(JsonNumber.from_int(0))
    let encoded = try json::pretty(tree)
    println(encoded)
    number.as_int()
}
```

## Values and ownership

`JsonValue` has `Null`, `Bool`, `Number`, `String`, `Array`, and `Object` cases.
Arrays contain `List<JsonValue>`; objects contain ordered `List<JsonMember>`
entries with public `key` and `value` fields. Construct these cases directly or
use `json::parse(String)` and `json::parse_bytes(Bytes)`.

Parsing borrows the source. `stringify`, `pretty`, and lookups borrow the tree.
`get(key)` and `at(index)` return independent copies of selected subtrees.
Missing keys, invalid indexes, and the wrong container kind return `Option.None`;
a present JSON null returns `Option.Some(JsonValue.Null)`. Dots in keys are literal.
`as_string`, `as_number`, and `as_bool` return optional typed payloads without
coercion; `null?()` tests for null. `copy()` copies the entire tree.

## Numbers

`JsonNumber.parse` validates a number token with no surrounding whitespace.
Numbers retain precision and spelling, including `-0`, `1.2300e+04`, and values
beyond machine numeric ranges. `text()` returns that spelling.

`from_int` constructs an exact number. `as_int` accepts integer syntax within the
signed 64-bit range; fractions and exponent notation return `NumberRange` even
when mathematically integral. `from_float` uses round-trippable binary64 text and
rejects NaN and infinities. `as_float` permits rounding and underflow to zero,
but rejects overflow to infinity. Neither conversion changes the stored text.

## Validation and output

The parser accepts one complete value with JSON whitespace. It rejects comments,
trailing commas, trailing data, a leading BOM, invalid UTF-8, unescaped control
bytes, and lone UTF-16 surrogate escapes. Paired surrogate escapes decode to a
Unicode scalar. Duplicate keys are rejected after escape decoding, without Unicode
normalization. Serialization also rejects duplicate keys in manually built trees.

`stringify` emits compact JSON. `pretty` indents with two spaces.
`stringify_with(tree, JsonWriteOptions { indent: 4, max_depth: 128 })` selects
indentation from zero (compact) through eight spaces. Output preserves object
order and number spelling, escapes controls, and has no trailing newline.
It is deterministic for a given tree, but is not canonical JSON for signatures.

Parsing and writing default to 128 nested containers. `parse_with` accepts
`JsonParseOptions { max_depth: ... }`; both options types support depths 0..256.
An outer array or object counts as one level; depth zero accepts scalars only.
Invalid settings return `InvalidOptions`; excessive nesting returns `DepthLimit`.

`JsonError` provides `kind`, `message`, `offset`, `line`, and `column`. Source
offsets are zero-based UTF-8 bytes; lines and byte columns are one-based, with LF
starting a new line. Non-source errors and invalid UTF-8 use offset -1 and
line/column zero. Branch on `kind`, not message text. Failures return no partial
tree or serialized output.

The implementation builds a complete in-memory tree and output buffer; it is
not streaming. Object lookup scans members in order; current list indexing also
copies inspected entries and their subtrees. Duplicate checking is quadratic in
the number of keys in an object. Limit input size in the caller when processing
untrusted or very large documents. There is no automatic mapping to user-defined
record types; inspect the tree explicitly.
