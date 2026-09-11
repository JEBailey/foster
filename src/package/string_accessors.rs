//! Builtin string accessors execute Foster library code even without an import.
//! Before type checking, conservatively load that code for any matching member.
use crate::ast::{BranchTest, ClosureBody, Expr, Program, Stmt};
use crate::block::Block;

pub(super) fn required(program: &Program) -> bool {
    program
        .constants
        .iter()
        .any(|constant| expression(&constant.value))
        || program
            .functions
            .iter()
            .any(|function| block(&function.body))
        || program.tests.iter().any(|test| block(&test.body))
}

fn block(body: &Block<Stmt>) -> bool {
    body.iter().any(|statement| match statement {
        Stmt::Return { value, guard } => {
            expression(value) || guard.as_ref().is_some_and(expression)
        }
        Stmt::Assert { condition, message } => {
            expression(condition) || message.as_ref().is_some_and(expression)
        }
        Stmt::Loop { body } => block(body),
        Stmt::Break { guard } | Stmt::Continue { guard } => guard.as_ref().is_some_and(expression),
        Stmt::Bind { value, .. } | Stmt::Assign { value, .. } | Stmt::Expr(value) => {
            expression(value)
        }
        Stmt::Function(function) => block(&function.body),
        Stmt::Set { place, value } => expression(place) || expression(value),
    })
}

fn expression(value: &Expr) -> bool {
    match value {
        Expr::Member { object, name } => {
            matches!(name.as_str(), "length" | "head" | "rest") || expression(object)
        }
        Expr::Spanned {
            expression: value, ..
        }
        | Expr::Reference(value)
        | Expr::MoveOut(value)
        | Expr::Remote(value)
        | Expr::Await(value)
        | Expr::Try(value)
        | Expr::Unary { operand: value, .. }
        | Expr::Qualified {
            namespace: value, ..
        } => expression(value),
        Expr::List(values) => values.iter().any(expression),
        Expr::Call { callee, arguments } | Expr::PartialApplication { callee, arguments } => {
            expression(callee) || arguments.iter().any(expression)
        }
        Expr::Index { object, index } => expression(object) || expression(index),
        Expr::Record {
            constructor,
            fields,
        } => expression(constructor) || fields.iter().any(|field| expression(&field.value)),
        Expr::Binary { left, right, .. } | Expr::Logical { left, right, .. } => {
            expression(left) || expression(right)
        }
        Expr::Branch { subject, arms } => {
            subject.as_deref().is_some_and(expression)
                || arms.iter().any(|arm| {
                    matches!(&arm.test, BranchTest::Condition(value) if expression(value))
                        || block(&arm.body)
                })
        }
        Expr::Closure { body, .. } => match body {
            ClosureBody::Expression(value) => expression(value),
            ClosureBody::Block(body) => block(body),
        },
        Expr::Unit
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::CodePoint(_)
        | Expr::Symbol(_)
        | Expr::Name(_)
        | Expr::Placeholder => false,
    }
}
