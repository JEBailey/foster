use std::fmt::Write;

use crate::ast::{Effect, EffectKind, ParameterMode, TypeExpr};
use crate::hir::{Compilation, ConstantId, FunctionId, ModuleId};

pub(super) const STYLE: &str = r#":root { color-scheme: light dark; font-family: Inter, ui-sans-serif, system-ui, sans-serif; }
body { margin: 0; color: #20242b; background: #f6f7f9; }
header { padding: 2.5rem max(2rem, calc((100% - 72rem) / 2)); color: white; background: #172033; }
header h1 { margin: 0 0 .4rem; font-size: 2rem; }
header p { margin: 0; color: #cbd5e1; }
main { max-width: 72rem; margin: 0 auto; padding: 2rem; }
a { color: #3564c4; text-decoration: none; } a:hover { text-decoration: underline; }
.crumb { display: inline-block; margin-bottom: 1rem; }
.module-list { display: grid; grid-template-columns: repeat(auto-fill, minmax(16rem, 1fr)); gap: 1rem; padding: 0; list-style: none; }
.module-list a, article { display: block; padding: 1rem 1.15rem; border: 1px solid #d8dde7; border-radius: .65rem; background: white; }
article { margin: 1rem 0; }
article h2 { margin: 0 0 .65rem; font-size: 1.2rem; }
pre { overflow-x: auto; padding: .85rem 1rem; border-radius: .45rem; color: #e7edf7; background: #202838; }
code { font-family: "Cascadia Code", "SFMono-Regular", Consolas, monospace; }
p code, li code { padding: .08rem .3rem; border-radius: .25rem; background: #e8ebf1; }
.badge { margin-left: .5rem; padding: .15rem .4rem; border-radius: 99px; font-size: .7rem; font-weight: 600; color: #596273; background: #edf0f5; vertical-align: middle; }
.empty { color: #667085; font-style: italic; }
@media (prefers-color-scheme: dark) { body { color: #e5e7eb; background: #111827; } article, .module-list a { border-color: #344052; background: #1b2434; } p code, li code { background: #303b4d; } a { color: #8db4ff; } }
"#;

pub(super) struct Site {
    pub index: String,
    pub modules: Vec<ModulePage>,
    pub module_count: usize,
    pub declaration_count: usize,
}

pub(super) struct ModulePage {
    pub file_name: String,
    pub html: String,
}

pub(super) fn site(compilation: &Compilation) -> Site {
    let mut index_items = String::new();
    let mut pages = Vec::new();
    let mut declaration_count = 0;
    for (module_id, module) in compilation.hir.modules.iter() {
        let file_name = module_file_name(&module.name);
        let count = module
            .functions
            .values()
            .filter(|id| visible_function(compilation, **id))
            .count()
            + module.constants.len()
            + module.records.len()
            + module.variant_types.len();
        declaration_count += count;
        let _ = write!(
            index_items,
            "<li><a href=\"modules/{file_name}\"><strong>{}</strong><br><small>{count} declaration{}</small></a></li>",
            escape(&module.name),
            if count == 1 { "" } else { "s" }
        );
        pages.push(ModulePage {
            file_name,
            html: module_page(compilation, module_id),
        });
    }
    let module_count = pages.len();
    Site {
        index: page(
            "Foster documentation",
            "<h1>Foster documentation</h1><p>Resolved package API reference</p>",
            &format!("<h2>Modules</h2><ul class=\"module-list\">{index_items}</ul>"),
            "style.css",
        ),
        modules: pages,
        module_count,
        declaration_count,
    }
}

fn module_page(compilation: &Compilation, module_id: ModuleId) -> String {
    let module = &compilation.hir.modules[module_id];
    let mut body = String::from("<a class=\"crumb\" href=\"../index.html\">← All modules</a>");
    if let Some(documentation) = &module.documentation {
        body.push_str("<article class=\"module-documentation\">");
        body.push_str(&markdown(documentation));
        body.push_str("</article>");
    }
    let mut count = 0;

    for constant_id in module.constants.values().copied() {
        count += 1;
        let constant = &compilation.hir.constants[constant_id];
        declaration(
            &mut body,
            &constant.name,
            constant.public,
            &constant_signature(compilation, constant_id),
            constant.documentation.as_deref(),
        );
    }
    for function_id in module.functions.values().copied() {
        if !visible_function(compilation, function_id) {
            continue;
        }
        count += 1;
        let function = &compilation.hir.functions[function_id];
        declaration(
            &mut body,
            &function.name,
            function.public,
            &function_signature(compilation, function_id),
            function.documentation.as_deref(),
        );
    }
    for record_id in module.records.values().copied() {
        count += 1;
        let record = &compilation.hir.records[record_id];
        declaration(
            &mut body,
            &record.name,
            record.public,
            &record_signature(record),
            record.documentation.as_deref(),
        );
    }
    for variant_id in module.variant_types.values().copied() {
        count += 1;
        let variant = &compilation.hir.variant_types[variant_id];
        declaration(
            &mut body,
            &variant.name,
            variant.public,
            &variant_signature(compilation, variant_id),
            variant.documentation.as_deref(),
        );
    }
    if count == 0 {
        body.push_str("<p class=\"empty\">This module has no declarations.</p>");
    }
    page(
        &format!("{} — Foster documentation", module.name),
        &format!(
            "<h1>module {}</h1><p>{count} declaration{}</p>",
            escape(&module.name),
            if count == 1 { "" } else { "s" }
        ),
        &body,
        "../style.css",
    )
}

fn visible_function(compilation: &Compilation, id: FunctionId) -> bool {
    !compilation.hir.functions[id].name.contains('$')
}

fn constant_signature(compilation: &Compilation, id: ConstantId) -> String {
    let constant = &compilation.hir.constants[id];
    let ty = compilation
        .types
        .constants
        .get(&id)
        .map(|ty| compilation.types.display(*ty))
        .unwrap_or_else(|| "_".into());
    format!(
        "{}const {}: {ty}",
        if constant.public { "pub " } else { "" },
        constant.name
    )
}

fn declaration(body: &mut String, name: &str, public: bool, signature: &str, docs: Option<&str>) {
    let _ = write!(
        body,
        "<article id=\"{}\"><h2>{}<span class=\"badge\">{}</span></h2><pre><code>{}</code></pre>",
        escape(name),
        escape(name),
        if public { "public" } else { "private" },
        escape(signature)
    );
    if let Some(docs) = docs {
        body.push_str(&markdown(docs));
    } else {
        body.push_str("<p class=\"empty\">No documentation provided.</p>");
    }
    body.push_str("</article>");
}

fn function_signature(compilation: &Compilation, id: FunctionId) -> String {
    let function = &compilation.hir.functions[id];
    let signature = compilation.types.function_type(id);
    let generics = angled(&function.type_parameters);
    let group_entries = function
        .groups
        .iter()
        .map(|group| format!("{}: group {}", group.name, type_expr(&group.element)))
        .collect::<Vec<_>>();
    let groups = squared(&group_entries);
    let parameters = function
        .parameters
        .iter()
        .enumerate()
        .map(|(index, local)| {
            let name = &compilation.hir.locals[*local].name;
            let ty = signature
                .and_then(|sig| sig.parameters.get(index))
                .map(|ty| compilation.types.display(*ty))
                .unwrap_or_else(|| "_".into());
            let consume = signature
                .and_then(|sig| sig.parameter_modes.get(index))
                .is_some_and(|mode| *mode == ParameterMode::Consume);
            format!("{name}: {}{ty}", if consume { "consume " } else { "" })
        })
        .collect::<Vec<_>>()
        .join(", ");
    let result = signature
        .map(|sig| compilation.types.display(sig.result))
        .unwrap_or_else(|| "Unit".into());
    let effects = signature.map_or_else(String::new, |sig| effects(&sig.effects, sig.suspends));
    format!(
        "{}func {}{generics}{groups}({parameters}) -> {result}{effects}",
        if function.public { "pub " } else { "" },
        function.name
    )
}

fn record_signature(record: &crate::hir::Record) -> String {
    let compositions = if record.compositions.is_empty() {
        String::new()
    } else {
        format!(
            " & {}",
            record
                .compositions
                .iter()
                .map(type_expr)
                .collect::<Vec<_>>()
                .join(" & ")
        )
    };
    let mut members = record
        .fields
        .iter()
        .map(|field| {
            format!(
                "    {}{}: {}",
                if field.public { "pub " } else { "" },
                field.name,
                type_expr(&field.ty)
            )
        })
        .collect::<Vec<_>>();
    members.extend(record.methods.iter().map(|method| {
        let parameters = method
            .parameters
            .iter()
            .map(|parameter| match &parameter.ty {
                Some(ty) => format!("{}: {}", parameter.name, type_expr(ty)),
                None => parameter.name.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let result = method
            .return_type
            .as_ref()
            .map(type_expr)
            .unwrap_or_else(|| "Unit".into());
        format!(
            "    {}func {}{}({parameters}) -> {result}{}",
            if method.public { "pub " } else { "" },
            method.name,
            angled(&method.type_parameters),
            effects(&method.effects, method.suspends)
        )
    }));
    let members = members.join("\n");
    format!(
        "{}type {}{}{compositions} {{\n{members}\n}}",
        if record.public { "pub " } else { "" },
        record.name,
        angled(&record.parameters)
    )
}

fn variant_signature(compilation: &Compilation, id: crate::hir::VariantTypeId) -> String {
    let variant = &compilation.hir.variant_types[id];
    let alternatives = variant
        .alternatives
        .iter()
        .map(|id| {
            let alternative = &compilation.hir.variants[*id];
            if alternative.payload.is_empty() {
                alternative.name.clone()
            } else {
                format!(
                    "{}({})",
                    alternative.name,
                    alternative
                        .payload
                        .iter()
                        .map(type_expr)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        })
        .collect::<Vec<_>>()
        .join(" | ");
    format!(
        "{}type {}{} = {alternatives}",
        if variant.public { "pub " } else { "" },
        variant.name,
        angled(&variant.parameters)
    )
}

fn type_expr(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Named(name, arguments) => format!(
            "{name}{}",
            angled(&arguments.iter().map(type_expr).collect::<Vec<_>>())
        ),
        TypeExpr::Intersection(members) => members
            .iter()
            .map(type_expr)
            .collect::<Vec<_>>()
            .join(" & "),
        TypeExpr::Reference { group, value } => format!("ref[{group}] {}", type_expr(value)),
        TypeExpr::Function {
            parameters,
            parameter_modes,
            result,
            effects: declared,
            suspends,
        } => {
            let parameters = parameters
                .iter()
                .zip(parameter_modes)
                .map(|(ty, mode)| {
                    format!(
                        "{}{}",
                        if *mode == ParameterMode::Consume {
                            "consume "
                        } else {
                            ""
                        },
                        type_expr(ty)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "func({parameters}) -> {}{}",
                type_expr(result),
                effects(declared, *suspends)
            )
        }
    }
}

fn effects(declared: &[Effect], suspends: bool) -> String {
    let mut entries = declared
        .iter()
        .map(|effect| {
            format!(
                "{} {}",
                match effect.kind {
                    EffectKind::Read => "read",
                    EffectKind::Mut => "mut",
                    EffectKind::Reshape => "reshape",
                    EffectKind::Consume => "consume",
                },
                effect.target
            )
        })
        .collect::<Vec<_>>();
    if suspends {
        entries.push("suspend".into());
    }
    if entries.is_empty() {
        String::new()
    } else {
        format!(" [{}]", entries.join(", "))
    }
}

fn angled(values: &[String]) -> String {
    if values.is_empty() {
        String::new()
    } else {
        format!("<{}>", values.join(", "))
    }
}

fn squared(values: &[String]) -> String {
    if values.is_empty() {
        String::new()
    } else {
        format!("[{}]", values.join(", "))
    }
}

fn page(title: &str, heading: &str, body: &str, stylesheet: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><link rel=\"stylesheet\" href=\"{stylesheet}\"></head><body><header>{heading}</header><main>{body}</main></body></html>",
        escape(title)
    )
}

fn module_file_name(name: &str) -> String {
    format!("{}.html", name.replace('.', "-"))
}

fn markdown(source: &str) -> String {
    let mut output = String::new();
    let options = pulldown_cmark::Options::ENABLE_TABLES
        | pulldown_cmark::Options::ENABLE_STRIKETHROUGH
        | pulldown_cmark::Options::ENABLE_TASKLISTS;
    pulldown_cmark::html::push_html(
        &mut output,
        pulldown_cmark::Parser::new_ext(source, options),
    );
    output
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documentation_is_escaped_and_inline_code_is_rendered() {
        assert_eq!(
            markdown("Use `<value>` safely."),
            "<p>Use <code>&lt;value&gt;</code> safely.</p>\n"
        );
    }

    #[test]
    fn documentation_supports_common_markdown_structures() {
        let rendered = markdown("- first\n- second\n\n**important**");
        assert!(rendered.contains("<ul>"));
        assert!(rendered.contains("<strong>important</strong>"));
    }
}
