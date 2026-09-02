use std::collections::HashMap;

use crate::compiler::Compilation;
use crate::error::FosterError;
use crate::hir::{self, ExprId, FunctionId, LocalId, ResolvedName};
use crate::intrinsics::{Intrinsic, OpcodeIntrinsic};
use crate::types::TypeInformation;

use super::{
    BytecodeFunction, Constant, Instruction, Program, Register, RuntimeRecord, RuntimeVariant,
    VerificationType,
};

mod lower;

pub fn compile(compilation: &Compilation) -> Result<Program, FosterError> {
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

fn opcode_intrinsic_instruction(
    intrinsic: OpcodeIntrinsic,
    destination: Register,
    receiver: Register,
    value: Register,
) -> Instruction {
    match intrinsic {
        OpcodeIntrinsic::ListPush => Instruction::Push {
            destination,
            object: receiver,
            value,
        },
        OpcodeIntrinsic::ListAppend => Instruction::Append {
            destination,
            object: receiver,
            value,
        },
    }
}

pub fn compile_with_options(
    compilation: &Compilation,
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
                RuntimeRecord {
                    name: value.name.clone(),
                    layout: std::sync::Arc::new(super::value::RecordLayout::new(fields)),
                },
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
    compiler.program.dispatch = compilation.types.dispatch.clone();
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
                RuntimeVariant {
                    parent: value.parent,
                    type_name: variant_type_names[&value.parent].clone(),
                    alternative: std::sync::Arc::from(value.name.as_str()),
                },
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
        .map(|main| crate::entry::accepts_arguments(&compilation.hir, &compilation.types, main))
        .transpose()?
        .unwrap_or(false);
    // Freeze all aggregate field/tag/capture/place layouts before any backend rewrite.  The VM
    // consumes the canonical operand order; native compilation consumes the same registry when
    // deciding whether a representation has been legalized for Cranelift.
    crate::codegen::layout::legalize(&mut compiler.program)?;
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
        let capture_types = captures
            .iter()
            .map(|capture| {
                let ty = self
                    .types
                    .local_type(capture.local)
                    .map(|ty| verification_type(self.hir, self.types, ty, 0))
                    .unwrap_or(VerificationType::Unknown);
                if capture.mode == crate::hir::CaptureMode::Ref
                    && !matches!(ty, VerificationType::Reference(_))
                {
                    VerificationType::Reference(Box::new(ty))
                } else {
                    ty
                }
            })
            .collect::<Vec<_>>();
        for capture in &captures {
            let register = lower.allocate();
            lower.locals.insert(capture.local, register);
        }
        for parameter in &function.parameters {
            let register = lower.allocate();
            lower.locals.insert(*parameter, register);
        }
        let intrinsic = function.intrinsic.as_deref().and_then(Intrinsic::from_key);
        let result = match intrinsic.and_then(Intrinsic::opcode) {
            Some(opcode) => {
                let [receiver, value] = function.parameters.as_slice() else {
                    return Err(FosterError::runtime(format!(
                        "intrinsic `{}` requires a receiver and one value",
                        function.name
                    )));
                };
                let destination = lower.allocate();
                let instruction = opcode_intrinsic_instruction(
                    opcode,
                    destination,
                    lower.locals[receiver],
                    lower.locals[value],
                );
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
                intrinsic_stub: matches!(intrinsic, Some(Intrinsic::Builtin(_))),
                parameters: function.parameters.len() as u16,
                parameter_types: self
                    .types
                    .function_type(function_id)
                    .map(|signature| {
                        signature
                            .parameters
                            .iter()
                            .map(|ty| verification_type(self.hir, self.types, *ty, 0))
                            .collect()
                    })
                    .unwrap_or_else(|| vec![VerificationType::Unknown; function.parameters.len()]),
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
                returns_reference: self
                    .types
                    .function_type(function_id)
                    .is_some_and(|signature| {
                        matches!(
                            self.types.types[signature.result],
                            crate::types::Type::Reference { .. }
                        )
                    }),
                captures: captures.len() as u16,
                capture_types,
                result_type: self
                    .types
                    .function_type(function_id)
                    .map(|signature| verification_type(self.hir, self.types, signature.result, 0))
                    .unwrap_or(VerificationType::Unknown),
                registers: lower.next_register,
                instructions: lower.instructions,
                instruction_spans: lower.spans,
            },
        );
        Ok(())
    }
}

fn verification_type(
    hir: &hir::PackageHir,
    information: &TypeInformation,
    ty: crate::types::TypeId,
    depth: usize,
) -> VerificationType {
    if depth >= 64 {
        return VerificationType::Unknown;
    }
    let nested = |ty| verification_type(hir, information, ty, depth + 1);
    match &information.types[ty] {
        crate::types::Type::Generic(_)
        | crate::types::Type::Intersection(_)
        | crate::types::Type::Module(_) => VerificationType::Unknown,
        crate::types::Type::Unit => VerificationType::Unit,
        crate::types::Type::Bool => VerificationType::Bool,
        crate::types::Type::Int => VerificationType::Integer,
        crate::types::Type::Float => VerificationType::Float,
        crate::types::Type::CodePoint => VerificationType::CodePoint,
        crate::types::Type::Byte => VerificationType::Byte,
        crate::types::Type::RawBytes => VerificationType::Bytes,
        crate::types::Type::RawByteBuffer => VerificationType::ByteBuffer,
        crate::types::Type::Reference { value, .. } => {
            VerificationType::Reference(Box::new(nested(*value)))
        }
        crate::types::Type::RawList(value) => VerificationType::List(Box::new(nested(*value))),
        // Sequence is a structural view implemented by multiple runtime representations.
        crate::types::Type::Sequence(_) => VerificationType::Unknown,
        crate::types::Type::Remote(value) => VerificationType::Remote(Box::new(nested(*value))),
        crate::types::Type::Future(value) => VerificationType::Future(Box::new(nested(*value))),
        crate::types::Type::Function(function) => VerificationType::Function {
            parameters: function.parameters.iter().map(|ty| nested(*ty)).collect(),
            parameter_modes: function.parameter_modes.clone(),
            result: Box::new(nested(function.result)),
        },
        crate::types::Type::Record { record, arguments } => {
            match information.record_names.get(record).map(String::as_str) {
                Some("List") => VerificationType::List(Box::new(
                    arguments
                        .first()
                        .copied()
                        .map(nested)
                        .unwrap_or(VerificationType::Unknown),
                )),
                Some("Bytes") => VerificationType::Bytes,
                Some("ByteBuffer") => VerificationType::ByteBuffer,
                // Method-only records are structural contracts and carry no unique runtime
                // representation. Their conformance proof has already been checked.
                _ if hir.records[*record].fields.is_empty() => VerificationType::Unknown,
                _ => VerificationType::Record(*record),
            }
        }
        crate::types::Type::Variant { variant, .. }
            if hir.variant_types[*variant].kind == crate::ast::VariantKind::Union =>
        {
            // Unions are erased structural views; their value keeps its member representation.
            VerificationType::Unknown
        }
        crate::types::Type::Variant { variant, .. } => VerificationType::Variant(*variant),
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
