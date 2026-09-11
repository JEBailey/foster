//! Type-only HTML rendering: resolve identities, never guess from declaration text.
use super::render::{effects, escape, module_file_name, visible_type};
use crate::ast::{ParameterMode, TypeExpr};
use crate::compiler::Compilation;
use crate::hir::ModuleId;
use crate::types::{Type, TypeId};

pub(super) struct TypeLinks<'a> {
    compilation: &'a Compilation,
    module: ModuleId,
    parameters: Vec<String>,
}

impl<'a> TypeLinks<'a> {
    pub fn new(compilation: &'a Compilation, module: ModuleId, parameters: &[String]) -> Self {
        Self {
            compilation,
            module,
            parameters: parameters.to_vec(),
        }
    }

    pub fn scoped(&self, parameters: &[String]) -> Self {
        let mut all = self.parameters.clone();
        all.extend_from_slice(parameters);
        Self::new(self.compilation, self.module, &all)
    }

    fn link(&self, module: ModuleId, anchor: &str, label: &str) -> String {
        let name = &self.compilation.hir.modules[module].name;
        if !self.compilation.package.is_input_module(name) {
            return escape(label);
        }
        let file = if module == self.module {
            String::new()
        } else {
            module_file_name(name)
        };
        format!(
            "<a class=\"type-link\" href=\"{}#{}\" title=\"{}\">{}</a>",
            escape(&file),
            escape(anchor),
            escape(&format!("{name}.{anchor}")),
            escape(label)
        )
    }

    fn declared(&self, module: ModuleId, name: &str, label: &str) -> Option<String> {
        let hir = &self.compilation.hir;
        let definitions = &hir.modules[module];
        let (public, docs) = if let Some(id) = definitions.records.get(name) {
            let record = &hir.records[*id];
            (record.public, record.documentation.as_deref())
        } else if let Some(id) = definitions.variant_types.get(name) {
            let variant = &hir.variant_types[*id];
            (variant.public, variant.documentation.as_deref())
        } else {
            return None;
        };
        Some(if visible_type(public, docs) {
            self.link(module, name, label)
        } else {
            escape(label)
        })
    }

    fn builtin(&self, name: &str) -> String {
        let module_name = match name {
            "Int" => "core.int",
            "Bool" => "core.bool",
            "Float" => "core.float",
            "CodePoint" => "core.code_point",
            "Byte" => "core.byte",
            "String" => "core.string",
            "List" => "core.list",
            "Bytes" => "core.bytes",
            "ByteBuffer" => "core.bytes.buffer",
            "Symbol" => "core.symbol",
            "Sequence" => "std.sequence",
            _ => return escape(name),
        };
        self.compilation.hir.module_named(module_name).map_or_else(
            || escape(name),
            |module| {
                self.declared(module, name, name)
                    .unwrap_or_else(|| self.link(module, "module-overview", name))
            },
        )
    }

    fn named(&self, name: &str) -> String {
        let label = name.replace('.', "::");
        if self.parameters.iter().any(|parameter| parameter == name) {
            return escape(&label);
        }
        let hir = &self.compilation.hir;
        let module = &hir.modules[self.module];
        if let Some((prefix, member)) = name.rsplit_once('.') {
            let target = module
                .imports
                .get(prefix)
                .copied()
                .or_else(|| {
                    let (first, rest) = prefix.split_once('.')?;
                    let imported = module.imports.get(first)?;
                    hir.module_named(&format!("{}.{}", hir.modules[*imported].name, rest))
                })
                .or_else(|| hir.module_named(prefix));
            return target
                .and_then(|id| self.declared(id, member, &label))
                .unwrap_or_else(|| escape(&label));
        }
        if let Some(value) = self.declared(self.module, name, &label) {
            return value;
        }
        let mut candidates = module
            .imports
            .values()
            .filter_map(|id| {
                let definitions = &hir.modules[*id];
                let public = definitions
                    .records
                    .get(name)
                    .is_some_and(|id| hir.records[*id].public)
                    || definitions
                        .variant_types
                        .get(name)
                        .is_some_and(|id| hir.variant_types[*id].public);
                public.then_some(*id)
            })
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        if candidates.len() == 1 {
            return self.declared(candidates[0], name, &label).unwrap();
        }
        if !candidates.is_empty() {
            return escape(&label);
        }
        self.builtin(name)
    }

    pub fn source(&self, ty: &TypeExpr) -> String {
        match ty {
            TypeExpr::Unit => "()".into(),
            TypeExpr::Named(name, arguments) => format!(
                "{}{}",
                self.named(name),
                arguments_html(arguments.iter().map(|ty| self.source(ty)).collect())
            ),
            TypeExpr::Intersection(types) => types
                .iter()
                .map(|ty| self.source(ty))
                .collect::<Vec<_>>()
                .join(" &amp; "),
            TypeExpr::Reference { group, value } => {
                format!("ref[{}] {}", escape(group), self.source(value))
            }
            TypeExpr::Function {
                parameters,
                parameter_modes,
                result,
                effects: declared,
                suspends,
            } => {
                format!(
                    "func({}) -&gt; {}{}",
                    parameters
                        .iter()
                        .zip(parameter_modes)
                        .map(|(ty, mode)| format!(
                            "{}{}",
                            if *mode == ParameterMode::Consume {
                                "consume "
                            } else {
                                ""
                            },
                            self.source(ty)
                        ))
                        .collect::<Vec<_>>()
                        .join(", "),
                    self.source(result),
                    escape(&effects(declared, *suspends))
                )
            }
        }
    }

    pub fn resolved(&self, id: TypeId) -> String {
        let types = &self.compilation.types;
        let hir = &self.compilation.hir;
        match &types.types[id] {
            Type::Record { record, arguments } => {
                let record = &hir.records[*record];
                format!(
                    "{}{}",
                    self.declared(record.module, &record.name, &record.name)
                        .unwrap_or_else(|| escape(&record.name)),
                    arguments_html(arguments.iter().map(|ty| self.resolved(*ty)).collect())
                )
            }
            Type::Variant { variant, arguments } => {
                let variant = &hir.variant_types[*variant];
                format!(
                    "{}{}",
                    self.declared(variant.module, &variant.name, &variant.name)
                        .unwrap_or_else(|| escape(&variant.name)),
                    arguments_html(arguments.iter().map(|ty| self.resolved(*ty)).collect())
                )
            }
            Type::Generic(name) => escape(name),
            Type::Reference { group, value } => {
                format!("ref[{}] {}", escape(group), self.resolved(*value))
            }
            Type::Intersection(types) => types
                .iter()
                .map(|ty| self.resolved(*ty))
                .collect::<Vec<_>>()
                .join(" &amp; "),
            Type::RawList(value)
            | Type::Sequence(value)
            | Type::Remote(value)
            | Type::Future(value) => {
                let name = match &types.types[id] {
                    Type::RawList(_) => "RawList",
                    Type::Sequence(_) => "Sequence",
                    Type::Remote(_) => "Remote",
                    _ => "Future",
                };
                format!(
                    "{}{}",
                    self.builtin(name),
                    arguments_html(vec![self.resolved(*value)])
                )
            }
            Type::Function(function) => format!(
                "func({}) -&gt; {}{}",
                function
                    .parameters
                    .iter()
                    .zip(&function.parameter_modes)
                    .map(|(ty, mode)| format!(
                        "{}{}",
                        if *mode == ParameterMode::Consume {
                            "consume "
                        } else {
                            ""
                        },
                        self.resolved(*ty)
                    ))
                    .collect::<Vec<_>>()
                    .join(", "),
                self.resolved(function.result),
                escape(&effects(&function.effects, function.suspends))
            ),
            _ => self.builtin(&types.display(id)),
        }
    }
}

fn arguments_html(values: Vec<String>) -> String {
    if values.is_empty() {
        String::new()
    } else {
        format!("&lt;{}&gt;", values.join(", "))
    }
}
