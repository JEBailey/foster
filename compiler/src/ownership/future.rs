//! Conservative identification of helpers that return actual remote requests.
//! An abstract Future can also be an immediately ready user computation, so its
//! return annotation alone is not a witness for remote FIFO completion.

use crate::hir::{self, ExprId, FunctionId, visit::Visitor};
use crate::types::{Type, TypeInformation};
use std::collections::HashSet;

pub(super) fn returns_remote_request(
    hir: &hir::PackageHir,
    types: &TypeInformation,
    function: FunctionId,
    visiting: &mut HashSet<FunctionId>,
) -> bool {
    if !visiting.insert(function) {
        return false;
    }
    let body = &hir.functions[function].body;
    let mut proof = Returns {
        types,
        visiting,
        valid: true,
    };
    proof.visit_block(hir, body);
    let valid = proof.valid && proof.result(hir, body);
    visiting.remove(&function);
    valid
}

struct Returns<'a> {
    types: &'a TypeInformation,
    visiting: &'a mut HashSet<FunctionId>,
    valid: bool,
}

impl Returns<'_> {
    fn result(&mut self, hir: &hir::PackageHir, body: &crate::block::Block<hir::Stmt>) -> bool {
        match body.last() {
            Some(hir::Stmt::Expr(value) | hir::Stmt::Return { value, .. }) => {
                self.value(hir, *value)
            }
            _ => false,
        }
    }

    fn value(&mut self, hir: &hir::PackageHir, value: ExprId) -> bool {
        if self.types.expression_type(value).is_some_and(|ty| matches!(&self.types.types[ty], Type::Record { record, .. } if Some(*record) == self.types.core.remote_future)) {
            return true;
        }
        match &hir.expressions[value] {
            hir::Expr::Call { callee, .. } => self
                .types
                .resolved_function_for_callee(*callee)
                .is_some_and(|function| {
                    returns_remote_request(hir, self.types, function, self.visiting)
                }),
            hir::Expr::Branch { arms, .. } => arms.iter().all(|arm| self.result(hir, &arm.body)),
            hir::Expr::Panic(_) => true,
            _ => false,
        }
    }
}

impl Visitor for Returns<'_> {
    fn visit_statement(&mut self, hir: &hir::PackageHir, statement: &hir::Stmt) {
        if let hir::Stmt::Return { value, .. } = statement {
            self.valid &= self.value(hir, *value);
        }
        hir::visit::walk_statement(self, hir, statement);
    }
}
