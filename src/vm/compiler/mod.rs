use std::collections::HashMap;

use crate::error::FosterError;
use crate::hir::{self, ExprId, FunctionId, LocalId, ResolvedName};
use crate::types::TypeInformation;

use super::{BytecodeFunction, Constant, Instruction, Program, Register};

mod lower;

pub fn compile(compilation: &hir::Compilation) -> Result<Program, FosterError> {
    compile_with_options(compilation, CompileOptions::default())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompileOptions {
    pub optimize: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self { optimize: true }
    }
}

pub fn compile_with_options(
    compilation: &hir::Compilation,
    options: CompileOptions,
) -> Result<Program, FosterError> {
    let closure_captures = compilation
        .hir
        .expressions
        .iter()
        .filter_map(|(_, expression)| match expression {
            hir::Expr::Closure { function, captures } => Some((*function, captures.clone())),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let mut compiler = Compiler {
        hir: &compilation.hir,
        types: &compilation.types,
        program: Program::default(),
        closure_captures,
    };
    for (function, _) in compilation.hir.functions.iter() {
        compiler.compile_function(function)?;
    }
    compiler.program.records = compilation
        .hir
        .records
        .iter()
        .map(|(id, value)| (id, value.name.clone()))
        .collect();
    compiler.program.record_layouts = compilation
        .hir
        .records
        .iter()
        .map(|(id, value)| {
            let mut fields = compilation
                .types
                .record_fields
                .get(&id)
                .cloned()
                .unwrap_or_else(|| {
                    value
                        .fields
                        .iter()
                        .map(|field| field.name.clone())
                        .collect()
                })
                .into_iter()
                .collect::<Vec<_>>();
            fields.sort();
            (
                id,
                std::sync::Arc::new(super::value::RecordLayout::new(fields)),
            )
        })
        .collect();
    compiler.program.string_record = compilation
        .hir
        .module_named("core.string")
        .and_then(|module| compilation.hir.record_named(module, "String"));
    compiler.program.symbol_record = compilation
        .hir
        .module_named("core.symbol")
        .and_then(|module| compilation.hir.record_named(module, "Symbol"));
    let contract_method_keys = compilation
        .types
        .resolved_calls
        .values()
        .filter_map(|call| match call {
            crate::types::ResolvedCall::ContractMethod { dispatch, .. } => Some(dispatch.clone()),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    for (record, definition) in compilation.hir.records.iter() {
        for (function, function_definition) in compilation
            .hir
            .functions
            .iter()
            .filter(|(_, function)| function.module == definition.module)
        {
            let receiver_matches = compilation
                .types
                .function_type(function)
                .and_then(|signature| signature.parameters.first())
                .is_some_and(|ty| {
                    matches!(
                        compilation.types.types[*ty],
                        crate::types::Type::Record { record: receiver, .. }
                            if receiver == record
                    )
                });
            if receiver_matches && function_definition.receiver.is_some() {
                let name = method_name(&function_definition.name);
                if let Some(dispatch_key) = compilation.types.method_dispatch_key(function, name) {
                    compiler
                        .program
                        .methods
                        .insert((record, dispatch_key), function);
                }
            }
        }
        for dispatch in &contract_method_keys {
            if let Some(function) = best_record_method(compilation, record, dispatch) {
                compiler
                    .program
                    .methods
                    .insert((record, dispatch.clone()), function);
            }
        }
    }
    for (variant, definition) in compilation.hir.variant_types.iter() {
        if definition.kind != crate::ast::VariantKind::Enum {
            continue;
        }
        for (function, function_definition) in compilation
            .hir
            .functions
            .iter()
            .filter(|(_, function)| function.module == definition.module)
        {
            let receiver_matches = compilation
                .types
                .function_type(function)
                .and_then(|signature| signature.parameters.first())
                .is_some_and(|ty| {
                    matches!(
                        compilation.types.types[*ty],
                        crate::types::Type::Variant { variant: receiver, .. } if receiver == variant
                    )
                });
            if receiver_matches && function_definition.receiver.is_some() {
                let name = method_name(&function_definition.name);
                if let Some(dispatch_key) = compilation.types.method_dispatch_key(function, name) {
                    compiler
                        .program
                        .variant_methods
                        .insert((variant, dispatch_key), function);
                }
            }
        }
        for dispatch in &contract_method_keys {
            if let Some(function) = best_variant_method(compilation, variant, dispatch) {
                compiler
                    .program
                    .variant_methods
                    .insert((variant, dispatch.clone()), function);
            }
        }
    }
    let variant_type_names = compilation
        .hir
        .variant_types
        .iter()
        .map(|(id, value)| (id, std::sync::Arc::<str>::from(value.name.as_str())))
        .collect::<HashMap<_, _>>();
    compiler.program.variants = compilation
        .hir
        .variants
        .iter()
        .filter(|(_, value)| {
            compilation.hir.variant_types[value.parent].kind == crate::ast::VariantKind::Enum
        })
        .map(|(id, value)| {
            (
                id,
                (
                    value.parent,
                    variant_type_names[&value.parent].clone(),
                    std::sync::Arc::from(value.name.as_str()),
                ),
            )
        })
        .collect();
    compiler.program.main = compilation
        .hir
        .module_named("main")
        .and_then(|module| compilation.hir.function_named(module, "main"));
    compiler.program.main_arguments = compiler
        .program
        .main
        .map(|main| crate::entry::accepts_arguments(compilation, main))
        .transpose()?
        .unwrap_or(false);
    if options.optimize {
        super::optimizer::optimize(&mut compiler.program);
    }
    super::optimizer::insert_drops(&mut compiler.program);
    Ok(compiler.program)
}

fn best_record_method(
    compilation: &crate::hir::Compilation,
    record: crate::hir::RecordId,
    dispatch: &crate::types::MethodKey,
) -> Option<crate::hir::FunctionId> {
    best_dispatch_method(compilation, dispatch, |function| {
        compilation
            .types
            .function_type(function)
            .and_then(|signature| signature.parameters.first())
            .is_some_and(|ty| matches!(compilation.types.types[*ty], crate::types::Type::Record { record: receiver, .. } if receiver == record))
    })
}

fn best_variant_method(
    compilation: &crate::hir::Compilation,
    variant: crate::hir::VariantTypeId,
    dispatch: &crate::types::MethodKey,
) -> Option<crate::hir::FunctionId> {
    best_dispatch_method(compilation, dispatch, |function| {
        compilation
            .types
            .function_type(function)
            .and_then(|signature| signature.parameters.first())
            .is_some_and(|ty| matches!(compilation.types.types[*ty], crate::types::Type::Variant { variant: receiver, .. } if receiver == variant))
    })
}

fn best_dispatch_method(
    compilation: &crate::hir::Compilation,
    dispatch: &crate::types::MethodKey,
    receiver_matches: impl Fn(crate::hir::FunctionId) -> bool,
) -> Option<crate::hir::FunctionId> {
    compilation
        .hir
        .functions
        .iter()
        .filter(|(function, definition)| {
            definition.receiver.is_some() && receiver_matches(*function)
        })
        .filter_map(|(function, definition)| {
            let key = compilation
                .types
                .method_dispatch_key(function, method_name(&definition.name))?;
            method_key_matches(&key, dispatch).then_some((dispatch_generics(&key), function))
        })
        .min_by_key(|(generics, function)| (*generics, function.into_raw().into_u32()))
        .map(|(_, function)| function)
}

fn method_key_matches(
    pattern: &crate::types::MethodKey,
    concrete: &crate::types::MethodKey,
) -> bool {
    let mut generics = HashMap::new();
    pattern.name == concrete.name
        && pattern.parameters.len() == concrete.parameters.len()
        && pattern.parameters.iter().zip(&concrete.parameters).all(
            |((left_mode, left), (right_mode, right))| {
                left_mode == right_mode && dispatch_type_matches(left, right, &mut generics)
            },
        )
}

fn dispatch_type_matches(
    pattern: &crate::types::DispatchTypeKey,
    concrete: &crate::types::DispatchTypeKey,
    generics: &mut HashMap<u32, crate::types::DispatchTypeKey>,
) -> bool {
    use crate::types::DispatchTypeKey;
    if let DispatchTypeKey::Generic(index) = pattern {
        return generics.entry(*index).or_insert_with(|| concrete.clone()) == concrete;
    }
    match (pattern, concrete) {
        (DispatchTypeKey::Reference(left), DispatchTypeKey::Reference(right))
        | (DispatchTypeKey::RawList(left), DispatchTypeKey::RawList(right))
        | (DispatchTypeKey::Sequence(left), DispatchTypeKey::Sequence(right))
        | (DispatchTypeKey::Remote(left), DispatchTypeKey::Remote(right))
        | (DispatchTypeKey::Future(left), DispatchTypeKey::Future(right)) => {
            dispatch_type_matches(left, right, generics)
        }
        (DispatchTypeKey::Record(left, left_args), DispatchTypeKey::Record(right, right_args)) => {
            left == right && dispatch_types_match(left_args, right_args, generics)
        }
        (
            DispatchTypeKey::Variant(left, left_args),
            DispatchTypeKey::Variant(right, right_args),
        ) => left == right && dispatch_types_match(left_args, right_args, generics),
        (DispatchTypeKey::Intersection(left), DispatchTypeKey::Intersection(right)) => {
            dispatch_types_match(left, right, generics)
        }
        (
            DispatchTypeKey::Function(left, left_result),
            DispatchTypeKey::Function(right, right_result),
        ) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|((lm, lt), (rm, rt))| lm == rm && dispatch_type_matches(lt, rt, generics))
                && dispatch_type_matches(left_result, right_result, generics)
        }
        _ => pattern == concrete,
    }
}

fn dispatch_types_match(
    pattern: &[crate::types::DispatchTypeKey],
    concrete: &[crate::types::DispatchTypeKey],
    generics: &mut HashMap<u32, crate::types::DispatchTypeKey>,
) -> bool {
    pattern.len() == concrete.len()
        && pattern
            .iter()
            .zip(concrete)
            .all(|(left, right)| dispatch_type_matches(left, right, generics))
}

fn dispatch_generics(key: &crate::types::MethodKey) -> usize {
    fn count(ty: &crate::types::DispatchTypeKey) -> usize {
        use crate::types::DispatchTypeKey;
        match ty {
            DispatchTypeKey::Generic(_) => 1,
            DispatchTypeKey::Reference(value)
            | DispatchTypeKey::RawList(value)
            | DispatchTypeKey::Sequence(value)
            | DispatchTypeKey::Remote(value)
            | DispatchTypeKey::Future(value) => count(value),
            DispatchTypeKey::Record(_, values)
            | DispatchTypeKey::Intersection(values)
            | DispatchTypeKey::Variant(_, values) => values.iter().map(count).sum(),
            DispatchTypeKey::Function(parameters, result) => {
                parameters.iter().map(|(_, ty)| count(ty)).sum::<usize>() + count(result)
            }
            _ => 0,
        }
    }
    key.parameters.iter().map(|(_, ty)| count(ty)).sum()
}

fn method_name(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(_, member)| member)
}

struct Compiler<'a> {
    hir: &'a hir::PackageHir,
    types: &'a TypeInformation,
    program: Program,
    closure_captures: HashMap<FunctionId, Vec<hir::Capture>>,
}

struct FunctionCompiler<'a> {
    hir: &'a hir::PackageHir,
    types: &'a TypeInformation,
    closure_captures: &'a HashMap<FunctionId, Vec<hir::Capture>>,
    constants: &'a mut Vec<Constant>,
    function: FunctionId,
    locals: HashMap<LocalId, Register>,
    instructions: Vec<Instruction>,
    spans: Vec<std::ops::Range<usize>>,
    next_register: u16,
    loops: Vec<LoopContext>,
}

struct LoopContext {
    start: usize,
    breaks: Vec<usize>,
}

impl Compiler<'_> {
    fn compile_function(&mut self, function_id: FunctionId) -> Result<(), FosterError> {
        let function = &self.hir.functions[function_id];
        let mut lower = FunctionCompiler {
            hir: self.hir,
            types: self.types,
            closure_captures: &self.closure_captures,
            constants: &mut self.program.constants,
            function: function_id,
            locals: HashMap::new(),
            instructions: Vec::new(),
            spans: Vec::new(),
            next_register: 0,
            loops: Vec::new(),
        };
        let captures = self
            .closure_captures
            .get(&function_id)
            .cloned()
            .unwrap_or_default();
        for capture in &captures {
            let register = lower.allocate();
            lower.locals.insert(capture.local, register);
        }
        for parameter in &function.parameters {
            let register = lower.allocate();
            lower.locals.insert(*parameter, register);
        }
        let result = match function.intrinsic.as_deref() {
            Some("list.push" | "list.append") => {
                let [receiver, value] = function.parameters.as_slice() else {
                    return Err(FosterError::runtime(format!(
                        "intrinsic `{}` requires a receiver and one value",
                        function.name
                    )));
                };
                let destination = lower.allocate();
                let instruction = match function.intrinsic.as_deref() {
                    Some("list.push") => Instruction::Push {
                        destination,
                        object: lower.locals[receiver],
                        value: lower.locals[value],
                    },
                    Some("list.append") => Instruction::Append {
                        destination,
                        object: lower.locals[receiver],
                        value: lower.locals[value],
                    },
                    _ => unreachable!(),
                };
                lower.emit(instruction, function.span.clone());
                destination
            }
            _ => {
                let mut result = lower.load_constant(Constant::Unit, function.span.clone())?;
                lower.compile_statements(&function.body, &function.span, &mut result)?;
                result
            }
        };
        let ends_with_unconditional_return = matches!(
            function.body.last(),
            Some(hir::Stmt::Return { guard: None, .. })
        );
        if !ends_with_unconditional_return {
            lower.emit(
                Instruction::Return { source: result },
                function.span.clone(),
            );
        }
        self.program.functions.insert(
            function_id,
            BytecodeFunction {
                name: function.name.clone(),
                parameters: function.parameters.len() as u16,
                parameter_modes: self
                    .types
                    .function_type(function_id)
                    .map(|signature| signature.parameter_modes.clone())
                    .unwrap_or_else(|| {
                        vec![crate::ast::ParameterMode::Borrow; function.parameters.len()]
                    }),
                mutable_parameters: function
                    .parameters
                    .iter()
                    .enumerate()
                    .map(|(index, _)| {
                        let Some(signature) = self.types.function_type(function_id) else {
                            return false;
                        };
                        if signature.parameter_modes[index] != crate::ast::ParameterMode::Borrow {
                            return false;
                        }
                        let crate::types::Type::Reference { group, .. } =
                            &self.types.types[signature.parameters[index]]
                        else {
                            return false;
                        };
                        function.effects.iter().any(|effect| {
                            matches!(
                                effect.kind,
                                crate::ast::EffectKind::Mut | crate::ast::EffectKind::Reshape
                            ) && effect.target.root == *group
                        })
                    })
                    .collect(),
                captures: captures.len() as u16,
                registers: lower.next_register,
                instructions: lower.instructions,
                instruction_spans: lower.spans,
            },
        );
        Ok(())
    }
}

impl FunctionCompiler<'_> {
    pub(super) fn compile_statements(
        &mut self,
        statements: &crate::block::Block<hir::Stmt>,
        fallback_span: &std::ops::Range<usize>,
        result: &mut Register,
    ) -> Result<(), FosterError> {
        for (statement, statement_span) in statements.iter_spanned() {
            let span = if statement_span.is_empty() {
                fallback_span.clone()
            } else {
                statement_span.clone()
            };
            match statement {
                hir::Stmt::Return { value, guard } => {
                    if let Some(guard) = guard {
                        let condition = self.expression(*guard)?;
                        let jump = self.emit(
                            Instruction::JumpIfFalse {
                                condition,
                                target: 0,
                            },
                            span.clone(),
                        );
                        *result = self.expression(*value)?;
                        self.emit(Instruction::Return { source: *result }, span);
                        let target = self.instructions.len();
                        self.patch_target(jump, target)?;
                    } else {
                        *result = self.expression(*value)?;
                        self.emit(Instruction::Return { source: *result }, span);
                    }
                }
                hir::Stmt::Assert { condition, message } => {
                    let condition = self.expression(*condition)?;
                    let message = message
                        .map(|message| self.expression(message))
                        .transpose()?;
                    self.emit(Instruction::Assert { condition, message }, span.clone());
                    *result = self.load_constant(Constant::Unit, span)?;
                }
                hir::Stmt::Loop { body } => {
                    let cfg = crate::control_flow::LoopCfg::new();
                    let mut offsets = [0; 3];
                    offsets[cfg.header.0] = self.instructions.len();
                    offsets[cfg.body.0] = self.instructions.len();
                    self.loops.push(LoopContext {
                        start: offsets[cfg.header.0],
                        breaks: Vec::new(),
                    });
                    self.compile_statements(body, &span, result)?;
                    self.emit(
                        Instruction::Jump {
                            target: offsets[cfg.header.0],
                        },
                        span.clone(),
                    );
                    offsets[cfg.exit.0] = self.instructions.len();
                    *result = self.load_constant(Constant::Unit, span)?;
                    let context = self.loops.pop().expect("a loop context was pushed");
                    for jump in context.breaks {
                        self.patch_target(jump, offsets[cfg.exit.0])?;
                    }
                }
                hir::Stmt::Break { guard } => {
                    self.compile_break(*guard, span)?;
                }
                hir::Stmt::Continue { guard } => {
                    self.compile_continue(*guard, span)?;
                }
                hir::Stmt::Bind { local, value } => {
                    let destination = self.allocate();
                    self.locals.insert(*local, destination);
                    let value = self.expression(*value)?;
                    self.emit(
                        Instruction::Move {
                            destination,
                            source: value,
                        },
                        span,
                    );
                    *result = destination;
                }
                hir::Stmt::Assign { local, value } => {
                    let value = self.expression(*value)?;
                    let destination = self.locals[local];
                    self.emit(
                        Instruction::Move {
                            destination,
                            source: value,
                        },
                        span,
                    );
                    *result = destination;
                }
                hir::Stmt::Expr(value) => *result = self.expression(*value)?,
                hir::Stmt::Set { place, value } => {
                    let value = self.expression(*value)?;
                    self.store_place(*place, value, span.clone())?;
                    *result = value;
                }
            }
        }
        Ok(())
    }

    fn compile_break(
        &mut self,
        guard: Option<ExprId>,
        span: std::ops::Range<usize>,
    ) -> Result<(), FosterError> {
        let skip = if let Some(guard) = guard {
            let condition = self.expression(guard)?;
            Some(self.emit(
                Instruction::JumpIfFalse {
                    condition,
                    target: 0,
                },
                span.clone(),
            ))
        } else {
            None
        };
        self.loops
            .last()
            .ok_or_else(|| FosterError::runtime("loop transfer has no enclosing loop"))?;
        let jump = self.emit(Instruction::Jump { target: 0 }, span);
        self.loops
            .last_mut()
            .expect("loop context exists")
            .breaks
            .push(jump);
        if let Some(skip) = skip {
            self.patch_target(skip, self.instructions.len())?;
        }
        Ok(())
    }

    fn compile_continue(
        &mut self,
        guard: Option<ExprId>,
        span: std::ops::Range<usize>,
    ) -> Result<(), FosterError> {
        let skip = if let Some(guard) = guard {
            let condition = self.expression(guard)?;
            Some(self.emit(
                Instruction::JumpIfFalse {
                    condition,
                    target: 0,
                },
                span.clone(),
            ))
        } else {
            None
        };
        let target = self
            .loops
            .last()
            .ok_or_else(|| FosterError::runtime("continue has no enclosing loop"))?
            .start;
        self.emit(Instruction::Jump { target }, span);
        if let Some(skip) = skip {
            self.patch_target(skip, self.instructions.len())?;
        }
        Ok(())
    }

    pub(super) fn compile_branch_body(
        &mut self,
        arm: &hir::BranchArm,
        fallback_span: &std::ops::Range<usize>,
    ) -> Result<Register, FosterError> {
        let mut result = self.load_constant(Constant::Unit, fallback_span.clone())?;
        self.compile_statements(&arm.body, fallback_span, &mut result)?;
        Ok(result)
    }
}
