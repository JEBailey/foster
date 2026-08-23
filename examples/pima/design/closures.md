# Closures and partial application

> Historical design note. The executable `../closure.fos` and
> `../closure_ownership.fos` ports supersede this pre-implementation sketch.

Pima's `closure.pima` and `curried_example.pima` suggest first-class closures and placeholder-based
partial application. The intended Foster spelling is:

```foster
func multiplier(factor: Int) -> func(Int) -> Int {
    func apply(value: Int) -> Int {
        factor * value
    }
    apply
}

let triple = multiplier(3)
println(triple(12))

let add_five = add(5, _)
```

This is not implemented. We need closure environment lowering, function types in the parser, capture
analysis, and a decision about ownership of captured values. `_` partial application should be
library/compiler sugar over a closure, not a separate runtime mechanism.

