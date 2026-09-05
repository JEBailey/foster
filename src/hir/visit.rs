use super::{BranchTest, Expr, ExprId, LocalId, PackageHir, Stmt};

pub(crate) trait Visitor {
    fn visit_expression(&mut self, hir: &PackageHir, expression: ExprId) {
        walk_expression(self, hir, expression);
    }

    fn visit_statement(&mut self, hir: &PackageHir, statement: &Stmt) {
        walk_statement(self, hir, statement);
    }

    fn visit_block(&mut self, hir: &PackageHir, block: &crate::block::Block<Stmt>) {
        walk_block(self, hir, block);
    }

    fn visit_local_use(&mut self, _local: LocalId) {}

    fn visit_local_definition(&mut self, _local: LocalId) {}
}

pub(crate) fn walk_block<V: Visitor + ?Sized>(
    visitor: &mut V,
    hir: &PackageHir,
    block: &crate::block::Block<Stmt>,
) {
    for statement in block {
        visitor.visit_statement(hir, statement);
    }
}

pub(crate) fn walk_statement<V: Visitor + ?Sized>(
    visitor: &mut V,
    hir: &PackageHir,
    statement: &Stmt,
) {
    match statement {
        Stmt::Return { value, guard } => {
            if let Some(guard) = guard {
                visitor.visit_expression(hir, *guard);
            }
            visitor.visit_expression(hir, *value);
        }
        Stmt::Assert { condition, message } => {
            visitor.visit_expression(hir, *condition);
            if let Some(message) = message {
                visitor.visit_expression(hir, *message);
            }
        }
        Stmt::Loop { body } => visitor.visit_block(hir, body),
        Stmt::Break { guard } | Stmt::Continue { guard } => {
            if let Some(guard) = guard {
                visitor.visit_expression(hir, *guard);
            }
        }
        Stmt::Bind { local, value } => {
            visitor.visit_expression(hir, *value);
            visitor.visit_local_definition(*local);
        }
        Stmt::Assign { local, value } => {
            visitor.visit_expression(hir, *value);
            visitor.visit_local_use(*local);
        }
        Stmt::Expr(value) => visitor.visit_expression(hir, *value),
        Stmt::Set { place, value } => {
            visitor.visit_expression(hir, *value);
            visitor.visit_expression(hir, *place);
        }
    }
}

pub(crate) fn walk_expression<V: Visitor + ?Sized>(
    visitor: &mut V,
    hir: &PackageHir,
    expression: ExprId,
) {
    match &hir.expressions[expression] {
        Expr::Name(super::ResolvedName::Local(local)) => visitor.visit_local_use(*local),
        Expr::List(values) => {
            for value in values {
                visitor.visit_expression(hir, *value);
            }
        }
        Expr::Call { callee, arguments } => {
            visitor.visit_expression(hir, *callee);
            for argument in arguments {
                visitor.visit_expression(hir, *argument);
            }
        }
        Expr::Member { object, .. }
        | Expr::Reference(object)
        | Expr::MoveOut(object)
        | Expr::Remote(object)
        | Expr::Await(object)
        | Expr::Unary {
            operand: object, ..
        } => visitor.visit_expression(hir, *object),
        Expr::Try { value, .. } => visitor.visit_expression(hir, *value),
        Expr::Index { object, index }
        | Expr::Binary {
            left: object,
            right: index,
            ..
        } => {
            visitor.visit_expression(hir, *object);
            visitor.visit_expression(hir, *index);
        }
        Expr::Record { fields, .. } => {
            for (_, value) in fields {
                visitor.visit_expression(hir, *value);
            }
        }
        Expr::Branch { subject, arms } => {
            if let Some(subject) = subject {
                visitor.visit_expression(hir, *subject);
            }
            for arm in arms {
                if let BranchTest::Condition(condition) = arm.test {
                    visitor.visit_expression(hir, condition);
                }
                visitor.visit_block(hir, &arm.body);
            }
        }
        Expr::Closure { .. }
        | Expr::Unit
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::CodePoint(_)
        | Expr::Symbol(_)
        | Expr::Name(_) => {}
    }
}
