//! Pass-specific barriers. Value folding does not change storage identity.

use crate::vm::Instruction;

/// Only these instructions can carry scalar facts through unchanged storage.
/// Unknown/new operations are barriers until their effects have been audited.
pub(super) fn invalidates_facts(instruction: &Instruction) -> bool {
    !matches!(
        instruction,
        Instruction::LoadConstant { .. }
            | Instruction::Move { .. }
            | Instruction::Unary { .. }
            | Instruction::Binary { .. }
            | Instruction::Jump { .. }
            | Instruction::JumpIfFalse { .. }
            | Instruction::Return { .. }
    )
}
