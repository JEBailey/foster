use super::*;

use crate::vm::{Machine, compile};

#[test]
fn round_trips_and_executes_compiled_program() {
    let source = "enum Choice = Left(Int) | Right(Int)\n\
            func unwrap(value: Choice) -> Int { branch value { Choice.Left(number) -> number _ -> 0 } }\n\
            func main() -> Int {\n assert(true, \"round-trip assertion\")\n let values = [20, 22]\n unwrap(Choice.Left(values[0] + values[1]))\n }";
    let compilation = crate::compile(source).unwrap();
    let program = compile(&compilation).unwrap();
    let bytes = encode_program(&program).unwrap();
    let decoded = decode_program(&bytes).unwrap();
    assert_eq!(program, decoded);
    assert_eq!(
        Machine::new(&decoded).run_main().unwrap(),
        crate::vm::Value::Integer(42)
    );
    assert_eq!(bytes, encode_program(&decoded).unwrap());
}

#[test]
fn round_trips_a_reference_to_an_expression_temporary() {
    let source = r#"
func observe[value: group Int](item: ref[value] Int) -> Int { item }
func make() -> Int { 42 }
func main() -> Int { observe(ref (make())) }
"#;
    let compilation = crate::compile(source).unwrap();
    let program = compile(&compilation).unwrap();
    assert!(program.functions.values().any(|function| {
        function.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                crate::vm::Instruction::MakeWholeReference { .. }
            )
        })
    }));
    let decoded = decode_program(&encode_program(&program).unwrap()).unwrap();
    assert_eq!(program, decoded);
    assert_eq!(
        Machine::new(&decoded).run_main().unwrap(),
        crate::vm::Value::Integer(42)
    );
}

#[test]
fn rejects_invalid_envelopes() {
    assert!(decode_program(b"not bytecode").is_err());
    let compilation = crate::compile("func main() -> Int { 42 }").unwrap();
    let program = compile(&compilation).unwrap();
    let mut bytes = encode_program(&program).unwrap();
    bytes[8..10].copy_from_slice(&(FORMAT_VERSION - 1).to_le_bytes());
    assert!(
        decode_program(&bytes)
            .unwrap_err()
            .to_string()
            .contains("version")
    );
}

#[test]
fn foster_toml_parser_survives_bytecode_round_trips() {
    let source = r#"
import core.result
import std.toml

func main() -> Int {
    branch parse("answer = 42\n") {
        Result.Error(_) -> 0
        Result.Ok(document) -> branch document.entries.head.value {
            TomlValue.Int(value) -> value
            _ -> 0
        }
    }
}
"#;
    let compilation = crate::compile(source).unwrap();
    let program = compile(&compilation).unwrap();
    let decoded = decode_program(&encode_program(&program).unwrap()).unwrap();
    assert_eq!(
        Machine::new(&decoded).run_main().unwrap(),
        crate::vm::Value::Integer(42)
    );
}

#[test]
fn time_clock_builtins_survive_bytecode_round_trips() {
    let source = r#"
import std.time

func main() -> Bool {
    let wall = now()
    let first = ContinuousClock.new().now()
    let second = ContinuousClock.new().now()
    wall.nanosecond() >= 0 && wall.nanosecond() < 1000000000 && first.until(second).total_nanoseconds() >= 0
}
"#;
    let compilation = crate::compile(source).unwrap();
    let program = compile(&compilation).unwrap();
    let decoded = decode_program(&encode_program(&program).unwrap()).unwrap();
    assert_eq!(program, decoded);
    assert_eq!(
        Machine::new(&decoded).run_main().unwrap(),
        crate::vm::Value::Bool(true)
    );
}
