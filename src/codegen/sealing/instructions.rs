use crate::codegen::ir::{self, Value};
use crate::codegen::storage::{self, Slot};
pub(super) fn portable_instruction(
    instruction: &storage::Instruction,
    sources: &[Option<Value>],
    destinations: &[(Slot, Value)],
) -> ir::PortableInstruction {
    let source = |register: &Slot| {
        sources[usize::from(register.0)].expect("instruction operands were validated above")
    };
    let destination = |register: &Slot| {
        destinations
            .iter()
            .find_map(|(candidate, value)| (candidate == register).then_some(*value))
            .expect("instruction destinations were allocated above")
    };
    match instruction {
        storage::Instruction::LoadConstant {
            destination: output,
            constant,
        } => ir::PortableInstruction::LoadConstant {
            destination: destination(output),
            constant: *constant,
        },
        storage::Instruction::Move {
            destination: output,
            source: input,
        } => ir::PortableInstruction::Move {
            destination: destination(output),
            source: source(input),
        },
        storage::Instruction::Unary {
            destination: output,
            operator,
            operand,
        } => ir::PortableInstruction::Unary {
            destination: destination(output),
            operator: *operator,
            operand: source(operand),
        },
        storage::Instruction::Binary {
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
        storage::Instruction::MakeList {
            destination: output,
            element_type,
            elements,
        } => ir::PortableInstruction::MakeList {
            destination: destination(output),
            element_type: element_type.clone(),
            elements: elements.iter().map(source).collect(),
        },
        storage::Instruction::Index {
            destination: output,
            object,
            index,
        } => ir::PortableInstruction::Index {
            destination: destination(output),
            object: source(object),
            index: source(index),
        },
        storage::Instruction::MakeRecord {
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
        storage::Instruction::MakeVariant {
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
        storage::Instruction::LoadField {
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
        storage::Instruction::StoreField {
            object,
            field,
            source: input,
        } => ir::PortableInstruction::StoreField {
            object: source(object),
            field: field.clone(),
            source: source(input),
        },
        storage::Instruction::StoreIndex {
            object,
            index,
            source: input,
        } => ir::PortableInstruction::StoreIndex {
            object: source(object),
            index: source(index),
            source: source(input),
        },
        storage::Instruction::MakeReference {
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
        storage::Instruction::MakeWholeReference {
            destination: output,
            pointee_type,
            object,
        } => ir::PortableInstruction::MakeWholeReference {
            destination: destination(output),
            pointee_type: pointee_type.clone(),
            object: source(object),
        },
        storage::Instruction::MakeFieldReference {
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
        storage::Instruction::MoveOut {
            by_reference,
            destination: output,
            source: input,
        } => ir::PortableInstruction::MoveOut {
            by_reference: *by_reference,
            destination: destination(output),
            source: source(input),
        },
        storage::Instruction::Push {
            destination: output,
            object,
            value,
        } => ir::PortableInstruction::Push {
            destination: destination(output),
            object: source(object),
            value: source(value),
        },
        storage::Instruction::Append {
            destination: output,
            object,
            value,
        } => ir::PortableInstruction::Append {
            destination: destination(output),
            object: source(object),
            value: source(value),
        },
        storage::Instruction::Contains {
            destination: output,
            value,
            candidates,
        } => ir::PortableInstruction::Contains {
            destination: destination(output),
            value: source(value),
            candidates: candidates.iter().map(source).collect(),
        },
        storage::Instruction::Builtin {
            destination: output,
            builtin,
            arguments,
        } => ir::PortableInstruction::Builtin {
            destination: destination(output),
            builtin: *builtin,
            arguments: arguments.iter().map(source).collect(),
        },
        storage::Instruction::SpawnRemote {
            destination: output,
            value,
        } => ir::PortableInstruction::SpawnRemote {
            destination: destination(output),
            value: source(value),
        },
        storage::Instruction::SpawnRemoteBorrow {
            destination: output,
            source: input,
        } => ir::PortableInstruction::SpawnRemoteBorrow {
            destination: destination(output),
            source: source(input),
        },
        storage::Instruction::RemoteCall {
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
        storage::Instruction::Await {
            destination: output,
            future,
        } => ir::PortableInstruction::Await {
            destination: destination(output),
            future: source(future),
        },
        storage::Instruction::MatchPattern {
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
        storage::Instruction::Assert { condition, message } => ir::PortableInstruction::Assert {
            condition: source(condition),
            message: message.as_ref().map(source),
        },
        storage::Instruction::Call {
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
        storage::Instruction::CallMethod {
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
        storage::Instruction::CallContractMethod {
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
        storage::Instruction::MakeClosure {
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
        storage::Instruction::CallValue {
            destination: output,
            callee,
            arguments,
        } => ir::PortableInstruction::CallValue {
            destination: destination(output),
            callee: source(callee),
            arguments: arguments.iter().map(source).collect(),
        },
        storage::Instruction::CallClosure {
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
        storage::Instruction::Drop { .. }
        | storage::Instruction::Jump { .. }
        | storage::Instruction::JumpIfFalse { .. }
        | storage::Instruction::Return { .. } => unreachable!("handled while sealing control flow"),
    }
}
