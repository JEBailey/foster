use crate::hir::{self, BranchTest, CaptureMode, ExprId, FunctionId, ResolvedName};
use crate::types::TypeInformation;

use super::{
    BasicBlock, BlockId, BorrowValue, Comparison, ComparisonKind, ComparisonOperand, Function,
    InvalidationKind, LoanDefinition, LoanId, MirPoint, Operation, Place, Program, TemporaryId,
    Terminator, UseMode,
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
) -> Program {
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
                result_provenance,
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
    result_provenance: &'a std::collections::HashMap<FunctionId, super::ResultProvenance>,
    loans: Vec<LoanDefinition>,
    loops: Vec<LoopTargets>,
    next_temporary: usize,
    temporary_scopes: Vec<Vec<(ExprId, Place)>>,
    active_temporaries: std::collections::HashMap<ExprId, Place>,
}

#[derive(Clone, Copy)]
struct LoopTargets {
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
            next_temporary: 0,
            temporary_scopes: Vec::new(),
            active_temporaries: std::collections::HashMap::new(),
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
                    self.begin_temporary_scope();
                    self.expression(*guard, Context::Read);
                    self.end_temporary_scope(self.span(*guard));
                    let returned = self.block();
                    let continued = self.block();
                    self.terminate(Terminator::Branch(vec![returned, continued]));
                    self.current = returned;
                    self.begin_temporary_scope();
                    self.expression(*value, Context::Consume);
                    self.emit_return(*value);
                    self.end_temporary_scope(self.span(*value));
                    self.emit_active_temporary_destruction(self.span(*value));
                    self.emit_scope_destruction(self.span(*value));
                    self.terminate(Terminator::Return);
                    self.current = continued;
                } else {
                    self.begin_temporary_scope();
                    self.expression(*value, Context::Consume);
                    self.emit_return(*value);
                    self.end_temporary_scope(self.span(*value));
                    self.emit_active_temporary_destruction(self.span(*value));
                    self.emit_scope_destruction(self.span(*value));
                    self.terminate(Terminator::Return);
                    self.current = self.block();
                }
            }
            hir::Stmt::Assert { condition, message } => {
                self.begin_temporary_scope();
                self.expression(*condition, Context::Read);
                if let Some(message) = message {
                    self.expression(*message, Context::Read);
                }
                let span =
                    message.map_or_else(|| self.span(*condition), |message| self.span(message));
                let failed = self.block();
                let continued = self.block();
                self.terminate(Terminator::Branch(vec![failed, continued]));
                self.current = failed;
                self.emit_active_temporary_destruction(span.clone());
                self.emit_scope_destruction(span.clone());
                self.terminate(Terminator::Fail);
                self.current = continued;
                self.end_temporary_scope(span);
            }
            hir::Stmt::Loop { body, .. } => {
                let cfg = crate::control_flow::LoopCfg::new();
                let blocks = [self.block(), self.block(), self.block()];
                self.terminate(Terminator::Goto(blocks[cfg.header.0]));
                self.current = blocks[cfg.header.0];
                self.terminate(Terminator::Goto(blocks[cfg.body.0]));
                self.current = blocks[cfg.body.0];
                self.loops.push(LoopTargets {
                    continue_to: blocks[cfg.header.0],
                    break_to: blocks[cfg.exit.0],
                });
                for statement in body {
                    self.statement(statement, false);
                }
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
                self.begin_temporary_scope();
                self.expression_into(*value, Context::Consume, Some(Self::local_place(*local)));
                self.initialize(*local, self.span(*value));
                if is_last {
                    self.emit(Operation::ReturnBorrower {
                        value: BorrowValue::Place(Self::local_place(*local)),
                        kind: self.return_kind(*value),
                        span: self.span(*value),
                    });
                }
                self.end_temporary_scope(self.span(*value));
            }
            hir::Stmt::Assign { local, value } => {
                self.begin_temporary_scope();
                self.expression_into(*value, Context::Consume, Some(Self::local_place(*local)));
                self.initialize(*local, self.span(*value));
                self.end_temporary_scope(self.span(*value));
            }
            hir::Stmt::Expr(value) => {
                self.begin_temporary_scope();
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
                self.end_temporary_scope(self.span(*value));
            }
            hir::Stmt::Set { place, value } => {
                self.begin_temporary_scope();
                let destination = self.owned_place(*place);
                self.expression_into(*value, Context::Consume, destination);
                self.place_use(*place, UseMode::Write);
                self.end_temporary_scope(self.span(*value));
            }
        }
    }

    fn loop_transfer(&mut self, guard: Option<ExprId>, target: BlockId) {
        if let Some(guard) = guard {
            self.begin_temporary_scope();
            self.expression(guard, Context::Read);
            self.end_temporary_scope(self.span(guard));
            let transferred = self.block();
            let continued = self.block();
            self.terminate(Terminator::Branch(vec![transferred, continued]));
            self.current = transferred;
            self.emit_active_temporary_destruction(self.span(guard));
            self.terminate(Terminator::Goto(target));
            self.current = continued;
        } else {
            let span = self.hir.functions[self.function].span.clone();
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
                if let hir::Expr::Member { object, .. } = self.hir.expressions[*callee]
                    && self.owned_place(object).is_none()
                {
                    let place = self.materialize_temporary(object);
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
                    if matches!(context, Context::Borrow) && self.owned_place(*argument).is_none() {
                        let place = self.materialize_temporary(*argument);
                        self.emit(Operation::Use {
                            place,
                            mode: UseMode::Borrow,
                            span: self.span(*argument),
                        });
                    }
                }
                self.emit_call_invalidations(*callee, arguments, expression);
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
                    Context::Call => UseMode::Call,
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
                if let Some(place) = hir::queries::expression_place(self.hir, *place) {
                    self.emit(Operation::Invalidate {
                        place: Place::from_hir(place),
                        kind: InvalidationKind::Consume,
                        span: self.span(expression),
                    });
                }
            }
            hir::Expr::Remote(value) => {
                self.expression(*value, Context::Consume);
            }
            hir::Expr::Await(value) => {
                self.expression(*value, Context::Consume);
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
            hir::Expr::Unary { operand, .. } => self.expression(*operand, Context::Read),
            hir::Expr::Binary { left, right, .. } => {
                self.expression(*left, Context::Read);
                self.expression(*right, Context::Read);
            }
            hir::Expr::Branch { subject, arms } => {
                let subject = *subject;
                let arms = arms.clone();
                let destination =
                    (!self.copy_expression(expression)).then(|| self.reserve_temporary(expression));
                self.lower_branch_expression(subject, &arms, context, destination);
            }
            hir::Expr::Closure { captures, .. } => {
                for capture in captures {
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
    }

    fn lower_branch_expression(
        &mut self,
        subject: Option<ExprId>,
        arms: &[hir::BranchArm],
        context: Context,
        destination: Option<Place>,
    ) {
        let pattern_source = if let Some(subject) = subject {
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
        self.terminate(Terminator::Goto(blocks[cfg.entry().0]));

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
                            self.expression(*condition, Context::Read);
                            let targets = [blocks[matched.0], blocks[unmatched.0]];
                            if let Some(condition) = self.boolean_condition_place(*condition) {
                                self.terminate(Terminator::BooleanBranch { condition, targets });
                            } else if let Some((comparison, polarity)) =
                                self.comparison_condition(*condition)
                            {
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
                        (BranchTest::Pattern(pattern), Some(unmatched)) => {
                            let matched = blocks[matched.0];
                            let unmatched = blocks[unmatched.0];
                            match pattern.unspanned() {
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
        if let Some(place) = self.owned_place(expression) {
            self.emit(Operation::Use {
                place,
                mode,
                span: self.span(expression),
            });
        }
    }

    fn borrow_value(&mut self, expression: ExprId) -> BorrowValue {
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
            | hir::Expr::Index { .. }
                if self.owned_place(expression).is_some() =>
            {
                BorrowValue::Place(self.owned_place(expression).unwrap())
            }
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
            hir::Expr::Closure { captures, .. } => BorrowValue::Merge(
                captures
                    .iter()
                    .filter_map(|capture| match capture.mode {
                        CaptureMode::Ref => {
                            let origin = Self::local_place(capture.local);
                            Some(self.issue_reborrow(origin, self.span(expression)))
                        }
                        CaptureMode::Move | CaptureMode::Pending => {
                            Some(BorrowValue::MovePlace(Self::local_place(capture.local)))
                        }
                        CaptureMode::Copy => None,
                    })
                    .collect(),
            ),
            hir::Expr::Call { callee, arguments } => {
                self.call_result_borrow_value(*callee, arguments)
            }
            hir::Expr::Branch { .. } => BorrowValue::Empty,
            hir::Expr::MoveOut(value) => hir::queries::expression_place(self.hir, *value)
                .map(Place::from_hir)
                .map(BorrowValue::MovePlace)
                .unwrap_or(BorrowValue::Empty),
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

    fn call_result_borrow_value(&mut self, callee: ExprId, arguments: &[ExprId]) -> BorrowValue {
        let direct = self.types.resolved_function_for_callee(callee);
        let Some(function) = direct else {
            let mut values = vec![self.borrow_value(callee)];
            values.extend(
                arguments
                    .iter()
                    .map(|argument| self.borrow_value(*argument)),
            );
            return BorrowValue::Merge(values);
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
        let place = Place::from_hir(hir::queries::expression_place(self.hir, expression)?);
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

    fn boolean_condition_place(&self, expression: ExprId) -> Option<Place> {
        let ty = self.types.expression_type(expression)?;
        matches!(self.types.types[ty], crate::types::Type::Bool)
            .then(|| self.owned_place(expression))
            .flatten()
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
        let mut left = self.comparison_operand(left)?;
        let mut right = self.comparison_operand(right)?;
        let (kind, polarity) = match operator {
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
        Some((Comparison { left, kind, right }, polarity))
    }

    fn comparison_operand(&self, expression: ExprId) -> Option<ComparisonOperand> {
        match self.hir.expressions[expression] {
            hir::Expr::Integer(value) => Some(ComparisonOperand::Integer(value)),
            _ => self.owned_place(expression).map(ComparisonOperand::Place),
        }
    }

    fn initialize(&mut self, local: hir::LocalId, span: std::ops::Range<usize>) {
        self.emit(Operation::Initialize {
            place: Self::local_place(local),
            span,
        });
    }

    fn begin_temporary_scope(&mut self) {
        self.temporary_scopes.push(Vec::new());
    }

    fn end_temporary_scope(&mut self, span: std::ops::Range<usize>) {
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
