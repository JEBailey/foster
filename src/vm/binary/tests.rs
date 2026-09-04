use super::*;

use crate::vm::{CompileOptions, Machine, compile, compile_with_options};

#[test]
fn decoder_rejects_a_forged_specialized_method_return_type() {
    let compilation = crate::compile(
        "type Echo<T> = { value: T }\nfunc Echo.get<T>(self: Echo<T>) -> T { self.value }\nfunc main() -> Bool { Echo { value: true }.get() }",
    ).unwrap();
    let program = compile_with_options(&compilation, CompileOptions { optimize: false }).unwrap();
    let mut bytes = encode_program(&program).unwrap();
    let mut main = program.functions[&program.main.unwrap()].clone();
    let mut original = Writer { bytes: Vec::new() };
    original.function(&main).unwrap();
    let offsets = bytes
        .windows(original.bytes.len())
        .enumerate()
        .filter_map(|(index, window)| (window == original.bytes).then_some(index))
        .collect::<Vec<_>>();
    assert_eq!(offsets.len(), 1);
    main.result_type = VerificationType::Integer;
    let mut forged = Writer { bytes: Vec::new() };
    forged.function(&main).unwrap();
    bytes.splice(offsets[0]..offsets[0] + original.bytes.len(), forged.bytes);
    let error = decode_program(&bytes).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("return value type Bool, expected Integer"),
        "{error}"
    );
}

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
fn round_trips_generic_closure_specializations() {
    let source = r#"
func apply_capture<T>(value: T, number: Int) -> Int [consume value] {
    let action = [move value] (input: Int) -> {
        value
        input
    }
    action(number)
}

func make_capture<T>(value: T) [consume value] {
    [move value] (input: Int) -> {
        value
        input
    }
}

func main() -> Int {
    let getter = make_capture(0)
    getter(20) + apply_capture(0, 22)
}
"#;
    let compilation = crate::compile(source).unwrap();
    let program = compile_with_options(&compilation, CompileOptions { optimize: false }).unwrap();
    let optimized = compile(&compilation).unwrap();
    assert!(program.functions.values().any(|function| {
        function.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::MakeClosure { specialization, .. }
                    if !specialization.is_empty()
            )
        })
    }));
    assert!(optimized.functions.values().any(|function| {
        function.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::CallClosure { specialization, .. }
                    if !specialization.is_empty()
            )
        })
    }));
    let decoded = decode_program(&encode_program(&optimized).unwrap()).unwrap();
    assert_eq!(optimized, decoded);
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
