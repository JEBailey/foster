use super::queries::expression_uses_local;
use super::*;

pub(super) fn check_closure_ownership(hir: &PackageHir) -> Result<(), FosterError> {
    for (function_id, function) in hir.functions.iter() {
        let mut moved = std::collections::HashSet::<LocalId>::new();
        check_closure_statement_block(
            hir,
            function_id,
            &function.body,
            &function.span,
            &mut moved,
        )?;
    }
    Ok(())
}

fn check_closure_statement_block(
    hir: &PackageHir,
    function_id: FunctionId,
    statements: &crate::block::Block<Stmt>,
    fallback_span: &std::ops::Range<usize>,
    moved: &mut std::collections::HashSet<LocalId>,
) -> Result<(), FosterError> {
    let function = &hir.functions[function_id];
    for (statement, span) in statements.iter_spanned() {
        let statement_span = if span.is_empty() {
            fallback_span.clone()
        } else {
            span.clone()
        };
        let expressions = statement_expressions(statement);
        if let Some(local) = moved.iter().find(|local| {
            expressions
                .iter()
                .any(|expression| expression_uses_local(hir, *expression, **local))
        }) {
            return Err(FosterError::runtime(format!(
                "in `{}.{}`: captured value `{}` was already moved into a closure",
                hir.modules[function.module].name, function.name, hir.locals[*local].name
            ))
            .with_fallback_location(
                hir.modules[function.module].name.clone(),
                statement_span.clone(),
                "this statement uses a value that was already moved",
            ));
        }
        if let Stmt::Assign { local, .. } = statement
            && moved.contains(local)
        {
            return Err(FosterError::runtime(format!(
                "in `{}.{}`: cannot assign moved value `{}`",
                hir.modules[function.module].name, function.name, hir.locals[*local].name
            ))
            .with_fallback_location(
                hir.modules[function.module].name.clone(),
                statement_span.clone(),
                "this assignment targets a moved value",
            ));
        }
        for expression in expressions {
            if let Expr::Closure { captures, .. } = &hir.expressions[expression] {
                moved.extend(
                    captures
                        .iter()
                        .filter(|capture| capture.mode == CaptureMode::Move)
                        .map(|capture| capture.local),
                );
            }
        }
        if let Stmt::Loop { body } = statement {
            check_closure_statement_block(hir, function_id, body, &statement_span, moved)?;
        }
    }
    Ok(())
}

pub(super) fn validate_groups_and_effects(hir: &PackageHir) -> Result<(), FosterError> {
    for (_, function) in hir.functions.iter() {
        let is_method = function
            .parameters
            .first()
            .is_some_and(|parameter| hir.locals[*parameter].name == "self");
        let parameter_names = function
            .parameters
            .iter()
            .map(|parameter| hir.locals[*parameter].name.as_str())
            .collect::<std::collections::HashSet<_>>();
        let mut type_parameters = std::collections::HashSet::new();
        for parameter in &function.type_parameters {
            if !type_parameters.insert(parameter.as_str()) {
                return Err(FosterError::runtime(format!(
                    "function `{}` declares type parameter `{parameter}` more than once",
                    function.name
                ))
                .with_fallback_location(
                    hir.modules[function.module].name.clone(),
                    function.span.clone(),
                    "this function has duplicate type parameters",
                ));
            }
        }
        let mut declared = std::collections::HashSet::new();
        for group in &function.groups {
            if type_parameters.contains(group.name.as_str()) {
                return Err(FosterError::runtime(format!(
                    "function `{}` uses `{}` as both a type parameter and a group parameter",
                    function.name, group.name
                ))
                .with_fallback_location(
                    hir.modules[function.module].name.clone(),
                    function.span.clone(),
                    "this function reuses a type parameter as a group parameter",
                ));
            }
            if !declared.insert(group.name.as_str()) {
                return Err(FosterError::runtime(format!(
                    "function `{}` declares group `{}` more than once",
                    function.name, group.name
                ))
                .with_fallback_location(
                    hir.modules[function.module].name.clone(),
                    function.span.clone(),
                    "this function has duplicate group parameters",
                ));
            }
        }
        for (annotation, span) in function
            .parameter_types
            .iter()
            .zip(&function.parameter_type_spans)
        {
            if let Some(annotation) = annotation {
                validate_type_groups(annotation, &declared, &function.name).map_err(|error| {
                    error.with_fallback_location(
                        hir.modules[function.module].name.clone(),
                        span.clone().unwrap_or_else(|| function.span.clone()),
                        "this type annotation uses an invalid group",
                    )
                })?;
            }
        }
        if let Some(annotation) = &function.return_type {
            validate_type_groups(annotation, &declared, &function.name).map_err(|error| {
                error.with_fallback_location(
                    hir.modules[function.module].name.clone(),
                    function.span.clone(),
                    "this result type uses an invalid group",
                )
            })?;
        }
        for (index, effect) in function.effects.iter().enumerate() {
            let root = effect.target.root.as_str();
            if !function.name.contains('$')
                && ((root == "self" && !is_method)
                    || (root != "self"
                        && !declared.contains(root)
                        && !parameter_names.contains(root)))
            {
                return Err(FosterError::runtime(format!(
                    "function `{}` uses undeclared effect group `{root}`",
                    function.name
                ))
                .with_fallback_location(
                    hir.modules[function.module].name.clone(),
                    function
                        .effect_spans
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| function.span.clone()),
                    "this effect refers to an undeclared group",
                ));
            }
        }
    }
    Ok(())
}

fn validate_type_groups(
    ty: &ast::TypeExpr,
    declared: &std::collections::HashSet<&str>,
    function: &str,
) -> Result<(), FosterError> {
    match ty {
        ast::TypeExpr::Unit => {}
        ast::TypeExpr::Named(_, arguments) => {
            for argument in arguments {
                validate_type_groups(argument, declared, function)?;
            }
        }
        ast::TypeExpr::Intersection(members) => {
            for member in members {
                validate_type_groups(member, declared, function)?;
            }
        }
        ast::TypeExpr::Reference { group, value } => {
            if !declared.contains(group.as_str()) {
                return Err(FosterError::runtime(format!(
                    "function `{function}` uses undeclared reference group `{group}`"
                )));
            }
            validate_type_groups(value, declared, function)?;
        }
        ast::TypeExpr::Function {
            parameters,
            result,
            effects,
            ..
        } => {
            for parameter in parameters {
                validate_type_groups(parameter, declared, function)?;
            }
            validate_type_groups(result, declared, function)?;
            for effect in effects {
                let root = effect.target.root.as_str();
                if root != "self" && !declared.contains(root) {
                    return Err(FosterError::runtime(format!(
                        "function `{function}` uses undeclared effect group `{root}`"
                    )));
                }
            }
        }
    }
    Ok(())
}

pub(super) fn infer_ref_capture_effects(hir: &mut PackageHir) {
    let closures = hir
        .expressions
        .iter()
        .filter_map(|(_, expression)| match expression {
            Expr::Closure { function, captures } => Some((*function, captures.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    for (function, captures) in closures {
        if !hir.functions[function].effects.is_empty() || hir.functions[function].suspends {
            continue;
        }
        for capture in captures
            .into_iter()
            .filter(|capture| capture.mode == CaptureMode::Ref)
        {
            let local = &hir.locals[capture.local];
            let owner = &hir.functions[local.function];
            let target = owner
                .parameters
                .iter()
                .position(|parameter| *parameter == capture.local)
                .and_then(|index| owner.parameter_types[index].as_ref())
                .and_then(|annotation| match annotation {
                    ast::TypeExpr::Reference { group, .. } => Some(group.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| local.name.clone());
            let effect = ast::Effect {
                kind: capture_effect_kind(hir, function, capture.local),
                target: ast::GroupPath::root(target),
            };
            if !hir.functions[function].effects.contains(&effect) {
                hir.functions[function].effects.push(effect);
                hir.functions[function].effect_spans.push(0..0);
            }
        }
    }
}

fn capture_effect_kind(
    hir: &PackageHir,
    function: FunctionId,
    captured: LocalId,
) -> ast::EffectKind {
    struct CaptureEffect {
        captured: LocalId,
        kind: ast::EffectKind,
    }

    impl super::visit::Visitor for CaptureEffect {
        fn visit_statement(&mut self, hir: &PackageHir, statement: &Stmt) {
            if self.kind == ast::EffectKind::Reshape {
                return;
            }
            if matches!(statement, Stmt::Assign { local, .. } if *local == self.captured)
                || matches!(statement, Stmt::Set { place, .. } if expression_uses_local(hir, *place, self.captured))
            {
                self.kind = ast::EffectKind::Mut;
            }
            super::visit::walk_statement(self, hir, statement);
        }

        fn visit_expression(&mut self, hir: &PackageHir, expression: ExprId) {
            if self.kind == ast::EffectKind::Reshape {
                return;
            }
            if matches!(
                &hir.expressions[expression],
                Expr::Call { callee, .. }
                    if matches!(
                        &hir.expressions[*callee],
                        Expr::Member { object, name }
                            if name == "push"
                                && matches!(
                                    hir.expressions[*object],
                                    Expr::Name(ResolvedName::Local(found)) if found == self.captured
                                )
                    )
            ) {
                self.kind = ast::EffectKind::Reshape;
                return;
            }
            super::visit::walk_expression(self, hir, expression);
        }
    }

    let mut visitor = CaptureEffect {
        captured,
        kind: ast::EffectKind::Read,
    };
    super::visit::Visitor::visit_block(&mut visitor, hir, &hir.functions[function].body);
    visitor.kind
}

fn statement_expressions(statement: &Stmt) -> Vec<ExprId> {
    match statement {
        Stmt::Return { value, guard } => guard.iter().copied().chain([*value]).collect(),
        Stmt::Assert { condition, message } => {
            message.iter().copied().chain([*condition]).collect()
        }
        Stmt::Loop { .. } => Vec::new(),
        Stmt::Break { guard } | Stmt::Continue { guard } => guard.iter().copied().collect(),
        Stmt::Bind { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::Set { value, .. }
        | Stmt::Expr(value) => vec![*value],
    }
}

pub(super) fn infer_capture_modes(
    hir: &mut PackageHir,
    types: &crate::types::TypeInformation,
) -> Result<(), FosterError> {
    let closures = hir
        .expressions
        .iter()
        .filter_map(|(id, expression)| matches!(expression, Expr::Closure { .. }).then_some(id))
        .collect::<Vec<_>>();
    for closure in closures {
        let Expr::Closure { captures, .. } = &mut hir.expressions[closure] else {
            unreachable!()
        };
        for capture in captures {
            let ty = types
                .local_type(capture.local)
                .expect("captured locals have inferred types");
            if capture.mode == CaptureMode::Pending {
                capture.mode = if is_copy_type(types, ty) {
                    CaptureMode::Copy
                } else {
                    CaptureMode::Move
                };
            } else if capture.mode == CaptureMode::Copy && !is_copy_type(types, ty) {
                let local = &hir.locals[capture.local];
                let function = &hir.functions[local.function];
                return Err(FosterError::runtime(format!(
                    "captured value `{}` is not Copy",
                    local.name
                ))
                .with_fallback_location(
                    hir.modules[function.module].name.clone(),
                    local.span.clone(),
                    "this captured value cannot be copied",
                ));
            }
        }
    }
    Ok(())
}

fn is_copy_type(types: &crate::types::TypeInformation, ty: crate::types::TypeId) -> bool {
    types.is_copy(ty)
}
