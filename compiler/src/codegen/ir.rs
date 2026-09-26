pub use foster_bytecode::codegen::ir::*;

#[cfg(test)]
pub(crate) fn test_value(index: u32) -> Value {
    let mut values = ValueBuilder::default();
    for _ in 0..=index {
        values.allocate(Type::Unit, None);
    }
    values.value(index as usize).unwrap()
}
