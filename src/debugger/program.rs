use crate::{compiler::Compilation, error::FosterError, hir::FunctionId, vm};
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

pub(super) struct Local {
    pub name: String,
    pub register: usize,
    pub scope: Range<usize>,
    pub parameter: bool,
}
pub(super) struct Function {
    pub name: String,
    pub path: PathBuf,
    pub lines: Vec<usize>,
    pub spans: Vec<Range<usize>>,
    pub locals: Vec<Local>,
}
pub(super) struct DebugProgram {
    pub program: vm::Program,
    pub functions: HashMap<FunctionId, Function>,
}

impl DebugProgram {
    pub fn compile(compilation: &Compilation) -> Result<Self, FosterError> {
        let (program, locals) = vm::compile_debug(compilation)?;
        let mut functions = HashMap::new();
        for (id, slots) in locals {
            let declaration = &compilation.hir.functions[id];
            let module = &compilation.hir.modules[declaration.module];
            let Some(source_module) = compilation.package.modules.get(&module.name) else {
                continue;
            };
            let (Some(path), Some(source), Some(bytecode)) = (
                &source_module.source_path,
                &source_module.source,
                program.functions.get(&id),
            ) else {
                continue;
            };
            if !path.exists() {
                continue;
            }
            let mut stack = Vec::new();
            let mut scopes = Vec::new();
            for token in crate::lexer::lex(source)? {
                match token.kind {
                    crate::lexer::TokenKind::LBrace => stack.push(token.range.start),
                    crate::lexer::TokenKind::RBrace => {
                        if let Some(start) = stack.pop() {
                            scopes.push(start..token.range.end)
                        }
                    }
                    _ => {}
                }
            }
            let mut bindings = BindingSpans::default();
            crate::hir::visit::Visitor::visit_block(
                &mut bindings,
                &compilation.hir,
                &declaration.body,
            );
            let locals = slots
                .into_iter()
                .filter_map(|(local, slot)| {
                    let binding_span = bindings.spans.get(&local);
                    let pattern_scope = bindings.pattern_scopes.get(&local);
                    let local = &compilation.hir.locals[local];
                    let span = binding_span.unwrap_or(&local.span);
                    if local.name.starts_with('$') || local.name.starts_with('<') {
                        return None;
                    }
                    let parameter =
                        local.kind == crate::hir::LocalKind::Parameter || local.function != id;
                    let end = scopes
                        .iter()
                        .filter(|scope| scope.contains(&span.start))
                        .min_by_key(|scope| scope.len())
                        .map_or(declaration.span.end, |scope| scope.end);
                    Some(Local {
                        name: local.name.clone(),
                        register: usize::from(slot.0),
                        scope: if let Some(scope) = pattern_scope {
                            scope.clone()
                        } else if parameter {
                            declaration.span.clone()
                        } else {
                            span.end..end
                        },
                        parameter,
                    })
                })
                .collect();
            let spans = bytecode
                .instruction_spans
                .iter()
                .zip(&bytecode.instructions)
                .map(|(span, instruction)| {
                    if span == &declaration.span
                        && matches!(instruction, vm::Instruction::Return { .. })
                    {
                        declaration
                            .body
                            .iter_spanned()
                            .last()
                            .map_or_else(|| span.clone(), |(_, span)| span.clone())
                    } else {
                        span.clone()
                    }
                })
                .collect::<Vec<_>>();
            // Drops and empty compiler-generated spans are not source sequence points.
            let lines = spans
                .iter()
                .zip(&bytecode.instructions)
                .map(|(span, instruction)| {
                    if span.is_empty()
                        || span == &declaration.span
                        || matches!(instruction, vm::Instruction::Drop { .. })
                    {
                        return 0;
                    }
                    source.as_bytes()[..span.start.min(source.len())]
                        .iter()
                        .filter(|byte| **byte == b'\n')
                        .count()
                        + 1
                })
                .collect();
            functions.insert(
                id,
                Function {
                    name: format!("{}::{}", module.name, declaration.name),
                    path: normalize(path.as_std_path()),
                    lines,
                    spans,
                    locals,
                },
            );
        }
        Ok(Self { program, functions })
    }
}

pub(super) fn normalize(path: &Path) -> PathBuf {
    crate::package::watch_path(&path.canonicalize().unwrap_or_else(|_| path.to_owned()))
}

#[derive(Default)]
struct BindingSpans {
    current: Range<usize>,
    spans: HashMap<crate::hir::LocalId, Range<usize>>,
    pattern_scopes: HashMap<crate::hir::LocalId, Range<usize>>,
}
impl crate::hir::visit::Visitor for BindingSpans {
    fn visit_block(
        &mut self,
        hir: &crate::hir::PackageHir,
        block: &crate::block::Block<crate::hir::Stmt>,
    ) {
        for (statement, span) in block.iter_spanned() {
            let previous = std::mem::replace(&mut self.current, span.clone());
            self.visit_statement(hir, statement);
            self.current = previous;
        }
    }
    fn visit_expression(&mut self, hir: &crate::hir::PackageHir, expression: crate::hir::ExprId) {
        if let crate::hir::Expr::Branch { arms, .. } = &hir.expressions[expression] {
            for arm in arms {
                if let crate::hir::BranchTest::Pattern(pattern) = &arm.test {
                    let mut locals = Vec::new();
                    pattern.binding_locals(&mut locals);
                    let mut statements = arm.body.iter_spanned();
                    if let Some((_, first)) = statements.next() {
                        let end = statements.last().map_or(first.end, |(_, span)| span.end);
                        for local in locals {
                            self.pattern_scopes.insert(local, first.start..end);
                        }
                    }
                }
            }
        }
        crate::hir::visit::walk_expression(self, hir, expression);
    }
    fn visit_local_definition(&mut self, local: crate::hir::LocalId) {
        self.spans.insert(local, self.current.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn debugging_uses_the_normal_unoptimized_pipeline() {
        let compilation = crate::compile("func twice(value: Int) -> Int { value * 2 }\nfunc main() -> Int { let value = 21\ntwice(value) }").unwrap();
        let debug = DebugProgram::compile(&compilation).unwrap();
        let normal =
            vm::compile_with_options(&compilation, vm::CompileOptions { optimize: false }).unwrap();
        assert_eq!(debug.program, normal);
    }
}
