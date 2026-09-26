//! Lower surface iteration to scoped bindings, loops, and Option pattern matching.
use super::*;

// `$` cannot occur in source identifiers, so neither the import nor cursor can be captured.

impl Parser {
    pub(super) fn install_iteration_import(&self, imports: &mut Vec<Import>) {
        if let Some(span) = &self.iteration_span {
            imports.push(Import {
                span: span.clone(),
                path: vec!["core".into(), "option".into()],
                alias: Some(ITERATION_OPTION_MODULE.into()),
            });
        }
    }

    pub(super) fn for_loop(&mut self) -> Result<Stmt, FosterError> {
        let start = self.tokens[self.current - 1].range.start;
        let item_span = self.peek().range.clone();
        let item = self.expect_ident("expected item name after `for`")?;
        self.expect(&TokenKind::In, "expected `in` after for-loop item")?;
        let collection_start = self.peek().range.start;
        let previous = self.suppress_record_literal;
        self.suppress_record_literal = true;
        let collection = self.expression();
        self.suppress_record_literal = previous;
        let collection = collection?;
        let collection_span = collection_start..self.tokens[self.current - 1].range.end;
        let mut item_body = self.block()?;
        let span = start..self.tokens[self.current - 1].range.end;
        self.iteration_span.get_or_insert_with(|| span.clone());

        let cursor = format!("$for_cursor_{start}");
        let call = |object, name: &str| Expr::Spanned {
            span: collection_span.clone(),
            expression: Box::new(Expr::Call {
                callee: Box::new(Expr::Member {
                    object: Box::new(object),
                    name: name.into(),
                }),
                arguments: Vec::new(),
            }),
        };
        let case = |name: &str, fields| {
            BranchTest::Pattern(Pattern::Variant {
                path: vec![ITERATION_OPTION_MODULE.into(), "Option".into(), name.into()],
                enum_accessor: true,
                fields,
            })
        };
        let item = Pattern::Spanned {
            span: item_span,
            pattern: Box::new(if item == "_" {
                Pattern::Wildcard
            } else {
                Pattern::Binding(item)
            }),
        };
        // A for-loop discards the body's final value and itself produces unit.
        item_body.push(Stmt::Expr(Expr::Unit), span.end..span.end);
        let step = Expr::Branch {
            subject: Some(Box::new(call(Expr::Name(cursor.clone()), "next"))),
            arms: vec![
                BranchArm {
                    test: case("Some", vec![item]),
                    body: item_body,
                },
                BranchArm {
                    test: case("None", vec![]),
                    body: crate::block::Block::single(Stmt::Break { guard: None }, span.clone()),
                },
            ],
        };
        let mut scope = crate::block::Block::single(
            Stmt::Bind {
                name: cursor,
                value: call(collection, "iterator"),
            },
            collection_span.clone(),
        );
        scope.push(
            Stmt::Loop {
                body: crate::block::Block::single(Stmt::Expr(step), span.clone()),
            },
            span.clone(),
        );
        scope.push(Stmt::Expr(Expr::Unit), span.end..span.end);
        // A branch arm provides lexical scope without introducing another loop target.
        Ok(Stmt::Expr(Expr::Spanned {
            span,
            expression: Box::new(Expr::Branch {
                subject: None,
                arms: vec![BranchArm {
                    test: BranchTest::Wildcard,
                    body: scope,
                }],
            }),
        }))
    }
}
