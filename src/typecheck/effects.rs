use std::collections::{HashMap, HashSet};

use super::*;

pub(super) struct EffectDerivation<'a, 'hir> {
    checker: &'a Checker<'hir>,
    owners: HashMap<LocalId, crate::ast::GroupPath>,
    contract: HashSet<String>,
    derived: Vec<crate::ast::Effect>,
    pub(super) suspends: bool,
}

impl<'a, 'hir> EffectDerivation<'a, 'hir> {
    pub(super) fn new(checker: &'a Checker<'hir>, function: FunctionId) -> Self {
        let definition = &checker.hir.functions[function];
        let mut contract = definition
            .groups
            .iter()
            .map(|group| group.name.clone())
            .collect::<HashSet<_>>();
        if !definition.effects_explicit {
            contract.extend(
                definition
                    .parameters
                    .iter()
                    .map(|parameter| checker.hir.locals[*parameter].name.clone()),
            );
        }
        contract.extend(
            definition
                .effects
                .iter()
                .filter(|effect| {
                    definition
                        .parameters
                        .iter()
                        .any(|parameter| checker.hir.locals[*parameter].name == effect.target.root)
                })
                .map(|effect| effect.target.root.clone()),
        );
        if definition.name.contains('$') {
            contract.extend(
                definition
                    .effects
                    .iter()
                    .map(|effect| effect.target.root.clone()),
            );
        }
        let mut owners = HashMap::new();
        for (index, local) in definition.parameters.iter().enumerate() {
            let reference_parameter_group = checker.functions[&function]
                .parameters
                .get(index)
                .and_then(reference_group)
                .map(crate::ast::GroupPath::root);
            let declared_parameter_group = definition
                .effects
                .iter()
                .any(|effect| {
                    effect.kind != crate::ast::EffectKind::Consume
                        && effect.target.root == checker.hir.locals[*local].name
                })
                .then(|| crate::ast::GroupPath::root(checker.hir.locals[*local].name.clone()));
            let owner = reference_parameter_group
                .or(declared_parameter_group)
                .unwrap_or_else(|| {
                    if definition.effects_explicit {
                        crate::ast::GroupPath::root(FRAME_GROUP)
                    } else {
                        crate::ast::GroupPath::root(checker.hir.locals[*local].name.clone())
                    }
                });
            owners.insert(*local, owner);
        }
        if let Some(self_local) = definition.receiver {
            owners.insert(self_local, crate::ast::GroupPath::root("self"));
            contract.insert("self".to_owned());
        }
        for (local, ty) in &checker.locals {
            if checker.hir.locals[*local].function != function {
                let group =
                    checker.local_groups.get(local).cloned().unwrap_or_else(|| {
                        reference_group(&checker.resolved(ty.clone())).unwrap_or_else(|| {
                            match checker.hir.locals[*local].name.as_str() {
                                "self" => "self".to_owned(),
                                _ => FRAME_GROUP.to_owned(),
                            }
                        })
                    });
                owners.entry(*local).or_insert_with(|| group.into());
            }
        }
        Self {
            checker,
            owners,
            contract,
            derived: Vec::new(),
            suspends: false,
        }
    }

    pub(super) fn effects(&self) -> Vec<crate::ast::Effect> {
        self.derived.clone()
    }

    fn add(&mut self, kind: crate::ast::EffectKind, group: crate::ast::GroupPath) {
        if group.root == FRAME_GROUP || !self.contract.contains(&group.root) {
            return;
        }
        let effect = crate::ast::Effect {
            kind,
            target: group,
        };
        if effects_are_subset(std::slice::from_ref(&effect), &self.derived) {
            return;
        }
        self.derived.retain(|old| {
            !effects_are_subset(std::slice::from_ref(old), std::slice::from_ref(&effect))
        });
        self.derived.push(effect);
    }

    pub(super) fn walk_statements(&mut self, statements: &crate::block::Block<hir::Stmt>) {
        self.walk_statement_block(statements, true);
    }

    fn walk_statement_block(
        &mut self,
        statements: &crate::block::Block<hir::Stmt>,
        consumes_result: bool,
    ) {
        for (index, statement) in statements.iter().enumerate() {
            let is_last = consumes_result && index + 1 == statements.len();
            match statement {
                hir::Stmt::Return { value, guard } => {
                    if let Some(guard) = guard {
                        self.walk_expr(*guard);
                    }
                    self.walk_consumed_expr(*value);
                }
                hir::Stmt::Assert { condition, message } => {
                    self.walk_expr(*condition);
                    if let Some(message) = message {
                        self.walk_expr(*message);
                    }
                }
                hir::Stmt::Loop { body, .. } => {
                    self.walk_statement_block(body, false);
                }
                hir::Stmt::Break { guard } | hir::Stmt::Continue { guard } => {
                    if let Some(guard) = guard {
                        self.walk_expr(*guard);
                    }
                }
                hir::Stmt::Bind { local, value } => {
                    self.walk_consumed_expr(*value);
                    let group = match self.checker.hir.expressions[*value] {
                        hir::Expr::Reference(place) => self.place_group(place),
                        _ => self
                            .checker
                            .locals
                            .get(local)
                            .and_then(|ty| reference_group(&self.checker.resolved(ty.clone())))
                            .map(crate::ast::GroupPath::root)
                            .unwrap_or_else(|| crate::ast::GroupPath::root(FRAME_GROUP)),
                    };
                    self.owners.insert(*local, group);
                }
                hir::Stmt::Assign { local, value } => {
                    self.walk_consumed_expr(*value);
                    self.add(crate::ast::EffectKind::Mut, self.local_group(*local));
                }
                hir::Stmt::Expr(value) if is_last => self.walk_consumed_expr(*value),
                hir::Stmt::Expr(value) => self.walk_expr(*value),
                hir::Stmt::Set { place, value } => {
                    self.walk_consumed_expr(*value);
                    self.walk_place_address(*place);
                    self.add(crate::ast::EffectKind::Mut, self.place_group(*place));
                }
            }
        }
    }

    fn walk_consumed_expr(&mut self, expression: ExprId) {
        match &self.checker.hir.expressions[expression] {
            hir::Expr::Member { name, .. }
                if matches!(name.as_str(), "iterator" | "head" | "rest") =>
            {
                // Sequence views and Iterable cursors are borrowed projections;
                // producing one does not transfer the source collection.
                self.walk_expr(expression);
            }
            hir::Expr::Name(hir::ResolvedName::Local(local))
                if !self.expression_is_copy(expression) =>
            {
                self.add(crate::ast::EffectKind::Consume, self.local_group(*local));
            }
            hir::Expr::Member { .. } | hir::Expr::Index { .. }
                if !self.expression_is_copy(expression) =>
            {
                self.add(
                    crate::ast::EffectKind::Consume,
                    self.place_group(expression),
                );
            }
            hir::Expr::List(values) => {
                values
                    .iter()
                    .for_each(|value| self.walk_consumed_expr(*value));
            }
            hir::Expr::Record { fields, .. } => {
                fields
                    .iter()
                    .for_each(|(_, value)| self.walk_consumed_expr(*value));
            }
            hir::Expr::Branch { subject, arms } => {
                if let Some(subject) = subject {
                    self.walk_expr(*subject);
                }
                for arm in arms {
                    if let hir::BranchTest::Condition(test) = arm.test {
                        self.walk_expr(test);
                    }
                    self.walk_statement_block(&arm.body, true);
                }
            }
            hir::Expr::Closure { captures, .. } => {
                for capture in captures {
                    match capture.mode {
                        hir::CaptureMode::Move => self.add(
                            crate::ast::EffectKind::Consume,
                            self.local_group(capture.local),
                        ),
                        hir::CaptureMode::Copy => self.add(
                            crate::ast::EffectKind::Read,
                            self.local_group(capture.local),
                        ),
                        hir::CaptureMode::Ref | hir::CaptureMode::Pending => {}
                    }
                }
            }
            _ => self.walk_expr(expression),
        }
    }

    fn expression_is_copy(&self, expression: ExprId) -> bool {
        self.checker
            .expressions
            .get(&expression)
            .is_some_and(|ty| self.checker.is_copy_type(ty))
    }

    fn walk_expr(&mut self, expression: ExprId) {
        match &self.checker.hir.expressions[expression] {
            hir::Expr::Name(ResolvedName::Local(local)) => {
                self.add(crate::ast::EffectKind::Read, self.local_group(*local))
            }
            hir::Expr::List(values) => values.iter().for_each(|value| self.walk_expr(*value)),
            hir::Expr::Call { callee, arguments } => {
                self.walk_call(*callee, arguments);
                if self.call_target(*callee).is_some_and(|function| {
                    matches!(
                        self.checker.hir.functions[function].intrinsic.as_deref(),
                        Some("list.push" | "list.append")
                    )
                }) {
                    arguments
                        .iter()
                        .for_each(|argument| self.walk_consumed_expr(*argument));
                    return;
                }
                if matches!(
                    self.checker.hir.expressions[*callee],
                    hir::Expr::Name(hir::ResolvedName::Variant(_))
                ) {
                    arguments
                        .iter()
                        .for_each(|argument| self.walk_consumed_expr(*argument));
                    return;
                }
                if !matches!(
                    self.checker.hir.expressions[*callee],
                    hir::Expr::Member { .. }
                ) {
                    self.walk_expr(*callee);
                }
                arguments
                    .iter()
                    .for_each(|argument| self.walk_expr(*argument));
            }
            hir::Expr::Member { .. } => {
                self.add(crate::ast::EffectKind::Read, self.place_group(expression));
            }
            hir::Expr::Index { object, index } => {
                let _ = object;
                self.add(crate::ast::EffectKind::Read, self.place_group(expression));
                self.walk_expr(*index);
            }
            hir::Expr::Reference(place) => self.walk_place_address(*place),
            hir::Expr::MoveOut(place) => {
                self.walk_place_address(*place);
                self.add(crate::ast::EffectKind::Consume, self.place_group(*place));
            }
            hir::Expr::Remote(value) => self.walk_expr(*value),
            hir::Expr::Await(value) => {
                self.suspends = true;
                self.walk_expr(*value);
            }
            hir::Expr::Record { fields, .. } => {
                fields.iter().for_each(|(_, value)| self.walk_expr(*value))
            }
            hir::Expr::Unary { operand, .. } => self.walk_expr(*operand),
            hir::Expr::Binary { left, right, .. } => {
                self.walk_expr(*left);
                self.walk_expr(*right);
            }
            hir::Expr::Branch { subject, arms } => {
                if let Some(subject) = subject {
                    self.walk_expr(*subject);
                }
                for arm in arms {
                    if let hir::BranchTest::Condition(test) = arm.test {
                        self.walk_expr(test);
                    }
                    self.walk_statement_block(&arm.body, false);
                }
            }
            hir::Expr::Closure { .. }
            | hir::Expr::Unit
            | hir::Expr::Bool(_)
            | hir::Expr::Integer(_)
            | hir::Expr::Float(_)
            | hir::Expr::String(_)
            | hir::Expr::CodePoint(_)
            | hir::Expr::Symbol(_)
            | hir::Expr::Name(_) => {}
        }
    }

    fn walk_call(&mut self, callee: ExprId, arguments: &[ExprId]) {
        match &self.checker.hir.expressions[callee] {
            hir::Expr::Member { object, name } => {
                let receiver = self
                    .checker
                    .resolved(self.checker.expressions[object].clone());
                if let Ty::Remote(value) = receiver {
                    let receiver_group = match *value {
                        Ty::Reference(group, _) => Some(group),
                        _ => None,
                    };
                    if let Some(method) = self.method_for(*object, name) {
                        let definition = &self.checker.hir.functions[method];
                        let parameter_names = definition
                            .parameters
                            .iter()
                            .skip(1)
                            .map(|parameter| self.checker.hir.locals[*parameter].name.clone())
                            .collect::<Vec<_>>();
                        let effects = definition.effects.clone();
                        for effect in effects {
                            let group = if effect.target.root == "self" {
                                receiver_group
                                    .as_ref()
                                    .map(|group| crate::ast::GroupPath::root(group.clone()))
                            } else {
                                parameter_names
                                    .iter()
                                    .position(|name| *name == effect.target.root)
                                    .and_then(|index| arguments.get(index))
                                    .map(|argument| self.place_group(*argument))
                            };
                            if let Some(group) = group {
                                self.add(effect.kind, group.with_children(&effect.target.children));
                            }
                        }
                    }
                    return;
                }
                if let Some(method) = self.method_for(*object, name) {
                    self.apply_callee(method, Some(*object), arguments);
                    return;
                }
                if let Some(Ty::Callable {
                    parameters,
                    effects,
                    suspends,
                    ..
                }) = self
                    .checker
                    .expressions
                    .get(&callee)
                    .map(|ty| self.checker.resolved(ty.clone()))
                {
                    self.suspends |= suspends;
                    for effect in effects {
                        let group = if effect.target.root == "self" {
                            self.place_group(*object)
                        } else {
                            parameters
                                .iter()
                                .position(|parameter| {
                                    reference_group(parameter).as_deref()
                                        == Some(effect.target.root.as_str())
                                })
                                .and_then(|index| arguments.get(index))
                                .and_then(|argument| self.argument_group(*argument))
                                .unwrap_or_else(|| {
                                    crate::ast::GroupPath::root(effect.target.root.clone())
                                })
                        }
                        .with_children(&effect.target.children);
                        self.add(effect.kind, group);
                    }
                    return;
                }
                self.add(crate::ast::EffectKind::Read, self.place_group(*object));
            }
            _ => {}
        }
        match self.checker.hir.expressions[callee] {
            hir::Expr::Name(ResolvedName::Builtin(
                Builtin::TcpRead
                | Builtin::TcpWrite
                | Builtin::TcpReadBytes
                | Builtin::TcpWriteBytes,
            )) => {
                if let Some(buffer) = arguments.first() {
                    self.add(crate::ast::EffectKind::Mut, self.place_group(*buffer));
                }
            }
            hir::Expr::Name(ResolvedName::Function(target)) => {
                self.apply_callee(target, None, arguments)
            }
            _ => {
                if let Some(Ty::Callable {
                    parameters,
                    effects,
                    suspends,
                    ..
                }) = self
                    .checker
                    .expressions
                    .get(&callee)
                    .map(|ty| self.checker.resolved(ty.clone()))
                {
                    self.suspends |= suspends;
                    for effect in effects {
                        let group = parameters
                            .iter()
                            .position(|parameter| {
                                reference_group(parameter).as_deref()
                                    == Some(effect.target.root.as_str())
                            })
                            .and_then(|index| arguments.get(index))
                            .and_then(|argument| self.argument_group(*argument))
                            .unwrap_or_else(|| {
                                crate::ast::GroupPath::root(effect.target.root.clone())
                            })
                            .with_children(&effect.target.children);
                        self.add(effect.kind, group);
                    }
                }
            }
        }
    }

    fn apply_callee(&mut self, target: FunctionId, receiver: Option<ExprId>, arguments: &[ExprId]) {
        let definition = &self.checker.hir.functions[target];
        self.suspends |= definition.suspends;
        for effect in &definition.effects {
            let group = match (effect.target.root.as_str(), receiver) {
                ("self", Some(receiver)) => self
                    .place_group(receiver)
                    .with_children(&effect.target.children),
                ("self", None) => crate::ast::GroupPath::root(FRAME_GROUP),
                _ => {
                    let offset = usize::from(receiver.is_some());
                    definition
                        .parameters
                        .iter()
                        .enumerate()
                        .find(|(index, local)| {
                            self.checker.hir.locals[**local].name == effect.target.root
                                || self.checker.functions[&target]
                                    .parameters
                                    .get(*index)
                                    .and_then(reference_group)
                                    .as_deref()
                                    == Some(effect.target.root.as_str())
                        })
                        .and_then(|(index, _)| arguments.get(index.saturating_sub(offset)))
                        .and_then(|argument| self.argument_group(*argument))
                        .unwrap_or_else(|| crate::ast::GroupPath::root(effect.target.root.clone()))
                        .with_children(&effect.target.children)
                }
            };
            self.add(effect.kind, group);
        }
    }

    fn method_for(&self, object: ExprId, name: &str) -> Option<FunctionId> {
        let receiver = self
            .checker
            .resolved(self.checker.expressions.get(&object)?.clone());
        let (module, owner) = match receiver {
            Ty::Record(record, _) => (
                self.checker.hir.records[record].module,
                self.checker.hir.records[record].name.as_str(),
            ),
            Ty::RawBytes => (self.checker.hir.module_named("core.bytes")?, "RawBytes"),
            Ty::RawByteBuffer => (
                self.checker.hir.module_named("core.bytes.buffer")?,
                "RawByteBuffer",
            ),
            _ => return None,
        };
        let method = self
            .checker
            .hir
            .function_named(module, &format!("{owner}.{name}"))?;
        self.checker.hir.functions[method]
            .receiver
            .is_some()
            .then_some(method)
    }

    fn call_target(&self, callee: ExprId) -> Option<FunctionId> {
        match &self.checker.hir.expressions[callee] {
            hir::Expr::Name(ResolvedName::Function(function)) => Some(*function),
            hir::Expr::Member { object, name } => self
                .checker
                .extension_methods
                .get(&callee)
                .copied()
                .or_else(|| self.method_for(*object, name)),
            _ => None,
        }
    }

    fn argument_group(&self, expression: ExprId) -> Option<crate::ast::GroupPath> {
        match self.checker.hir.expressions[expression] {
            hir::Expr::Reference(place) => Some(self.place_group(place)),
            hir::Expr::Name(ResolvedName::Local(local)) => Some(self.local_group(local)),
            _ => self
                .checker
                .expressions
                .get(&expression)
                .and_then(|ty| reference_group(&self.checker.resolved(ty.clone())))
                .map(crate::ast::GroupPath::root),
        }
    }

    fn walk_place_address(&mut self, expression: ExprId) {
        match self.checker.hir.expressions[expression] {
            hir::Expr::Member { object, .. } => self.walk_place_address(object),
            hir::Expr::Index { object, index } => {
                self.walk_place_address(object);
                self.walk_expr(index);
            }
            hir::Expr::Reference(place) => self.walk_place_address(place),
            _ => {}
        }
    }

    fn place_group(&self, expression: ExprId) -> crate::ast::GroupPath {
        match self.checker.hir.expressions[expression] {
            hir::Expr::Name(ResolvedName::Local(local)) => self.local_group(local),
            hir::Expr::Member { object, ref name } => self.place_group(object).child(name.clone()),
            hir::Expr::Index { object, .. } => self.place_group(object).child("items"),
            hir::Expr::Reference(object) => self.place_group(object),
            _ => crate::ast::GroupPath::root(FRAME_GROUP),
        }
    }

    fn local_group(&self, local: LocalId) -> crate::ast::GroupPath {
        self.owners
            .get(&local)
            .cloned()
            .unwrap_or_else(|| crate::ast::GroupPath::root(FRAME_GROUP))
    }
}
