use super::*;

use crate::vm::{CompileOptions, Machine, compile, compile_with_options};

#[test]
fn alternatives_keep_tag_16_and_reject_noncanonical_encodings() {
    use ExecutableType as T;
    let alternatives = T::alternatives(vec![T::Integer, T::Bool]);
    let mut writer = Writer { bytes: Vec::new() };
    writer.verification_type(&alternatives).unwrap();
    assert_eq!(writer.bytes, vec![16, 2, 0, 0, 0, 2, 3]);
    assert_eq!(
        Reader {
            bytes: &writer.bytes,
            offset: 0
        }
        .verification_type(0)
        .unwrap(),
        alternatives
    );
    for members in [
        vec![],
        vec![T::Integer],
        vec![T::Integer, T::Bool],
        vec![T::Bool, T::Bool],
        vec![T::Unknown, T::Bool],
        vec![T::Integer, alternatives],
    ] {
        let mut writer = Writer { bytes: Vec::new() };
        writer.u8(16);
        writer.u32(members.len()).unwrap();
        for member in members {
            writer.verification_type(&member).unwrap();
        }
        assert!(
            Reader {
                bytes: &writer.bytes,
                offset: 0
            }
            .verification_type(0)
            .unwrap_err()
            .to_string()
            .contains("non-canonical")
        );
    }
}

#[test]
fn native_views_cannot_be_serialized_as_alternatives() {
    use ExecutableType as T;
    for view in [
        T::intersection(vec![T::Bool, T::Integer]),
        T::AliasArguments {
            alias: id(0),
            arguments: vec![T::Bool, T::Integer],
        },
    ] {
        let mut writer = Writer { bytes: Vec::new() };
        assert!(
            writer
                .verification_type(&T::List(Box::new(view)))
                .unwrap_err()
                .to_string()
                .contains("native structural metadata")
        );
    }
}

#[test]
fn decoder_rejects_noncanonical_specialization_names() {
    for names in [["Z", "A"], ["T", "T"]] {
        let mut writer = Writer { bytes: Vec::new() };
        writer.u32(names.len()).unwrap();
        for name in names {
            writer.string(name).unwrap();
            writer.verification_type(&ExecutableType::Integer).unwrap();
        }
        let error = Reader {
            bytes: &writer.bytes,
            offset: 0,
        }
        .specialization()
        .unwrap_err();
        assert!(error.to_string().contains("unsorted or duplicate"));
    }
}

#[test]
fn decoder_rejects_missing_and_extra_callable_modes_even_when_nested() {
    for modes in [0, 2] {
        for nested in [false, true] {
            let mut writer = Writer { bytes: Vec::new() };
            if nested {
                writer.u8(13);
                writer.u32(1).unwrap();
            }
            writer.u8(13);
            writer.u32(1).unwrap();
            writer.verification_type(&ExecutableType::Integer).unwrap();
            writer.u32(modes).unwrap();
            for _ in 0..modes {
                writer.parameter_mode(ParameterMode::Consume);
            }
            writer.verification_type(&ExecutableType::Unit).unwrap();
            if nested {
                writer.u32(1).unwrap();
                writer.parameter_mode(ParameterMode::Borrow);
                writer.verification_type(&ExecutableType::Unit).unwrap();
            }
            let error = Reader {
                bytes: &writer.bytes,
                offset: 0,
            }
            .verification_type(0)
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("parameter types and modes must align"),
                "{error}"
            );
        }
    }
}

#[test]
fn callable_encoding_preserves_the_existing_two_vector_format() {
    let callable = ExecutableType::Function {
        parameters: vec![
            crate::types::Parameter {
                ty: ExecutableType::Integer,
                mode: ParameterMode::Consume,
            },
            crate::types::Parameter {
                ty: ExecutableType::Bool,
                mode: ParameterMode::Borrow,
            },
        ],
        result: Box::new(ExecutableType::Unit),
    };
    let mut expected = Writer { bytes: Vec::new() };
    expected.u8(13);
    expected.u32(2).unwrap();
    expected
        .verification_type(&ExecutableType::Integer)
        .unwrap();
    expected.verification_type(&ExecutableType::Bool).unwrap();
    expected.u32(2).unwrap();
    expected.parameter_mode(ParameterMode::Consume);
    expected.parameter_mode(ParameterMode::Borrow);
    expected.verification_type(&ExecutableType::Unit).unwrap();
    let mut actual = Writer { bytes: Vec::new() };
    actual.verification_type(&callable).unwrap();
    assert_eq!(actual.bytes, expected.bytes);
    assert_eq!(
        Reader {
            bytes: &actual.bytes,
            offset: 0
        }
        .verification_type(0)
        .unwrap(),
        callable
    );
}

#[test]
fn round_trips_and_executes_generic_contract_result_types() {
    let compilation = crate::compile(
        "func first<T>(values: Sequence<T>) -> T { values.head() }\nfunc main() -> Int { first([42]) }",
    ).unwrap();
    for optimize in [false, true] {
        let program = compile_with_options(&compilation, CompileOptions { optimize }).unwrap();
        let decoded = decode_program(&encode_program(&program).unwrap()).unwrap();
        assert_eq!(program, decoded);
        assert_eq!(
            Machine::new(&decoded).run_main().unwrap(),
            crate::vm::Value::Integer(42)
        );
    }
}

#[test]
fn rejects_contract_result_metadata_that_disagrees_with_its_use() {
    let compilation = crate::compile(
        "func first(values: Sequence<Int>) -> Int { values.head() }\nfunc main() -> Int { first([42]) }",
    ).unwrap();
    let mut program =
        compile_with_options(&compilation, CompileOptions { optimize: false }).unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    let function = compilation.hir.function_named(module, "first").unwrap();
    let result = program
        .functions
        .get_mut(&function)
        .unwrap()
        .instructions
        .iter_mut()
        .find_map(|instruction| match instruction {
            Instruction::CallContractMethod {
                name, result_type, ..
            } if name == "head" => Some(result_type),
            _ => None,
        })
        .unwrap();
    *result = ExecutableType::Bool;
    let error = verify(&program).unwrap_err();
    assert!(
        error
            .message
            .contains("return value type Bool, expected Integer"),
        "{}",
        error.message
    );
}

#[test]
fn decoder_rejects_a_forged_specialized_method_return_type() {
    let compilation = crate::compile(
        "type Echo<T> = { value: T }\nimpl Echo {\n    func get<T>(self: Echo<T>) -> T { self.value }\n}\nfunc main() -> Bool { Echo { value: true }.get() }",
    ).unwrap();
    let program = compile_with_options(&compilation, CompileOptions { optimize: false }).unwrap();
    let mut bytes = encode_program(&program).unwrap();
    let mut main = program.functions[&program.metadata.main.unwrap()].clone();
    let mut original = Writer { bytes: Vec::new() };
    original.function(&main).unwrap();
    let offsets = bytes
        .windows(original.bytes.len())
        .enumerate()
        .filter_map(|(index, window)| (window == original.bytes).then_some(index))
        .collect::<Vec<_>>();
    assert_eq!(offsets.len(), 1);
    main.result_type = ExecutableType::Integer;
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
        .metadata
        .records
        .values()
        .find(|record| record.name == "Box")
        .unwrap();
    assert_eq!(
        record
            .fields()
            .iter()
            .map(|field| field.ty.clone())
            .collect::<Vec<_>>(),
        vec![ExecutableType::Generic("T".into())]
    );
    assert_eq!(record.parameters, vec!["T"]);
    let some = program
        .metadata
        .variants
        .values()
        .find(|variant| variant.alternative.as_ref() == "Some")
        .unwrap();
    assert_eq!(some.payload, vec![ExecutableType::Generic("T".into())]);
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
                        == &Specialization::try_new(vec![("T".into(), ExecutableType::Integer)]).unwrap()
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
                        if type_arguments == &vec![ExecutableType::Generic("T".into())]
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

#[test]
fn remote_outcomes_round_trip_with_nominal_metadata() {
    let compilation = crate::compile(
        r#"
import core.result
import core.remote_error
type Worker = {}
impl Worker {
    func fail(self) -> Int {
        assert(false, "remote")
        42
    }
}
func main() -> Bool {
    let worker = remote Worker {}
    (await worker.fail()) == Result.Error(RemoteError.Failed("assertion failed: remote"))
}
"#,
    )
    .unwrap();
    let program = compile(&compilation).unwrap();
    let decoded = decode_program(&encode_program(&program).unwrap()).unwrap();
    assert_eq!(decoded, program);
    assert_eq!(
        Machine::new(&decoded).run_main().unwrap(),
        crate::vm::Value::Bool(true)
    );
    let mut malformed = program.clone();
    malformed.metadata.remote_error = None;
    assert!(crate::vm::verify(&malformed).is_err());
    let mut malformed = program;
    malformed.metadata.remote_error = malformed.metadata.remote_result;
    assert!(crate::vm::verify(&malformed).is_err());
}

#[test]
fn round_trip_preserves_core_identities_with_same_named_user_records() {
    let compilation = crate::compile(include_str!(
        "../../../tests/fixtures/programs/core_name_collisions.fos"
    ))
    .unwrap();
    for optimize in [false, true] {
        let program = compile_with_options(&compilation, CompileOptions { optimize }).unwrap();
        let decoded = decode_program(&encode_program(&program).unwrap()).unwrap();
        assert_eq!(program, decoded);
        assert_eq!(decoded.metadata.bytes_record, compilation.types.core.bytes);
        assert_eq!(decoded.metadata.list_record, compilation.types.core.list);
        assert_eq!(
            Machine::new(&decoded).run_main().unwrap(),
            crate::vm::Value::Integer(42)
        );
    }
}
