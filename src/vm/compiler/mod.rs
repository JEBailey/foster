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
    compiler.program.string_record = compilation
        .hir
        .module_named("core.string")
        .and_then(|module| compilation.hir.record_named(module, "String"));
    for (record, definition) in compilation.hir.records.iter() {
        for (name, function) in &compilation.hir.modules[definition.module].functions {
            let receiver_matches = compilation
                .types
                .function_type(*function)
                .and_then(|signature| signature.parameters.first())
                .is_some_and(|ty| {
                    matches!(
                        compilation.types.types[*ty],
                        crate::types::Type::Record { record: receiver, .. }
                            if receiver == record
                    )
                });
            if receiver_matches
                && compilation.hir.functions[*function]
                    .parameters
                    .first()
                    .is_some_and(|parameter| compilation.hir.locals[*parameter].name == "self")
            {
                compiler
                    .program
                    .methods
                    .insert((record, name.clone()), *function);
            }
        }
    }
    compiler.program.variants = compilation
        .hir
        .variants
        .iter()
        .map(|(id, value)| {
            (
                id,
                (
                    compilation.hir.variant_types[value.parent].name.clone(),
                    value.name.clone(),
                ),
            )
        })
        .collect();
    compiler.program.main = compilation
        .hir
        .module_named("main")
        .and_then(|module| compilation.hir.function_named(module, "main"));
    if options.optimize {
        super::optimizer::optimize(&mut compiler.program);
    }
    super::optimizer::insert_drops(&mut compiler.program);
    Ok(compiler.program)
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
        let mut result = lower.load_constant(Constant::Unit, function.span.clone())?;
        for (index, statement) in function.body.iter().enumerate() {
            let span = function
                .statement_spans
                .get(index)
                .cloned()
                .unwrap_or_else(|| function.span.clone());
            match statement {
                hir::Stmt::Return { value, guard } => {
                    if let Some(guard) = guard {
                        let condition = lower.expression(*guard)?;
                        let jump = lower.emit(
                            Instruction::JumpIfFalse {
                                condition,
                                target: 0,
                            },
                            span.clone(),
                        );
                        result = lower.expression(*value)?;
                        lower.emit(Instruction::Return { source: result }, span);
                        let target = lower.instructions.len();
                        lower.patch_target(jump, target)?;
                    } else {
                        result = lower.expression(*value)?;
                        lower.emit(Instruction::Return { source: result }, span);
                    }
                }
                hir::Stmt::Bind { local, value } => {
                    let destination = lower.allocate();
                    lower.locals.insert(*local, destination);
                    let value = lower.expression(*value)?;
                    lower.emit(
                        Instruction::Move {
                            destination,
                            source: value,
                        },
                        span,
                    );
                    result = destination;
                }
                hir::Stmt::Assign { local, value } => {
                    let value = lower.expression(*value)?;
                    let destination = lower.locals[local];
                    lower.emit(
                        Instruction::Move {
                            destination,
                            source: value,
                        },
                        span,
                    );
                    result = destination;
                }
                hir::Stmt::Expr(value) => result = lower.expression(*value)?,
                hir::Stmt::Set { place, value } => {
                    let value = lower.expression(*value)?;
                    lower.store_place(*place, value, span.clone())?;
                    result = value;
                }
            }
        }
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
                mutable_parameters: function
                    .parameters
                    .iter()
                    .enumerate()
                    .map(|(index, parameter)| {
                        let name = &self.hir.locals[*parameter].name;
                        self.types
                            .function_type(function_id)
                            .is_some_and(|signature| {
                                signature.parameter_modes[index]
                                    == crate::ast::ParameterMode::Borrow
                            })
                            && function.effects.iter().any(|effect| {
                                effect.kind == crate::ast::EffectKind::Mut
                                    && effect.target.root == *name
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
