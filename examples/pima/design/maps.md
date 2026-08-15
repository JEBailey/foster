# Maps

> Historical design note. `../maps.foster` is the authoritative typed-map port.

Pima's `maps.pima` becomes:

```foster
user = Map {
    :name: "Ada"
    :score: 95
    :total: 90 + 5
}

updated = user.put(:score, 96)

println("original:", user.get(:score))
println("updated:", updated.get(:score))
println("keys:", updated.keys)
```

Open questions are the map literal syntax, whether symbol keys receive special field-like sugar,
and whether `put` consumes/persistently copies a map or mutates it through a declared group effect.

