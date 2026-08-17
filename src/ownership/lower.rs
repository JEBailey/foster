use crate::hir::{self, BranchTest, CaptureMode, ExprId, FunctionId, ResolvedName};
use crate::types::TypeInformation;

use super::{BasicBlock, BlockId, Function, Operation, Program, Terminator, UseMode};

#[derive(Clone, Copy)]
enum Context {
    Read,
    Borrow,
    Consume,
}

pub(super) fn lower(hir: &hir::PackageHir, types: &TypeInformation) -> Program {
    let mut program = Program::default();
    let captures = hir
        .expressions
        .iter()
        .filter_map(|(_, expression)| match expression {
            hir::Expr::Closure { function, captures } => Some((
                *function,
                captures
                    .iter()
                    .map(|capture| capture.local)
                    .collect::<Vec<_>>(),
            )),
            _ => None,
        })
        .collect::<std::collections::HashMap<_, _>>();
    for (function, _) in hir.functions.iter() {
        program.functions.insert(
            function,
            Builder::new(
                hir,
                types,
                function,
                captures.get(&function).map_or(&[], Vec::as_slice),
            )
            .lower(),
        );
    }
    program
}

struct Builder<'a> {
    hir: &'a hir::PackageHir,
    types: &'a TypeInformation,
    function: FunctionId,
    blocks: Vec<BasicBlock>,
    current: BlockId,
    captures: &'a [hir::LocalId],
}

impl<'a> Builder<'a> {
    fn new(
        hir: &'a hir::PackageHir,
        types: &'a TypeInformation,
        function: FunctionId,
        captures: &'a [hir::LocalId],
    ) -> Self {
        Self {
            hir,
            types,
            function,
            blocks: vec![BasicBlock::default()],
            current: 0,
            captures,
        }
    }

    fn lower(mut self) -> Function {
        let definition = &self.hir.functions[self.function];
        for parameter in &definition.parameters {
            self.initialize(*parameter, definition.span.clone());
        }
        for capture in self.captures {
            self.initialize(*capture, definition.span.clone());
        }
        for (index, statement) in definition.body.iter().enumerate() {
            self.statement(statement, index + 1 == definition.body.len());
        }
        if matches!(
            self.blocks[self.current].terminator,
            Terminator::Unreachable
        ) {
            self.blocks[self.current].terminator = Terminator::Return;
        }
        Function {
            entry: 0,
            blocks: self.blocks,
        }
    }

    fn statement(&mut self, statement: &hir::Stmt, is_last: bool) {
        match statement {
            hir::Stmt::Return { value, guard } => {
                if let Some(guard) = guard {
                    self.expression(*guard, Context::Read);
                    let returned = self.block();
                    let continued = self.block();
                    self.terminate(Terminator::Branch(vec![returned, continued]));
                    self.current = returned;
                    self.expression(*value, Context::Consume);
                    self.terminate(Terminator::Return);
                    self.current = continued;
                } else {
                    self.expression(*value, Context::Consume);
                    self.terminate(Terminator::Return);
                    self.current = self.block();
                }
            }
            hir::Stmt::Bind { local, value } => {
                self.expression(*value, Context::Consume);
                self.initialize(*local, self.span(*value));
            }
            hir::Stmt::Assign { local, value } => {
                self.expression(*value, Context::Consume);
                self.initialize(*local, self.span(*value));
            }
            hir::Stmt::Expr(value) => self.expression(
                *value,
                if is_last {
                    Context::Consume
                } else {
                    Context::Read
                },
            ),
            hir::Stmt::Set { place, value } => {
                self.expression(*value, Context::Consume);
                self.place_use(*place, UseMode::Borrow);
            }
        }
    }

    fn expression(&mut self, expression: ExprId, context: Context) {
        match &self.hir.expressions[expression] {
            hir::Expr::Name(ResolvedName::Local(_)) => {
                let mode = match context {
                    Context::Read => UseMode::Read,
                    Context::Borrow => UseMode::Borrow,
                    Context::Consume if self.copy_expression(expression) => UseMode::Copy,
                    Context::Consume => UseMode::Move,
                };
                self.place_use(expression, mode);
            }
            hir::Expr::List(values) => {
                for value in values {
                    self.expression(*value, Context::Consume);
                }
            }
            hir::Expr::Call { callee, arguments } => {
                self.expression(*callee, Context::Read);
                let parameter_modes = self
                    .types
                    .expression_type(*callee)
                    .and_then(|ty| match &self.types.types[ty] {
                        crate::types::Type::Function(function) => {
                            Some(function.parameter_modes.clone())
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                for (index, argument) in arguments.iter().enumerate() {
                    let context = if parameter_modes.get(index)
                        == Some(&crate::ast::ParameterMode::Consume)
                    {
                        Context::Consume
                    } else {
                        Context::Borrow
                    };
                    self.expression(*argument, context);
                }
            }
            hir::Expr::Member { object, .. } | hir::Expr::Index { object, .. }
                if self.owned_place(expression).is_some() =>
            {
                if let hir::Expr::Index { index, .. } = self.hir.expressions[expression] {
                    self.expression(index, Context::Read);
                }
                let mode = match context {
                    Context::Read => UseMode::Read,
                    Context::Borrow => UseMode::Borrow,
                    Context::Consume if self.copy_expression(expression) => UseMode::Copy,
                    Context::Consume => UseMode::Move,
                };
                self.place_use(expression, mode);
                let _ = object;
            }
            hir::Expr::Member { object, .. } => self.expression(*object, Context::Read),
            hir::Expr::Index { object, index } => {
                self.expression(*object, Context::Read);
                self.expression(*index, Context::Read);
            }
            hir::Expr::Reference(place) => self.place_use(*place, UseMode::Borrow),
            hir::Expr::MoveOut(place) => self.place_use(*place, UseMode::Move),
            hir::Expr::Remote(value) | hir::Expr::Await(value) => {
                self.expression(*value, Context::Consume);
            }
            hir::Expr::Record { fields, .. } => {
                for (_, value) in fields {
                    self.expression(*value, Context::Consume);
                }
            }
            hir::Expr::Unary { operand, .. } => self.expression(*operand, Context::Read),
            hir::Expr::Binary { left, right, .. } => {
                self.expression(*left, Context::Read);
                self.expression(*right, Context::Read);
            }
            hir::Expr::Branch { subject, arms } => {
                if let Some(subject) = subject {
                    self.expression(*subject, Context::Read);
                }
                for arm in arms {
                    if let BranchTest::Condition(condition) = arm.test {
                        self.expression(condition, Context::Read);
                    }
                }
                let origin = self.current;
                let targets = (0..arms.len()).map(|_| self.block()).collect::<Vec<_>>();
                let join = self.block();
                self.current = origin;
                self.terminate(Terminator::Branch(targets.clone()));
                for (target, arm) in targets.into_iter().zip(arms) {
                    self.current = target;
                    if let BranchTest::Pattern(pattern) = &arm.test {
                        self.initialize_pattern(pattern, self.span(arm.value));
                    }
                    self.expression(arm.value, context);
                    self.terminate(Terminator::Goto(join));
                }
                self.current = join;
            }
            hir::Expr::Closure { captures, .. } => {
                for capture in captures {
                    let mode = match capture.mode {
                        CaptureMode::Copy => UseMode::Copy,
                        CaptureMode::Move | CaptureMode::Pending => UseMode::Move,
                        CaptureMode::Ref => UseMode::Borrow,
                    };
                    self.emit(Operation::Use {
                        place: hir::Place {
                            root: capture.local,
                            projections: Vec::new(),
                        },
                        mode,
                        span: self.span(expression),
                    });
                }
            }
            hir::Expr::Unit
            | hir::Expr::Bool(_)
            | hir::Expr::Integer(_)
            | hir::Expr::Float(_)
            | hir::Expr::String(_)
            | hir::Expr::CodePoint(_)
            | hir::Expr::Symbol(_)
            | hir::Expr::Name(_) => {}
        }
    }

    fn place_use(&mut self, expression: ExprId, mode: UseMode) {
        if let Some(place) = self.owned_place(expression) {
            self.emit(Operation::Use {
                place,
                mode,
                span: self.span(expression),
            });
        }
    }

    fn owned_place(&self, expression: ExprId) -> Option<hir::Place> {
        let place = hir::queries::expression_place(self.hir, expression)?;
        match &self.hir.expressions[expression] {
            hir::Expr::Member { object, name } => {
                let ty = self.types.expression_type(*object)?;
                type_has_field(self.types, ty, name).then_some(place)
            }
            hir::Expr::Index { .. }
            | hir::Expr::Name(ResolvedName::Local(_))
            | hir::Expr::Reference(_) => Some(place),
            _ => None,
        }
    }

    fn copy_expression(&self, expression: ExprId) -> bool {
        self.types
            .expression_type(expression)
            .is_some_and(|ty| self.types.is_copy(ty))
    }

    fn initialize(&mut self, local: hir::LocalId, span: std::ops::Range<usize>) {
        self.emit(Operation::Initialize { local, span });
    }

    fn initialize_pattern(&mut self, pattern: &hir::Pattern, span: std::ops::Range<usize>) {
        let span = pattern.span().unwrap_or(span);
        match pattern.unspanned() {
            hir::Pattern::Binding(local) => self.initialize(*local, span),
            hir::Pattern::Variant { fields, .. } => {
                for field in fields {
                    self.initialize_pattern(field, span.clone());
                }
            }
            _ => {}
        }
    }

    fn span(&self, expression: ExprId) -> std::ops::Range<usize> {
        self.hir
            .expression_spans
            .get(&expression)
            .cloned()
            .unwrap_or_else(|| self.hir.functions[self.function].span.clone())
    }

    fn emit(&mut self, operation: Operation) {
        self.blocks[self.current].operations.push(operation);
    }

    fn terminate(&mut self, terminator: Terminator) {
        self.blocks[self.current].terminator = terminator;
    }

    fn block(&mut self) -> BlockId {
        let id = self.blocks.len();
        self.blocks.push(BasicBlock::default());
        id
    }
}

fn type_has_field(types: &TypeInformation, ty: crate::types::TypeId, name: &str) -> bool {
    match &types.types[ty] {
        crate::types::Type::Record { record, .. } => types
            .record_fields
            .get(record)
            .is_some_and(|fields| fields.contains(name)),
        crate::types::Type::Intersection(members) => members
            .iter()
            .any(|member| type_has_field(types, *member, name)),
        _ => false,
    }
}
