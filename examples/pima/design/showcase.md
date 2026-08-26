# Showcase translation

> Historical design note. `../showcase.fos` is the authoritative executable port.

Pima's showcase combines typed errors, mutable objects, closures, collection pipelines, code blocks,
and attempted error capture. In intended Foster syntax, its central API becomes:

```foster
type AccountError =
    InvalidOpeningBalance

type Account = {
    owner: String
    balance: Int
    history: List<Int>
}

func create_account(owner: String, opening_balance: Int)
    -> Account
    throws AccountError
{
    throw InvalidOpeningBalance if opening_balance < 0
    Account { owner balance: opening_balance history: [] }
}

extend Account {
    func deposit[mut accounts: group Account](self: ref[accounts] Account, amount: Int) {
        self.balance = self.balance + amount
        self.history.push(amount)
    }
}

let triple = (value: Int) -> value * 3
let selected = range(1, 6).map(triple).filter((value) -> value > 10)
let total = selected.fold(0, (left, right) -> left + right)
```

This is a target example, not currently executable. It gives us a useful acceptance test for
records, effects, closures, iterators, typed errors, and collection APIs.

