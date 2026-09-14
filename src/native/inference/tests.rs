use super::*;

#[test]
fn unavailable_arguments_in_unreachable_blocks_use_the_callee_abi() {
    let compilation = crate::compile(
        "func identity(value: List<Int>) -> List<Int> { value }\nfunc main() -> List<Int> { identity([42]) }",
    )
    .unwrap();
    let prepared = crate::native::prepare(&compilation).unwrap();
    let (_, functions, _) = crate::vm::compile_shared(&compilation)
        .unwrap()
        .into_parts();
    let instance = prepared
        .instances
        .iter()
        .find(|instance| instance.key.function == prepared.main)
        .unwrap();
    let mut function = functions[&prepared.main].clone();
    let callee = function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| {
            if let ir::Instruction::Portable(Instruction::Call { function, .. }) =
                &instruction.instruction
            {
                Some(*function)
            } else {
                None
            }
        })
        .unwrap();
    let mut values = function.values.clone().into_builder();
    let empty = values.allocate(ir::Type::Opaque, None);
    let result = values.allocate(ir::Type::Opaque, None);
    function.values = values.finish();
    function.blocks.push(ir::BlockData {
        parameters: vec![empty],
        instructions: vec![
            ir::Instruction::Portable(Instruction::Call {
                destination: result,
                function: callee,
                specialization: Default::default(),
                arguments: vec![empty],
            })
            .with_span(0..0),
        ],
        terminator: ir::Terminator::Return(result),
        terminator_span: 0..0,
    });
    function
        .verify(
            &functions
                .iter()
                .map(|(id, function)| (*id, function.signature.clone()))
                .collect(),
        )
        .unwrap();
    let inferred = infer_value_types(
        &function,
        &[],
        &instance.key,
        prepared.environment(),
        &HashMap::new(),
    )
    .unwrap();
    assert!(inferred.empty.contains(&empty));
    let expected = prepared.function_types[&instance.ir_function].result;
    assert!(matches!(expected, NativeType::Object(_)));
    assert_eq!(inferred.types[empty.index()], expected);
    assert_eq!(inferred.types[result.index()], expected);
}

#[test]
fn inference_is_independent_of_storage_hints_and_block_order() {
    let compilation = crate::compile(
        r#"
func first(value: Int) -> Int { value + 1 }
func second(value: Int) -> Int { value + 2 }
func main() -> Int {
    let count = 0
    loop { count = count + 1
        break if count == 3 }
    let callback = branch count == 3 { true -> first
        _ -> second }
    callback(40)
}
"#,
    )
    .unwrap();
    let prepared = crate::native::prepare(&compilation).unwrap();
    let (_, functions, _) = crate::vm::compile_shared(&compilation)
        .unwrap()
        .into_parts();
    let instance = &prepared
        .instances
        .iter()
        .find(|instance| instance.key.function == prepared.main)
        .unwrap();
    let mut function = functions[&prepared.main].clone();
    let infer = |function: &ir::Function| {
        infer_value_types(
            function,
            &prepared.function_types[&instance.ir_function].parameters,
            &instance.key,
            prepared.environment(),
            &HashMap::new(),
        )
        .unwrap()
    };
    let expected = infer(&function);
    for hint in [None, Some(0)] {
        let mut values = function.values.clone().into_builder();
        for index in 0..values.len() {
            values.set_storage_hint(values.value(index).unwrap(), hint);
        }
        function.values = values.finish();
        assert_eq!(infer(&function), expected);
    }
    let count = function.blocks.len() as u32;
    let remap = |block: &mut ir::Block| block.0 = count - 1 - block.0;
    function.blocks.reverse();
    remap(&mut function.entry);
    for block in &mut function.blocks {
        match &mut block.terminator {
            ir::Terminator::Jump { target, .. } => remap(target),
            ir::Terminator::Branch {
                then_target,
                else_target,
                ..
            } => {
                remap(then_target);
                remap(else_target);
            }
            ir::Terminator::Return(_) => {}
        }
    }
    assert_eq!(infer(&function), expected);
}
