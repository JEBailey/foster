# Enums, destructuring, and patterns

> Historical design note. `../patterns.fos` is the authoritative enum-pattern port.

Pima's `patterns.pima` maps naturally to Foster enums and exhaustive `branch`:

```foster
type Score = { name: String, value: Int }

enum ScoreResult = Ok(Score)
    | Error(String)

func describe(result: ScoreResult) -> Int {
    branch result {
        Ok(score) -> {
            println(score.name, "scored", score.value)
            score.value
        }
        Error(message) -> {
            println("Error:", message)
            0
        }
    }
}

describe(Ok(Score { name: "Ada", value: 42 }))
```

The executable counterpart uses labelled cases with explicit payload types, block expressions,
pattern HIR, and exhaustiveness checking.

