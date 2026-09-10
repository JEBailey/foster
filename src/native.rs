//! Ahead-of-time native compilation through Cranelift.
//!
//! The native backend deliberately accepts a smaller language surface than the VM. Unsupported
//! operations are diagnosed before an object is emitted, which keeps the portable bytecode VM as
//! the reference implementation while native support grows.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;

use cranelift_codegen::ir::{
    AbiParam, Block as ClifBlock, InstBuilder, MemFlagsData, Signature as ClifSignature, StackSlot,
    StackSlotData, StackSlotKind, Type as ClifType, Value as ClifValue, types,
};
use cranelift_codegen::ir::{condcodes::FloatCC, condcodes::IntCC};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::FunctionBuilderContext;
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};
use la_arena::RawIdx;

use crate::ast::{BinaryOp, ParameterMode, UnaryOp};
use crate::codegen::ir;
use crate::codegen::layout::physical::{
    AlternativeLayout, DropField, DropPlan, PhysicalKind, PhysicalRegistry, ScalarKind,
    TargetLayout, ValueLayout, ValueSemantic,
};
use crate::codegen::layout::{LayoutId, LayoutKind, Registry as LayoutRegistry};
use crate::compiler::Compilation;
use crate::error::FosterError;
use crate::hir::{FunctionId, Pattern};
use crate::types::{Type, TypeId};
use crate::vm::{
    self, BytecodeFunction, Constant, Instruction, Program, Register, VerificationType,
};

pub mod abi;
mod buffers;
use buffers::{
    allocate_native_buffer, allocate_native_buffer_dynamic, allocate_native_byte_buffer,
    allocate_native_bytes, append_native_buffer, clone_native_buffer, copy_native_bytes,
    native_buffer_element_address, native_buffer_tail, native_bytes_tail, push_native_buffer,
};
mod fields;
use fields::lower_native_field;
mod handles;
use handles::{
    allocate_native_handle, native_buffer_layout, native_bytes_layout, native_handle_layout,
    native_reference_receiver, native_release_address,
};
mod host;
use host::{NativeHostArguments, lower_native_host_intrinsic};
mod inference;
use inference::{
    dereference_native_type, field_type, infer_register_types, native_intrinsic_result_type,
    native_verification_type,
};
mod legalize;
use legalize::{
    ErasedConversion, callable_conversion, erased_conversion, lower_shared_to_native_ir,
};
mod literals;
use literals::{instruction_name, runtime_strings};
mod lowering;
use lowering::{lower_native_ir, native_type_from_value_layout, native_type_semantic};
mod machine;
use machine::{
    cranelift_representation, cranelift_type, load_physical_value, native_field_helper,
    physical_cranelift_type, reference_load_helper, reference_store_helper, runtime_signature,
    signature, store_physical_value,
};
mod operations;
use operations::{
    fail_if, lower_binary, lower_native_terminator, propagate_native_failure, runtime_call,
    write_native_newline, write_native_separator, write_native_value, zero_i64,
};
mod portable;
use portable::lower_portable_native;
mod remote;
use remote::{
    lower_native_await, lower_native_remote_call, lower_native_spawn_remote,
    lower_result_error_conversion,
};
mod representation;
use representation::{
    concrete_closure_result, concrete_native_type, instruction_layout_type,
    native_builtin_result_types, native_type, record_uses_dynamic_dispatch,
    specialized_verification_type,
};
mod specialization;
use specialization::{
    FlowFacts, VerifiedRemoteCall, collect_function_types, contract_argument_matches,
    contract_candidates, reachable_instances, resolve_specialization, verification_type_for_native,
    verified_remote_calls,
};
mod thunks;
use thunks::{
    declare_callable_thunks, declare_remote_thunks, define_callable_thunks, define_function,
    define_remote_thunks, native_to_remote_word, remote_word_to_native,
};
mod validation;
use validation::validate_program;

mod cleanup;
mod copy;
mod dispatch;
use cleanup::{FailureCleanup, NativeBuilder as FunctionBuilder};
mod emission;
mod text_boundary;
use emission::{emit_object, ordered_entries};
mod ownership;
mod program;
mod runtime;
mod runtime_cache;
pub use ownership::MemoryManagement;
use ownership::*;
pub use program::{LogicalSignature, NativeFunction, NativeProgram, prepare};

/// Primitive Foster values supported by the native ABI.
pub use crate::codegen::ir::Type as NativeType;

/// Controls machine-code optimization performed by Cranelift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompileOptions {
    pub optimize: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self { optimize: true }
    }
}

/// A linkable native object and the type returned by its exported entry point.
#[derive(Debug)]
pub struct ObjectArtifact {
    pub bytes: Vec<u8>,
    pub result: NativeType,
    pub accepts_arguments: bool,
    runtime_strings: Vec<String>,
    releases_result: bool,
}

/// Immutable inputs for specialization and representation legalization.
#[derive(Clone, Copy)]
struct NativeIrEnvironment<'a> {
    compilation: &'a Compilation,
    program: &'a Program,
    function_types: &'a HashMap<FunctionId, ir::Signature>,
    runtime_string_indices: &'a HashMap<u16, u64>,
    runtime_literal_indices: &'a HashMap<String, u64>,
    layouts: &'a LayoutRegistry,
    physical_layouts: &'a PhysicalRegistry,
    instances: &'a HashMap<SpecializationKey, FunctionId>,
    builtin_result_types: &'a HashMap<crate::intrinsics::Builtin, crate::vm::VerificationType>,
}

/// Shared immutable state for lowering one module's functions to Cranelift.
struct NativeBackend<'a> {
    ir: NativeIrEnvironment<'a>,
    functions: &'a HashMap<FunctionId, FuncId>,
    callable_thunks: &'a HashMap<LayoutId, FuncId>,
    remote_thunks: &'a HashMap<FunctionId, FuncId>,
    release_thunks: &'a HashMap<LayoutId, FuncId>,
    objects: ObjectRuntime<'a>,
}

#[derive(Clone, Copy)]
struct PatternSubject {
    value: ClifValue,
    ty: NativeType,
}

#[derive(Clone, Copy)]
struct NativeLowering<'a, 'backend> {
    function: &'a ir::Function,
    values: &'a HashMap<ir::Value, ClifValue>,
    homes: &'a HashMap<u16, StackSlot>,
    mutable_parameter_homes: &'a HashSet<u16>,
    backend: &'a NativeBackend<'backend>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct SpecializationKey {
    function: FunctionId,
    substitutions: crate::vm::Specialization,
}

#[derive(Debug, Clone)]
struct NativeInstance {
    key: SpecializationKey,
    ir_function: FunctionId,
}

#[derive(Debug, Clone, Copy)]
struct ContractCandidate {
    layout: LayoutId,
    implementation: FunctionId,
    function: FunctionId,
}

/// Render the same verified program consumed by native object emission.
pub fn emit_ir(compilation: &Compilation) -> Result<String, FosterError> {
    Ok(prepare(compilation)?.emit_ir())
}

/// Prepare and compile the reachable portion of main to a host-native object.
pub fn compile_object(
    compilation: &Compilation,
    options: CompileOptions,
) -> Result<ObjectArtifact, FosterError> {
    prepare(compilation)?.compile_object(options)
}

/// Compile and link a standalone host executable using the installed Rust linker toolchain.
pub fn build_executable(
    compilation: &Compilation,
    output: impl AsRef<Path>,
    options: CompileOptions,
) -> Result<(), FosterError> {
    prepare(compilation)?.build_executable(output, options)
}

fn absolute_path(path: &Path) -> Result<PathBuf, FosterError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(path))
            .map_err(|error| native_error(format!("cannot resolve output path: {error}")))
    }
}

fn native_error(message: impl Into<String>) -> FosterError {
    FosterError::runtime(message).with_code("E0900")
}

struct TemporaryDirectory {
    path: PathBuf,
}

impl TemporaryDirectory {
    fn create() -> Result<Self, FosterError> {
        static NEXT_DIRECTORY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = format!(
            "foster-native-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| native_error(format!("system clock error: {error}")))?
                .as_nanos(),
            NEXT_DIRECTORY.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(unique);
        fs::create_dir(&path).map_err(|error| {
            native_error(format!(
                "cannot create native build directory `{}`: {error}",
                path.display()
            ))
        })?;
        Ok(Self { path })
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_ir_is_ssa_and_control_flow_carries_block_arguments() {
        let compilation = crate::compile(
            r#"
func main() -> Int {
    let value = 0
    loop {
        value = value + 1
        break if value == 3
    }
    value
}
"#,
        )
        .unwrap();
        let prepared = prepare(&compilation).unwrap();
        let program = &prepared.program;
        let main = prepared.main;
        let function = prepared
            .functions()
            .iter()
            .find(|function| function.source_function() == main)
            .unwrap()
            .ir();
        let function_types = &prepared.function_types;

        assert!(function.blocks.len() > 1);
        function.verify(function_types).unwrap();
        let mut definitions = function.parameters.iter().copied().collect::<HashSet<_>>();
        let mut has_branch = false;
        let mut has_back_edge = false;
        let mut has_pruned_parameters = false;
        for (block_index, block) in function.blocks.iter().enumerate() {
            has_pruned_parameters |=
                block.parameters.len() < usize::from(program.functions[&main].registers);
            for parameter in &block.parameters {
                assert!(definitions.insert(*parameter));
            }
            for instruction in &block.instructions {
                for destination in instruction.destinations() {
                    assert!(definitions.insert(destination));
                }
            }
            match &block.terminator {
                ir::Terminator::Jump { target, arguments } => {
                    assert_eq!(
                        arguments.len(),
                        function.blocks[target.0 as usize].parameters.len()
                    );
                    has_back_edge |= target.0 as usize <= block_index;
                }
                ir::Terminator::Branch {
                    then_target,
                    then_arguments,
                    else_target,
                    else_arguments,
                    ..
                } => {
                    has_branch = true;
                    assert_eq!(
                        then_arguments.len(),
                        function.blocks[then_target.0 as usize].parameters.len()
                    );
                    assert_eq!(
                        else_arguments.len(),
                        function.blocks[else_target.0 as usize].parameters.len()
                    );
                    has_back_edge |= then_target.0 as usize <= block_index
                        || else_target.0 as usize <= block_index;
                }
                ir::Terminator::Return(_) => {}
            }
        }
        assert_eq!(definitions.len(), function.value_types.len());
        assert!(has_branch);
        assert!(has_back_edge);
        assert!(has_pruned_parameters);
        assert_eq!(NativeType::Bool.representation(), ir::Representation::I8);
        assert_eq!(NativeType::Float.representation(), ir::Representation::F64);
    }
}
