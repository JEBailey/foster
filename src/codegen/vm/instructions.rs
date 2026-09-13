//! Explicit instruction mappings at the VM/SSA boundary.
use super::LowerError;
use super::emission::{Emission, reg};
use crate::codegen::ir::{self, Type, Value};
use crate::codegen::metadata::Constant;
use crate::codegen::types::ExecutableType;
use crate::vm::{self, Register};
use std::ops::Range;
pub(super) fn portable_instruction(
    instruction: &vm::Instruction,
    sources: &[Option<Value>],
    destinations: &[(Register, Value)],
) -> ir::PortableInstruction {
    let source = |register: &Register| {
        sources[usize::from(register.0)].expect("instruction operands were validated above")
    };
    let destination = |register: &Register| {
        destinations
            .iter()
            .find_map(|(candidate, value)| (candidate == register).then_some(*value))
            .expect("instruction destinations were allocated above")
    };
    match instruction {
        vm::Instruction::LoadConstant {
            destination: output,
            constant,
        } => ir::PortableInstruction::LoadConstant {
            destination: destination(output),
            constant: *constant,
        },
        vm::Instruction::Move {
            destination: output,
            source: input,
        } => ir::PortableInstruction::Move {
            destination: destination(output),
            source: source(input),
        },
        vm::Instruction::Unary {
            destination: output,
            operator,
            operand,
        } => ir::PortableInstruction::Unary {
            destination: destination(output),
            operator: *operator,
            operand: source(operand),
        },
        vm::Instruction::Binary {
            destination: output,
            operator,
            left,
            right,
        } => ir::PortableInstruction::Binary {
            destination: destination(output),
            operator: *operator,
            left: source(left),
            right: source(right),
        },
        vm::Instruction::MakeList {
            destination: output,
            element_type,
            elements,
        } => ir::PortableInstruction::MakeList {
            destination: destination(output),
            element_type: element_type.clone(),
            elements: elements.iter().map(source).collect(),
        },
        vm::Instruction::Index {
            destination: output,
            object,
            index,
        } => ir::PortableInstruction::Index {
            destination: destination(output),
            object: source(object),
            index: source(index),
        },
        vm::Instruction::MakeRecord {
            destination: output,
            record,
            type_arguments,
            fields,
        } => ir::PortableInstruction::MakeRecord {
            destination: destination(output),
            record: *record,
            type_arguments: type_arguments.clone(),
            fields: fields
                .iter()
                .map(|(name, register)| (name.clone(), source(register)))
                .collect(),
        },
        vm::Instruction::MakeVariant {
            destination: output,
            variant,
            type_arguments,
            payload,
        } => ir::PortableInstruction::MakeVariant {
            destination: destination(output),
            variant: *variant,
            type_arguments: type_arguments.clone(),
            payload: payload.iter().map(source).collect(),
        },
        vm::Instruction::LoadField {
            destination: output,
            object,
            field,
            by_reference,
        } => ir::PortableInstruction::LoadField {
            destination: destination(output),
            object: source(object),
            field: field.clone(),
            by_reference: *by_reference,
        },
        vm::Instruction::StoreField {
            object,
            field,
            source: input,
        } => ir::PortableInstruction::StoreField {
            object: source(object),
            field: field.clone(),
            source: source(input),
        },
        vm::Instruction::StoreIndex {
            object,
            index,
            source: input,
        } => ir::PortableInstruction::StoreIndex {
            object: source(object),
            index: source(index),
            source: source(input),
        },
        vm::Instruction::MakeReference {
            destination: output,
            pointee_type,
            object,
            index,
        } => ir::PortableInstruction::MakeReference {
            destination: destination(output),
            pointee_type: pointee_type.clone(),
            object: source(object),
            index: source(index),
        },
        vm::Instruction::MakeWholeReference {
            destination: output,
            pointee_type,
            object,
        } => ir::PortableInstruction::MakeWholeReference {
            destination: destination(output),
            pointee_type: pointee_type.clone(),
            object: source(object),
        },
        vm::Instruction::MakeFieldReference {
            destination: output,
            pointee_type,
            object,
            field,
        } => ir::PortableInstruction::MakeFieldReference {
            destination: destination(output),
            pointee_type: pointee_type.clone(),
            object: source(object),
            field: field.clone(),
        },
        vm::Instruction::MoveOut {
            by_reference,
            destination: output,
            source: input,
        } => ir::PortableInstruction::MoveOut {
            by_reference: *by_reference,
            destination: destination(output),
            source: source(input),
        },
        vm::Instruction::Push {
            destination: output,
            object,
            value,
        } => ir::PortableInstruction::Push {
            destination: destination(output),
            object: source(object),
            value: source(value),
        },
        vm::Instruction::Append {
            destination: output,
            object,
            value,
        } => ir::PortableInstruction::Append {
            destination: destination(output),
            object: source(object),
            value: source(value),
        },
        vm::Instruction::Contains {
            destination: output,
            value,
            candidates,
        } => ir::PortableInstruction::Contains {
            destination: destination(output),
            value: source(value),
            candidates: candidates.iter().map(source).collect(),
        },
        vm::Instruction::Builtin {
            destination: output,
            builtin,
            arguments,
        } => ir::PortableInstruction::Builtin {
            destination: destination(output),
            builtin: *builtin,
            arguments: arguments.iter().map(source).collect(),
        },
        vm::Instruction::SpawnRemote {
            destination: output,
            value,
        } => ir::PortableInstruction::SpawnRemote {
            destination: destination(output),
            value: source(value),
        },
        vm::Instruction::SpawnRemoteBorrow {
            destination: output,
            source: input,
        } => ir::PortableInstruction::SpawnRemoteBorrow {
            destination: destination(output),
            source: source(input),
        },
        vm::Instruction::RemoteCall {
            destination: output,
            remote,
            function,
            arguments,
        } => ir::PortableInstruction::RemoteCall {
            destination: destination(output),
            remote: source(remote),
            function: *function,
            arguments: arguments
                .iter()
                .map(|(mode, register)| (*mode, source(register)))
                .collect(),
        },
        vm::Instruction::Await {
            destination: output,
            future,
        } => ir::PortableInstruction::Await {
            destination: destination(output),
            future: source(future),
        },
        vm::Instruction::MatchPattern {
            destination: output,
            subject,
            pattern,
            bindings,
        } => ir::PortableInstruction::MatchPattern {
            destination: destination(output),
            subject: source(subject),
            pattern: pattern.clone(),
            bindings: bindings.iter().map(destination).collect(),
        },
        vm::Instruction::Assert { condition, message } => ir::PortableInstruction::Assert {
            condition: source(condition),
            message: message.as_ref().map(source),
        },
        vm::Instruction::Call {
            destination: output,
            function,
            specialization,
            arguments,
        } => ir::PortableInstruction::Call {
            destination: destination(output),
            function: *function,
            specialization: specialization.clone(),
            arguments: arguments.iter().map(source).collect(),
        },
        vm::Instruction::CallMethod {
            destination: output,
            receiver,
            function,
            specialization,
            arguments,
        } => ir::PortableInstruction::CallMethod {
            destination: destination(output),
            receiver: source(receiver),
            function: *function,
            specialization: specialization.clone(),
            arguments: arguments.iter().map(source).collect(),
        },
        vm::Instruction::CallContractMethod {
            destination: output,
            receiver,
            slot,
            name,
            arguments,
            result_type,
        } => ir::PortableInstruction::CallContractMethod {
            destination: destination(output),
            receiver: source(receiver),
            slot: *slot,
            name: name.clone(),
            arguments: arguments.iter().map(source).collect(),
            result_type: result_type.clone(),
        },
        vm::Instruction::MakeClosure {
            destination: output,
            function,
            specialization,
            captures,
        } => ir::PortableInstruction::MakeClosure {
            destination: destination(output),
            function: *function,
            specialization: specialization.clone(),
            captures: captures
                .iter()
                .map(|(mode, register)| (*mode, source(register)))
                .collect(),
        },
        vm::Instruction::CallValue {
            destination: output,
            callee,
            arguments,
        } => ir::PortableInstruction::CallValue {
            destination: destination(output),
            callee: source(callee),
            arguments: arguments.iter().map(source).collect(),
        },
        vm::Instruction::CallClosure {
            destination: output,
            function,
            specialization,
            captures,
            arguments,
        } => ir::PortableInstruction::CallClosure {
            destination: destination(output),
            function: *function,
            specialization: specialization.clone(),
            captures: captures
                .iter()
                .map(|(mode, register)| (*mode, source(register)))
                .collect(),
            arguments: arguments.iter().map(source).collect(),
        },
        vm::Instruction::Drop { .. }
        | vm::Instruction::Jump { .. }
        | vm::Instruction::JumpIfFalse { .. }
        | vm::Instruction::Return { .. } => unreachable!("handled while sealing control flow"),
    }
}

pub(super) fn lower_instruction(
    instruction: &ir::Instruction,
    registers: &[Option<Register>],
    constants: &mut Vec<Constant>,
    emissions: &mut Vec<Emission>,
    span: Range<usize>,
) -> Result<(), LowerError> {
    let instruction = match instruction {
        ir::Instruction::Constant { destination, value } => {
            let value = match value {
                ir::Constant::Unit => Constant::Unit,
                ir::Constant::Bool(value) => Constant::Bool(*value),
                ir::Constant::Integer(value) => Constant::Integer(*value),
                ir::Constant::Float(value) => Constant::Float(*value),
                ir::Constant::CodePoint(value) => Constant::CodePoint(*value),
                ir::Constant::RuntimeString(_) => {
                    return Err(LowerError(
                        "runtime-string addresses must be legalized before VM lowering".into(),
                    ));
                }
            };
            let constant = constants
                .iter()
                .position(|existing| existing == &value)
                .unwrap_or_else(|| {
                    constants.push(value);
                    constants.len() - 1
                });
            vm::Instruction::LoadConstant {
                destination: reg(registers, *destination),
                constant: u16::try_from(constant)
                    .map_err(|_| LowerError("too many VM constants".into()))?,
            }
        }
        ir::Instruction::Unary {
            destination,
            operator,
            operand,
        } => vm::Instruction::Unary {
            destination: reg(registers, *destination),
            operator: *operator,
            operand: reg(registers, *operand),
        },
        ir::Instruction::IntegerExtend {
            destination,
            operand,
        } => vm::Instruction::Move {
            destination: reg(registers, *destination),
            source: reg(registers, *operand),
        },
        ir::Instruction::Binary {
            destination,
            operator,
            left,
            right,
        } => vm::Instruction::Binary {
            destination: reg(registers, *destination),
            operator: *operator,
            left: reg(registers, *left),
            right: reg(registers, *right),
        },
        ir::Instruction::Call {
            destination,
            function,
            specialization,
            arguments,
        } => vm::Instruction::Call {
            destination: reg(registers, *destination),
            function: *function,
            specialization: specialization.clone(),
            arguments: arguments
                .iter()
                .map(|value| reg(registers, *value))
                .collect(),
        },
        ir::Instruction::RuntimeCall { helper, .. } => {
            return Err(LowerError(format!(
                "runtime helper `{helper}` has no portable VM opcode"
            )));
        }
        ir::Instruction::WrapCallable { .. }
        | ir::Instruction::StringToBytes { .. }
        | ir::Instruction::BoxValue { .. }
        | ir::Instruction::UnboxValue { .. }
        | ir::Instruction::ConvertResultError { .. } => {
            return Err(LowerError(
                "native representation conversion has no portable VM opcode".into(),
            ));
        }
        ir::Instruction::Assert { condition, message } => vm::Instruction::Assert {
            condition: reg(registers, *condition),
            message: message.map(|value| reg(registers, value)),
        },
        ir::Instruction::Portable(instruction) => lower_portable(instruction, registers),
    };
    emissions.push(Emission::Instruction(instruction, span));
    Ok(())
}

fn lower_portable(
    instruction: &ir::PortableInstruction,
    registers: &[Option<Register>],
) -> vm::Instruction {
    let get = |value: &Value| reg(registers, *value);
    match instruction {
        ir::PortableInstruction::Drop { value } => vm::Instruction::Drop {
            register: get(value),
        },
        ir::PortableInstruction::LoadConstant {
            destination,
            constant,
        } => vm::Instruction::LoadConstant {
            destination: get(destination),
            constant: *constant,
        },
        ir::PortableInstruction::Move {
            destination,
            source,
        }
        | ir::PortableInstruction::CopyOnWrite {
            destination,
            source,
        } => vm::Instruction::Move {
            destination: get(destination),
            source: get(source),
        },
        ir::PortableInstruction::Unary {
            destination,
            operator,
            operand,
        } => vm::Instruction::Unary {
            destination: get(destination),
            operator: *operator,
            operand: get(operand),
        },
        ir::PortableInstruction::Binary {
            destination,
            operator,
            left,
            right,
        } => vm::Instruction::Binary {
            destination: get(destination),
            operator: *operator,
            left: get(left),
            right: get(right),
        },
        ir::PortableInstruction::MakeList {
            destination,
            element_type,
            elements,
        } => vm::Instruction::MakeList {
            destination: get(destination),
            element_type: element_type.clone(),
            elements: elements.iter().map(get).collect(),
        },
        ir::PortableInstruction::Index {
            destination,
            object,
            index,
        } => vm::Instruction::Index {
            destination: get(destination),
            object: get(object),
            index: get(index),
        },
        ir::PortableInstruction::MakeRecord {
            destination,
            record,
            type_arguments,
            fields,
        } => vm::Instruction::MakeRecord {
            destination: get(destination),
            record: *record,
            type_arguments: type_arguments.clone(),
            fields: fields
                .iter()
                .map(|(name, value)| (name.clone(), get(value)))
                .collect(),
        },
        ir::PortableInstruction::MakeVariant {
            destination,
            variant,
            type_arguments,
            payload,
        } => vm::Instruction::MakeVariant {
            destination: get(destination),
            variant: *variant,
            type_arguments: type_arguments.clone(),
            payload: payload.iter().map(get).collect(),
        },
        ir::PortableInstruction::LoadField {
            destination,
            object,
            field,
            by_reference,
        } => vm::Instruction::LoadField {
            destination: get(destination),
            object: get(object),
            field: field.clone(),
            by_reference: *by_reference,
        },
        ir::PortableInstruction::StoreField {
            object,
            field,
            source,
        } => vm::Instruction::StoreField {
            object: get(object),
            field: field.clone(),
            source: get(source),
        },
        ir::PortableInstruction::StoreIndex {
            object,
            index,
            source,
        } => vm::Instruction::StoreIndex {
            object: get(object),
            index: get(index),
            source: get(source),
        },
        ir::PortableInstruction::MakeReference {
            destination,
            pointee_type,
            object,
            index,
        } => vm::Instruction::MakeReference {
            destination: get(destination),
            pointee_type: pointee_type.clone(),
            object: get(object),
            index: get(index),
        },
        ir::PortableInstruction::MakeWholeReference {
            destination,
            pointee_type,
            object,
        } => vm::Instruction::MakeWholeReference {
            destination: get(destination),
            pointee_type: pointee_type.clone(),
            object: get(object),
        },
        ir::PortableInstruction::MakeFieldReference {
            destination,
            pointee_type,
            object,
            field,
        } => vm::Instruction::MakeFieldReference {
            destination: get(destination),
            pointee_type: pointee_type.clone(),
            object: get(object),
            field: field.clone(),
        },
        ir::PortableInstruction::MoveOut {
            by_reference,
            destination,
            source,
        } => vm::Instruction::MoveOut {
            by_reference: *by_reference,
            destination: get(destination),
            source: get(source),
        },
        ir::PortableInstruction::Push {
            destination,
            object,
            value,
        } => vm::Instruction::Push {
            destination: get(destination),
            object: get(object),
            value: get(value),
        },
        ir::PortableInstruction::Append {
            destination,
            object,
            value,
        } => vm::Instruction::Append {
            destination: get(destination),
            object: get(object),
            value: get(value),
        },
        ir::PortableInstruction::Contains {
            destination,
            value,
            candidates,
        } => vm::Instruction::Contains {
            destination: get(destination),
            value: get(value),
            candidates: candidates.iter().map(get).collect(),
        },
        ir::PortableInstruction::Builtin {
            destination,
            builtin,
            arguments,
        } => vm::Instruction::Builtin {
            destination: get(destination),
            builtin: *builtin,
            arguments: arguments.iter().map(get).collect(),
        },
        ir::PortableInstruction::SpawnRemote { destination, value } => {
            vm::Instruction::SpawnRemote {
                destination: get(destination),
                value: get(value),
            }
        }
        ir::PortableInstruction::SpawnRemoteBorrow {
            destination,
            source,
        } => vm::Instruction::SpawnRemoteBorrow {
            destination: get(destination),
            source: get(source),
        },
        ir::PortableInstruction::RemoteCall {
            destination,
            remote,
            function,
            arguments,
        } => vm::Instruction::RemoteCall {
            destination: get(destination),
            remote: get(remote),
            function: *function,
            arguments: arguments
                .iter()
                .map(|(mode, value)| (*mode, get(value)))
                .collect(),
        },
        ir::PortableInstruction::Await {
            destination,
            future,
        } => vm::Instruction::Await {
            destination: get(destination),
            future: get(future),
        },
        ir::PortableInstruction::MatchPattern {
            destination,
            subject,
            pattern,
            bindings,
        } => vm::Instruction::MatchPattern {
            destination: get(destination),
            subject: get(subject),
            pattern: pattern.clone(),
            bindings: bindings.iter().map(get).collect(),
        },
        ir::PortableInstruction::Assert { condition, message } => vm::Instruction::Assert {
            condition: get(condition),
            message: message.as_ref().map(get),
        },
        ir::PortableInstruction::Call {
            destination,
            function,
            specialization,
            arguments,
        } => vm::Instruction::Call {
            destination: get(destination),
            function: *function,
            specialization: specialization.clone(),
            arguments: arguments.iter().map(get).collect(),
        },
        ir::PortableInstruction::CallMethod {
            destination,
            receiver,
            function,
            specialization,
            arguments,
        } => vm::Instruction::CallMethod {
            destination: get(destination),
            receiver: get(receiver),
            function: *function,
            specialization: specialization.clone(),
            arguments: arguments.iter().map(get).collect(),
        },
        ir::PortableInstruction::CallContractMethod {
            destination,
            receiver,
            slot,
            name,
            arguments,
            result_type,
        } => vm::Instruction::CallContractMethod {
            destination: get(destination),
            receiver: get(receiver),
            slot: *slot,
            name: name.clone(),
            arguments: arguments.iter().map(get).collect(),
            result_type: result_type.clone(),
        },
        ir::PortableInstruction::MakeClosure {
            destination,
            function,
            specialization,
            captures,
        } => vm::Instruction::MakeClosure {
            destination: get(destination),
            function: *function,
            specialization: specialization.clone(),
            captures: captures
                .iter()
                .map(|(mode, value)| (*mode, get(value)))
                .collect(),
        },
        ir::PortableInstruction::CallValue {
            destination,
            callee,
            arguments,
        } => vm::Instruction::CallValue {
            destination: get(destination),
            callee: get(callee),
            arguments: arguments.iter().map(get).collect(),
        },
        ir::PortableInstruction::CallClosure {
            destination,
            function,
            specialization,
            captures,
            arguments,
        } => vm::Instruction::CallClosure {
            destination: get(destination),
            function: *function,
            specialization: specialization.clone(),
            captures: captures
                .iter()
                .map(|(mode, value)| (*mode, get(value)))
                .collect(),
            arguments: arguments.iter().map(get).collect(),
        },
    }
}

pub(super) fn verification_type(ty: Type) -> ExecutableType {
    match ty {
        Type::Unit => ExecutableType::Unit,
        Type::Bool => ExecutableType::Bool,
        Type::Int => ExecutableType::Integer,
        Type::Float => ExecutableType::Float,
        Type::CodePoint => ExecutableType::CodePoint,
        Type::Byte => ExecutableType::Byte,
        Type::Opaque | Type::String | Type::Object(_) => ExecutableType::Unknown,
    }
}
