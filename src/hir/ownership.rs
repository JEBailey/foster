use super::queries::expression_uses_local;
use super::*;

pub(super) fn check_closure_ownership(hir: &PackageHir) -> Result<(), FosterError> {
    for (_, function) in hir.functions.iter() {
        let mut moved = std::collections::HashSet::<LocalId>::new();
        for statement in &function.body {
            let expression = match statement {
                Stmt::Return { value, .. }
                | Stmt::Bind { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::Set { value, .. }
                | Stmt::Expr(value) => *value,
            };
            if let Some(local) = moved
                .iter()
                .find(|local| expression_uses_local(hir, expression, **local))
            {
                return Err(FosterError::runtime(format!(
                    "in `{}.{}`: captured value `{}` was already moved into a closure",
                    hir.modules[function.module].name, function.name, hir.locals[*local].name
                )));
            }
            if let Stmt::Assign { local, .. } = statement
                && moved.contains(local)
            {
                return Err(FosterError::runtime(format!(
                    "in `{}.{}`: cannot assign moved value `{}`",
                    hir.modules[function.module].name, function.name, hir.locals[*local].name
                )));
            }
            if let Expr::Closure { captures, .. } = &hir.expressions[expression] {
                moved.extend(
                    captures
                        .iter()
                        .filter(|capture| capture.mode == CaptureMode::Move)
                        .map(|capture| capture.local),
                );
            }
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
                )));
            }
        }
        let mut declared = std::collections::HashSet::new();
        for group in &function.groups {
            if type_parameters.contains(group.name.as_str()) {
                return Err(FosterError::runtime(format!(
                    "function `{}` uses `{}` as both a type parameter and a group parameter",
                    function.name, group.name
                )));
            }
            if !declared.insert(group.name.as_str()) {
                return Err(FosterError::runtime(format!(
                    "function `{}` declares group `{}` more than once",
                    function.name, group.name
                )));
            }
        }
        for annotation in function
            .parameter_types
            .iter()
            .filter_map(Option::as_ref)
            .chain(function.return_type.iter())
        {
            validate_type_groups(annotation, &declared, &function.name)?;
        }
        for effect in &function.effects {
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
                )));
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
    let mut kind = ast::EffectKind::Read;
    for statement in &hir.functions[function].body {
        if matches!(statement, Stmt::Assign { local, .. } if *local == captured) {
            kind = ast::EffectKind::Mut;
        }
        if matches!(statement, Stmt::Set { place, .. } if expression_uses_local(hir, *place, captured))
        {
            kind = ast::EffectKind::Mut;
        }
        let expression = match statement {
            Stmt::Return { value, .. }
            | Stmt::Bind { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::Set { value, .. }
            | Stmt::Expr(value) => *value,
        };
        if expression_reshapes_local(hir, expression, captured) {
            return ast::EffectKind::Reshape;
        }
    }
    kind
}

fn expression_reshapes_local(hir: &PackageHir, expression: ExprId, local: LocalId) -> bool {
    match &hir.expressions[expression] {
        Expr::Call { callee, arguments } => {
            let direct = matches!(
                &hir.expressions[*callee],
                Expr::Member { object, name }
                    if name == "push"
                        && matches!(hir.expressions[*object], Expr::Name(ResolvedName::Local(found)) if found == local)
            );
            direct
                || expression_reshapes_local(hir, *callee, local)
                || arguments
                    .iter()
                    .any(|argument| expression_reshapes_local(hir, *argument, local))
        }
        Expr::Member { object, .. }
        | Expr::Reference(object)
        | Expr::MoveOut(object)
        | Expr::Remote(object)
        | Expr::Await(object) => {
            expression_reshapes_local(hir, *object, local)
        }
        Expr::Index { object, index }
        | Expr::Binary {
            left: object,
            right: index,
            ..
        } => {
            expression_reshapes_local(hir, *object, local)
                || expression_reshapes_local(hir, *index, local)
        }
        Expr::Unary { operand, .. } => expression_reshapes_local(hir, *operand, local),
        Expr::List(items) => items
            .iter()
            .any(|item| expression_reshapes_local(hir, *item, local)),
        Expr::Branch { subject, arms } => subject.is_some_and(|subject| expression_reshapes_local(hir, subject, local)) || arms.iter().any(|arm| {
            matches!(arm.test, BranchTest::Condition(condition) if expression_reshapes_local(hir, condition, local))
                || expression_reshapes_local(hir, arm.value, local)
        }),
        Expr::Record { fields, .. } => fields
            .iter()
            .any(|(_, value)| expression_reshapes_local(hir, *value, local)),
        _ => false,
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
                return Err(FosterError::runtime(format!(
                    "captured value `{}` is not Copy",
                    hir.locals[capture.local].name
                )));
            }
        }
    }
    Ok(())
}

fn is_copy_type(types: &crate::types::TypeInformation, ty: crate::types::TypeId) -> bool {
    types.is_copy(ty)
}
