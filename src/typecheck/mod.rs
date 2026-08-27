use std::collections::HashMap;

mod annotations;
mod calls;
mod composition;
mod constants;
mod context;
mod effects;
mod expressions;
mod output;
mod predicates;
mod records;
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
    self, Builtin, ConstantId, ExprId, FunctionId, LocalId, RecordId, ResolvedName, VariantTypeId,
};
use crate::types::{FunctionType, Type, TypeId, TypeInformation};

type InferredEffects = HashMap<FunctionId, (Vec<crate::ast::Effect>, bool)>;

struct CheckOutput {
    types: TypeInformation,
    diagnostics: Vec<crate::diagnostic::Diagnostic>,
    inferred_effects: InferredEffects,
}

pub fn check(
    hir: &mut hir::PackageHir,
) -> Result<(TypeInformation, Vec<crate::diagnostic::Diagnostic>), FosterError> {
    loop {
        let inferred = Checker::new(hir).check(false)?.inferred_effects;
        let mut changed = false;
        for (function, (effects, suspends)) in inferred {
            let definition = &mut hir.functions[function];
            if definition.effects != effects || definition.suspends != suspends {
                definition.effects = effects;
                definition.effect_spans.clear();
                definition.suspends = suspends;
                definition.suspend_span = None;
                changed = true;
            }
        }
        if !changed {
            let output = Checker::new(hir).check(true)?;
            return Ok((output.types, output.diagnostics));
        }
    }
}

impl<'a> Checker<'a> {
    fn new(hir: &'a hir::PackageHir) -> Self {
        Self {
            hir,
            next_variable: 0,
            substitutions: HashMap::new(),
            functions: HashMap::new(),
            constants: HashMap::new(),
            locals: HashMap::new(),
            local_groups: HashMap::new(),
            expressions: HashMap::new(),
            extension_methods: HashMap::new(),
            member_constraints: Vec::new(),
            diagnostics: Vec::new(),
            inferred_effects: HashMap::new(),
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

    fn check(mut self, validate_effects: bool) -> Result<CheckOutput, FosterError> {
        self.check_record_declarations()?;
        self.check_variant_declarations()?;
        self.declare_constants()?;
        self.declare_signatures()?;
        self.check_record_compositions()?;
        self.check_variant_compositions()?;
        for (function, _) in self.hir.functions.iter() {
            self.check_function(function)?;
        }
        self.solve_member_constraints()?;
        self.check_derived_effects(validate_effects)?;
        let diagnostics = std::mem::take(&mut self.diagnostics);
        let inferred_effects = std::mem::take(&mut self.inferred_effects);
        Ok(CheckOutput {
            types: self.finish()?,
            diagnostics,
            inferred_effects,
        })
    }

    fn check_derived_effects(&mut self, validate: bool) -> Result<(), FosterError> {
        for (function, definition) in self.hir.functions.iter() {
            if definition.intrinsic.is_some() {
                continue;
            }
            let mut derivation = EffectDerivation::new(self, function);
            derivation.walk_statements(&definition.body);
            let actual = derivation.effects();
            let derived_suspends = derivation.suspends;
            drop(derivation);
            if !definition.effects_explicit {
                self.inferred_effects
                    .insert(function, (actual, derived_suspends));
                continue;
            }
            if !validate {
                continue;
            }
            if !effects_are_subset(&actual, &definition.effects) {
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
            if derived_suspends && !definition.suspends && !is_entry {
                return Err(self.error(
                    function,
                    "function body may suspend; add `suspend` to its signature",
                ));
            }
            if !definition.name.contains('$') {
                for (index, declared) in definition.effects.iter().enumerate() {
                    if declared.kind != crate::ast::EffectKind::Consume
                        && !effects_are_subset(std::slice::from_ref(declared), &actual)
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
                if definition.suspends && !derived_suspends {
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
                        if variant.kind == crate::ast::VariantKind::Union
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
                                "union"
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
                let declared_owner = self.resolve_nominal_type(module, owner)?;
                let receiver_owner = self.resolve_nominal_type(module, receiver_name)?;
                if declared_owner != receiver_owner {
                    return Err(FosterError::runtime(format!(
                        "method `{}` is owned by `{owner}` but receives `{receiver_name}`",
                        function.name
                    )));
                }
            }
            let result = match function.return_type.as_ref() {
                Some(annotation) => self.annotation_type(module, annotation, &generics)?,
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
