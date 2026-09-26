//! Resolved compiler-data adapter for the Foster documentation renderer.
use super::type_links::TypeLinks;
use crate::ast::{Effect, EffectKind, ParameterMode};
use crate::compiler::Compilation;
use crate::hir::{ConstantId, FunctionId, ModuleId};
use crate::{error::FosterError, tooling, vm::Value};

const STYLE: &str = include_str!("../../tools/driver/documentation/style.css");
const SCRIPT: &str = include_str!("../../tools/driver/documentation/script.html");
static TOOL: tooling::Tool = tooling::Tool::new(include_bytes!(concat!(
    env!("OUT_DIR"),
    "/documentation.fbc"
)));

pub(super) struct Site {
    #[cfg(test)]
    pub index: String,
    #[cfg(test)]
    pub modules: Vec<ModulePage>,
    pub module_count: usize,
    pub declaration_count: usize,
}

#[cfg(test)]
pub(super) struct ModulePage {
    pub file_name: String,
    pub html: String,
}

// Count-prefixed string stream, consumed by Reader in documentation.fos.
// Optional values use a presence flag so missing and empty documentation differ.
#[derive(Default)]
struct Data(Vec<String>);
impl Data {
    fn text(&mut self, value: impl Into<String>) {
        self.0.push(value.into());
    }
    fn number(&mut self, value: usize) {
        self.text(value.to_string());
    }
    fn flag(&mut self, value: bool) {
        self.text(if value { "1" } else { "0" });
    }
    fn optional(&mut self, value: Option<&str>) {
        self.flag(value.is_some());
        if let Some(value) = value {
            self.text(value);
        }
    }
    fn docs(&mut self, value: Option<&str>) {
        self.optional(value.map(markdown).as_deref());
    }
    fn group(&mut self, name: &str, kind: &str, owner: &str, count: usize) {
        self.text(name);
        self.text(name);
        self.text(kind);
        self.text(owner);
        self.number(count);
    }
    fn overload(&mut self, public: bool, signature: String, docs: Option<&str>) {
        self.flag(public);
        self.text(signature);
        self.docs(docs);
    }
}

#[cfg(test)]
pub(super) fn site(compilation: &Compilation) -> Result<Site, FosterError> {
    render(compilation, "render", "")
}

pub(super) fn write(
    compilation: &Compilation,
    output: &std::path::Path,
) -> Result<Site, FosterError> {
    let output = output
        .to_str()
        .ok_or_else(|| FosterError::runtime("documentation output path must be valid UTF-8"))?;
    render(compilation, "write", output)
}

fn render(compilation: &Compilation, operation: &str, output: &str) -> Result<Site, FosterError> {
    let mut data = Data::default();
    for text in [operation, output, STYLE, SCRIPT] {
        data.text(text);
    }
    let mut modules = compilation
        .hir
        .modules
        .iter()
        .filter(|(_, module)| compilation.package.is_input_module(&module.name))
        .collect::<Vec<_>>();
    modules.sort_by(|a, b| a.1.name.cmp(&b.1.name));
    data.number(modules.len());
    for (id, module) in modules {
        data.text(&module.name);
        data.docs(module.documentation.as_deref());
        provided_types(&mut data, compilation, id);
        let records = module
            .records
            .values()
            .copied()
            .filter(|id| {
                let ty = &compilation.hir.records[*id];
                visible_type(ty.public, ty.documentation.as_deref())
            })
            .collect::<Vec<_>>();
        let variants = module
            .variant_types
            .values()
            .copied()
            .filter(|id| {
                let ty = &compilation.hir.variant_types[*id];
                visible_type(ty.public, ty.documentation.as_deref())
            })
            .collect::<Vec<_>>();
        let functions = module
            .functions
            .values()
            .map(|overloads| {
                overloads
                    .iter()
                    .copied()
                    .filter(|id| visible_function(compilation, *id))
                    .collect::<Vec<_>>()
            })
            .filter(|overloads| !overloads.is_empty())
            .collect::<Vec<_>>();
        data.number(records.len() + variants.len() + module.constants.len() + functions.len());
        for id in records {
            let ty = &compilation.hir.records[id];
            data.group(&ty.name, "type", "", 1);
            data.overload(
                ty.public,
                record_signature(compilation, ty),
                ty.documentation.as_deref(),
            );
        }
        for id in variants {
            let ty = &compilation.hir.variant_types[id];
            data.group(&ty.name, "variant", "", 1);
            data.overload(
                ty.public,
                variant_signature(compilation, id),
                ty.documentation.as_deref(),
            );
        }
        for id in module.constants.values().copied() {
            let constant = &compilation.hir.constants[id];
            data.group(&constant.name, "constant", "", 1);
            data.overload(
                constant.public,
                constant_signature(compilation, id),
                constant.documentation.as_deref(),
            );
        }
        for overloads in functions {
            let first = &compilation.hir.functions[overloads[0]];
            let owner = function_owner(compilation, overloads[0]).filter(|owner| {
                let hidden_record = module.records.get(owner).is_some_and(|id| {
                    let ty = &compilation.hir.records[*id];
                    !visible_type(ty.public, ty.documentation.as_deref())
                });
                let hidden_variant = module.variant_types.get(owner).is_some_and(|id| {
                    let ty = &compilation.hir.variant_types[*id];
                    !visible_type(ty.public, ty.documentation.as_deref())
                });
                !hidden_record && !hidden_variant
            });
            data.group(
                &first.name,
                "function",
                owner.as_deref().unwrap_or(""),
                overloads.len(),
            );
            for id in overloads {
                let function = &compilation.hir.functions[id];
                data.overload(
                    function.public,
                    function_signature(compilation, id),
                    function.documentation.as_deref(),
                );
            }
        }
    }
    decode_site(&TOOL.run(data.0)?)
}

fn invalid_response() -> FosterError {
    FosterError::runtime("Foster documentation tool returned an invalid response")
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value, FosterError> {
    match value {
        Value::Record { fields, .. } => fields.get(key).ok_or_else(invalid_response),
        _ => Err(invalid_response()),
    }
}
#[cfg(test)]
fn text_field(value: &Value, key: &str) -> Result<String, FosterError> {
    tooling::string(field(value, key)?)
}
fn count_field(value: &Value, key: &str) -> Result<usize, FosterError> {
    match field(value, key)? {
        Value::Integer(count) => usize::try_from(*count).map_err(|_| invalid_response()),
        _ => Err(invalid_response()),
    }
}
fn decode_site(value: &Value) -> Result<Site, FosterError> {
    #[cfg(test)]
    let modules = field(value, "modules")?
        .as_list()
        .ok_or_else(invalid_response)?
        .iter()
        .map(|page| {
            Ok(ModulePage {
                file_name: text_field(page, "file_name")?,
                html: text_field(page, "html")?,
            })
        })
        .collect::<Result<Vec<_>, FosterError>>()?;
    Ok(Site {
        #[cfg(test)]
        index: text_field(value, "index")?,
        #[cfg(test)]
        modules,
        module_count: count_field(value, "module_count")?,
        declaration_count: count_field(value, "declaration_count")?,
    })
}

fn visibility_badge(public: bool) -> &'static str {
    if public {
        "<span class=\"badge visibility-public\">public</span>"
    } else {
        "<span class=\"badge visibility-private\">private</span>"
    }
}

pub(super) fn visible_type(public: bool, documentation: Option<&str>) -> bool {
    public || documentation.is_some_and(|docs| !docs.trim().is_empty())
}

fn provided_types(cards: &mut Data, compilation: &Compilation, module_id: ModuleId) {
    let module = &compilation.hir.modules[module_id];
    cards.number(
        module
            .records
            .values()
            .filter(|id| compilation.hir.records[**id].public)
            .count()
            + module
                .variant_types
                .values()
                .filter(|id| compilation.hir.variant_types[**id].public)
                .count(),
    );
    for record_id in module.records.values().copied() {
        let record = &compilation.hir.records[record_id];
        if !record.public {
            continue;
        }
        type_card(
            cards,
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
                        "{}: {}{}",
                        escape(&field.name),
                        TypeLinks::new(compilation, module_id, &record.parameters)
                            .source(&field.ty),
                        visibility_badge(field.public)
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
            cards,
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
}

#[allow(clippy::too_many_arguments)]
fn type_card(
    cards: &mut Data,
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
    cards.text(name);
    cards.flag(public);
    cards.docs(docs);
    cards.number(4);
    let module = &compilation.hir.modules[module_id];
    let contracts = module
        .records
        .get(name)
        .map(|id| {
            let ty = &compilation.hir.records[*id];
            (&ty.parameters, &ty.compositions)
        })
        .or_else(|| {
            module.variant_types.get(name).map(|id| {
                let ty = &compilation.hir.variant_types[*id];
                (&ty.parameters, &ty.compositions)
            })
        });
    if let Some((parameters, compositions)) = contracts {
        let links = TypeLinks::new(compilation, module_id, parameters);
        type_members(
            cards,
            "Contracts",
            compositions.iter().map(|ty| (links.source(ty), None)),
        );
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
        .filter_map(|overloads| {
            overloads
                .iter()
                .copied()
                .find(|id| compilation.hir.functions[*id].public)
        })
        .filter(|id| {
            compilation.hir.functions[*id].public
                && function_owner(compilation, *id).as_deref() == Some(name)
        })
        .collect::<Vec<_>>();
    cards.number(functions.len());
    for id in functions {
        let function = &compilation.hir.functions[id];
        cards.text(&function.name);
        cards.text(function.name.rsplit('.').next().unwrap_or(&function.name));
        cards.optional(
            function
                .documentation
                .as_deref()
                .and_then(|docs| docs.lines().find(|line| !line.trim().is_empty())),
        );
    }
}

fn type_members<'a>(
    cards: &mut Data,
    heading: &str,
    members: impl Iterator<Item = (String, Option<&'a str>)>,
) {
    let members = members.collect::<Vec<_>>();
    cards.text(heading);
    cards.number(members.len());
    for (name, docs) in members {
        cards.text(name);
        cards.optional(docs);
    }
}

fn function_owner(compilation: &Compilation, id: FunctionId) -> Option<String> {
    let function = &compilation.hir.functions[id];
    if let Some((owner, _)) = function.name.split_once('.') {
        return Some(owner.to_owned());
    }
    let first = function.parameters.first()?.local;
    if compilation.hir.locals[first].name != "self" {
        return None;
    }
    compilation
        .types
        .function_type(id)
        .and_then(|signature| signature.parameters.first())
        .map(|ty| compilation.types.display(ty.ty))
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

fn function_signature(compilation: &Compilation, id: FunctionId) -> String {
    let function = &compilation.hir.functions[id];
    let links = TypeLinks::new(compilation, function.module, &function.type_parameters);
    let signature = compilation.types.function_type(id);
    let generic_entries = function
        .type_parameters
        .iter()
        .map(|name| {
            let mut entry = escape(name);
            for constraint in function
                .constraints
                .iter()
                .filter(|constraint| &constraint.parameter == name)
            {
                entry.push_str(" &amp; ");
                entry.push_str(&links.source(&constraint.requirement));
            }
            entry
        })
        .collect::<Vec<_>>();
    let generics = if generic_entries.is_empty() {
        String::new()
    } else {
        format!("&lt;{}&gt;", generic_entries.join(", "))
    };
    let parameters = function
        .parameters
        .iter()
        .enumerate()
        .map(|(index, local)| {
            let name = &compilation.hir.locals[local.local].name;
            let ty = signature
                .and_then(|sig| sig.parameters.get(index))
                .map(|ty| links.resolved(ty.ty))
                .unwrap_or_else(|| "_".into());
            let consume = signature
                .and_then(|sig| sig.parameters.get(index).map(|p| &p.mode))
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
    format!(
        "{}func {name}{generics}({parameters}) -&gt; {result}{effects}",
        if function.public { "pub " } else { "" },
    )
}

fn record_signature(compilation: &Compilation, record: &crate::hir::Record) -> String {
    let links = TypeLinks::new(compilation, record.module, &record.parameters);
    let compositions = if record.compositions.is_empty() {
        String::new()
    } else {
        format!(
            " &amp; {} &amp;",
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
                "    {}{}: {}{}",
                if field.public { "pub " } else { "" },
                field.name,
                links.source(&field.ty),
                visibility_badge(field.public)
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
        "{}type {}{} ={compositions} {{\n{members}\n}}",
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
        "{}{} {}{} = {alternatives}{}",
        if variant.public { "pub " } else { "" },
        if variant.kind == crate::ast::VariantKind::Enum {
            "enum"
        } else {
            "type"
        },
        variant.name,
        escape(&angled(&variant.parameters)),
        variant
            .compositions
            .iter()
            .map(|ty| format!("\n    &amp; {}", links.source(ty)))
            .collect::<String>()
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
    #[test]
    fn constrained_signatures_link_requirement_types() {
        let compilation = crate::compile("pub type Copy = { pub func copy(self) -> self [read self] }\npub type Box<T> = { value: T }\nimpl Box<T & Copy> { pub func copied(self) -> T [read self] { self.value.copy() } }\nfunc main() -> Int { 42 }").unwrap();
        let module = compilation.hir.module_named("main").unwrap();
        let method = compilation
            .hir
            .function_named(module, "Box.copied")
            .unwrap();
        let rendered = super::function_signature(&compilation, method);
        assert!(rendered.contains("T &amp; <a"), "{rendered}");
        assert!(rendered.contains("Copy</a>"), "{rendered}");
    }
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
        let site = site(&compilation).unwrap();
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
        let site = site(&compilation).unwrap();
        let string = &site
            .modules
            .iter()
            .find(|page| page.file_name == "core-string.html")
            .unwrap()
            .html;
        assert!(string.contains("href=\"core-option.html#Option\""));
        assert!(string.contains("href=\"core-code_point.html#module-overview\""));
        assert!(string.contains("href=\"std-iter.html#Iterator\""));
        assert!(string.contains("<h4>Contracts</h4>"));
        assert!(string.contains("href=\"core-copy.html#Copy\""));
        assert!(string.contains("href=\"std-collections.html#Collection\""));
        let toml = site
            .modules
            .iter()
            .find(|page| page.file_name == "std-toml.html")
            .unwrap();
        let signature = toml
            .html
            .split("<article id=\"TomlValue\">")
            .nth(1)
            .unwrap()
            .split("</pre>")
            .next()
            .unwrap();
        assert!(signature.contains("href=\"core-copy.html#Copy\""));
        assert!(
            string.contains("<li data-owner=\"String\" data-declaration><a href=\"#String.first\"")
        );
        assert!(string.contains("<li data-owner=\"GraphemeCursor\" data-declaration>"));
        // Grouping must preserve one navigation destination per declaration.
        for page in &site.modules {
            let navigation = page.html.split("</nav>").next().unwrap();
            let mut anchors = std::collections::BTreeSet::new();
            for entry in navigation.split("data-declaration><a href=\"#").skip(1) {
                let anchor = entry.split('"').next().unwrap();
                assert!(anchors.insert(anchor), "duplicate navigation: {anchor}");
                assert!(page.html.contains(&format!("<article id=\"{anchor}\"")));
            }
        }
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
    fn overloaded_functions_keep_every_signature_description_and_visibility() {
        let compilation = crate::compile(
            "/// Integer conversion.\nfunc convert(value: Int) -> Int { value }\n\
             /// Boolean conversion.\npub func convert(value: Bool) -> Int { branch value { true -> 1\n_ -> 0 } }\n\
             func main() {}",
        ).unwrap();
        let site = site(&compilation).unwrap();
        let html = &site.modules[0].html;
        assert!(html.contains("func convert(value: Int)"));
        assert!(html.contains("pub func convert(value: Bool)"));
        assert!(html.contains("Integer conversion."));
        assert!(html.contains("Boolean conversion."));
        assert!(html.contains("Overload 1<span class=\"badge visibility-private\">"));
        assert!(html.contains("Overload 2<span class=\"badge visibility-public\">"));
        assert_eq!(html.matches("<article id=\"convert\">").count(), 1);
        assert_eq!(site.declaration_count, 2);
    }

    #[test]
    fn field_visibility_is_explicit_in_summaries_and_signatures() {
        let compilation = crate::compile(
            "pub type Example = { pub exposed: Int, hidden: Bool }\n\
             /// A documented private type.\n\
             type Internal = { pub exposed: Int, hidden: Bool }\n\
             func main() {}",
        )
        .unwrap();
        let site = site(&compilation).unwrap();
        let html = &site.modules[0].html;
        let overview = html.split("<article id=\"Example\">").next().unwrap();
        assert!(overview.contains("Int<span class=\"badge visibility-public\">public</span>"));
        assert!(overview.contains("Bool<span class=\"badge visibility-private\">private</span>"));
        for name in ["Example", "Internal"] {
            let declaration = html
                .split(&format!("<article id=\"{name}\">"))
                .nth(1)
                .unwrap()
                .split("</pre>")
                .next()
                .unwrap();
            assert!(
                declaration.contains(
                    "pub exposed: Int<span class=\"badge visibility-public\">public</span>"
                )
            );
            assert!(
                declaration.contains(
                    "hidden: Bool<span class=\"badge visibility-private\">private</span>"
                )
            );
        }
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
        let site = site(&compilation).unwrap();
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
        let site = site(&compilation).unwrap();

        assert_eq!(site.module_count, 1);
        assert!(site.index.contains("data-module=\"main\""));
        assert!(!site.index.contains("data-module=\"core"));
        assert_eq!(site.modules.len(), 1);
        assert_eq!(site.modules[0].file_name, "main.html");
        assert!(!site.modules[0].html.contains("class=\"type-link\""));
    }
}
