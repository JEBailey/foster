use super::*;

use crate::vm::{Machine, compile};

#[test]
fn round_trips_and_executes_compiled_program() {
    let source = "type Choice = | Left(Int) | Right(Int)\n\
            func unwrap(value: Choice) -> Int { branch value { Choice.Left(number) -> number _ -> 0 } }\n\
            func main() -> Int {\n let values = [20, 22]\n unwrap(Choice.Left(values[0] + values[1]))\n }";
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
fn rejects_invalid_envelopes() {
    assert!(decode_program(b"not bytecode").is_err());
    let compilation = crate::compile("func main() -> Int { 42 }").unwrap();
    let program = compile(&compilation).unwrap();
    let mut bytes = encode_program(&program).unwrap();
    bytes[8] = 99;
    assert!(
        decode_program(&bytes)
            .unwrap_err()
            .to_string()
            .contains("version")
    );
}
