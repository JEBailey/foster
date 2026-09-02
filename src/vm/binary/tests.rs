use super::*;

use crate::vm::{CompileOptions, Machine, compile, compile_with_options};

#[test]
fn round_trips_and_executes_compiled_program() {
    let source = "enum Choice = Left(Int) | Right(Int)\n\
            func unwrap(value: Choice) -> Int { branch value { Choice.Left(number) -> number _ -> 0 } }\n\
            func main() -> Int {\n assert(true, \"round-trip assertion\")\n let values = [20, 22]\n unwrap(Choice.Left(values[0] + values[1]))\n }";
    let compilation = crate::compile(source).unwrap();
    let program = compile_with_options(&compilation, CompileOptions { optimize: false }).unwrap();
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
fn round_trips_generic_aggregate_layout_metadata() {
    let source = r#"
type Box<T> = { value: T }
enum Maybe<T> = None | Some(T)

func make<T>(value: T) -> Box<T> { Box { value } }
func main() -> Int { make(42).value }
"#;
    let compilation = crate::compile(source).unwrap();
    let program = compile_with_options(&compilation, CompileOptions { optimize: false }).unwrap();
    let record = program
        .records
        .values()
        .find(|record| record.name == "Box")
        .unwrap();
    assert_eq!(
        record.field_types,
        vec![VerificationType::Generic("T".into())]
    );
    assert_eq!(record.parameters, vec!["T"]);
    let some = program
        .variants
        .values()
        .find(|variant| variant.alternative.as_ref() == "Some")
        .unwrap();
    assert_eq!(some.payload, vec![VerificationType::Generic("T".into())]);
    assert_eq!(some.parameters, vec!["T"]);
    let make = program
        .functions
        .iter()
        .find_map(|(id, function)| (function.name == "make").then_some(*id))
        .unwrap();
    assert!(program.functions.values().any(|function| {
        function.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Call {
                    function,
                    specialization,
                    ..
                } if *function == make
                    && specialization
                        == &vec![("T".into(), VerificationType::Integer)]
            )
        })
    }));
    assert!(
        program.functions[&make]
            .instructions
            .iter()
            .any(|instruction| {
                matches!(
                    instruction,
                    Instruction::MakeRecord { type_arguments, .. }
                        if type_arguments == &vec![VerificationType::Generic("T".into())]
                )
            })
    );
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
