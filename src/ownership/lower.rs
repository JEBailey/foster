use crate::hir::{self, BranchTest, CaptureMode, ExprId, FunctionId, ResolvedName};
use crate::types::TypeInformation;

use super::{
    BasicBlock, BlockId, BorrowValue, Function, InvalidationKind, LoanDefinition, LoanId, MirPoint,
    Operation, Program, Terminator, UseMode,
};

#[derive(Clone, Copy)]
enum Context {
    Read,
    Borrow,
    Consume,
    Call,
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
    loans: Vec<LoanDefinition>,
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
            loans: Vec::new(),
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
            self.blocks[self.current].terminator = Terminator::Return;
        }
        Function {
            entry: 0,
            blocks: self.blocks,
            loans: self.loans,
            result_provenance: result_provenance(definition, self.hir),
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
                    self.emit_return(*value);
                    self.terminate(Terminator::Return);
                    self.current = continued;
                } else {
                    self.expression(*value, Context::Consume);
                    self.emit_return(*value);
                    self.terminate(Terminator::Return);
                    self.current = self.block();
                }
            }
            hir::Stmt::Bind { local, value } => {
                self.expression_into(*value, Context::Consume, Some(Self::local_place(*local)));
                self.initialize(*local, self.span(*value));
                if is_last {
                    self.emit(Operation::ReturnBorrower {
                        value: BorrowValue::Place(Self::local_place(*local)),
                        kind: self.return_kind(*value),
                        span: self.span(*value),
                    });
                }
            }
            hir::Stmt::Assign { local, value } => {
                self.expression_into(*value, Context::Consume, Some(Self::local_place(*local)));
                self.initialize(*local, self.span(*value));
            }
            hir::Stmt::Expr(value) => {
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
            }
            hir::Stmt::Set { place, value } => {
                let destination = self.owned_place(*place);
                self.expression_into(*value, Context::Consume, destination);
                self.place_use(*place, UseMode::Write);
            }
        }
    }

    fn expression_into(
        &mut self,
        expression: ExprId,
        context: Context,
        destination: Option<hir::Place>,
    ) {
        if let hir::Expr::Branch { subject, arms } = &self.hir.expressions[expression]
            && let Some(destination) = destination
        {
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
                self.expression_into(arm.value, context, Some(destination.clone()));
                self.terminate(Terminator::Goto(join));
            }
            self.current = join;
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
            hir::Expr::Reference(place) => self.place_use(*place, UseMode::Borrow),
            hir::Expr::MoveOut(place) => {
                self.place_use(*place, UseMode::Move);
                if let Some(place) = hir::queries::expression_place(self.hir, *place) {
                    self.emit(Operation::Invalidate {
                        place,
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

    fn borrow_value(&mut self, expression: ExprId) -> BorrowValue {
        match &self.hir.expressions[expression] {
            hir::Expr::Reference(origin) => {
                let Some(origin) = hir::queries::expression_place(self.hir, *origin) else {
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
            hir::Expr::List(values) => BorrowValue::Merge(
                values
                    .iter()
                    .map(|value| self.borrow_value(*value))
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
                .map(BorrowValue::MovePlace)
                .unwrap_or(BorrowValue::Empty),
            hir::Expr::Remote(value)
            | hir::Expr::Await(value)
            | hir::Expr::Unary { operand: value, .. } => self.borrow_value(*value),
            hir::Expr::Binary { left, right, .. } => {
                BorrowValue::Merge(vec![self.borrow_value(*left), self.borrow_value(*right)])
            }
            _ => BorrowValue::Empty,
        }
    }

    fn call_result_borrow_value(&mut self, callee: ExprId, arguments: &[ExprId]) -> BorrowValue {
        let direct = match self.hir.expressions[callee] {
            hir::Expr::Name(ResolvedName::Function(function)) => Some(function),
            hir::Expr::Member { .. } => self.types.extension_methods.get(&callee).copied(),
            _ => None,
        };
        let Some(function) = direct else {
            let mut values = vec![self.borrow_value(callee)];
            values.extend(
                arguments
                    .iter()
                    .map(|argument| self.borrow_value(*argument)),
            );
            return BorrowValue::Merge(values);
        };
        let definition = &self.hir.functions[function];
        let offset = usize::from(matches!(
            self.hir.expressions[callee],
            hir::Expr::Member { .. }
        ));
        let summary = result_provenance(definition, self.hir);
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

    fn issue_reborrow(&mut self, origin: hir::Place, span: std::ops::Range<usize>) -> BorrowValue {
        let id = LoanId(self.loans.len());
        self.loans.push(LoanDefinition {
            id,
            origin: origin.clone(),
            issued_at: MirPoint {
                block: self.current,
                operation: self.blocks[self.current].operations.len(),
            },
            parent: None,
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
                place,
                kind,
                span: self.span(expression),
            });
        }
    }

    fn local_place(local: hir::LocalId) -> hir::Place {
        hir::Place {
            root: local,
            projections: Vec::new(),
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

fn result_provenance(function: &hir::Function, hir: &hir::PackageHir) -> super::ResultProvenance {
    let parameters = function
        .parameter_types
        .iter()
        .enumerate()
        .filter_map(|(index, annotation)| {
            let crate::ast::TypeExpr::Reference { group, .. } = annotation.as_ref()? else {
                return None;
            };
            hir::queries::type_exposes_group(function.return_type.as_ref(), group).then_some(index)
        })
        .collect::<Vec<_>>();
    let receiver = function
        .parameters
        .first()
        .is_some_and(|parameter| hir.locals[*parameter].name == "self")
        && hir::queries::type_exposes_group(function.return_type.as_ref(), "self");
    super::ResultProvenance {
        fresh_owned: parameters.is_empty() && !receiver,
        parameters,
        receiver,
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
