struct ErrorCase {
    name: &'static str,
    source: &'static str,
    expected: &'static str,
}

#[test]
fn rejects_invalid_local_declarations_and_assignments_with_actionable_messages() {
    let cases = [
        ErrorCase {
            name: "missing local name",
            source: "func main() { let = 1 }",
            expected: "expected local name after `let`",
        },
        ErrorCase {
            name: "keyword used as local name",
            source: "func main() { let let = 1 }",
            expected: "expected local name after `let`",
        },
        ErrorCase {
            name: "missing equals sign",
            source: "func main() { let value 1 }",
            expected: "expected `=` after local name",
        },
        ErrorCase {
            name: "missing initializer",
            source: "func main() { let value = }",
            expected: "expected expression",
        },
        ErrorCase {
            name: "assignment before declaration",
            source: "func main() { value = unknown_rhs }",
            expected: "cannot assign to undeclared local `value`; declare it with `let value = ...`",
        },
        ErrorCase {
            name: "duplicate local declaration",
            source: "func main() { let value = 1\nlet value = unknown_rhs }",
            expected: "local `value` is already declared; omit `let` to assign to it",
        },
        ErrorCase {
            name: "parameter redeclaration",
            source: "func main(value: Int) { let value = 1 }",
            expected: "local `value` is already declared; omit `let` to assign to it",
        },
        ErrorCase {
            name: "module constant shadowing",
            source: "const VALUE = 1\nfunc main() { let VALUE = 2 }",
            expected: "local `VALUE` conflicts with a module constant of the same name",
        },
        ErrorCase {
            name: "module constant assignment",
            source: "const VALUE = 1\nfunc main() { VALUE = 2 }",
            expected: "cannot assign to constant `VALUE`",
        },
        ErrorCase {
            name: "local declaration at module scope",
            source: "let value = 1",
            expected: "local declarations are only allowed inside function, closure, or test bodies",
        },
        ErrorCase {
            name: "guarded local declaration",
            source: "func main() { let value = 1 if true }",
            expected: "postfix `if` may only guard a control statement",
        },
        ErrorCase {
            name: "non-place assignment target",
            source: "func main() { (1 + 2) = 3 }",
            expected: "left side of assignment is not a place",
        },
    ];

    for case in cases {
        let error = match foster::compile(case.source) {
            Ok(_) => panic!("{} unexpectedly compiled", case.name),
            Err(error) => error,
        };
        assert!(
            error.message.contains(case.expected),
            "{} produced `{}`; expected it to contain `{}`",
            case.name,
            error.message,
            case.expected
        );
    }
}
