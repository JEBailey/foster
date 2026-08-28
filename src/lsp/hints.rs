use lsp_types::{
    Documentation, InlayHint, InlayHintKind, InlayHintLabel, InlayHintLabelPart,
    InlayHintLabelPartTooltip, InlayHintParams, InlayHintTooltip, MarkupContent, MarkupKind,
    ParameterInformation, ParameterLabel, SignatureHelp, SignatureHelpParams, SignatureInformation,
};

use crate::hir::{Compilation, Expr, ExprId, LocalKind};

use super::byte_range_to_lsp;
use super::workspace::{callable_presentation, module_for_uri, position_to_offset};

struct FunctionMapping {
    semantic: std::ops::Range<usize>,
    current: std::ops::Range<usize>,
}

pub(super) fn inlay_hints(
    compilation: &Compilation,
    params: &InlayHintParams,
) -> Option<Vec<InlayHint>> {
    let module_id = module_for_uri(compilation, &params.text_document.uri)?;
    let module = &compilation.hir.modules[module_id];
    let source = compilation
        .package
        .module(&module.name)?
        .source
        .as_deref()?;
    let requested_start = position_to_offset(source, params.range.start)?;
    let requested_end = position_to_offset(source, params.range.end)?;
    let mut hints = Vec::new();

    for (local_id, local) in compilation
        .hir
        .locals
        .iter()
        .filter(|(_, local)| compilation.hir.functions[local.function].module == module_id)
    {
        if local.span.end < requested_start || local.span.end > requested_end {
            continue;
        }
        let inferred = match local.kind {
            LocalKind::Binding => true,
            LocalKind::Parameter => {
                let function = &compilation.hir.functions[local.function];
                function
                    .parameters
                    .iter()
                    .position(|parameter| *parameter == local_id)
                    .and_then(|index| function.parameter_types.get(index))
                    .is_some_and(Option::is_none)
            }
        };
        if !inferred {
            continue;
        }
        let Some(ty) = compilation.types.local_type(local_id) else {
            continue;
        };
        let display = compilation.types.display(ty);
        hints.push(InlayHint {
            position: byte_range_to_lsp(source, local.span.end..local.span.end).start,
            label: InlayHintLabel::String(format!(": {display}")),
            kind: Some(InlayHintKind::TYPE),
            text_edits: None,
            tooltip: Some(InlayHintTooltip::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: format!("Inferred type: `{display}`"),
            })),
            padding_left: None,
            padding_right: None,
            data: None,
        });
    }

    for (expression_id, expression) in compilation.hir.expressions.iter() {
        let Expr::Call { callee, arguments } = expression else {
            continue;
        };
        if compilation
            .hir
            .expression_functions
            .get(&expression_id)
            .is_none_or(|function| compilation.hir.functions[*function].module != module_id)
        {
            continue;
        }
        let Some(callable) = callable_presentation(compilation, *callee) else {
            continue;
        };
        for (argument, parameter) in arguments.iter().zip(&callable.parameters) {
            let Some(span) = compilation.hir.expression_spans.get(argument) else {
                continue;
            };
            if span.start < requested_start || span.start > requested_end {
                continue;
            }
            if matches!(&compilation.hir.expressions[*argument], Expr::Name(crate::hir::ResolvedName::Local(local)) if compilation.hir.locals[*local].name == *parameter)
            {
                continue;
            }
            hints.push(InlayHint {
                position: byte_range_to_lsp(source, span.start..span.start).start,
                label: InlayHintLabel::LabelParts(vec![InlayHintLabelPart {
                    value: format!("{parameter}:"),
                    tooltip: Some(InlayHintLabelPartTooltip::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: callable
                            .documentation
                            .clone()
                            .unwrap_or_else(|| format!("Parameter of `{}`", callable.signature)),
                    })),
                    location: callable.definition.clone(),
                    command: None,
                }]),
                kind: Some(InlayHintKind::PARAMETER),
                text_edits: None,
                tooltip: None,
                padding_left: None,
                padding_right: Some(true),
                data: None,
            });
        }
    }
    hints.sort_by_key(|hint| (hint.position.line, hint.position.character));
    Some(hints)
}

/// Reuse hints from a last-good compilation only inside functions whose complete source text is
/// unchanged. This keeps a broken function from suppressing hints elsewhere without risking stale
/// insertion positions in the edited function itself.
pub(super) fn inlay_hints_for_unchanged_functions(
    compilation: &Compilation,
    params: &InlayHintParams,
    current_source: &str,
) -> Option<Vec<InlayHint>> {
    let module_id = module_for_uri(compilation, &params.text_document.uri)?;
    let module = &compilation.hir.modules[module_id];
    let source_module = compilation.package.module(&module.name)?;
    let semantic_source = source_module.source.as_deref()?;
    let program = source_module.program.as_ref()?;
    let mappings = program
        .functions
        .iter()
        .map(|function| &function.span)
        .chain(program.tests.iter().map(|test| &test.span))
        .filter_map(|span| unchanged_function_mapping(semantic_source, current_source, span))
        .collect::<Vec<_>>();
    if mappings.is_empty() {
        return Some(Vec::new());
    }

    let mut semantic_params = params.clone();
    semantic_params.range =
        byte_range_to_lsp(semantic_source, 0..semantic_source.len().saturating_sub(1));
    let requested_start = position_to_offset(current_source, params.range.start)?;
    let requested_end = position_to_offset(current_source, params.range.end)?;
    let mut hints = inlay_hints(compilation, &semantic_params)?;
    hints.retain_mut(|hint| {
        let Some(semantic_offset) = position_to_offset(semantic_source, hint.position) else {
            return false;
        };
        let Some(mapping) = mappings.iter().find(|mapping| {
            mapping.semantic.start <= semantic_offset && semantic_offset <= mapping.semantic.end
        }) else {
            return false;
        };
        let current_offset = mapping.current.start + semantic_offset - mapping.semantic.start;
        if current_offset < requested_start || current_offset > requested_end {
            return false;
        }
        hint.position = byte_range_to_lsp(current_source, current_offset..current_offset).start;
        remap_hint_locations(
            hint,
            &params.text_document.uri,
            semantic_source,
            current_source,
            &mappings,
        );
        true
    });
    hints.sort_by_key(|hint| (hint.position.line, hint.position.character));
    Some(hints)
}

fn unchanged_function_mapping(
    semantic_source: &str,
    current_source: &str,
    span: &std::ops::Range<usize>,
) -> Option<FunctionMapping> {
    let text = semantic_source.get(span.clone())?;
    let mut matches = current_source.match_indices(text);
    let (start, _) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(FunctionMapping {
        semantic: span.clone(),
        current: start..start + text.len(),
    })
}

fn remap_hint_locations(
    hint: &mut InlayHint,
    uri: &lsp_types::Uri,
    semantic_source: &str,
    current_source: &str,
    mappings: &[FunctionMapping],
) {
    let InlayHintLabel::LabelParts(parts) = &mut hint.label else {
        return;
    };
    for part in parts {
        let Some(location) = &mut part.location else {
            continue;
        };
        if &location.uri != uri {
            continue;
        }
        let Some(start) = position_to_offset(semantic_source, location.range.start) else {
            continue;
        };
        let Some(end) = position_to_offset(semantic_source, location.range.end) else {
            continue;
        };
        let Some(mapping) = mappings
            .iter()
            .find(|mapping| mapping.semantic.start <= start && end <= mapping.semantic.end)
        else {
            continue;
        };
        let current_start = mapping.current.start + start - mapping.semantic.start;
        let current_end = mapping.current.start + end - mapping.semantic.start;
        location.range = byte_range_to_lsp(current_source, current_start..current_end);
    }
}

pub(super) fn signature_help(
    compilation: &Compilation,
    params: &SignatureHelpParams,
) -> Option<SignatureHelp> {
    let position = &params.text_document_position_params;
    let module_id = module_for_uri(compilation, &position.text_document.uri)?;
    let module = &compilation.hir.modules[module_id];
    let source = compilation
        .package
        .module(&module.name)?
        .source
        .as_deref()?;
    let offset = position_to_offset(source, position.position)?;
    let (call, callee, arguments) = enclosing_call(compilation, module_id, offset)?;
    let callable = callable_presentation(compilation, callee)?;
    let parameters = callable
        .parameters
        .iter()
        .map(|parameter| ParameterInformation {
            label: ParameterLabel::Simple(parameter.clone()),
            documentation: None,
        })
        .collect::<Vec<_>>();
    let active = active_argument(compilation, call, arguments, offset)
        .min(parameters.len().saturating_sub(1));
    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label: callable.signature,
            documentation: callable.documentation.map(|documentation| {
                Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: documentation,
                })
            }),
            parameters: Some(parameters),
            active_parameter: Some(u32::try_from(active).unwrap_or(u32::MAX)),
        }],
        active_signature: Some(0),
        active_parameter: Some(u32::try_from(active).unwrap_or(u32::MAX)),
    })
}

fn enclosing_call(
    compilation: &Compilation,
    module: crate::hir::ModuleId,
    offset: usize,
) -> Option<(ExprId, ExprId, &[ExprId])> {
    compilation
        .hir
        .expressions
        .iter()
        .filter_map(|(id, expression)| {
            let Expr::Call { callee, arguments } = expression else {
                return None;
            };
            let span = compilation.hir.expression_spans.get(&id)?;
            (span.start <= offset
                && offset <= span.end
                && compilation
                    .hir
                    .expression_functions
                    .get(&id)
                    .is_some_and(|function| compilation.hir.functions[*function].module == module))
            .then_some((id, *callee, arguments.as_slice(), span.end - span.start))
        })
        .min_by_key(|(_, _, _, width)| *width)
        .map(|(id, callee, arguments, _)| (id, callee, arguments))
}

fn active_argument(
    compilation: &Compilation,
    _call: ExprId,
    arguments: &[ExprId],
    offset: usize,
) -> usize {
    arguments
        .iter()
        .take_while(|argument| {
            compilation
                .hir
                .expression_spans
                .get(argument)
                .is_some_and(|span| span.end < offset)
        })
        .count()
}
