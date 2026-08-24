# Variants, destructuring, and patterns

> Historical design note. `../patterns.fos` is the authoritative variant-pattern port.

Pima's `patterns.pima` maps naturally to closed Foster variants and exhaustive `branch`:

```foster
type Ok = { name: String score: Int }
type Error = { message: String }

type ScoreResult =
    | Ok
    | Error

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

The executable counterpart uses these distinct member record types, block expressions, pattern
HIR, and exhaustiveness checking.

