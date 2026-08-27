# Stateful objects as records

> Historical design note. `../object_test.fos` is the authoritative record-and-method port.

Pima's `object_test.pima` does not require class inheritance. In Foster it should be a record with
methods and mutation effects:

```foster
type Square = {
    length: Int
    width: Int
}

extend Square {
    func area(self) -> Int {
        self.length * self.width
    }

    func Square.set_width[mut square: group Square](self: ref[square] Square, width: Int) {
        self.width = width
    }
}

let square1 = Square { length: 5 width: 80 }
let square2 = Square { length: 5 width: 80 }
square1.set_width(40)
```

This needs records, method lookup, assignment, place analysis, and group effects. Construction
should not require a general `new` operator for inline values.

