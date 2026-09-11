use std::fmt::Write;

use super::type_links::TypeLinks;
use crate::ast::{Effect, EffectKind, ParameterMode, TypeExpr};
use crate::compiler::Compilation;
use crate::hir::{ConstantId, FunctionId, ModuleId};

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
.type-link { text-decoration: underline; text-underline-offset: .18em; }
pre .type-link { color: #a8c9ff; }
pre .type-link:hover { color: white; }
code { font-family: "Cascadia Code", "SFMono-Regular", Consolas, monospace; }
p code, li code { padding: .08rem .3rem; border-radius: .25rem; background: #e8ebf1; }
.badge { margin-left: .45rem; padding: .15rem .4rem; border-radius: 99px; font-size: .68rem; font-weight: 700; letter-spacing: .02em; color: #596273; background: #edf0f5; vertical-align: middle; }
.kind { color: #2356a8; background: #e8f0ff; }
.empty, .no-results { color: #667085; font-style: italic; }
[hidden] { display: none !important; }
.skip-link { position: absolute; left: 1rem; top: -5rem; padding: .75rem; background: white; z-index: 2; }
.skip-link:focus { top: 1rem; }
:focus-visible { outline: 3px solid #628ae0; outline-offset: 3px; }
header h1, article, .module-list strong { overflow-wrap: anywhere; }
.module-layout { display: grid; grid-template-columns: 17rem minmax(0, 1fr); gap: 2rem; align-items: start; }
.module-content { min-width: 0; }
.on-this-page { position: sticky; top: 1rem; max-height: calc(100vh - 2rem); overflow: auto; }
.on-this-page summary { cursor: pointer; font-weight: 700; }
.on-this-page .crumb { margin: 0 0 .75rem; }
.on-this-page label { display: block; margin-top: 1rem; font-size: .85rem; font-weight: 600; }
.on-this-page .filter { margin: .35rem 0 .5rem; padding: .55rem; }
.on-this-page ul { display: block; padding: 0; list-style: none; max-height: max(8rem, calc(100vh - 20rem)); overflow: auto; }
.on-this-page li a { display: block; padding: .4rem .25rem; overflow-wrap: anywhere; }
.on-this-page small { color: #596273; font-size: .75rem; margin-left: .35rem; }
.on-this-page a[aria-current="location"] { background: #e8f0ff; border-radius: .25rem; font-weight: 700; }
.filter-status { font-size: .85rem; color: #596273; }
.back-to-navigation { display: none; }
article:target { outline: 2px solid #628ae0; outline-offset: 3px; }
.type-grid { grid-template-columns: repeat(auto-fit, minmax(min(100%, 20rem), 1fr)); }
@media (max-width: 60rem) { .module-layout { display: block; } .on-this-page { position: static; max-height: none; } .on-this-page ul { max-height: 16rem; overflow: auto; } }
@media (max-width: 60rem) { .back-to-navigation { display: inline-block; margin-top: .75rem; padding-block: .35rem; font-size: .85rem; } }
@media (max-width: 36rem) { header { padding-block: 2rem; } main { padding-top: 1.25rem; } .on-this-page ul { display: block; } }
@media (prefers-color-scheme: dark) { body { color: #e5e7eb; background: #111827; } article, .module-list a, .on-this-page, .summary span, .filter { border-color: #344052; background: #1b2434; } p code, li code { background: #303b4d; } a { color: #8db4ff; } .module-list small, .summary, .type-summary small, .type-summary h4 { color: #aab5c5; } .kind { color: #b9d2ff; background: #263c60; } }
@media (prefers-color-scheme: dark) { .empty, .no-results, .filter-status, .on-this-page small { color: #aab5c5; } .on-this-page a[aria-current="location"] { background: #263c60; } .skip-link { background: #1b2434; } }
"#;

const SCRIPT: &str = r##"<script>
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
    empty.hidden = visible > 0;
  });
}
const declarationFilter = document.querySelector('[data-declaration-filter]');
if (declarationFilter) {
  const entries = [...document.querySelectorAll('[data-declaration]')];
  const status = document.querySelector('[data-declaration-status]');
  declarationFilter.addEventListener('input', () => {
    const query = declarationFilter.value.trim().toLowerCase();
    let visible = 0;
    for (const entry of entries) {
      entry.hidden = !entry.textContent.toLowerCase().includes(query);
      if (!entry.hidden) visible++;
    }
    status.textContent = visible ? `${visible} of ${entries.length} declarations` : 'No matching declarations. Try another name or kind.';
  });
  declarationFilter.addEventListener('keydown', event => {
    if (event.key === 'Escape') {
      declarationFilter.value = '';
      declarationFilter.dispatchEvent(new Event('input'));
    }
  });
}
const navigationLinks = [...document.querySelectorAll('.on-this-page a[href^="#"]')];
function updateCurrentLocation() {
  for (const link of navigationLinks) {
    if (link.hash === location.hash) link.setAttribute('aria-current', 'location');
    else link.removeAttribute('aria-current');
  }
}
window.addEventListener('hashchange', updateCurrentLocation);
updateCurrentLocation();
</script>"##;

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
        if !compilation.package.is_input_module(&module.name) {
            continue;
        }
        let file_name = module_file_name(&module.name);
        let count = module
            .functions
            .values()
            .filter_map(|overloads| overloads.first().copied())
            .filter(|id| visible_function(compilation, *id))
            .count()
            + module.constants.len()
            + module
                .records
                .values()
                .map(|id| &compilation.hir.records[*id])
                .filter(|record| visible_type(record.public, record.documentation.as_deref()))
                .count()
            + module
                .variant_types
                .values()
                .map(|id| &compilation.hir.variant_types[*id])
                .filter(|variant| visible_type(variant.public, variant.documentation.as_deref()))
                .count();
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
                "<div class=\"summary\"><span>{module_count} module{module_suffix}</span><span>{declaration_count} declaration{declaration_suffix}</span></div><h2>Modules</h2><label for=\"module-filter\">Filter modules by name</label><input id=\"module-filter\" class=\"filter\" type=\"search\" placeholder=\"e.g. collections or string\" aria-label=\"Filter modules\" data-module-filter><ul class=\"module-list\">{index_items}</ul><p class=\"no-results\" role=\"status\" data-no-results hidden>No modules match your filter.</p>",
                module_suffix = if module_count == 1 { "" } else { "s" },
                declaration_suffix = if declaration_count == 1 { "" } else { "s" },
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
    let mut body = String::from("<div id=\"module-overview\"><h2>Overview</h2>");
    if let Some(documentation) = &module.documentation {
        body.push_str("<article class=\"module-documentation\">");
        body.push_str(&markdown(documentation));
        body.push_str("</article>");
    }
    body.push_str(&provided_types(compilation, module_id));
    body.push_str("</div>");
    let mut count = 0;
    let mut contents = String::new();

    for record_id in module.records.values().copied() {
        let record = &compilation.hir.records[record_id];
        if !visible_type(record.public, record.documentation.as_deref()) {
            continue;
        }
        count += 1;
        contents_entry(&mut contents, &record.name, &record.name, "type");
        declaration(
            &mut body,
            &record.name,
            &record.name,
            record.public,
            &record_signature(compilation, record),
            record.documentation.as_deref(),
            "type",
        );
    }
    for variant_id in module.variant_types.values().copied() {
        let variant = &compilation.hir.variant_types[variant_id];
        if !visible_type(variant.public, variant.documentation.as_deref()) {
            continue;
        }
        count += 1;
        contents_entry(&mut contents, &variant.name, &variant.name, "variant");
        declaration(
            &mut body,
            &variant.name,
            &variant.name,
            variant.public,
            &variant_signature(compilation, variant_id),
            variant.documentation.as_deref(),
            "variant",
        );
    }
    for constant_id in module.constants.values().copied() {
        count += 1;
        let constant = &compilation.hir.constants[constant_id];
        contents_entry(&mut contents, &constant.name, &constant.name, "constant");
        declaration(
            &mut body,
            &constant.name,
            &constant.name,
            constant.public,
            &constant_signature(compilation, constant_id),
            constant.documentation.as_deref(),
            "constant",
        );
    }
    for function_id in module
        .functions
        .values()
        .filter_map(|overloads| overloads.first().copied())
    {
        if !visible_function(compilation, function_id) {
            continue;
        }
        count += 1;
        let function = &compilation.hir.functions[function_id];
        let source_name = source_function_name(function);
        contents_entry(&mut contents, &function.name, &source_name, "function");
        declaration(
            &mut body,
            &function.name,
            &source_name,
            function.public,
            &function_signature(compilation, function_id),
            function.documentation.as_deref(),
            "function",
        );
    }
    if count == 0 {
        body.push_str("<p class=\"empty\">This module has no declarations.</p>");
    }
    let body = format!(
        "<div class=\"module-layout\"><nav id=\"page-navigation\" class=\"on-this-page\" aria-label=\"On this page\"><a class=\"crumb\" href=\"../index.html\">← All modules</a><details open><summary>On this page</summary><a href=\"#module-overview\">Overview</a><label for=\"declaration-filter\">Find a declaration</label><input id=\"declaration-filter\" class=\"filter\" type=\"search\" placeholder=\"Name or kind…\" data-declaration-filter><p class=\"filter-status\" role=\"status\" data-declaration-status>{count} declarations</p><ul>{contents}</ul></details></nav><div class=\"module-content\">{body}</div></div>"
    );
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

pub(super) fn visible_type(public: bool, documentation: Option<&str>) -> bool {
    public || documentation.is_some_and(|docs| !docs.trim().is_empty())
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
                .map(|field| {
                    format!(
                        "{}: {}",
                        escape(&field.name),
                        TypeLinks::new(compilation, module_id, &record.parameters)
                            .source(&field.ty)
                    )
                })
                .collect(),
            Vec::new(),
            record
                .methods
                .iter()
                .map(|method| (method.name.as_str(), method.documentation.as_deref()))
                .collect(),
            "Cases",
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
                .map(|id| {
                    variant_alternative_signature(
                        &TypeLinks::new(compilation, module_id, &variant.parameters),
                        &compilation.hir.variants[*id],
                    )
                })
                .collect(),
            variant
                .methods
                .iter()
                .map(|method| (method.name.as_str(), method.documentation.as_deref()))
                .collect(),
            if variant.kind == crate::ast::VariantKind::Enum {
                "Cases"
            } else {
                "Union members"
            },
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
    alternatives_title: &str,
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
        fields.into_iter().map(|value| (value, None)),
    );
    type_members(
        cards,
        alternatives_title,
        variants.into_iter().map(|value| (value, None)),
    );
    type_members(
        cards,
        "Required methods",
        requirements
            .into_iter()
            .map(|(name, docs)| (escape(name), docs)),
    );

    let functions = compilation.hir.modules[module_id]
        .functions
        .values()
        .filter_map(|overloads| overloads.first().copied())
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
    members: impl Iterator<Item = (String, Option<&'a str>)>,
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
            name,
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
    let links = TypeLinks::new(compilation, constant.module, &[]);
    let ty = compilation
        .types
        .constants
        .get(&id)
        .map(|ty| links.resolved(*ty))
        .unwrap_or_else(|| "_".into());
    format!(
        "{}const {}: {ty}",
        if constant.public { "pub " } else { "" },
        constant.name
    )
}

fn contents_entry(contents: &mut String, anchor: &str, name: &str, kind: &str) {
    let _ = write!(
        contents,
        "<li data-declaration><a href=\"#{}\">{} <small>{kind}</small></a></li>",
        escape(anchor),
        escape(name)
    );
}

fn declaration(
    body: &mut String,
    anchor: &str,
    name: &str,
    public: bool,
    signature: &str,
    docs: Option<&str>,
    kind: &str,
) {
    let _ = write!(
        body,
        "<article id=\"{}\"><h2><a class=\"anchor\" href=\"#{}\">{}</a><span class=\"badge kind\">{kind}</span><span class=\"badge\">{}</span></h2><pre><code>{}</code></pre>",
        escape(anchor),
        escape(anchor),
        escape(name),
        if public { "public" } else { "private" },
        signature
    );
    if let Some(docs) = docs {
        body.push_str(&markdown(docs));
    } else {
        body.push_str("<p class=\"empty\">No documentation provided.</p>");
    }
    body.push_str(
        "<a class=\"back-to-navigation\" href=\"#page-navigation\">↑ On this page</a></article>",
    );
}

fn function_signature(compilation: &Compilation, id: FunctionId) -> String {
    let function = &compilation.hir.functions[id];
    let links = TypeLinks::new(compilation, function.module, &function.type_parameters);
    let signature = compilation.types.function_type(id);
    let generics = escape(&angled(&function.type_parameters));
    let group_entries = function
        .groups
        .iter()
        .map(|group| {
            format!(
                "{}: group {}",
                escape(&group.name),
                links.source(&group.element)
            )
        })
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
                .map(|ty| links.resolved(*ty))
                .unwrap_or_else(|| "_".into());
            let consume = signature
                .and_then(|sig| sig.parameter_modes.get(index))
                .is_some_and(|mode| *mode == ParameterMode::Consume);
            format!("{name}: {}{ty}", if consume { "consume " } else { "" })
        })
        .collect::<Vec<_>>()
        .join(", ");
    let result = signature
        .map(|sig| links.resolved(sig.result))
        .unwrap_or_else(|| "()".into());
    let effects = signature.map_or_else(String::new, |sig| {
        escape(&effects(&sig.effects, sig.suspends))
    });
    let name = function.name.rsplit('.').next().unwrap_or(&function.name);
    let signature = format!(
        "{}func {name}{generics}{groups}({parameters}) -&gt; {result}{effects}",
        if function.public { "pub " } else { "" },
    );
    match &function.owner {
        Some(owner) => format!(
            "impl {} {{\n    {signature}\n}}",
            links.source(&TypeExpr::Named(owner.clone(), vec![]))
        ),
        None => signature,
    }
}

fn source_function_name(function: &crate::hir::Function) -> String {
    function.name.clone()
}

fn record_signature(compilation: &Compilation, record: &crate::hir::Record) -> String {
    let links = TypeLinks::new(compilation, record.module, &record.parameters);
    let compositions = if record.compositions.is_empty() {
        String::new()
    } else {
        format!(
            " &amp; {}",
            record
                .compositions
                .iter()
                .map(|ty| links.source(ty))
                .collect::<Vec<_>>()
                .join(" &amp; ")
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
                links.source(&field.ty)
            )
        })
        .collect::<Vec<_>>();
    members.extend(record.methods.iter().map(|method| {
        let links = links.scoped(&method.type_parameters);
        let parameters = method
            .parameters
            .iter()
            .map(|parameter| match &parameter.ty {
                Some(ty) => format!("{}: {}", parameter.name, links.source(ty)),
                None => parameter.name.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let result = method
            .return_type
            .as_ref()
            .map(|ty| links.source(ty))
            .unwrap_or_else(|| "()".into());
        format!(
            "    {}func {}{}({parameters}) -&gt; {result}{}",
            if method.public { "pub " } else { "" },
            method.name,
            escape(&angled(&method.type_parameters)),
            escape(&effects(&method.effects, method.suspends))
        )
    }));
    let members = members.join("\n");
    format!(
        "{}type {}{}{compositions} {{\n{members}\n}}",
        if record.public { "pub " } else { "" },
        record.name,
        escape(&angled(&record.parameters))
    )
}

fn variant_signature(compilation: &Compilation, id: crate::hir::VariantTypeId) -> String {
    let variant = &compilation.hir.variant_types[id];
    let links = TypeLinks::new(compilation, variant.module, &variant.parameters);
    let alternatives = variant
        .alternatives
        .iter()
        .map(|id| variant_alternative_signature(&links, &compilation.hir.variants[*id]))
        .collect::<Vec<_>>();
    let alternatives = if variant.kind == crate::ast::VariantKind::Enum {
        alternatives.join("\n    | ")
    } else {
        alternatives.join(" | ")
    };
    format!(
        "{}{} {}{} = {alternatives}",
        if variant.public { "pub " } else { "" },
        if variant.kind == crate::ast::VariantKind::Enum {
            "enum"
        } else {
            "type"
        },
        variant.name,
        escape(&angled(&variant.parameters))
    )
}

fn variant_alternative_signature(
    links: &TypeLinks<'_>,
    alternative: &crate::hir::Variant,
) -> String {
    if let Some(member) = &alternative.member {
        links.source(member)
    } else if alternative.payload.is_none() {
        alternative.name.clone()
    } else {
        format!(
            "{}({})",
            alternative.name,
            alternative
                .payload
                .iter()
                .map(|ty| links.source(ty))
                .next()
                .expect("a payload-bearing enum case has a payload type")
        )
    }
}

pub(super) fn effects(declared: &[Effect], suspends: bool) -> String {
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
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><link rel=\"stylesheet\" href=\"{stylesheet}\"></head><body><a class=\"skip-link\" href=\"#main-content\">Skip to content</a><header>{heading}</header><main id=\"main-content\" tabindex=\"-1\">{body}</main>{SCRIPT}</body></html>",
        escape(title)
    )
}

pub(super) fn module_file_name(name: &str) -> String {
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

pub(super) fn escape(value: &str) -> String {
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
    fn type_links_resolve_imports_nested_types_and_generic_shadowing() {
        let root = std::env::temp_dir().join(format!(
            "foster-type-links-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("first.fos"),
            "pub type Item = { pub value: Int }\npub type Box<T> = { pub value: T }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("second.fos"),
            "pub type Item = { pub value: Int }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("main.fos"),
            r#"
import first as one
import second as two
pub type Holder = {
    pub first: one::Box<one::Item>
    pub second: two::Item
    pub func convert<Item>(self, value: Item) -> Item [read self]
}
pub enum Outcome = Some(one::Item) | None
type Hidden = { value: Int }
func hidden(value: Hidden) -> Int { 0 }
pub func compare(Item: one::Item, other: two::Item) -> Int { 0 }
pub func generic<Item>(value: Item) -> Item { value }
func main() -> Int { 0 }
"#,
        )
        .unwrap();
        let compilation = crate::check_package(&root).unwrap();
        let site = site(&compilation);
        let html = &site
            .modules
            .iter()
            .find(|page| page.file_name == "main.html")
            .unwrap()
            .html;
        assert!(html.contains("href=\"first.html#Box\""));
        assert!(html.contains("href=\"first.html#Item\""));
        assert!(html.contains("href=\"second.html#Item\""));
        assert!(html.contains("Item: <a class=\"type-link\" href=\"first.html#Item\""));
        let generic = function_signature(
            &compilation,
            compilation
                .hir
                .function_named(compilation.hir.module_named("main").unwrap(), "generic")
                .unwrap(),
        );
        assert!(
            generic.contains("func generic&lt;Item&gt;(value: consume Item) -&gt; Item"),
            "{generic}"
        );
        assert!(html.contains("func convert&lt;Item&gt;(self, value: Item) -&gt; Item"));
        assert!(!html.contains("href=\"#Hidden\""));
        assert!(!html.contains("&lt;a class="));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn library_type_links_only_target_generated_pages_and_anchors() {
        let compilation = crate::check_package("library").unwrap();
        let site = site(&compilation);
        let string = &site
            .modules
            .iter()
            .find(|page| page.file_name == "core-string.html")
            .unwrap()
            .html;
        assert!(string.contains("href=\"core-option.html#Option\""));
        assert!(string.contains("href=\"core-code_point.html#module-overview\""));
        assert!(string.contains("href=\"std-iter.html#Iterator\""));
        for page in &site.modules {
            for link in page.html.split("class=\"type-link\" href=\"").skip(1) {
                let href = link.split('"').next().unwrap();
                let (file, anchor) = href.split_once('#').unwrap();
                let target = if file.is_empty() {
                    page
                } else {
                    site.modules
                        .iter()
                        .find(|page| page.file_name == file)
                        .expect("linked module must be generated")
                };
                assert!(
                    target.html.contains(&format!("id=\"{anchor}\"")),
                    "missing target: {href}"
                );
            }
        }
    }

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

    #[test]
    fn site_omits_undocumented_private_types() {
        let compilation = crate::compile(
            r#"
type HiddenRecord = { value: Int }
enum HiddenEnum = Value
type HiddenAlias = Int
///
type BlankRecord = { value: Int }
/**   */
enum BlankEnum = Value
/// A documented private record.
type DocumentedRecord = { value: Int }
/// A documented private enum.
enum DocumentedEnum = Value
/// A documented private alias.
type DocumentedAlias = Int
pub type PublicRecord = { value: Int }
pub enum PublicEnum = Value
pub type PublicAlias = Int
func main() -> Int { 0 }
"#,
        )
        .unwrap();
        let site = site(&compilation);
        let html = &site.modules[0].html;

        for name in [
            "HiddenRecord",
            "HiddenEnum",
            "HiddenAlias",
            "BlankRecord",
            "BlankEnum",
        ] {
            assert!(!html.contains(name), "unexpected private type: {name}");
        }
        for name in [
            "DocumentedRecord",
            "DocumentedEnum",
            "DocumentedAlias",
            "PublicRecord",
            "PublicEnum",
            "PublicAlias",
        ] {
            assert!(
                html.contains(&format!("<article id=\"{name}\"")),
                "missing type: {name}"
            );
            assert!(
                html.contains(&format!("href=\"#{name}\"")),
                "missing navigation: {name}"
            );
        }
        assert_eq!(site.declaration_count, 7);
        assert!(site.index.contains("<span>7 declarations</span>"));
        assert!(site.index.contains("<small>7 declarations</small>"));
        assert!(html.contains("<p>7 declarations</p>"));
    }

    #[test]
    fn site_omits_embedded_modules() {
        let compilation = crate::compile(
            "import core.option\n\nfunc main() -> Option<Int> { Option.Some(42) }\n",
        )
        .unwrap();
        let site = site(&compilation);

        assert_eq!(site.module_count, 1);
        assert!(site.index.contains("data-module=\"main\""));
        assert!(!site.index.contains("data-module=\"core"));
        assert_eq!(site.modules.len(), 1);
        assert_eq!(site.modules[0].file_name, "main.html");
        assert!(!site.modules[0].html.contains("class=\"type-link\""));
    }
}
