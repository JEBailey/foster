//! The immutable boundary between specialization/verification and native emission.
use super::*;

/// Logical types and parameter ownership retained after scalar/pointer legalization.
#[derive(Debug, Clone)]
pub struct LogicalSignature {
    pub captures: Vec<VerificationType>,
    pub parameters: Vec<VerificationType>,
    pub parameter_modes: Vec<ParameterMode>,
    pub result: VerificationType,
}

/// One verified specialization. Only preparation can construct or modify it.
pub struct NativeFunction {
    pub(super) instance: NativeInstance,
    pub(super) ir: ir::Function,
    pub(super) mutable_parameter_homes: HashSet<u16>,
    pub(super) home_types: std::collections::BTreeMap<u16, NativeType>,
    logical_signature: LogicalSignature,
    management: Vec<MemoryManagement>,
    // Compact logical evidence keyed by construction storage home, retaining CFG alternatives.
    logical_register_types: Vec<Vec<VerificationType>>,
}

impl NativeFunction {
    pub fn ir(&self) -> &ir::Function {
        &self.ir
    }
    pub fn source_function(&self) -> FunctionId {
        self.instance.key.function
    }
    pub fn specialization(&self) -> &vm::Specialization {
        &self.instance.key.substitutions
    }
    pub fn logical_signature(&self) -> &LogicalSignature {
        &self.logical_signature
    }
    /// Lifetime-management policy for each value in the legalized SSA function.
    pub fn management(&self) -> &[MemoryManagement] {
        &self.management
    }
    /// Verified logical alternatives for each construction register (before ABI lowering).
    pub fn logical_register_types(&self) -> &[Vec<VerificationType>] {
        &self.logical_register_types
    }
    /// Source-level logical alternatives for an SSA value, when it has a source storage home.
    /// ABI-only temporaries have no separate Foster identity and return an empty slice.
    pub fn logical_types(&self, value: ir::Value) -> &[VerificationType] {
        self.ir.storage_hints[value.0 as usize]
            .map_or(&[], |home| &self.logical_register_types[usize::from(home)])
    }
}

/// A reusable host-native program, fully lowered and verified before any object is emitted.
///
/// Rendering and object emission consume the same immutable functions, layouts, and call targets.
/// The source compilation is borrowed only for nominal/source metadata used by the emitter.
pub struct NativeProgram<'a> {
    pub(super) compilation: &'a Compilation,
    pub(super) program: Program,
    pub(super) layouts: LayoutRegistry,
    pub(super) physical_layouts: PhysicalRegistry,
    pub(super) main: FunctionId,
    pub(super) instances: Vec<NativeInstance>,
    pub(super) instance_ids: HashMap<SpecializationKey, FunctionId>,
    pub(super) function_types: HashMap<FunctionId, ir::Signature>,
    pub(super) builtin_result_types: HashMap<crate::intrinsics::Builtin, VerificationType>,
    pub(super) runtime_strings: Vec<String>,
    pub(super) runtime_string_indices: HashMap<u16, u64>,
    pub(super) runtime_literal_indices: HashMap<String, u64>,
    pub(super) functions: Vec<NativeFunction>,
}

/// Prepare once, then render or emit any number of objects without repeating specialization.
pub fn prepare(compilation: &Compilation) -> Result<NativeProgram<'_>, FosterError> {
    let shared = vm::compile_shared(compilation)?;
    let mut program = shared.metadata;
    let mut layouts = crate::codegen::layout::legalize(&mut program)?;
    let main = program
        .main
        .ok_or_else(|| native_error("native compilation requires a `main` function"))?;
    let instances = reachable_instances(compilation, &program, &shared.functions, main)?;
    let instance_ids = instances
        .iter()
        .map(|instance| (instance.key.clone(), instance.ir_function))
        .collect();
    let builtin_result_types = native_builtin_result_types(compilation)?;
    let function_types = collect_function_types(
        compilation,
        &program,
        &instances,
        &builtin_result_types,
        &mut layouts,
    )?;
    let physical_layouts =
        PhysicalRegistry::build(&layouts, TargetLayout::host()).map_err(|error| {
            native_error(format!("cannot calculate native object layouts: {error}"))
        })?;
    validate_program(compilation, &program, &instances, &function_types, &layouts)?;
    let (runtime_strings, runtime_string_indices, runtime_literal_indices) =
        runtime_strings(&program);
    let mut prepared = NativeProgram {
        compilation,
        program,
        layouts,
        physical_layouts,
        main,
        instances,
        instance_ids,
        function_types,
        builtin_result_types,
        runtime_strings,
        runtime_string_indices,
        runtime_literal_indices,
        functions: Vec::new(),
    };
    let mut states = None;
    for instance in &prepared.instances {
        let source = &prepared.program.functions[&instance.key.function];
        if states
            .as_ref()
            .is_none_or(|(function, _)| *function != instance.key.function)
        {
            states = Some((
                instance.key.function,
                vm::type_states(&prepared.program, source)?,
            ));
        }
        let source_states = &states.as_ref().unwrap().1;
        let environment = prepared.environment();
        let lowered = lower_shared_to_native_ir(
            &shared.functions[&instance.key.function],
            source,
            source_states,
            &prepared.function_types[&instance.ir_function],
            &instance.key,
            environment,
        )?;
        lowered.verify(&prepared.function_types).map_err(|error| {
            native_error(format!("invalid native IR for `{}`: {error}", source.name))
        })?;
        for instruction in lowered.blocks.iter().flat_map(|block| &block.instructions) {
            if let ir::Instruction::RuntimeCall {
                helper, signature, ..
            } = instruction
            {
                abi::verify_call(helper, signature).map_err(native_error)?;
            }
        }
        let layouts = NativeLayouts {
            program: &prepared.program,
            logical: &prepared.layouts,
            physical: &prepared.physical_layouts,
        };
        let management = lowered
            .value_types
            .iter()
            .map(|ty| layouts.management(*ty))
            .collect();
        let parameter_offset = lowered
            .parameters
            .len()
            .saturating_sub(source.mutable_parameters.len());
        let mut mutable_parameter_homes = lowered.parameters[parameter_offset..]
            .iter()
            .zip(&source.mutable_parameters)
            .filter_map(|(value, mutable)| {
                mutable
                    .then(|| lowered.storage_hints[value.0 as usize])
                    .flatten()
            })
            .collect::<HashSet<_>>();
        if compilation.hir.functions[instance.key.function]
            .receiver
            .is_some()
            && let Some(receiver) = lowered.parameters.get(parameter_offset)
            && let Some(home) = lowered.storage_hints[receiver.0 as usize]
        {
            mutable_parameter_homes.insert(home);
        }
        let mut home_types = std::collections::BTreeMap::new();
        for (value, home) in lowered.storage_hints.iter().enumerate() {
            if let Some(home) = home {
                home_types
                    .entry(*home)
                    .or_insert(lowered.value_types[value]);
            }
        }
        let specialize = |ty: &VerificationType| ty.specialize(&instance.key.substitutions);
        let mut logical_register_types = vec![BTreeSet::new(); usize::from(source.registers)];
        for registers in source_states.iter().flatten() {
            for (types, ty) in logical_register_types.iter_mut().zip(registers) {
                if let Some(ty) = ty {
                    types.insert(specialize(ty));
                }
            }
        }
        let logical_register_types = logical_register_types
            .into_iter()
            .map(|types| types.into_iter().collect())
            .collect();
        prepared.functions.push(NativeFunction {
            instance: instance.clone(),
            ir: lowered,
            mutable_parameter_homes,
            home_types,
            management,
            logical_signature: LogicalSignature {
                captures: source.capture_types.iter().map(specialize).collect(),
                parameters: source.parameter_types.iter().map(specialize).collect(),
                parameter_modes: source.parameter_modes.clone(),
                result: specialize(&source.result_type),
            },
            logical_register_types,
        });
    }
    Ok(prepared)
}

impl NativeProgram<'_> {
    pub fn functions(&self) -> &[NativeFunction] {
        &self.functions
    }
    pub fn layouts(&self) -> &LayoutRegistry {
        &self.layouts
    }
    pub fn physical_layouts(&self) -> &PhysicalRegistry {
        &self.physical_layouts
    }

    pub fn emit_ir(&self) -> String {
        use std::fmt::Write;
        let mut output = String::from("foster-codegen-ir 1\n\n");
        for function in &self.functions {
            writeln!(
                output,
                "; function #{} {:?}\n{}\n",
                function.source_function().into_raw().into_u32(),
                function.specialization(),
                function.ir
            )
            .unwrap();
        }
        output
    }

    pub fn compile_object(&self, options: CompileOptions) -> Result<ObjectArtifact, FosterError> {
        emit_object(self, options)
    }

    pub fn build_executable(
        &self,
        output: impl AsRef<Path>,
        options: CompileOptions,
    ) -> Result<(), FosterError> {
        runtime::link_executable(self.compile_object(options)?, output.as_ref(), options)
    }

    pub(super) fn environment(&self) -> NativeIrEnvironment<'_> {
        NativeIrEnvironment {
            compilation: self.compilation,
            program: &self.program,
            function_types: &self.function_types,
            runtime_string_indices: &self.runtime_string_indices,
            runtime_literal_indices: &self.runtime_literal_indices,
            layouts: &self.layouts,
            physical_layouts: &self.physical_layouts,
            instances: &self.instance_ids,
            builtin_result_types: &self.builtin_result_types,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_prepared_program_renders_and_emits_without_mutating_its_ir() {
        let compilation = crate::compile(
            r#"
type Box<T> = { value: T }
func identity<T>(value: T) -> T { value }
func sum(left: Box<Int>, right: Box<Int>, extra: Int) -> Int {
    left.value + right.value + extra
}
func main() -> Int { sum(identity(Box { value: 20 }), Box { value: 20 }, 2) }
"#,
        )
        .unwrap();
        let prepared = prepare(&compilation).unwrap();
        let rendered = prepared.emit_ir();
        assert_eq!(rendered, super::super::emit_ir(&compilation).unwrap());
        let functions = prepared.functions.as_ptr();
        for optimize in [false, true] {
            let first = prepared
                .compile_object(CompileOptions { optimize })
                .unwrap();
            let second = prepared
                .compile_object(CompileOptions { optimize })
                .unwrap();
            assert!(
                first.bytes == second.bytes,
                "repeated emission changed object bytes"
            );
            assert_eq!(first.result, NativeType::Int);
            assert_eq!(prepared.functions.as_ptr(), functions);
            assert_eq!(prepared.emit_ir(), rendered);
        }
    }

    #[test]
    fn prepared_functions_retain_logical_identity_and_management_policy() {
        let compilation = crate::compile(
            r#"
type Box<T> = { value: T }
func identity<T>(value: T) -> T { value }
func main() -> Int {
    assert(identity(:hello) == :hello)
    assert(identity("hello") == "hello")
    identity(Box { value: 42 }).value
}
"#,
        )
        .unwrap();
        let prepared = prepare(&compilation).unwrap();
        let string = VerificationType::Record {
            record: prepared.program.string_record.unwrap(),
            arguments: Vec::new(),
        };
        let symbol = VerificationType::Record {
            record: prepared.program.symbol_record.unwrap(),
            arguments: Vec::new(),
        };
        for expected in [&string, &symbol] {
            let function = prepared
                .functions()
                .iter()
                .find(|function| {
                    function.ir.name.starts_with("identity")
                        && &function.logical_signature().result == expected
                })
                .unwrap();
            assert_eq!(function.ir.signature.result, NativeType::String);
            assert!(
                function
                    .logical_register_types()
                    .iter()
                    .any(|types| types.contains(expected))
            );
        }
        assert!(prepared.functions().iter().any(|function| {
            function
                .management()
                .iter()
                .any(|policy| matches!(policy, MemoryManagement::ManagedObject(_)))
        }));
        assert!(prepared.functions().iter().any(|function| {
            function
                .management()
                .contains(&MemoryManagement::UnmanagedRuntime)
        }));
        for function in prepared.functions() {
            assert_eq!(function.management().len(), function.ir.value_types.len());
            assert_eq!(
                function.logical_signature().parameter_modes.len(),
                function.logical_signature().parameters.len()
            );
        }
    }

    #[test]
    fn missing_entry_is_rejected_by_preparation() {
        let compilation = crate::compile("func helper() -> Int { 42 }").unwrap();
        let error = prepare(&compilation).err().unwrap();
        assert!(error.message.contains("requires a `main` function"));
    }
}
