# Variants, destructuring, and patterns

> Historical design note. `../patterns.foster` is the authoritative variant-pattern port.

Pima's `patterns.pima` maps naturally to closed Foster variants and exhaustive `branch`:

```foster
type ScoreResult =
    | Ok(String, Int)
    | Error(String)

func describe(result: ScoreResult) -> Int {
    branch result {
        Ok(name, score) -> {
            println(name, "scored", score)
            score
        }
        Error(message) -> {
            println("Error:", message)
            0
        }
    }
}

(name, score) = ("Ada", 42)
describe(Ok(name, score))
```

This awaits tuple syntax, record/variant declarations, block expressions, pattern HIR, and
exhaustiveness checking.

