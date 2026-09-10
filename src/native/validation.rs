//! Reject unsupported programs before native emission.
use super::{
    Compilation, FosterError, FunctionId, HashMap, Instruction, LayoutRegistry, NativeInstance,
    Program, instruction_name, ir, native_error,
};

pub(super) fn validate_program(
    compilation: &Compilation,
    program: &Program,
    instances: &[NativeInstance],
    function_types: &HashMap<FunctionId, ir::Signature>,
    layouts: &LayoutRegistry,
) -> Result<(), FosterError> {
    let main = program.main.expect("validated above");
    let main_function = &program.functions[&main];
    if main_function.parameters != u16::from(program.main_arguments) || main_function.captures != 0
    {
        return Err(native_error(
            "native `main` must take no parameters or one `std.process.Arguments` parameter",
        ));
    }
    for instance in instances {
        let body = &program.functions[&instance.key.function];
        if usize::from(body.captures) + usize::from(body.parameters)
            != function_types[&instance.ir_function].parameters.len()
        {
            return Err(native_error(format!(
                "parameter metadata for `{}` is inconsistent",
                body.name
            )));
        }
        for (index, instruction) in body.instructions.iter().enumerate() {
            let supported = matches!(
                instruction,
                Instruction::Drop { .. }
                    | Instruction::LoadConstant { .. }
                    | Instruction::Move { .. }
                    | Instruction::Unary { .. }
                    | Instruction::Binary { .. }
                    | Instruction::Jump { .. }
                    | Instruction::JumpIfFalse { .. }
                    | Instruction::Assert { .. }
                    | Instruction::Call { .. }
                    | Instruction::CallMethod { .. }
                    | Instruction::CallClosure { .. }
                    | Instruction::MakeClosure { .. }
                    | Instruction::CallValue { .. }
                    | Instruction::MakeRecord { .. }
                    | Instruction::MakeVariant { .. }
                    | Instruction::MakeList { .. }
                    | Instruction::LoadField { .. }
                    | Instruction::StoreField { .. }
                    | Instruction::StoreIndex { .. }
                    | Instruction::MatchPattern { .. }
                    | Instruction::Index { .. }
                    | Instruction::MakeReference { .. }
                    | Instruction::MakeWholeReference { .. }
                    | Instruction::MakeFieldReference { .. }
                    | Instruction::MoveOut { .. }
                    | Instruction::Push { .. }
                    | Instruction::Append { .. }
                    | Instruction::Contains { .. }
                    | Instruction::Builtin { .. }
                    | Instruction::SpawnRemote { .. }
                    | Instruction::SpawnRemoteBorrow { .. }
                    | Instruction::RemoteCall { .. }
                    | Instruction::Await { .. }
                    | Instruction::CallContractMethod { .. }
                    | Instruction::Return { .. }
            );
            if !supported {
                let layout_note = match instruction {
                    Instruction::MakeRecord { record, .. } => layouts
                        .record(*record)
                        .map(|layout| format!(" (legalized as boxed layout l{})", layout.0))
                        .unwrap_or_default(),
                    Instruction::MakeVariant { variant, .. } => program
                        .variants
                        .get(variant)
                        .and_then(|variant| layouts.variant(variant.parent))
                        .map(|layout| format!(" (legalized as boxed layout l{})", layout.0))
                        .unwrap_or_default(),
                    Instruction::MakeClosure { function, .. } => layouts
                        .closure(*function)
                        .map(|layout| format!(" (legalized as boxed layout l{})", layout.0))
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                let mut error = native_error(format!(
                    "native compilation of `{}` does not yet support instruction `{}`{}",
                    body.name,
                    instruction_name(instruction),
                    layout_note
                ))
                .with_help("use `foster build` without `--native` for the complete VM language");
                if let Some(span) = body.instruction_spans.get(index) {
                    error =
                        error.with_primary_label(span.clone(), "unsupported in the native backend");
                }
                return Err(error);
            }
            if let Instruction::Builtin { builtin, .. } = instruction
                && builtin.descriptor().native == crate::intrinsics::NativeIntrinsic::Unavailable
            {
                return Err(native_error(format!(
                    "native compilation of `{}` has no lowering for intrinsic `{builtin:?}`",
                    body.name
                ))
                .with_help("move the algorithm into Foster or register a typed native primitive"));
            }
        }
    }
    let _ = compilation;
    Ok(())
}
