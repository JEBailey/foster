use crate::hir::{self, BranchTest, CaptureMode, ExprId, FunctionId, ResolvedName};
use crate::types::TypeInformation;

use super::{
    BasicBlock, BlockId, BorrowValue, Comparison, ComparisonKind, ComparisonOperand,
    FailureOperation, Function, InvalidationKind, LoanDefinition, LoanId, MirPoint, Operation,
    Place, Program, TemporaryId, Terminator, UseMode,
};

#[derive(Clone, Copy)]
enum Context {
    Read,
    Borrow,
    Consume,
    Call,
}

pub(super) fn lower(
    hir: &hir::PackageHir,
    types: &TypeInformation,
    result_provenance: &std::collections::HashMap<FunctionId, super::ResultProvenance>,
) -> Result<Program, crate::error::FosterError> {
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
        crate::compiler::cancellation::check()?;
        program.functions.insert(
            function,
            Builder::new(
                hir,
                types,
                function,
                captures.get(&function).map_or(&[], Vec::as_slice),
                result_provenance,
            )
            .lower(),
        );
    }
    Ok(program)
}

struct Builder<'a> {
    hir: &'a hir::PackageHir,
    types: &'a TypeInformation,
    function: FunctionId,
    blocks: Vec<BasicBlock>,
    current: BlockId,
    captures: &'a [hir::LocalId],
    result_provenance: &'a std::collections::HashMap<FunctionId, super::ResultProvenance>,
    loans: Vec<LoanDefinition>,
    loops: Vec<LoopTargets>,
    remote_scopes: Vec<Vec<hir::LocalId>>,
    next_temporary: usize,
    temporary_scopes: Vec<Vec<(ExprId, Place)>>,
    active_temporaries: std::collections::HashMap<ExprId, Place>,
    remote_temporaries: std::collections::HashSet<ExprId>,
}

#[derive(Clone, Copy)]
struct LoopTargets {
    scope_depth: usize,
    continue_to: BlockId,
    break_to: BlockId,
}

impl<'a> Builder<'a> {
    fn new(
        hir: &'a hir::PackageHir,
        types: &'a TypeInformation,
        function: FunctionId,
        captures: &'a [hir::LocalId],
        result_provenance: &'a std::collections::HashMap<FunctionId, super::ResultProvenance>,
    ) -> Self {
        Self {
            hir,
            types,
            function,
            blocks: vec![BasicBlock::default()],
            current: 0,
            captures,
            result_provenance,
            loans: Vec::new(),
            loops: Vec::new(),
            remote_scopes: Vec::new(),
            next_temporary: 0,
            temporary_scopes: Vec::new(),
            active_temporaries: std::collections::HashMap::new(),
            remote_temporaries: std::collections::HashSet::new(),
        }
    }

    fn lower(mut self) -> Function {
        let definition = &self.hir.functions[self.function];
        for (index, parameter) in definition.parameters.iter().enumerate() {
            self.initialize(*parameter, definition.span.clone());
            if matches!(
                definition.parameter_types[index],
                Some(crate::ast::TypeExpr::Reference { .. })
            ) {
                let destination = Self::local_place(*parameter);
                let value = self.issue_reborrow(destination.clone(), definition.span.clone());
                self.emit(Operation::StoreBorrower {
                    destination,
                    value,
                    span: definition.span.clone(),
                });
            }
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
            self.emit_scope_destruction(definition.span.clone());
            self.blocks[self.current].terminator = Terminator::Return;
        }
        Function {
            entry: 0,
            blocks: self.blocks,
            loans: self.loans,
            result_provenance: super::ResultProvenance::default(),
        }
    }

    fn statement(&mut self, statement: &hir::Stmt, is_last: bool) {
        match statement {
            hir::Stmt::Return { value, guard } => {
                if let Some(guard) = guard {
                    let returned = self.block();
                    let continued = self.block();
                    self.full_expression_condition(*guard, [returned, continued]);
                    self.current = returned;
                    self.begin_full_expression();
                    self.expression(*value, Context::Consume);
                    self.emit_return(*value);
                    self.end_full_expression(self.span(*value));
                    self.emit_active_temporary_destruction(self.span(*value));
                    self.emit_scope_destruction(self.span(*value));
                    self.terminate(Terminator::Return);
                    self.current = continued;
                } else {
                    self.begin_full_expression();
                    self.expression(*value, Context::Consume);
                    self.emit_return(*value);
                    self.end_full_expression(self.span(*value));
                    self.emit_active_temporary_destruction(self.span(*value));
                    self.emit_scope_destruction(self.span(*value));
                    self.terminate(Terminator::Return);
                    self.current = self.block();
                }
            }
            hir::Stmt::Assert { condition, message } => {
                self.begin_full_expression();
                self.expression(*condition, Context::Read);
                if let Some(message) = message {
                    self.expression(*message, Context::Read);
                }
                let span =
                    message.map_or_else(|| self.span(*condition), |message| self.span(message));
                let failed = self.block();
                let continued = self.block();
                self.terminate(match self.hir.expressions[*condition] {
                    hir::Expr::Bool(false) => Terminator::Goto(failed),
                    hir::Expr::Bool(true) => Terminator::Goto(continued),
                    _ => Terminator::Branch(vec![failed, continued]),
                });
                self.current = failed;
                self.emit_active_temporary_destruction(span.clone());
                self.emit_scope_destruction(span.clone());
                self.terminate(Terminator::Fail);
                self.current = continued;
                self.end_full_expression(span);
            }
            hir::Stmt::Loop { body, .. } => {
                let cfg = crate::control_flow::LoopCfg::new();
                let blocks = [self.block(), self.block(), self.block()];
                self.terminate(Terminator::Goto(blocks[cfg.header.0]));
                self.current = blocks[cfg.header.0];
                self.terminate(Terminator::Goto(blocks[cfg.body.0]));
                self.current = blocks[cfg.body.0];
                self.loops.push(LoopTargets {
                    scope_depth: self.remote_scopes.len(),
                    continue_to: blocks[cfg.header.0],
                    break_to: blocks[cfg.exit.0],
                });
                self.remote_scopes.push(
                    body.iter()
                        .filter_map(|statement| match statement {
                            hir::Stmt::Bind { local, .. } => Some(*local),
                            _ => None,
                        })
                        .collect(),
                );
                for statement in body {
                    self.statement(statement, false);
                }
                self.end_remote_scopes(
                    self.remote_scopes.len() - 1,
                    self.hir.functions[self.function].span.clone(),
                );
                self.remote_scopes.pop();
                if matches!(
                    self.blocks[self.current].terminator,
                    Terminator::Unreachable
                ) {
                    self.terminate(Terminator::Goto(blocks[cfg.header.0]));
                }
                self.loops.pop();
                self.current = blocks[cfg.exit.0];
            }
            hir::Stmt::Break { guard } => {
                let target = self
                    .loops
                    .last()
                    .expect("HIR validates loop transfers")
                    .break_to;
                self.loop_transfer(*guard, target);
            }
            hir::Stmt::Continue { guard } => {
                let target = self
                    .loops
                    .last()
                    .expect("HIR validates loop transfers")
                    .continue_to;
                self.loop_transfer(*guard, target);
            }
            hir::Stmt::Bind { local, value } => {
                self.begin_full_expression();
                self.local_value(*local, *value);
                if is_last {
                    self.emit(Operation::ReturnBorrower {
                        value: BorrowValue::Place(Self::local_place(*local)),
                        kind: self.return_kind(*value),
                        span: self.span(*value),
                    });
                }
                self.end_full_expression(self.span(*value));
            }
            hir::Stmt::Assign { local, value } => {
                self.begin_full_expression();
                self.local_value(*local, *value);
                self.end_full_expression(self.span(*value));
            }
            hir::Stmt::Expr(value) => {
                self.begin_full_expression();
                self.expression(
                    *value,
                    if is_last {
                        Context::Consume
                    } else {
                        Context::Read
                    },
                );
                if is_last {
                    self.emit_return(*value);
                }
                self.end_full_expression(self.span(*value));
            }
            hir::Stmt::Set { place, value } => {
                self.begin_full_expression();
                let destination = self.owned_place(*place);
                // Preserve the complete value before evaluating any dynamic
                // component of the destination. The temporary also lets a
                // branch-valued right side join before selecting the place.
                let value_temporary = self.reserve_assignment_temporary(*value);
                self.expression_into(*value, Context::Consume, Some(value_temporary.clone()));
                self.assignment_place_address(*place);
                let copy = self.copy_expression(*value);
                self.emit(Operation::Use {
                    place: value_temporary.clone(),
                    mode: if copy { UseMode::Copy } else { UseMode::Move },
                    span: self.span(*value),
                });
                if let Some(destination) = destination {
                    self.emit(Operation::StoreBorrower {
                        destination,
                        value: if copy {
                            BorrowValue::Empty
                        } else {
                            BorrowValue::MovePlace(value_temporary)
                        },
                        span: self.span(*value),
                    });
                }
                self.place_use(*place, UseMode::Write);
                self.end_full_expression(self.span(*value));
            }
        }
    }

    fn loop_transfer(&mut self, guard: Option<ExprId>, target: BlockId) {
        if let Some(guard) = guard {
            let transferred = self.block();
            let continued = self.block();
            self.full_expression_condition(guard, [transferred, continued]);
            self.current = transferred;
            self.end_remote_scopes(self.loops.last().unwrap().scope_depth, self.span(guard));
            self.emit_active_temporary_destruction(self.span(guard));
            self.terminate(Terminator::Goto(target));
            self.current = continued;
        } else {
            let span = self.hir.functions[self.function].span.clone();
            self.end_remote_scopes(self.loops.last().unwrap().scope_depth, span.clone());
            self.emit_active_temporary_destruction(span);
            self.terminate(Terminator::Goto(target));
            self.current = self.block();
        }
    }

    fn expression_into(
        &mut self,
        expression: ExprId,
        context: Context,
        destination: Option<Place>,
    ) {
        if let hir::Expr::Branch { subject, arms } = &self.hir.expressions[expression]
            && let Some(destination) = destination
        {
            let subject = *subject;
            let arms = arms.clone();
            self.lower_branch_expression(subject, &arms, context, Some(destination));
            return;
        }

        self.expression(expression, context);
        if let Some(destination) = destination {
            let value = if self.copy_expression(expression) {
                BorrowValue::Empty
            } else if matches!(context, Context::Consume)
                && !self.copy_expression(expression)
                && matches!(
                    self.hir.expressions[expression],
                    hir::Expr::Name(ResolvedName::Local(_))
                        | hir::Expr::Member { .. }
                        | hir::Expr::Index { .. }
                )
                && let Some(place) = self.owned_place(expression)
            {
                BorrowValue::MovePlace(place)
            } else {
                self.borrow_value(expression)
            };
            self.emit(Operation::StoreBorrower {
                destination,
                value,
                span: self.span(expression),
            });
        }
    }

    fn expression(&mut self, expression: ExprId, context: Context) {
        match &self.hir.expressions[expression] {
            hir::Expr::Name(ResolvedName::Local(_)) => {
                let mode = match context {
                    Context::Read => UseMode::Read,
                    Context::Borrow => UseMode::Borrow,
                    Context::Call => UseMode::Call,
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
                self.expression(*callee, Context::Call);
                let mut transferred = Vec::new();
                if let hir::Expr::Member { object, .. } = self.hir.expressions[*callee]
                    && self.owned_place(object).is_none()
                {
                    let place = self.materialize_temporary(object);
                    if self
                        .types
                        .resolved_function_for_callee(*callee)
                        .and_then(|function| self.types.function_type(function))
                        .is_some_and(|signature| {
                            signature.parameter_modes.first()
                                == Some(&crate::ast::ParameterMode::Consume)
                        })
                        || self.types.expression_type(*callee).is_some_and(|ty| {
                            matches!(&self.types.types[ty], crate::types::Type::Function(signature)
                            if signature.effects.iter().any(|effect| {
                                effect.kind == crate::ast::EffectKind::Consume
                                    && effect.target.root == "self"
                                    && effect.target.children.is_empty()
                            }))
                        })
                    {
                        transferred.push(place.clone());
                    }
                    self.emit(Operation::Use {
                        place,
                        mode: UseMode::Borrow,
                        span: self.span(object),
                    });
                }
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
                    if matches!(context, Context::Consume) {
                        // Earlier arguments remain caller-owned if evaluating a
                        // later argument fails. Ownership transfers at invocation.
                        transferred.push(self.materialize_temporary(*argument));
                    }
                    if matches!(context, Context::Borrow) && self.owned_place(*argument).is_none() {
                        let place = self.materialize_temporary(*argument);
                        self.emit(Operation::Use {
                            place,
                            mode: UseMode::Borrow,
                            span: self.span(*argument),
                        });
                    }
                }
                for (index, argument) in arguments.iter().enumerate() {
                    if parameter_modes.get(index) == Some(&crate::ast::ParameterMode::Consume) {
                        let value = self.borrow_value(*argument);
                        self.emit(Operation::RemoteConsume {
                            value,
                            span: self.span(expression),
                        });
                    }
                }
                self.emit_call_invalidations(*callee, arguments, expression);
                for place in transferred {
                    self.emit(Operation::Use {
                        place,
                        mode: UseMode::Move,
                        span: self.span(expression),
                    });
                }
                if let Some(operation) = self.call_failure(*callee) {
                    self.failure_edge(operation, expression);
                }
                let owner_paths = self
                    .types
                    .expression_type(expression)
                    .map(|ty| self.remote_owner_paths(ty))
                    .unwrap_or_default();
                if !owner_paths.is_empty() {
                    let root = self.reserve_temporary(expression);
                    self.remote_temporaries.insert(expression);
                    for projections in owner_paths {
                        let mut destination = root.clone();
                        destination.projections.extend(projections);
                        self.emit(Operation::RemoteOwner {
                            destination,
                            span: self.span(expression),
                        });
                    }
                }

                if self
                    .types
                    .expression_type(expression)
                    .is_some_and(|ty| matches!(self.types.types[ty], crate::types::Type::Future(_)))
                {
                    let mut origins = if matches!(
                        self.types.resolved_call(*callee),
                        Some(crate::types::ResolvedCall::Method { remote: true, .. })
                    ) {
                        Vec::new()
                    } else {
                        arguments
                            .iter()
                            .map(|argument| self.borrow_value(*argument))
                            .collect::<Vec<_>>()
                    };
                    if let hir::Expr::Member { object, .. } = self.hir.expressions[*callee] {
                        origins.push(self.borrow_value(object));
                    }
                    let borrowed = BorrowValue::Empty;
                    let destination = self.reserve_temporary(expression);
                    self.remote_temporaries.insert(expression);
                    self.emit(Operation::StoreBorrower {
                        destination: destination.clone(),
                        value: borrowed,
                        span: self.span(expression),
                    });
                    self.emit(Operation::RemoteRequest {
                        destination,
                        owner: BorrowValue::Merge(origins),
                        span: self.span(expression),
                    });
                }
            }
            hir::Expr::Member { .. } | hir::Expr::Index { .. }
                if self.owned_place(expression).is_some() =>
            {
                let mode = match context {
                    Context::Read => UseMode::Read,
                    Context::Borrow => UseMode::Borrow,
                    Context::Call => UseMode::Call,
                    Context::Consume if self.copy_expression(expression) => UseMode::Copy,
                    Context::Consume => UseMode::Move,
                };
                self.place_use(expression, mode);
            }
            hir::Expr::Member { object, .. } => self.expression(*object, Context::Read),
            hir::Expr::Index { object, index } => {
                self.expression(*object, Context::Read);
                if self.owned_place(*object).is_none() {
                    self.materialize_temporary(*object);
                }
                self.expression(*index, Context::Read);
                self.failure_edge(FailureOperation::Bounds { expression }, expression);
            }
            hir::Expr::Reference(place) => {
                if self.owned_place(*place).is_some() {
                    self.place_use(*place, UseMode::Borrow);
                } else {
                    self.expression(*place, Context::Consume);
                    let place = self.materialize_temporary(*place);
                    self.emit(Operation::Use {
                        place,
                        mode: UseMode::Borrow,
                        span: self.span(expression),
                    });
                }
            }
            hir::Expr::MoveOut(place) => {
                self.place_use(*place, UseMode::Move);
                if let Some(place) =
                    crate::semantics::expression_place(self.hir, &self.types.member_kinds, *place)
                {
                    self.emit(Operation::Invalidate {
                        place: Place::from_hir(place),
                        kind: InvalidationKind::Consume,
                        span: self.span(expression),
                    });
                }
            }
            hir::Expr::Remote(value) => {
                self.expression(*value, Context::Consume);
                let borrowed = BorrowValue::Empty;
                let destination = self.reserve_temporary(expression);
                self.remote_temporaries.insert(expression);
                self.emit(Operation::StoreBorrower {
                    destination: destination.clone(),
                    value: borrowed,
                    span: self.span(expression),
                });
                self.emit(Operation::RemoteOwner {
                    destination,
                    span: self.span(expression),
                });
            }
            hir::Expr::Await(value) => {
                self.expression(*value, Context::Consume);
                let future = self.borrow_value(*value);
                self.emit(Operation::RemoteComplete {
                    future,
                    span: self.span(expression),
                });
                self.emit(Operation::Suspend {
                    span: self.span(expression),
                });
            }
            hir::Expr::Try { value, .. } => {
                self.expression(*value, Context::Consume);
                let returned = self.block();
                let continued = self.block();
                self.terminate(Terminator::Branch(vec![returned, continued]));
                self.current = returned;
                self.emit_return(*value);
                self.emit_active_temporary_destruction(self.span(expression));
                self.emit_scope_destruction(self.span(expression));
                self.terminate(Terminator::Return);
                self.current = continued;
            }
            hir::Expr::Record { fields, .. } => {
                for (_, value) in fields {
                    self.expression(*value, Context::Consume);
                }
            }
            hir::Expr::Unary { operand, operator } => {
                self.expression(*operand, Context::Read);
                if *operator == crate::ast::UnaryOp::Negate
                    && self
                        .types
                        .expression_type(*operand)
                        .is_some_and(|ty| matches!(self.types.types[ty], crate::types::Type::Int))
                {
                    self.failure_edge(FailureOperation::Arithmetic { expression }, expression);
                }
            }
            hir::Expr::Binary {
                left,
                right,
                operator,
            } => {
                self.expression(*left, Context::Read);
                self.expression(*right, Context::Read);
                use crate::ast::BinaryOp;
                let checked_integer = matches!(
                    operator,
                    BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
                ) && self
                    .types
                    .expression_type(expression)
                    .is_some_and(|ty| matches!(self.types.types[ty], crate::types::Type::Int));
                if checked_integer || matches!(operator, BinaryOp::ShiftLeft | BinaryOp::ShiftRight)
                {
                    self.failure_edge(FailureOperation::Arithmetic { expression }, expression);
                }
            }
            hir::Expr::Branch { subject, arms } => {
                let subject = *subject;
                let arms = arms.clone();
                let destination =
                    (!self.copy_expression(expression)).then(|| self.reserve_temporary(expression));
                self.lower_branch_expression(subject, &arms, context, destination);
            }
            hir::Expr::Closure { captures, .. } => {
                // A reference capture exposes storage to later calls through the
                // environment. Do not retain target identities across that alias.
                if captures
                    .iter()
                    .any(|capture| capture.mode == CaptureMode::Ref)
                {
                    self.emit(Operation::ForgetCallableTargets);
                }
                for capture in captures {
                    if let Some(source) = capture.source {
                        self.expression(
                            source,
                            match capture.mode {
                                CaptureMode::Ref => Context::Borrow,
                                CaptureMode::Copy | CaptureMode::Move | CaptureMode::Pending => {
                                    Context::Consume
                                }
                            },
                        );
                        continue;
                    }
                    let mode = match capture.mode {
                        CaptureMode::Copy => UseMode::Copy,
                        CaptureMode::Move | CaptureMode::Pending => UseMode::Move,
                        CaptureMode::Ref => UseMode::Borrow,
                    };
                    self.emit(Operation::Use {
                        place: Self::local_place(capture.local),
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

    fn lower_branch_arm(
        &mut self,
        arm: &hir::BranchArm,
        context: Context,
        destination: Option<Place>,
    ) {
        let Some(last) = arm.body.last() else {
            return;
        };
        self.remote_scopes.push(
            arm.body
                .iter()
                .filter_map(|statement| match statement {
                    hir::Stmt::Bind { local, .. } => Some(*local),
                    _ => None,
                })
                .collect(),
        );
        for statement in arm.body.iter().take(arm.body.len() - 1) {
            self.statement(statement, false);
        }
        if let hir::Stmt::Expr(value) = last {
            if let Some(destination) = destination {
                self.expression_into(*value, context, Some(destination));
            } else {
                self.expression(*value, context);
            }
        } else {
            self.statement(last, false);
        }
        self.end_remote_scopes(
            self.remote_scopes.len() - 1,
            self.hir.functions[self.function].span.clone(),
        );
        self.remote_scopes.pop();
    }

    fn end_remote_scopes(&mut self, depth: usize, span: std::ops::Range<usize>) {
        let places = self.remote_scopes[depth..]
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().rev())
            .map(|local| Place::local(*local))
            .collect();
        self.emit(Operation::RemoteScopeEnd { places, span });
    }

    fn lower_branch_expression(
        &mut self,
        subject: Option<ExprId>,
        arms: &[hir::BranchArm],
        context: Context,
        destination: Option<Place>,
    ) {
        let boolean_subject = subject.filter(|subject| {
            self.types
                .expression_type(*subject)
                .is_some_and(|ty| matches!(self.types.types[ty], crate::types::Type::Bool))
                && arms.iter().all(|arm| match &arm.test {
                    BranchTest::Wildcard => true,
                    BranchTest::Pattern(pattern) => matches!(
                        pattern.unspanned(),
                        hir::Pattern::Bool(_) | hir::Pattern::Wildcard
                    ),
                    _ => false,
                })
        });
        let pattern_source = if let Some(subject) = subject.filter(|_| boolean_subject.is_none()) {
            self.expression(subject, Context::Read);
            let has_bindings = arms.iter().any(|arm| {
                matches!(&arm.test, BranchTest::Pattern(pattern) if Self::pattern_has_bindings(pattern))
            });
            if has_bindings && !self.copy_expression(subject) {
                self.owned_place(subject)
                    .or_else(|| Some(self.materialize_temporary(subject)))
            } else {
                None
            }
        } else {
            None
        };

        let cfg = crate::control_flow::BranchCfg::new(arms);
        let blocks = cfg.nodes().map(|_| self.block()).collect::<Vec<_>>();
        if let Some(subject) = boolean_subject {
            let target = |value| {
                let matched = arms.iter().position(|arm| match &arm.test {
                    BranchTest::Wildcard => true,
                    BranchTest::Pattern(pattern) => match pattern.unspanned() {
                        hir::Pattern::Bool(expected) => *expected == value,
                        hir::Pattern::Wildcard => true,
                        _ => false,
                    },
                    _ => false,
                });
                matched.map_or(blocks[cfg.exit().0], |arm| blocks[arm * 2 + 1])
            };
            self.condition(subject, [target(true), target(false)]);
        } else {
            self.terminate(Terminator::Goto(blocks[cfg.entry().0]));
        }

        for (node_id, node) in cfg.nodes() {
            match node {
                crate::control_flow::BranchNode::Test {
                    arm,
                    matched,
                    unmatched,
                } => {
                    self.current = blocks[node_id.0];
                    match (&arms[arm].test, unmatched) {
                        (BranchTest::Condition(condition), Some(unmatched)) => {
                            self.condition(*condition, [blocks[matched.0], blocks[unmatched.0]]);
                        }
                        (BranchTest::Pattern(pattern), Some(unmatched)) => {
                            let matched = blocks[matched.0];
                            let unmatched = blocks[unmatched.0];
                            match pattern.unspanned() {
                                hir::Pattern::Wildcard | hir::Pattern::Binding(_) => {
                                    self.terminate(Terminator::Goto(matched))
                                }
                                hir::Pattern::Bool(expected) => {
                                    if let Some(condition) = subject
                                        .and_then(|subject| self.boolean_condition_place(subject))
                                    {
                                        let targets = if *expected {
                                            [matched, unmatched]
                                        } else {
                                            [unmatched, matched]
                                        };
                                        self.terminate(Terminator::BooleanBranch {
                                            condition,
                                            targets,
                                        });
                                    } else if let Some((comparison, polarity)) = subject
                                        .and_then(|subject| self.comparison_condition(subject))
                                    {
                                        let targets = if *expected == polarity {
                                            [matched, unmatched]
                                        } else {
                                            [unmatched, matched]
                                        };
                                        self.terminate(Terminator::ComparisonBranch {
                                            comparison,
                                            targets,
                                        });
                                    } else {
                                        self.terminate(Terminator::Branch(vec![
                                            matched, unmatched,
                                        ]));
                                    }
                                }
                                hir::Pattern::Variant { variant, .. } => {
                                    if let Some(subject) =
                                        subject.and_then(|subject| self.owned_place(subject))
                                    {
                                        self.terminate(Terminator::VariantBranch {
                                            subject,
                                            variant: *variant,
                                            targets: [matched, unmatched],
                                        });
                                    } else {
                                        self.terminate(Terminator::Branch(vec![
                                            matched, unmatched,
                                        ]));
                                    }
                                }
                                _ => {
                                    self.terminate(Terminator::Branch(vec![matched, unmatched]));
                                }
                            }
                        }
                        (BranchTest::Wildcard, None) => {
                            self.terminate(Terminator::Goto(blocks[matched.0]));
                        }
                        _ => unreachable!("semantic branch CFG matches HIR tests"),
                    }
                }
                crate::control_flow::BranchNode::Body { arm, completed } => {
                    self.current = blocks[node_id.0];
                    let arm = &arms[arm];
                    if let BranchTest::Pattern(pattern) = &arm.test {
                        self.initialize_pattern(
                            pattern,
                            pattern_source.as_ref(),
                            subject,
                            self.branch_arm_span(arm),
                        );
                    }
                    self.lower_branch_arm(arm, context, destination.clone());
                    if let Some(completed) = completed
                        && matches!(
                            self.blocks[self.current].terminator,
                            Terminator::Unreachable
                        )
                    {
                        self.terminate(Terminator::Goto(blocks[completed.0]));
                    }
                }
                crate::control_flow::BranchNode::Exit => {}
            }
        }
        self.current = blocks[cfg.exit().0];
    }

    fn branch_arm_span(&self, arm: &hir::BranchArm) -> std::ops::Range<usize> {
        arm.body
            .first_span()
            .cloned()
            .unwrap_or_else(|| self.hir.functions[self.function].span.clone())
    }

    fn place_use(&mut self, expression: ExprId, mode: UseMode) {
        // Assignment already selected its destination before transferring the RHS.
        if mode != UseMode::Write {
            self.assignment_place_address(expression);
        }
        if let Some(place) = self.owned_place(expression) {
            self.emit(Operation::Use {
                place,
                mode,
                span: self.span(expression),
            });
        }
    }

    /// Evaluates only the expressions required to select an assignment place.
    /// The caller invokes this after evaluating the right-hand value.
    fn assignment_place_address(&mut self, expression: ExprId) {
        match self.hir.expressions[expression] {
            hir::Expr::Member { object, .. } => self.assignment_place_address(object),
            hir::Expr::Index { object, index } => {
                self.assignment_place_address(object);
                self.expression(index, Context::Read);
                self.failure_edge(FailureOperation::Bounds { expression }, expression);
            }
            hir::Expr::Reference(place) => self.assignment_place_address(place),
            _ => {}
        }
    }

    fn borrow_value(&mut self, expression: ExprId) -> BorrowValue {
        if self.remote_temporaries.contains(&expression)
            && let Some(place) = self.active_temporaries.remove(&expression)
        {
            let loans = Box::new(self.borrow_value(expression));
            self.active_temporaries.insert(expression, place.clone());
            return BorrowValue::Tracked { place, loans };
        }
        if let Some(place) = self.active_temporaries.get(&expression) {
            return BorrowValue::Place(place.clone());
        }
        match &self.hir.expressions[expression] {
            hir::Expr::Reference(origin) => {
                let origin = self
                    .owned_place(*origin)
                    .or_else(|| self.active_temporaries.get(origin).cloned());
                let Some(origin) = origin else {
                    return BorrowValue::Empty;
                };
                self.issue_reborrow(origin, self.span(expression))
            }
            hir::Expr::Name(ResolvedName::Local(_))
            | hir::Expr::Member { .. }
            | hir::Expr::Index { .. } => self
                .owned_place(expression)
                .map(BorrowValue::Place)
                .unwrap_or(BorrowValue::Empty),
            hir::Expr::List(values) => BorrowValue::Fields(
                values
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        (
                            vec![hir::Projection::Index {
                                expression: *value,
                                constant: Some(
                                    i64::try_from(index)
                                        .expect("list literal index fits in an i64"),
                                ),
                            }],
                            self.borrow_value(*value),
                        )
                    })
                    .collect(),
            ),
            hir::Expr::Record { fields, .. } => BorrowValue::Fields(
                fields
                    .iter()
                    .map(|(name, value)| {
                        (
                            vec![hir::Projection::Field(name.clone())],
                            self.borrow_value(*value),
                        )
                    })
                    .collect(),
            ),
            hir::Expr::Closure { function, captures } => BorrowValue::Callable {
                parameters: self.callable_parameters(*function),
                environment: Box::new(BorrowValue::Merge(
                    captures
                        .iter()
                        .filter_map(|capture| match (capture.mode, capture.source) {
                            (CaptureMode::Ref, Some(source)) => {
                                let origin = self
                                    .owned_place(source)
                                    .or_else(|| self.active_temporaries.get(&source).cloned())?;
                                Some(self.issue_reborrow(origin, self.span(expression)))
                            }
                            (CaptureMode::Move | CaptureMode::Pending, Some(source)) => Some(
                                self.owned_place(source)
                                    .map(BorrowValue::MovePlace)
                                    .unwrap_or_else(|| self.borrow_value(source)),
                            ),
                            (CaptureMode::Copy, Some(_)) => None,
                            (CaptureMode::Ref, None) => {
                                let origin = Self::local_place(capture.local);
                                Some(self.issue_reborrow(origin, self.span(expression)))
                            }
                            (CaptureMode::Move | CaptureMode::Pending, None) => {
                                Some(BorrowValue::MovePlace(Self::local_place(capture.local)))
                            }
                            (CaptureMode::Copy, None) => None,
                        })
                        .collect(),
                )),
            },
            hir::Expr::Name(ResolvedName::Function(function)) => BorrowValue::Callable {
                parameters: self.callable_parameters(*function),
                environment: Box::new(BorrowValue::Empty),
            },
            hir::Expr::Call { callee, arguments } => {
                self.call_result_borrow_value(*callee, arguments)
            }
            hir::Expr::Branch { .. } => BorrowValue::Empty,
            hir::Expr::MoveOut(value) => {
                crate::semantics::expression_place(self.hir, &self.types.member_kinds, *value)
                    .map(Place::from_hir)
                    .map(BorrowValue::MovePlace)
                    .unwrap_or(BorrowValue::Empty)
            }
            hir::Expr::Remote(value)
            | hir::Expr::Await(value)
            | hir::Expr::Unary { operand: value, .. } => self.borrow_value(*value),
            hir::Expr::Try { .. } => BorrowValue::Empty,
            hir::Expr::Binary { left, right, .. } => {
                BorrowValue::Merge(vec![self.borrow_value(*left), self.borrow_value(*right)])
            }
            _ => BorrowValue::Empty,
        }
    }

    fn remote_owner_paths(&self, ty: crate::types::TypeId) -> Vec<Vec<hir::Projection>> {
        fn visit(
            builder: &Builder<'_>,
            ty: crate::types::TypeId,
            substitutions: &std::collections::HashMap<String, crate::types::TypeId>,
            depth: usize,
        ) -> Vec<Vec<hir::Projection>> {
            if depth >= 32 {
                return vec![];
            }
            match &builder.types.types[ty] {
                crate::types::Type::Remote(_) => vec![vec![]],
                crate::types::Type::Generic(name) => substitutions
                    .get(name)
                    .filter(|actual| **actual != ty)
                    .map_or_else(Vec::new, |actual| {
                        visit(builder, *actual, substitutions, depth + 1)
                    }),
                crate::types::Type::Record { record, arguments } => {
                    let mut nested = substitutions.clone();
                    for (name, argument) in builder.hir.records[*record]
                        .parameters
                        .iter()
                        .zip(arguments)
                    {
                        let argument = match &builder.types.types[*argument] {
                            crate::types::Type::Generic(name) => {
                                substitutions.get(name).copied().unwrap_or(*argument)
                            }
                            _ => *argument,
                        };
                        nested.insert(name.clone(), argument);
                    }
                    builder
                        .types
                        .record_field_types
                        .get(record)
                        .into_iter()
                        .flatten()
                        .flat_map(|(field, ty)| {
                            visit(builder, *ty, &nested, depth + 1)
                                .into_iter()
                                .map(|path| {
                                    std::iter::once(hir::Projection::Field(field.clone()))
                                        .chain(path)
                                        .collect()
                                })
                                .collect::<Vec<_>>()
                        })
                        .collect()
                }
                _ => vec![],
            }
        }
        visit(self, ty, &std::collections::HashMap::new(), 0)
    }

    fn callable_parameters(&self, function: FunctionId) -> Vec<usize> {
        let mut parameters = self.result_provenance[&function].parameters.clone();
        if let Some(signature) = self.types.function_type(function) {
            // Existing inferred summaries seed explicit reference parameters. Hidden
            // borrowers in aggregate/callable parameters still require a fallback.
            for (index, ty) in signature.parameters.iter().enumerate() {
                if !matches!(self.types.types[*ty], crate::types::Type::Reference { .. })
                    && super::callables::may_borrow(self.hir, self.types, *ty)
                {
                    parameters.push(index);
                }
            }
        }
        parameters.sort_unstable();
        parameters.dedup();
        parameters
    }

    fn call_result_borrow_value(&mut self, callee: ExprId, arguments: &[ExprId]) -> BorrowValue {
        let direct = self.types.resolved_function_for_callee(callee);
        let Some(function) = direct else {
            let signature =
                self.types
                    .expression_type(callee)
                    .and_then(|ty| match &self.types.types[ty] {
                        crate::types::Type::Function(signature) => Some(signature),
                        _ => None,
                    });
            if signature.is_some_and(|signature| {
                !super::callables::may_borrow(self.hir, self.types, signature.result)
            }) {
                return BorrowValue::Empty;
            }
            let fallback_parameters = signature.map_or_else(
                || (0..arguments.len()).collect(),
                |signature| super::callables::parameter_origins(self.types, signature),
            );
            return BorrowValue::Invocation {
                callee: Box::new(self.borrow_value(callee)),
                arguments: arguments
                    .iter()
                    .map(|argument| self.borrow_value(*argument))
                    .collect(),
                fallback_parameters,
            };
        };
        let offset = usize::from(matches!(
            self.hir.expressions[callee],
            hir::Expr::Member { .. }
        ));
        let summary = &self.result_provenance[&function];
        let mut values = Vec::new();
        if summary.receiver
            && let hir::Expr::Member { object, .. } = self.hir.expressions[callee]
        {
            values.push(self.borrow_value(object));
        }
        values.extend(
            summary
                .parameters
                .iter()
                .filter_map(|parameter| parameter.checked_sub(offset))
                .filter_map(|parameter| arguments.get(parameter))
                .map(|argument| self.borrow_value(*argument)),
        );
        BorrowValue::Merge(values)
    }

    fn issue_reborrow(&mut self, origin: Place, span: std::ops::Range<usize>) -> BorrowValue {
        let id = LoanId(self.loans.len());
        self.loans.push(LoanDefinition {
            id,
            origin: origin.clone(),
            issued_at: MirPoint {
                block: self.current,
                operation: self.blocks[self.current].operations.len(),
            },
            parents: std::collections::HashSet::new(),
            span,
        });
        BorrowValue::Reborrow { loan: id, origin }
    }

    fn emit_return(&mut self, expression: ExprId) {
        let value = if self.copy_expression(expression) {
            BorrowValue::Empty
        } else {
            self.borrow_value(expression)
        };
        self.emit(Operation::ReturnBorrower {
            value,
            kind: self.return_kind(expression),
            span: self.span(expression),
        });
    }

    fn return_kind(&self, expression: ExprId) -> super::ReturnKind {
        self.types
            .expression_type(expression)
            .map(|ty| match self.types.types[ty] {
                crate::types::Type::Reference { .. } => super::ReturnKind::Reference,
                crate::types::Type::Function(_) => super::ReturnKind::Closure,
                _ => super::ReturnKind::Aggregate,
            })
            .unwrap_or(super::ReturnKind::Aggregate)
    }

    fn emit_call_invalidations(
        &mut self,
        callee: ExprId,
        arguments: &[ExprId],
        expression: ExprId,
    ) {
        if self
            .types
            .expression_type(callee)
            .is_none_or(|ty| match &self.types.types[ty] {
                crate::types::Type::Function(signature) => signature
                    .effects
                    .iter()
                    .any(|effect| effect.kind != crate::ast::EffectKind::Read),
                _ => true,
            })
        {
            self.emit(Operation::ForgetCallableTargets);
        }
        let indirect = self.types.resolved_function_for_callee(callee).is_none()
            && !matches!(
                self.hir.expressions[callee],
                hir::Expr::Name(ResolvedName::Builtin(_))
            );
        if indirect {
            self.emit(Operation::ForgetPathFacts { place: None });
        }
        for (place, _) in super::effects::call_mutations(self.hir, self.types, callee, arguments) {
            self.emit(Operation::ForgetPathFacts {
                place: Some(Place::from_hir(place)),
            });
        }
        for (place, kind) in
            super::effects::call_invalidations(self.hir, self.types, callee, arguments)
        {
            self.emit(Operation::Invalidate {
                place: Place::from_hir(place),
                kind,
                span: self.span(expression),
            });
        }
    }

    fn local_place(local: hir::LocalId) -> Place {
        Place::local(local)
    }

    fn owned_place(&self, expression: ExprId) -> Option<Place> {
        crate::semantics::expression_place(self.hir, &self.types.member_kinds, expression)
            .map(Place::from_hir)
    }

    fn copy_expression(&self, expression: ExprId) -> bool {
        self.types
            .expression_type(expression)
            .is_some_and(|ty| self.types.is_copy(ty))
    }

    fn local_value(&mut self, local: hir::LocalId, value: ExprId) {
        if !self.saved_boolean(value, &mut 64) {
            self.expression_into(value, Context::Consume, Some(Self::local_place(local)));
            self.initialize(local, self.span(value));
            return;
        }
        let paths = [self.block(), self.block()];
        let continued = self.block();
        self.condition(value, paths);
        for (index, path) in paths.into_iter().enumerate() {
            self.current = path;
            self.emit(Operation::StoreBorrower {
                destination: Self::local_place(local),
                value: BorrowValue::Empty,
                span: self.span(value),
            });
            self.initialize(local, self.span(value));
            self.terminate(Terminator::BooleanValue {
                destination: Self::local_place(local),
                value: index == 0,
                target: continued,
            });
        }
        self.current = continued;
    }

    // Bound pure Boolean selection and integer comparison trees. Condition
    // lowering evaluates them once, including checked arithmetic failure edges.
    fn saved_boolean(&self, expression: ExprId, budget: &mut usize) -> bool {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        match &self.hir.expressions[expression] {
            hir::Expr::Bool(_) => true,
            hir::Expr::Name(ResolvedName::Local(_)) => self
                .types
                .expression_type(expression)
                .is_some_and(|ty| matches!(self.types.types[ty], crate::types::Type::Bool)),
            hir::Expr::Unary {
                operator: crate::ast::UnaryOp::Not,
                operand,
            } => self.saved_boolean(*operand, budget),
            hir::Expr::Binary { left, right, .. }
                if self.comparison_condition(expression).is_some()
                    && [*left, *right].into_iter().all(|operand| {
                        self.types.expression_type(operand).is_some_and(|ty| {
                            matches!(self.types.types[ty], crate::types::Type::Int)
                        })
                    }) =>
            {
                self.comparison_operand_with_budget(*left, budget).is_some()
                    && self
                        .comparison_operand_with_budget(*right, budget)
                        .is_some()
            }
            hir::Expr::Branch {
                subject: None,
                arms,
            } if arms.len() == 2 && arms[0].body.len() == 1 && arms[1].body.len() == 1 => {
                match (
                    &arms[0].test,
                    &arms[1].test,
                    &arms[0].body[0],
                    &arms[1].body[0],
                ) {
                    (
                        BranchTest::Condition(test),
                        BranchTest::Wildcard,
                        hir::Stmt::Expr(yes),
                        hir::Stmt::Expr(no),
                    ) => {
                        self.saved_boolean(*test, budget)
                            && self.saved_boolean(*yes, budget)
                            && self.saved_boolean(*no, budget)
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// Lower boolean value selection directly to control flow. HIR represents
    /// short-circuit operators as two single-expression branch arms; threading
    /// their results avoids throwing away operand facts at a temporary join.
    fn condition(&mut self, expression: ExprId, targets: [BlockId; 2]) {
        match &self.hir.expressions[expression] {
            hir::Expr::Bool(value) => {
                self.terminate(Terminator::Goto(targets[usize::from(!*value)]));
                return;
            }
            hir::Expr::Unary {
                operator: crate::ast::UnaryOp::Not,
                operand,
            } => {
                self.condition(*operand, [targets[1], targets[0]]);
                return;
            }
            hir::Expr::Branch {
                subject: None,
                arms,
            } if arms.len() == 2 && arms[0].body.len() == 1 && arms[1].body.len() == 1 => {
                if let (
                    BranchTest::Condition(test),
                    BranchTest::Wildcard,
                    hir::Stmt::Expr(yes),
                    hir::Stmt::Expr(no),
                ) = (
                    &arms[0].test,
                    &arms[1].test,
                    &arms[0].body[0],
                    &arms[1].body[0],
                ) {
                    let (test, yes, no) = (*test, *yes, *no);
                    let matched = self.block();
                    let unmatched = self.block();
                    self.condition(test, [matched, unmatched]);
                    self.current = matched;
                    self.condition(yes, targets);
                    self.current = unmatched;
                    self.condition(no, targets);
                    return;
                }
            }
            _ => {}
        }
        self.expression(expression, Context::Read);
        if let Some(condition) = self.boolean_condition_place(expression) {
            self.terminate(Terminator::BooleanBranch { condition, targets });
        } else if let Some((comparison, polarity)) = self.comparison_condition(expression) {
            let targets = if polarity {
                targets
            } else {
                [targets[1], targets[0]]
            };
            self.terminate(Terminator::ComparisonBranch {
                comparison,
                targets,
            });
        } else {
            self.terminate(Terminator::Branch(targets.to_vec()));
        }
    }

    fn full_expression_condition(&mut self, expression: ExprId, targets: [BlockId; 2]) {
        self.begin_full_expression();
        let cleanup = [self.block(), self.block()];
        self.condition(expression, cleanup);
        self.current = cleanup[0];
        self.end_full_expression(self.span(expression));
        let operations = self.blocks[self.current].operations.clone();
        self.terminate(Terminator::Goto(targets[0]));
        self.current = cleanup[1];
        self.blocks[self.current].operations.extend(operations);
        self.terminate(Terminator::Goto(targets[1]));
    }

    fn boolean_condition_place(&self, expression: ExprId) -> Option<Place> {
        let ty = self.types.expression_type(expression)?;
        matches!(self.types.types[ty], crate::types::Type::Bool)
            .then(|| self.predicate_place(expression))
            .flatten()
    }

    fn predicate_place(&self, expression: ExprId) -> Option<Place> {
        let place = self.owned_place(expression)?;
        // A computed index can change independently of the indexed storage.
        // Until its dependencies are represented, do not attach reusable facts.
        (!place
            .projections
            .iter()
            .any(|projection| matches!(projection, hir::Projection::Index { constant: None, .. })))
        .then_some(place)
    }

    fn comparison_condition(&self, expression: ExprId) -> Option<(Comparison, bool)> {
        let hir::Expr::Binary {
            left,
            operator,
            right,
        } = self.hir.expressions[expression]
        else {
            return None;
        };
        let integer_order = [left, right].into_iter().all(|operand| {
            self.types
                .expression_type(operand)
                .is_some_and(|ty| matches!(self.types.types[ty], crate::types::Type::Int))
        });
        let mut left = self.comparison_operand(left)?;
        let mut right = self.comparison_operand(right)?;
        let (mut kind, mut polarity) = match operator {
            crate::ast::BinaryOp::Equal => (ComparisonKind::Equal, true),
            crate::ast::BinaryOp::NotEqual => (ComparisonKind::Equal, false),
            crate::ast::BinaryOp::Less => (ComparisonKind::Less, true),
            crate::ast::BinaryOp::LessEqual => (ComparisonKind::LessEqual, true),
            crate::ast::BinaryOp::Greater => {
                std::mem::swap(&mut left, &mut right);
                (ComparisonKind::Less, true)
            }
            crate::ast::BinaryOp::GreaterEqual => {
                std::mem::swap(&mut left, &mut right);
                (ComparisonKind::LessEqual, true)
            }
            _ => return None,
        };
        // Integer order is total. Do not apply this complement identity to
        // floating-point operands, where NaN makes both comparisons false.
        if integer_order && kind == ComparisonKind::LessEqual {
            std::mem::swap(&mut left, &mut right);
            kind = ComparisonKind::Less;
            polarity = !polarity;
        }
        Some((Comparison { left, kind, right }, polarity))
    }

    fn comparison_operand(&self, expression: ExprId) -> Option<ComparisonOperand> {
        self.comparison_operand_with_budget(expression, &mut 64)
    }

    fn comparison_operand_with_budget(
        &self,
        expression: ExprId,
        budget: &mut usize,
    ) -> Option<ComparisonOperand> {
        *budget = budget.checked_sub(1)?;
        match self.hir.expressions[expression] {
            hir::Expr::Integer(value) => Some(ComparisonOperand::Integer(value)),
            hir::Expr::Binary {
                left,
                operator,
                right,
            } if matches!(
                operator,
                crate::ast::BinaryOp::Add
                    | crate::ast::BinaryOp::Subtract
                    | crate::ast::BinaryOp::Multiply
            ) && [expression, left, right].into_iter().all(|operand| {
                self.types
                    .expression_type(operand)
                    .is_some_and(|ty| matches!(self.types.types[ty], crate::types::Type::Int))
            }) =>
            {
                Some(ComparisonOperand::Arithmetic {
                    left: Box::new(self.comparison_operand_with_budget(left, budget)?),
                    operator,
                    right: Box::new(self.comparison_operand_with_budget(right, budget)?),
                })
            }
            _ => self
                .predicate_place(expression)
                .map(ComparisonOperand::Place),
        }
    }

    fn initialize(&mut self, local: hir::LocalId, span: std::ops::Range<usize>) {
        self.emit(Operation::Initialize {
            place: Self::local_place(local),
            span,
        });
    }

    /// Opens the lifetime boundary for every temporary created while
    /// evaluating one complete source expression.
    fn begin_full_expression(&mut self) {
        self.temporary_scopes.push(Vec::new());
    }

    /// Destroys full-expression temporaries in reverse creation order.
    fn end_full_expression(&mut self, span: std::ops::Range<usize>) {
        let temporaries = self
            .temporary_scopes
            .pop()
            .expect("temporary scopes are balanced");
        for (expression, place) in temporaries.into_iter().rev() {
            self.emit(Operation::Destroy {
                place,
                span: span.clone(),
            });
            self.active_temporaries.remove(&expression);
        }
    }

    fn emit_active_temporary_destruction(&mut self, span: std::ops::Range<usize>) {
        let places = self
            .temporary_scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().rev())
            .map(|(_, place)| place.clone())
            .collect::<Vec<_>>();
        for place in places {
            self.emit(Operation::Destroy {
                place,
                span: span.clone(),
            });
        }
    }

    fn failure_edge(&mut self, operation: FailureOperation, expression: ExprId) {
        let continued = self.block();
        let failed = self.block();
        let span = self.span(expression);
        self.terminate(Terminator::Checked {
            operation,
            span: span.clone(),
            targets: [continued, failed],
        });
        self.current = failed;
        self.emit_active_temporary_destruction(span.clone());
        self.emit_scope_destruction(span);
        self.terminate(Terminator::Fail);
        self.current = continued;
    }

    fn call_failure(&self, callee: ExprId) -> Option<FailureOperation> {
        use crate::intrinsics::{Builtin, BuiltinExecution};
        if matches!(
            self.types.resolved_call(callee),
            Some(crate::types::ResolvedCall::Method { remote: true, .. })
        ) {
            // The worker contains failures and delivers them through its future.
            return None;
        }
        let builtin = match self.hir.expressions[callee] {
            hir::Expr::Name(ResolvedName::Builtin(builtin)) => Some(builtin),
            _ => self
                .types
                .resolved_function_for_callee(callee)
                .and_then(|function| self.hir.functions[function].intrinsic.as_deref())
                .and_then(Builtin::from_intrinsic_key),
        };
        if let Some(builtin) = builtin
            && builtin.descriptor().execution == BuiltinExecution::Host
        {
            return matches!(
                builtin,
                Builtin::IoExists
                    | Builtin::IoIsFile
                    | Builtin::IoIsDirectory
                    | Builtin::IoJoin
                    | Builtin::IoParent
                    | Builtin::IoFileName
                    | Builtin::IoExtension
                    | Builtin::TimeMonotonicNow
                    | Builtin::TimeWallNow
            )
            .then_some(FailureOperation::Host { builtin });
        }
        Some(FailureOperation::Call { callee })
    }

    fn materialize_temporary(&mut self, expression: ExprId) -> Place {
        if let Some(place) = self.active_temporaries.get(&expression) {
            return place.clone();
        }
        let place = Place::temporary(TemporaryId(self.next_temporary));
        self.next_temporary += 1;
        let span = self.span(expression);
        self.emit(Operation::Initialize {
            place: place.clone(),
            span: span.clone(),
        });
        let value = if self.copy_expression(expression) {
            BorrowValue::Empty
        } else {
            self.borrow_value(expression)
        };
        self.emit(Operation::StoreBorrower {
            destination: place.clone(),
            value,
            span,
        });
        self.active_temporaries.insert(expression, place.clone());
        self.temporary_scopes
            .last_mut()
            .expect("temporary materialization requires an expression scope")
            .push((expression, place.clone()));
        place
    }

    fn reserve_temporary(&mut self, expression: ExprId) -> Place {
        if let Some(place) = self.active_temporaries.get(&expression) {
            return place.clone();
        }
        let place = Place::temporary(TemporaryId(self.next_temporary));
        self.next_temporary += 1;
        self.emit(Operation::Initialize {
            place: place.clone(),
            span: self.span(expression),
        });
        self.active_temporaries.insert(expression, place.clone());
        self.temporary_scopes
            .last_mut()
            .expect("temporary materialization requires an expression scope")
            .push((expression, place.clone()));
        place
    }

    fn reserve_assignment_temporary(&mut self, expression: ExprId) -> Place {
        let place = Place::temporary(TemporaryId(self.next_temporary));
        self.next_temporary += 1;
        self.emit(Operation::Initialize {
            place: place.clone(),
            span: self.span(expression),
        });
        // Unlike an expression temporary, this is the destination into which
        // the RHS is about to be evaluated. Caching it as the value of the RHS
        // would make that value appear to borrow from itself.
        self.temporary_scopes
            .last_mut()
            .expect("assignment temporary requires an expression scope")
            .push((expression, place.clone()));
        place
    }

    fn pattern_has_bindings(pattern: &hir::Pattern) -> bool {
        match pattern.unspanned() {
            hir::Pattern::Binding(_) => true,
            hir::Pattern::Variant { fields, .. } => fields.iter().any(Self::pattern_has_bindings),
            _ => false,
        }
    }

    fn initialize_pattern(
        &mut self,
        pattern: &hir::Pattern,
        source: Option<&Place>,
        source_expression: Option<ExprId>,
        span: std::ops::Range<usize>,
    ) {
        let span = pattern.span().unwrap_or(span);
        match pattern.unspanned() {
            hir::Pattern::Binding(local) => {
                self.initialize(*local, span.clone());
                if let Some(source) = source {
                    self.emit(Operation::StoreBorrower {
                        destination: Self::local_place(*local),
                        value: BorrowValue::Place(source.clone()),
                        span,
                    });
                }
            }
            hir::Pattern::Variant { fields, .. } => {
                for (index, field) in fields.iter().enumerate() {
                    let projected = source.map(|source| {
                        let mut projected = source.clone();
                        projected.projections.push(hir::Projection::Index {
                            expression: source_expression
                                .expect("pattern projection has a subject expression"),
                            constant: Some(
                                i64::try_from(index).expect("variant payload index fits in an i64"),
                            ),
                        });
                        projected
                    });
                    self.initialize_pattern(
                        field,
                        projected.as_ref(),
                        source_expression,
                        span.clone(),
                    );
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

    fn emit_scope_destruction(&mut self, span: std::ops::Range<usize>) {
        let definition = &self.hir.functions[self.function];
        let parameter_modes = self
            .types
            .function_type(self.function)
            .map(|function| function.parameter_modes.as_slice())
            .unwrap_or_default();
        let owned_parameters = definition
            .parameters
            .iter()
            .zip(parameter_modes)
            .filter_map(|(parameter, mode)| {
                (*mode == crate::ast::ParameterMode::Consume).then_some(*parameter)
            })
            .collect::<std::collections::HashSet<_>>();
        let mut locals = self
            .hir
            .locals
            .iter()
            .filter_map(|(local, definition)| {
                if definition.function != self.function {
                    return None;
                }
                (definition.kind == hir::LocalKind::Binding || owned_parameters.contains(&local))
                    .then_some(local)
            })
            .collect::<Vec<_>>();
        locals.reverse();
        for local in locals {
            self.emit(Operation::Destroy {
                place: Self::local_place(local),
                span: span.clone(),
            });
        }
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
