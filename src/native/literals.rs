//! Diagnostic instruction names and runtime literal collection.
use super::{Constant, HashMap, Instruction, Pattern, Program};

pub(super) fn instruction_name(instruction: &Instruction) -> &'static str {
    match instruction {
        Instruction::MakeList { .. } => "MakeList",
        Instruction::Index { .. } => "Index",
        Instruction::MakeRecord { .. } => "MakeRecord",
        Instruction::MakeVariant { .. } => "MakeVariant",
        Instruction::LoadField { .. } => "LoadField",
        Instruction::StoreField { .. } => "StoreField",
        Instruction::StoreIndex { .. } => "StoreIndex",
        Instruction::MakeReference { .. } => "MakeReference",
        Instruction::MakeWholeReference { .. } => "MakeWholeReference",
        Instruction::MakeFieldReference { .. } => "MakeFieldReference",
        Instruction::MoveOut { .. } => "MoveOut",
        Instruction::Push { .. } => "Push",
        Instruction::Append { .. } => "Append",
        Instruction::Contains { .. } => "Contains",
        Instruction::Builtin { .. } => "Builtin",
        Instruction::SpawnRemote { .. } => "SpawnRemote",
        Instruction::SpawnRemoteBorrow { .. } => "SpawnRemoteBorrow",
        Instruction::RemoteCall { .. } => "RemoteCall",
        Instruction::Await { .. } => "Await",
        Instruction::MatchPattern { .. } => "MatchPattern",
        Instruction::Assert { .. } => "Assert",
        Instruction::CallContractMethod { .. } => "CallContractMethod",
        Instruction::MakeClosure { .. } => "MakeClosure",
        Instruction::CallValue { .. } => "CallValue",
        Instruction::CallClosure { .. } => "CallClosure",
        _ => "supported instruction",
    }
}

pub(super) fn runtime_strings(
    program: &Program,
) -> (Vec<String>, HashMap<u16, u64>, HashMap<String, u64>) {
    let mut values = Vec::new();
    let mut indices = HashMap::new();
    let mut literals = HashMap::new();
    for (index, constant) in program.constants.iter().enumerate() {
        if let Constant::String(value) | Constant::Symbol(value) = constant {
            indices.insert(index as u16, values.len() as u64);
            literals.entry(value.clone()).or_insert(values.len() as u64);
            values.push(value.clone());
        }
    }
    let mut functions = program.functions.iter().collect::<Vec<_>>();
    functions.sort_unstable_by_key(|(function, _)| function.into_raw().into_u32());
    for (_, function) in functions {
        for pattern in function
            .instructions
            .iter()
            .filter_map(|instruction| match instruction {
                Instruction::MatchPattern { pattern, .. } => Some(pattern),
                _ => None,
            })
        {
            collect_pattern_literals(pattern, &mut |literal| {
                if !literals.contains_key(literal) {
                    let index = values.len() as u64;
                    literals.insert(literal.to_owned(), index);
                    values.push(literal.to_owned());
                }
            });
        }
    }
    (values, indices, literals)
}

fn collect_pattern_literals(pattern: &Pattern, visit: &mut impl FnMut(&str)) {
    match pattern.unspanned() {
        Pattern::String(value) | Pattern::Symbol(value) => visit(value),
        Pattern::Variant { fields, .. } => {
            for field in fields {
                collect_pattern_literals(field, visit);
            }
        }
        Pattern::Spanned { .. } => unreachable!(),
        Pattern::Wildcard
        | Pattern::Binding(_)
        | Pattern::Bool(_)
        | Pattern::Integer(_)
        | Pattern::Float(_)
        | Pattern::CodePoint(_) => {}
    }
}
