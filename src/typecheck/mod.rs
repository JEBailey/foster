use std::collections::{HashMap, HashSet};

mod annotations;
mod calls;
mod composition;
mod constants;
mod context;
mod effect_worklist;
mod effects;
mod expressions;
pub(crate) mod incremental;
mod output;
mod overloads;
mod predicates;
mod records;
mod substitutions;
mod transactions;
mod unify;
mod variants;
use context::*;
use effects::EffectDerivation;
use predicates::{
    FRAME_GROUP, callable_effects, contains_variable, effect_kind_name, effects_are_subset,
    function_parameter_modes, pattern_is_irrefutable, reference_group, remote_transferable,
};

use crate::ast::{BinaryOp, UnaryOp};
use crate::error::FosterError;
use crate::hir::{
    self, ConstantId, ExprId, FunctionId, LocalId, RecordId, ResolvedName, VariantTypeId,
};
use crate::intrinsics::{Builtin, Intrinsic};
use crate::types::{
    DispatchSlot, DispatchTypeKey, FunctionType, MethodKey, NominalTypeId, ResolvedCall, Type,
    TypeId, TypeInformation,
};

type DerivedEffects = HashMap<FunctionId, (Vec<crate::ast::Effect>, bool)>;

pub fn check(
    hir: &mut hir::PackageHir,
) -> Result<(TypeInformation, Vec<crate::diagnostic::Diagnostic>), FosterError> {
    check_bodies(hir, None, None).map_err(|errors| errors.into_iter().next().unwrap())
}

pub(crate) fn check_collecting(
    hir: &mut hir::PackageHir,
    recoverable: &HashSet<hir::ModuleId>,
    cache: Option<incremental::SharedBodyCache>,
) -> Result<(TypeInformation, Vec<crate::diagnostic::Diagnostic>), Vec<FosterError>> {
    check_bodies(hir, Some(recoverable), cache)
}

fn check_bodies(
    hir: &mut hir::PackageHir,
    recover: Option<&HashSet<hir::ModuleId>>,
    cache: Option<incremental::SharedBodyCache>,
) -> Result<(TypeInformation, Vec<crate::diagnostic::Diagnostic>), Vec<FosterError>> {
    loop {
        crate::compiler::cancellation::check().map_err(|e| vec![e])?;
        crate::compiler::profile::count("types.iterations");
        let mut checker = Checker::new(hir);
        checker.body_cache = cache.clone();
        let mut checker = checker.check(recover)?;
        let changed = checker
            .derived_effects
            .iter()
            .any(|(function, (effects, suspends))| {
                let definition = &hir.functions[*function];
                !definition.effects_explicit
                    && (definition.effects != *effects || definition.suspends != *suspends)
            });
        if !changed {
            // The current pass already checked every body against the fixed-point contracts.
            // Validate the published bounds using that same state instead of repeating all
            // declaration, expression, and structural checks in a fresh checker.
            crate::compiler::profile::measure("types.effects_validate", || {
                checker.validate_derived_effects()
            })
            .map_err(|e| vec![e])?;
            checker
                .check_composed_implementations()
                .map_err(|e| vec![e])?;
            checker.record_effect_summaries();
            let diagnostics = std::mem::take(&mut checker.diagnostics);
            return Ok((
                crate::compiler::profile::measure("types.finish", || checker.finish())
                    .map_err(|e| vec![e])?,
                diagnostics,
            ));
        }
        let inferred = std::mem::take(&mut checker.derived_effects);
        // Keep finalization checks on intermediate passes: malformed callable/member uses and
        // unresolved types must still be rejected before publishing their inferred effects.
        crate::compiler::profile::measure("types.finish", || checker.finish())
            .map_err(|e| vec![e])?;
        for (function, (effects, suspends)) in inferred {
            let definition = &mut hir.functions[function];
            if !definition.effects_explicit
                && (definition.effects != effects || definition.suspends != suspends)
            {
                definition.effects = effects;
                definition.effect_spans.clear();
                definition.suspends = suspends;
                definition.suspend_span = None;
            }
        }
    }
}

impl<'a> Checker<'a> {
    fn new(hir: &'a hir::PackageHir) -> Self {
        Self {
            body_cache: None,
            body_cacheable: true,
            record_fields_cache: Default::default(),
            record_methods_cache: Default::default(),
            hir,
            next_variable: 0,
            checked_requirements: Default::default(),
            substitutions: substitutions::Substitutions::default(),
            functions: Default::default(),
            constants: Default::default(),
            locals: Default::default(),
            local_groups: Default::default(),
            expressions: Default::default(),
            integer_promotions: Default::default(),
            member_kinds: Default::default(),
            bare_method_members: Default::default(),
            resolved_calls: Default::default(),
            dispatch_slots: Default::default(),
            dispatch_keys: Vec::new(),
            member_constraints: Vec::new(),
            diagnostics: Vec::new(),
            derived_effects: Default::default(),
            effect_seeds: Default::default(),
            effect_dependencies: Default::default(),
            resolving_aliases: Vec::new(),
        }
    }

    fn string_type(&self) -> Ty {
        let module = self
            .hir
            .module_named("core.string")
            .expect("the Foster String bootstrap module is installed");
        let record = self.hir.modules[module]
            .records
            .get("String")
            .copied()
            .expect("core.string defines String");
        Ty::Record(record, Vec::new())
    }

    fn is_string_type(&self, ty: &Ty) -> bool {
        self.resolved(ty.clone()) == self.string_type()
    }

    fn symbol_type(&self) -> Ty {
        let module = self
            .hir
            .module_named("core.symbol")
            .expect("the Foster Symbol bootstrap module is installed");
        let record = self.hir.modules[module]
            .records
            .get("Symbol")
            .copied()
            .expect("core.symbol defines Symbol");
        Ty::Record(record, Vec::new())
    }

    fn is_copy_type(&self, ty: &Ty) -> bool {
        matches!(
            self.resolved(ty.clone()),
            Ty::Unit | Ty::Bool | Ty::Int | Ty::Float | Ty::CodePoint | Ty::Byte
        ) || self.resolved(ty.clone()) == self.symbol_type()
    }

    fn bytes_type(&self) -> Ty {
        let module = self
            .hir
            .module_named("core.bytes")
            .expect("the Foster Bytes bootstrap module is installed");
        let record = self.hir.modules[module]
            .records
            .get("Bytes")
            .copied()
            .expect("core.bytes defines Bytes");
        Ty::Record(record, Vec::new())
    }

    fn is_bytes_type(&self, ty: &Ty) -> bool {
        self.resolved(ty.clone()) == self.bytes_type()
    }

    fn byte_buffer_type(&self) -> Ty {
        let module = self
            .hir
            .module_named("core.bytes.buffer")
            .expect("the Foster ByteBuffer bootstrap module is installed");
        let record = self.hir.modules[module]
            .records
            .get("ByteBuffer")
            .copied()
            .expect("core.bytes.buffer defines ByteBuffer");
        Ty::Record(record, Vec::new())
    }

    fn is_byte_buffer_type(&self, ty: &Ty) -> bool {
        self.resolved(ty.clone()) == self.byte_buffer_type()
    }

    fn list_type(&self, element: Ty) -> Ty {
        let module = self
            .hir
            .module_named("core.list")
            .expect("the Foster List bootstrap module is installed");
        let record = self.hir.modules[module]
            .records
            .get("List")
            .copied()
            .expect("core.list defines List");
        Ty::Record(record, vec![element])
    }

    fn list_element(&self, ty: &Ty) -> Option<Ty> {
        let Ty::Record(record, arguments) = self.resolved(ty.clone()) else {
            return None;
        };
        let module = self.hir.module_named("core.list")?;
        (self.hir.modules[module].records.get("List").copied() == Some(record))
            .then(|| arguments.into_iter().next())
            .flatten()
    }

    fn prepare(&mut self) -> Result<(), FosterError> {
        self.check_record_declarations()?;
        self.check_variant_declarations()?;
        self.declare_constants()?;
        self.declare_signatures()?;
        self.validate_overloads()?;
        self.check_record_compositions()?;
        self.check_variant_compositions()?;
        Ok(())
    }

    fn check(mut self, recover: Option<&HashSet<hir::ModuleId>>) -> Result<Self, Vec<FosterError>> {
        crate::compiler::profile::measure("types.declarations", || self.prepare())
            .map_err(|e| vec![e])?;
        let mut errors = Vec::new();
        for (function, _) in self.hir.functions.iter() {
            crate::compiler::cancellation::check().map_err(|e| vec![e])?;
            self.body_cacheable = true;
            let input = self.body_input(function);
            if let Some(error) = self.cached_failure(function, &input) {
                crate::compiler::profile::count("body.error_hit");
                errors.push(error);
                continue;
            }
            if crate::compiler::profile::measure("types.cache_lookup", || {
                self.reuse_body(function, &input)
            }) {
                crate::compiler::profile::count("body.hit");
                continue;
            }
            let constraints_before = self.member_constraints.len();
            let definition = &self.hir.functions[function];
            // Explicit signatures isolate body constraints. With an inferred signature, stop
            // at the first failure rather than attributing speculative downstream errors.
            let independent = definition.return_type.is_some()
                && definition.parameter_types.iter().all(Option::is_some);
            let checkpoint = (independent
                && recover.is_some_and(|modules| modules.contains(&definition.module)))
            .then(|| crate::compiler::profile::measure("types.checkpoint", || self.begin_body()));
            crate::compiler::profile::count(if definition.body.is_empty() {
                "body.empty_checked"
            } else {
                "body.checked"
            });
            if let Err(error) = crate::compiler::profile::measure("types.body_check", || {
                self.check_function(function)
            }) {
                if crate::compiler::cancellation::is_cancellation(&error) {
                    return Err(vec![error]);
                }
                self.record_failure(function, input, &error);
                errors.push(error);
                if let Some(checkpoint) = checkpoint {
                    crate::compiler::profile::measure("types.rollback", || {
                        self.rollback_body(checkpoint)
                    });
                } else {
                    return Err(errors);
                }
            } else {
                if let Some(checkpoint) = checkpoint {
                    crate::compiler::profile::measure("types.commit", || {
                        self.commit_body(checkpoint)
                    });
                }
                crate::compiler::profile::measure("types.cache_store", || {
                    self.record_body(function, input, constraints_before)
                });
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        self.solve_member_constraints().map_err(|e| vec![e])?;
        crate::compiler::profile::measure("types.effects", || self.derive_effects())
            .map_err(|e| vec![e])?;
        Ok(self)
    }

    fn validate_overloads(&self) -> Result<(), FosterError> {
        for (_, module) in self.hir.modules.iter() {
            for (name, overloads) in &module.functions {
                if overloads.len() < 2 {
                    continue;
                }
                let mut signatures = HashMap::<Vec<String>, FunctionId>::new();
                for function in overloads {
                    let definition = &self.hir.functions[*function];
                    let generics = definition
                        .type_parameters
                        .iter()
                        .enumerate()
                        .map(|(index, name)| (name.as_str(), index))
                        .collect::<HashMap<_, _>>();
                    let groups = definition
                        .groups
                        .iter()
                        .enumerate()
                        .map(|(index, group)| (group.name.as_str(), index))
                        .collect::<HashMap<_, _>>();
                    let signature = definition
                        .parameter_types
                        .iter()
                        .map(|parameter| {
                            parameter
                                .as_ref()
                                .map(|parameter| overload_type_key(parameter, &generics, &groups))
                                .ok_or_else(|| {
                                    self.error(
                                        *function,
                                        format!(
                                            "overloaded function `{name}` must give every parameter a type"
                                        ),
                                    )
                                })
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    if let Some(previous) = signatures.insert(signature, *function) {
                        return Err(self.error(
                            *function,
                            format!(
                                "overload `{name}` has the same parameter signature as another declaration; return types and effects do not distinguish overloads"
                            ),
                        )
                        .with_label(
                            self.hir.functions[previous].span.clone(),
                            "the first declaration with this parameter signature is here",
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_derived_effects(&mut self) -> Result<(), FosterError> {
        for (function, definition) in self.hir.functions.iter() {
            crate::compiler::cancellation::check()?;
            if definition.intrinsic.is_some()
                || self.hir.external_functions.contains_key(&function)
                || !definition.effects_explicit
            {
                continue;
            }
            // These summaries belong to this checker pass. Validation runs only after
            // inference converges, against exactly the contracts used for derivation.
            let (actual, derived_suspends) = &self.derived_effects[&function];
            if !effects_are_subset(actual, &definition.effects) {
                let missing = actual
                    .iter()
                    .filter(|effect| {
                        !effects_are_subset(std::slice::from_ref(effect), &definition.effects)
                    })
                    .map(|effect| format!("{:?} {}", effect.kind, effect.target).to_lowercase())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(self.error(
                    function,
                    format!("function body requires undeclared effect(s): {missing}"),
                ));
            }
            let is_entry =
                definition.name == "main" && self.hir.modules[definition.module].name == "main";
            if *derived_suspends && !definition.suspends && !is_entry {
                return Err(self.error(
                    function,
                    "function body may suspend; add `suspend` to its signature",
                ));
            }
            // Diagnose the source declaration once, rather than repeating its
            // advisory warnings for every specialization of an inherited body.
            if !definition.name.contains('$')
                && !self.hir.composition_owners.contains_key(&function)
            {
                for (index, declared) in definition.effects.iter().enumerate() {
                    if declared.kind != crate::ast::EffectKind::Consume
                        && !effects_are_subset(std::slice::from_ref(declared), actual)
                    {
                        let narrower = actual
                            .iter()
                            .filter(|required| {
                                effects_are_subset(
                                    std::slice::from_ref(*required),
                                    std::slice::from_ref(declared),
                                )
                            })
                            .map(|required| {
                                format!("`{} {}`", effect_kind_name(required.kind), required.target)
                            })
                            .collect::<Vec<_>>();
                        let message = if narrower.is_empty() {
                            format!(
                                "in `{}.{}`: declared `{} {}` is not required by the function body",
                                self.hir.modules[definition.module].name,
                                definition.name,
                                effect_kind_name(declared.kind),
                                declared.target
                            )
                        } else {
                            format!(
                                "in `{}.{}`: declared `{} {}` is overly broad; the function body requires only {}",
                                self.hir.modules[definition.module].name,
                                definition.name,
                                effect_kind_name(declared.kind),
                                declared.target,
                                narrower.join(", ")
                            )
                        };
                        let mut diagnostic =
                            crate::diagnostic::Diagnostic::warning("unused-effect", message)
                                .with_source_module(
                                    self.hir.modules[definition.module].name.clone(),
                                );
                        if let Some(span) = definition.effect_spans.get(index) {
                            let label = if narrower.is_empty() {
                                "this declared effect is not used by the function body"
                            } else {
                                "this declared effect grants broader access than the function body requires"
                            };
                            diagnostic = diagnostic.with_label(span.clone(), label);
                        }
                        self.diagnostics.push(diagnostic);
                    }
                }
                if definition.suspends && !*derived_suspends {
                    let mut diagnostic = crate::diagnostic::Diagnostic::warning(
                        "unused-suspend",
                        format!(
                            "in `{}.{}`: declared `suspend` is not required by the function body",
                            self.hir.modules[definition.module].name, definition.name
                        ),
                    )
                    .with_source_module(self.hir.modules[definition.module].name.clone());
                    if let Some(span) = &definition.suspend_span {
                        diagnostic =
                            diagnostic.with_label(span.clone(), "this function does not suspend");
                    }
                    self.diagnostics.push(diagnostic);
                }
            }
        }
        Ok(())
    }

    fn check_variant_declarations(&mut self) -> Result<(), FosterError> {
        for (_, variant) in self.hir.variant_types.iter() {
            let generics = variant
                .parameters
                .iter()
                .map(|p| (p.clone(), self.fresh()))
                .collect::<HashMap<_, _>>();
            for alternative in &variant.alternatives {
                let alternative = &self.hir.variants[*alternative];
                let annotations = alternative.member.iter().chain(alternative.payload.iter());
                for annotation in annotations {
                    let ty = self.annotation_type(variant.module, annotation, &generics)?;
                    if variant.public
                        && let Some(private) = self.private_type_in(&ty)
                    {
                        if variant.kind == crate::ast::VariantKind::Alias
                            && variant.alternatives.len() == 1
                            && variant.compositions.is_empty()
                            && variant.methods.is_empty()
                        {
                            return Err(FosterError::runtime(format!(
                                "public type alias `{}` exposes private type `{private}`",
                                variant.name
                            )));
                        }
                        return Err(FosterError::runtime(format!(
                            "public {} `{}` includes private type `{private}`",
                            if variant.kind == crate::ast::VariantKind::Enum {
                                "enum"
                            } else {
                                "type alias"
                            },
                            variant.name
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn check_record_declarations(&mut self) -> Result<(), FosterError> {
        let records = self
            .hir
            .records
            .iter()
            .map(|(id, record)| (id, record.clone()))
            .collect::<Vec<_>>();
        for (_, record) in records {
            let mut generics = HashMap::new();
            for parameter in &record.parameters {
                let generic = self.fresh();
                generics.insert(parameter.clone(), generic);
            }
            for field in &record.fields {
                let ty = self.annotation_type(record.module, &field.ty, &generics)?;
                if record.public
                    && field.public
                    && let Some(private) = self.private_type_in(&ty)
                {
                    return Err(FosterError::runtime(format!(
                        "public field `{}.{}` exposes private type `{private}`",
                        record.name, field.name
                    )));
                }
            }
        }
        Ok(())
    }

    fn declare_signatures(&mut self) -> Result<(), FosterError> {
        for (function_id, function) in self.hir.functions.iter() {
            let module = function.module;
            let source_module = self.hir.modules[module].name.clone();
            let generics = function
                .type_parameters
                .iter()
                .map(|parameter| (parameter.clone(), Ty::Generic(parameter.clone())))
                .collect::<HashMap<_, _>>();
            let parameters = function
                .parameter_types
                .iter()
                .zip(&function.parameter_type_spans)
                .map(|(annotation, span)| match annotation {
                    Some(annotation) => self
                        .annotation_type(module, annotation, &generics)
                        .map_err(|error| {
                            located_annotation_error(error, span.as_ref(), &source_module)
                        }),
                    None => Ok(self.fresh()),
                })
                .collect::<Result<Vec<_>, _>>()?;
            if function.receiver.is_some() {
                let owner = function
                    .owner
                    .as_deref()
                    .expect("package validation requires receivers to have an owner");
                let receiver_annotation = function
                    .parameter_types
                    .first()
                    .and_then(Option::as_ref)
                    .ok_or_else(|| {
                        FosterError::runtime(format!(
                            "method `{}` must give `self` the owner type `{owner}`",
                            function.name
                        ))
                    })?;
                let receiver_annotation = match receiver_annotation {
                    crate::ast::TypeExpr::Reference { value, .. } => value.as_ref(),
                    value => value,
                };
                let crate::ast::TypeExpr::Named(receiver_name, _) = receiver_annotation else {
                    return Err(FosterError::runtime(format!(
                        "method `{}` must give `self` the owner type `{owner}`",
                        function.name
                    )));
                };
                let primitive_owner =
                    |name: &str| matches!(name, "Bool" | "Int" | "Float" | "CodePoint" | "Byte");
                let owners_match = if primitive_owner(owner) || primitive_owner(receiver_name) {
                    owner == receiver_name
                } else {
                    self.resolve_nominal_type(
                        self.hir
                            .composition_owners
                            .get(&function_id)
                            .copied()
                            .unwrap_or(module),
                        owner,
                    )? == self.resolve_nominal_type(module, receiver_name)?
                };
                if !owners_match {
                    return Err(FosterError::runtime(format!(
                        "method `{}` is owned by `{owner}` but receives `{receiver_name}`",
                        function.name
                    )));
                }
            }
            let mut result_generics = generics.clone();
            if function.receiver.is_some() {
                result_generics.insert("self".into(), parameters[0].clone());
            }
            let result = match function.return_type.as_ref() {
                Some(annotation) => self.annotation_type(module, annotation, &result_generics)?,
                None => self.fresh(),
            };
            if function.public
                && let Some(private) = parameters
                    .iter()
                    .chain(std::iter::once(&result))
                    .find_map(|ty| self.private_type_in(ty))
            {
                return Err(FosterError::runtime(format!(
                    "public function `{}` exposes private type `{private}`",
                    function.name
                )));
            }
            self.functions.insert(
                function_id,
                Signature {
                    parameters,
                    parameter_modes: function_parameter_modes(self.hir, function_id),
                    result,
                },
            );
        }
        Ok(())
    }
}

fn overload_type_key(
    ty: &crate::ast::TypeExpr,
    generics: &HashMap<&str, usize>,
    groups: &HashMap<&str, usize>,
) -> String {
    match ty {
        crate::ast::TypeExpr::Unit => "()".to_owned(),
        crate::ast::TypeExpr::Named(name, arguments) => {
            let name = generics
                .get(name.as_str())
                .map_or_else(|| name.clone(), |index| format!("${index}"));
            if arguments.is_empty() {
                name
            } else {
                format!(
                    "{name}<{}>",
                    arguments
                        .iter()
                        .map(|argument| overload_type_key(argument, generics, groups))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            }
        }
        crate::ast::TypeExpr::Intersection(members) => {
            let mut members = members
                .iter()
                .map(|member| overload_type_key(member, generics, groups))
                .collect::<Vec<_>>();
            members.sort();
            members.join("&")
        }
        crate::ast::TypeExpr::Reference { group, value } => {
            let group = groups
                .get(group.as_str())
                .map_or_else(|| group.clone(), |index| format!("@{index}"));
            format!("ref[{group}]{}", overload_type_key(value, generics, groups))
        }
        crate::ast::TypeExpr::Function {
            parameters,
            parameter_modes,
            result,
            effects,
            suspends,
        } => format!(
            "func({})->{}[{:?};{:?};{suspends}]",
            parameters
                .iter()
                .zip(parameter_modes)
                .map(|(parameter, mode)| format!(
                    "{mode:?}:{}",
                    overload_type_key(parameter, generics, groups)
                ))
                .collect::<Vec<_>>()
                .join(","),
            overload_type_key(result, generics, groups),
            parameter_modes,
            effects
        ),
    }
}

fn located_annotation_error(
    mut error: FosterError,
    span: Option<&std::ops::Range<usize>>,
    source_module: &str,
) -> FosterError {
    if error.labels.is_empty()
        && let Some(span) = span
    {
        error = error.with_primary_label(span.clone(), "invalid type annotation");
    }
    if error.source_module.is_none() {
        error = error.with_source_module(source_module);
    }
    error
}
