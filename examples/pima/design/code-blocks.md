# Code blocks

> Historical design note. `../code_blocks.fos` is the authoritative lexical-closure port.

Pima code blocks are unusual: they are inert templates whose free names can be supplied by the
environment of a later `do`. They are not ordinary lexical closures.

The direct design port is:

```foster
block compact_report(name: String, score: Int) {
    println(name, ":", score)
}

func render_report(
    report: block(name: String, score: Int, passing: Bool),
    name: String,
    score: Int,
) -> () {
    let passing = score >= 70
    do report
}
```

We should not merge this feature with closures prematurely. A block template implies structural
requirements, caller-provided bindings, and potentially remote transfer. It needs a separate design
for required names, effects, ownership transfer, serialization, and declarations introduced by
`do`.

