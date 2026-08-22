use std::fmt::Write;

use crate::ast::{Effect, EffectKind, ParameterMode, TypeExpr};
use crate::hir::{Compilation, ConstantId, FunctionId, ModuleId};

pub(super) const STYLE: &str = r#":root { color-scheme: light dark; font-family: Inter, ui-sans-serif, system-ui, sans-serif; line-height: 1.55; }
* { box-sizing: border-box; }
body { margin: 0; color: #20242b; background: #f6f7f9; }
header { padding: 3rem max(1.5rem, calc((100% - 72rem) / 2)); color: white; background: linear-gradient(135deg, #172033, #253b66); }
header h1 { margin: 0 0 .35rem; font-size: clamp(1.8rem, 4vw, 2.5rem); letter-spacing: -.025em; }
header p { margin: 0; color: #d7e0ef; }
main { max-width: 72rem; margin: 0 auto; padding: 2rem 1.5rem 4rem; }
a { color: #3564c4; text-decoration: none; } a:hover { text-decoration: underline; }
.crumb { display: inline-flex; align-items: center; margin-bottom: 1rem; font-weight: 600; }
.summary { display: flex; gap: .75rem; flex-wrap: wrap; margin: 0 0 1.5rem; color: #596273; }
.summary span { padding: .3rem .7rem; border: 1px solid #d8dde7; border-radius: 99px; background: white; }
.filter { width: 100%; margin: 0 0 1.25rem; padding: .8rem 1rem; border: 1px solid #c9d0dc; border-radius: .6rem; color: inherit; background: white; font: inherit; }
.filter:focus { border-color: #3564c4; outline: 3px solid color-mix(in srgb, #3564c4 20%, transparent); }
.module-list { display: grid; grid-template-columns: repeat(auto-fill, minmax(16rem, 1fr)); gap: 1rem; padding: 0; list-style: none; }
.module-list li[hidden] { display: none; }
.module-list a, article, .on-this-page { display: block; padding: 1rem 1.15rem; border: 1px solid #d8dde7; border-radius: .65rem; background: white; }
.module-list a { height: 100%; transition: border-color .15s, transform .15s, box-shadow .15s; }
.module-list a:hover { border-color: #9bb2df; box-shadow: 0 .35rem 1rem #17203312; transform: translateY(-2px); text-decoration: none; }
.module-list small { color: #667085; }
.on-this-page { margin: 0 0 1.5rem; }
.on-this-page strong { display: block; margin-bottom: .45rem; }
.on-this-page ul { display: flex; flex-wrap: wrap; gap: .35rem 1.2rem; margin: 0; padding-left: 1.2rem; }
.type-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(20rem, 1fr)); gap: 1rem; margin-bottom: 2rem; }
.type-summary { margin: 0; }
.type-summary h3 { margin: 0 0 .35rem; font-size: 1.15rem; }
.type-summary h4 { margin: 1rem 0 .3rem; font-size: .82rem; color: #667085; letter-spacing: .06em; text-transform: uppercase; }
.type-summary ul { margin: .25rem 0 0; padding-left: 1.2rem; }
.type-summary li + li { margin-top: .35rem; }
.type-summary small { color: #667085; }
article { margin: 1rem 0; scroll-margin-top: 1rem; }
article h2 { margin: 0 0 .65rem; font-size: 1.2rem; }
.anchor { color: inherit; } .anchor:hover { text-decoration: none; } .anchor:hover::after { content: " #"; color: #8090aa; }
pre { overflow-x: auto; padding: .85rem 1rem; border-radius: .45rem; color: #e7edf7; background: #202838; }
code { font-family: "Cascadia Code", "SFMono-Regular", Consolas, monospace; }
p code, li code { padding: .08rem .3rem; border-radius: .25rem; background: #e8ebf1; }
.badge { margin-left: .45rem; padding: .15rem .4rem; border-radius: 99px; font-size: .68rem; font-weight: 700; letter-spacing: .02em; color: #596273; background: #edf0f5; vertical-align: middle; }
.kind { color: #2356a8; background: #e8f0ff; }
.empty, .no-results { color: #667085; font-style: italic; }
.no-results { display: none; }
@media (max-width: 36rem) { header { padding-block: 2rem; } main { padding-top: 1.25rem; } .on-this-page ul { display: block; } }
@media (prefers-color-scheme: dark) { body { color: #e5e7eb; background: #111827; } article, .module-list a, .on-this-page, .summary span, .filter { border-color: #344052; background: #1b2434; } p code, li code { background: #303b4d; } a { color: #8db4ff; } .module-list small, .summary, .type-summary small, .type-summary h4 { color: #aab5c5; } .kind { color: #b9d2ff; background: #263c60; } }
"#;

const SCRIPT: &str = r#"<script>
const filter = document.querySelector('[data-module-filter]');
if (filter) {
  const modules = [...document.querySelectorAll('[data-module]')];
  const empty = document.querySelector('[data-no-results]');
  filter.addEventListener('input', () => {
    const query = filter.value.trim().toLowerCase();
    let visible = 0;
    for (const module of modules) {
      module.hidden = !module.dataset.module.includes(query);
      if (!module.hidden) visible++;
    }
    empty.style.display = visible ? 'none' : 'block';
  });
}
</script>"#;

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
            "<li data-module=\"{}\"><a href=\"modules/{file_name}\"><strong>{}</strong><br><small>{count} declaration{}</small></a></li>",
            escape(&module.name.to_lowercase()),
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
            &format!(
                "<div class=\"summary\"><span>{module_count} modules</span><span>{declaration_count} declarations</span></div><h2>Modules</h2><input class=\"filter\" type=\"search\" placeholder=\"Filter modules…\" aria-label=\"Filter modules\" data-module-filter><ul class=\"module-list\">{index_items}</ul><p class=\"no-results\" data-no-results>No modules match your filter.</p>{SCRIPT}"
            ),
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
    body.push_str(&provided_types(compilation, module_id));
    let mut count = 0;
    let mut contents = String::new();

    for constant_id in module.constants.values().copied() {
        count += 1;
        let constant = &compilation.hir.constants[constant_id];
        contents_entry(&mut contents, &constant.name, "constant");
        declaration(
            &mut body,
            &constant.name,
            constant.public,
            &constant_signature(compilation, constant_id),
            constant.documentation.as_deref(),
            "constant",
        );
    }
    for function_id in module.functions.values().copied() {
        if !visible_function(compilation, function_id) {
            continue;
        }
        count += 1;
        let function = &compilation.hir.functions[function_id];
        contents_entry(&mut contents, &function.name, "function");
        declaration(
            &mut body,
            &function.name,
            function.public,
            &function_signature(compilation, function_id),
            function.documentation.as_deref(),
            "function",
        );
    }
    for record_id in module.records.values().copied() {
        count += 1;
        let record = &compilation.hir.records[record_id];
        contents_entry(&mut contents, &record.name, "type");
        declaration(
            &mut body,
            &record.name,
            record.public,
            &record_signature(record),
            record.documentation.as_deref(),
            "type",
        );
    }
    for variant_id in module.variant_types.values().copied() {
        count += 1;
        let variant = &compilation.hir.variant_types[variant_id];
        contents_entry(&mut contents, &variant.name, "variant");
        declaration(
            &mut body,
            &variant.name,
            variant.public,
            &variant_signature(compilation, variant_id),
            variant.documentation.as_deref(),
            "variant",
        );
    }
    if count == 0 {
        body.push_str("<p class=\"empty\">This module has no declarations.</p>");
    } else {
        body.insert_str(
            body.find("<article id=").unwrap_or(body.len()),
            &format!("<nav class=\"on-this-page\" aria-label=\"On this page\"><strong>On this page</strong><ul>{contents}</ul></nav>"),
        );
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

fn provided_types(compilation: &Compilation, module_id: ModuleId) -> String {
    let module = &compilation.hir.modules[module_id];
    let mut cards = String::new();
    for record_id in module.records.values().copied() {
        let record = &compilation.hir.records[record_id];
        if !record.public {
            continue;
        }
        type_card(
            &mut cards,
            compilation,
            module_id,
            &record.name,
            record.public,
            record.documentation.as_deref(),
            record
                .fields
                .iter()
                .map(|field| format!("{}: {}", field.name, type_expr(&field.ty)))
                .collect(),
            Vec::new(),
            record
                .methods
                .iter()
                .map(|method| (method.name.as_str(), method.documentation.as_deref()))
                .collect(),
        );
    }
    for variant_id in module.variant_types.values().copied() {
        let variant = &compilation.hir.variant_types[variant_id];
        if !variant.public {
            continue;
        }
        type_card(
            &mut cards,
            compilation,
            module_id,
            &variant.name,
            variant.public,
            variant.documentation.as_deref(),
            Vec::new(),
            variant
                .alternatives
                .iter()
                .map(|id| compilation.hir.variants[*id].name.clone())
                .collect(),
            variant
                .methods
                .iter()
                .map(|method| (method.name.as_str(), method.documentation.as_deref()))
                .collect(),
        );
    }
    if cards.is_empty() {
        String::new()
    } else {
        format!(
            "<section aria-labelledby=\"provided-types\"><h2 id=\"provided-types\">Provided types</h2><div class=\"type-grid\">{cards}</div></section>"
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn type_card(
    cards: &mut String,
    compilation: &Compilation,
    module_id: ModuleId,
    name: &str,
    public: bool,
    docs: Option<&str>,
    fields: Vec<String>,
    variants: Vec<String>,
    requirements: Vec<(&str, Option<&str>)>,
) {
    let _ = write!(
        cards,
        "<article class=\"type-summary\"><h3><a href=\"#{0}\">{0}</a><span class=\"badge\">{1}</span></h3>",
        escape(name),
        if public { "public" } else { "private" }
    );
    if let Some(docs) = docs {
        cards.push_str(&markdown(docs));
    }
    type_members(
        cards,
        "Fields",
        fields.iter().map(|value| (value.as_str(), None)),
    );
    type_members(
        cards,
        "Variants",
        variants.iter().map(|value| (value.as_str(), None)),
    );
    type_members(cards, "Required methods", requirements.into_iter());

    let functions = compilation.hir.modules[module_id]
        .functions
        .values()
        .copied()
        .filter(|id| {
            compilation.hir.functions[*id].public
                && function_owner(compilation, *id).as_deref() == Some(name)
        })
        .collect::<Vec<_>>();
    if !functions.is_empty() {
        cards.push_str("<h4>Functions and methods</h4><ul>");
        for id in functions {
            let function = &compilation.hir.functions[id];
            let label = function.name.rsplit('.').next().unwrap_or(&function.name);
            let summary = function
                .documentation
                .as_deref()
                .and_then(|docs| docs.lines().find(|line| !line.trim().is_empty()));
            let _ = write!(
                cards,
                "<li><a href=\"#{}\"><code>{}</code></a>{}</li>",
                escape(&function.name),
                escape(label),
                summary.map_or_else(String::new, |text| format!(
                    "<br><small>{}</small>",
                    escape(text)
                ))
            );
        }
        cards.push_str("</ul>");
    }
    cards.push_str("</article>");
}

fn type_members<'a>(
    cards: &mut String,
    heading: &str,
    members: impl Iterator<Item = (&'a str, Option<&'a str>)>,
) {
    let members = members.collect::<Vec<_>>();
    if members.is_empty() {
        return;
    }
    let _ = write!(cards, "<h4>{heading}</h4><ul>");
    for (name, docs) in members {
        let _ = write!(
            cards,
            "<li><code>{}</code>{}</li>",
            escape(name),
            docs.map_or_else(String::new, |text| format!(
                "<br><small>{}</small>",
                escape(text)
            ))
        );
    }
    cards.push_str("</ul>");
}

fn function_owner(compilation: &Compilation, id: FunctionId) -> Option<String> {
    let function = &compilation.hir.functions[id];
    if let Some((owner, _)) = function.name.split_once('.') {
        return Some(owner.to_owned());
    }
    let first = *function.parameters.first()?;
    if compilation.hir.locals[first].name != "self" {
        return None;
    }
    compilation
        .types
        .function_type(id)
        .and_then(|signature| signature.parameters.first())
        .map(|ty| compilation.types.display(*ty))
        .map(|ty| ty.split('<').next().unwrap_or(&ty).to_owned())
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

fn contents_entry(contents: &mut String, name: &str, kind: &str) {
    let _ = write!(
        contents,
        "<li><a href=\"#{}\">{} <small>{kind}</small></a></li>",
        escape(name),
        escape(name)
    );
}

fn declaration(
    body: &mut String,
    name: &str,
    public: bool,
    signature: &str,
    docs: Option<&str>,
    kind: &str,
) {
    let _ = write!(
        body,
        "<article id=\"{}\"><h2><a class=\"anchor\" href=\"#{}\">{}</a><span class=\"badge kind\">{kind}</span><span class=\"badge\">{}</span></h2><pre><code>{}</code></pre>",
        escape(name),
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
