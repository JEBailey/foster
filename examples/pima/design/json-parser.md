# JSON parser

> Historical design note. `../json_parser/` is the authoritative executable port.

The 281-line Pima JSON parser is a valuable future conformance test. Its public Foster shape should
be:

```foster
enum Json = Null
    | Boolean(Bool)
    | Number(Float)
    | JsonString(String)
    | Array(List<Json>)
    | Object(Map<String, Json>)

type JsonError = {
    message: String
    offset: Int
}

func parse(source: String) -> Json throws JsonError {
    let characters = source.characters
    (value, remaining) = parse_value(skip_whitespace(characters))
    throw JsonError { message: "trailing content" offset: source.length - remaining.length }
        if !remaining.empty?
    value
}
```

A faithful port depends on `Float`, tuples, records, enums, maps, character/scalar types, Unicode
escape handling, typed errors, and pattern matching. The original should become a compiler/runtime
conformance test after those foundations land, rather than being approximated with dynamically
typed values.

