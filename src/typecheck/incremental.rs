//! Body-result reuse for interactive checking. Declaration changes invalidate the session;
//! body edits invalidate only their entry and consumers of changed callable contracts.
use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use super::*;

// Compare fingerprints first, but retain exact text equality so collisions cannot reuse
// a different program. Cloning cache guards shares the source rather than copying it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SourceKey {
    fingerprint: [u64; 2],
    text: Rc<str>,
}

impl SourceKey {
    fn new(text: &str) -> Self {
        let fingerprint = [0u64, 1].map(|seed| {
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            seed.hash(&mut hash);
            text.hash(&mut hash);
            hash.finish()
        });
        Self {
            fingerprint,
            text: text.into(),
        }
    }

    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

pub(crate) type SharedBodyCache = Rc<RefCell<BodyCache>>;

#[derive(Default, Debug, Clone)]
pub(crate) struct AnalysisStats {
    pub checked: HashMap<String, usize>,
    pub reused: HashMap<String, usize>,
    pub effect_derived: HashMap<String, usize>,
    pub effect_reused: HashMap<String, usize>,
    pub pipeline_runs: usize,
}

#[derive(Default)]
pub(crate) struct BodyCache {
    declarations: SourceKey,
    shapes: HashMap<FunctionId, Shape>,
    entries: HashMap<String, BodyResult>,
    failures: HashMap<String, BodyFailure>,
    contracts: HashMap<String, (SourceKey, Vec<crate::ast::Effect>, bool)>,
    pub stats: AnalysisStats,
}

#[derive(Clone)]
struct Shape {
    key: String,
    source: SourceKey,
    raw_source: String,
    start: usize,
    dependencies: HashSet<FunctionId>,
    locals: Vec<LocalId>,
    expressions: Vec<ExprId>,
    eligible: bool,
}

#[derive(Clone, PartialEq, Eq)]
struct Contract {
    signature: Signature,
    effects: Vec<crate::ast::Effect>,
    suspends: bool,
}

#[derive(Clone)]
struct BodyResult {
    effect_summary: Option<effect_worklist::EffectSummary>,
    source: SourceKey,
    input: Signature,
    dependencies: Vec<(FunctionId, Contract)>,
    locals: Vec<Option<(Ty, Option<String>)>>,
    expressions: Vec<Option<Ty>>,
    promotions: Vec<usize>,
    members: Vec<(usize, crate::semantics::MemberKind)>,
    bare_methods: Vec<usize>,
    calls: Vec<(usize, ResolvedCall, Option<MethodKey>)>,
}

struct BodyFailure {
    source: String,
    input: Signature,
    dependencies: Vec<(FunctionId, Contract)>,
    error: FosterError,
}

impl BodyCache {
    pub(super) fn note_effect(&mut self, function: FunctionId, reused: bool) {
        let key = self.shapes[&function].key.clone();
        let counts = if reused {
            &mut self.stats.effect_reused
        } else {
            &mut self.stats.effect_derived
        };
        *counts.entry(key).or_default() += 1;
    }
    pub(crate) fn prepare(&mut self, hir: &mut hir::PackageHir, package: &crate::package::Package) {
        let declarations = SourceKey::new(&declaration_key(hir));
        if declarations != self.declarations {
            crate::compiler::profile::count("body_cache.declaration_reset");
            self.entries.clear();
            self.failures.clear();
            self.contracts.clear();
            self.declarations = declarations;
        }
        self.shapes.clear();
        let mut ordinals = HashMap::new();
        for (id, function) in hir.functions.iter() {
            let name = format!("{}::{}", hir.modules[function.module].name, function.name);
            let ordinal = ordinals.entry(name.clone()).or_insert(0);
            let key = format!("{name}#{ordinal}");
            *ordinal += 1;
            let raw_source = package
                .module(&hir.modules[function.module].name)
                .and_then(|module| module.source.as_deref())
                .and_then(|source| source.get(function.span.clone()));
            let source = raw_source.map(SourceKey::new);
            self.shapes.insert(
                id,
                Shape {
                    key,
                    eligible: source.is_some()
                        && !function.body.is_empty()
                        && !function.name.contains('$'),
                    source: source.unwrap_or_default(),
                    raw_source: raw_source.unwrap_or_default().to_owned(),
                    start: function.span.start,
                    dependencies: HashSet::new(),
                    locals: Vec::new(),
                    expressions: Vec::new(),
                },
            );
        }
        for (id, local) in hir.locals.iter() {
            self.shapes
                .get_mut(&local.function)
                .unwrap()
                .locals
                .push(id);
        }
        for (id, expression) in hir.expressions.iter() {
            let Some(owner) = hir.expression_functions.get(&id) else {
                continue;
            };
            let shape = self.shapes.get_mut(owner).unwrap();
            shape.expressions.push(id);
            if matches!(expression, hir::Expr::Closure { .. })
                || matches!(expression, hir::Expr::Name(ResolvedName::Local(local)) if hir.locals[*local].function != *owner)
            {
                shape.eligible = false;
            }
        }
        let live = self
            .shapes
            .values()
            .map(|shape| shape.key.clone())
            .collect::<HashSet<_>>();
        self.entries.retain(|key, _| live.contains(key));
        self.failures.retain(|key, _| live.contains(key));
        self.contracts.retain(|key, _| live.contains(key));
        // Restart changed dependency components from the empty effect row. Seeding a recursive
        // component with its old row could retain effects removed by the edit indefinitely.
        let mut dirty = self
            .shapes
            .iter()
            .filter_map(|(id, shape)| {
                (!self
                    .contracts
                    .get(&shape.key)
                    .is_some_and(|(source, _, _)| source == &shape.source))
                .then_some(*id)
            })
            .collect::<HashSet<_>>();
        let mut named = HashMap::<_, Vec<FunctionId>>::new();
        let mut members = HashMap::<_, Vec<FunctionId>>::new();
        for (id, function) in hir.functions.iter() {
            named
                .entry((function.module, function.name.as_str()))
                .or_default()
                .push(id);
            let member = function
                .name
                .rsplit('.')
                .next()
                .unwrap()
                .split('$')
                .next()
                .unwrap();
            members.entry(member).or_default().push(id);
        }
        let mut dependencies = HashMap::<FunctionId, HashSet<FunctionId>>::new();
        for (id, expression) in hir.expressions.iter() {
            let Some(owner) = hir.expression_functions.get(&id) else {
                continue;
            };
            let targets = dependencies.entry(*owner).or_default();
            match expression {
                hir::Expr::Name(ResolvedName::Function(target)) => {
                    let definition = &hir.functions[*target];
                    targets.extend(
                        named[&(definition.module, definition.name.as_str())]
                            .iter()
                            .copied(),
                    );
                }
                hir::Expr::Closure { function, .. } => {
                    targets.insert(*function);
                }
                hir::Expr::Member { name, .. } => {
                    targets.extend(members.get(name.as_str()).into_iter().flatten().copied());
                }
                _ => {}
            }
        }
        for (id, targets) in &dependencies {
            self.shapes.get_mut(id).unwrap().dependencies = targets.clone();
        }
        let mut callers = HashMap::<_, Vec<_>>::new();
        for (owner, targets) in &dependencies {
            for target in targets {
                callers.entry(*target).or_default().push(*owner);
            }
        }
        let mut pending = dirty
            .iter()
            .copied()
            .collect::<std::collections::VecDeque<_>>();
        while let Some(target) = pending.pop_front() {
            for owner in callers.get(&target).into_iter().flatten() {
                if dirty.insert(*owner) {
                    pending.push_back(*owner);
                }
            }
        }
        for (id, function) in hir.functions.iter_mut() {
            if !function.effects_explicit
                && !dirty.contains(&id)
                && let Some((_, effects, suspends)) = self.contracts.get(&self.shapes[&id].key)
            {
                function.effects = effects.clone();
                function.suspends = *suspends;
            }
        }
        self.stats.pipeline_runs += 1;
    }

    pub(crate) fn complete(&mut self, hir: &hir::PackageHir) {
        for (id, function) in hir.functions.iter() {
            let shape = &self.shapes[&id];
            if !shape.source.is_empty() && !function.body.is_empty() {
                self.contracts.insert(
                    shape.key.clone(),
                    (
                        shape.source.clone(),
                        function.effects.clone(),
                        function.suspends,
                    ),
                );
            }
        }
    }
}

fn declaration_key(hir: &hir::PackageHir) -> String {
    // Include arena order as well as stable names: cached nominal/callee IDs may be reused only
    // when declarations have the same identities. Spans and documentation are presentation data.
    struct DeclarationText(String);
    impl DeclarationText {
        fn push(&mut self, value: std::fmt::Arguments<'_>) {
            use std::fmt::Write;
            self.0.write_fmt(value).unwrap();
            self.0.push('\n');
        }
    }
    let mut parts = DeclarationText(String::new());
    for (_, module) in hir.modules.iter() {
        parts.push(format_args!("{:?}", (&module.name, &module.imports)));
    }
    for (_, record) in hir.records.iter() {
        let mut value = record.clone();
        value.span = 0..0;
        value.documentation = None;
        clean_methods(&mut value.methods);
        parts.push(format_args!("{value:?}"));
    }
    for (_, variant) in hir.variant_types.iter() {
        let mut value = variant.clone();
        value.span = 0..0;
        value.documentation = None;
        clean_methods(&mut value.methods);
        parts.push(format_args!("{value:?}"));
    }
    for (_, variant) in hir.variants.iter() {
        let mut value = variant.clone();
        value.span = 0..0;
        parts.push(format_args!("{value:?}"));
    }
    for (_, constant) in hir.constants.iter() {
        parts.push(format_args!(
            "{:?}",
            (
                &constant.name,
                constant.module,
                constant.public,
                &constant.value
            )
        ));
    }
    for (_, function) in hir.functions.iter() {
        parts.push(format_args!(
            "{:?}",
            (
                &function.name,
                function.module,
                &function.owner,
                function.receiver.is_some(),
                function.public,
                &function.intrinsic,
                &function.type_parameters,
                &function.constraints,
                &function.groups,
                function
                    .parameters
                    .iter()
                    .map(|p| &p.ty)
                    .collect::<Vec<_>>(),
                &function.return_type
            )
        ));
        if function.effects_explicit {
            parts.push(format_args!("{:?}", (&function.effects, function.suspends)));
        }
    }
    parts.0
}

fn clean_methods(methods: &mut [crate::ast::MethodRequirement]) {
    for method in methods {
        method.span = 0..0;
        method.documentation = None;
        for parameter in &mut method.parameters {
            parameter.span = 0..0;
            parameter.type_span = None;
        }
    }
}

impl Checker<'_> {
    pub(super) fn cached_failure(
        &self,
        function: FunctionId,
        input: &Option<Signature>,
    ) -> Option<FosterError> {
        let cache = self.body_cache.as_ref()?.borrow();
        let shape = &cache.shapes[&function];
        if !shape.eligible {
            return None;
        }
        let failure = cache.failures.get(&shape.key)?;
        if Some(&failure.input) != input.as_ref()
            || failure.source != shape.raw_source
            || failure
                .dependencies
                .iter()
                .any(|(id, contract)| self.body_contract(*id).as_ref() != Some(contract))
        {
            return None;
        }
        let mut error = failure.error.clone();
        for label in &mut error.labels {
            label.range.start += shape.start;
            label.range.end += shape.start;
        }
        Some(error)
    }

    pub(super) fn record_failure(
        &self,
        function: FunctionId,
        input: Option<Signature>,
        error: &FosterError,
    ) {
        let Some(cache) = &self.body_cache else {
            return;
        };
        let shape = cache.borrow().shapes[&function].clone();
        *cache
            .borrow_mut()
            .stats
            .checked
            .entry(shape.key.clone())
            .or_default() += 1;
        let Some(input) = input else { return };
        let definition = &self.hir.functions[function];
        if definition.return_type.is_none()
            || !definition.parameters.iter().all(|p| p.ty.is_some())
            || !shape.eligible
            || !self.body_cacheable
            || error.labels.is_empty()
            || error.source_module.as_deref()
                != Some(
                    self.hir.modules[self.hir.functions[function].module]
                        .name
                        .as_str(),
                )
            || error.labels.iter().any(|label| {
                label.range.start < shape.start
                    || label.range.end > shape.start + shape.raw_source.len()
            })
        {
            return;
        }
        let Some(dependencies) = shape
            .dependencies
            .iter()
            .map(|id| self.body_contract(*id).map(|contract| (*id, contract)))
            .collect::<Option<Vec<_>>>()
        else {
            return;
        };
        let mut error = error.clone();
        error.line = 0;
        error.column = 0;
        for label in &mut error.labels {
            label.range.start -= shape.start;
            label.range.end -= shape.start;
        }
        cache.borrow_mut().failures.insert(
            shape.key,
            BodyFailure {
                source: shape.raw_source,
                input,
                dependencies,
                error,
            },
        );
    }

    pub(super) fn record_effect_summaries(&self) {
        let Some(cache) = &self.body_cache else {
            return;
        };
        let mut cache = cache.borrow_mut();
        for (function, row) in &self.derived_effects {
            let shape = &cache.shapes[function];
            let key = shape.key.clone();
            let Some(entry) = cache.entries.get(&key) else {
                continue;
            };
            if entry.source != shape.source
                || self.body_input(*function).as_ref() != Some(&entry.input)
                || entry
                    .dependencies
                    .iter()
                    .any(|(id, contract)| self.body_contract(*id).as_ref() != Some(contract))
            {
                continue;
            }
            let summary = effect_worklist::EffectSummary {
                row: row.clone(),
                dependencies: self.effect_dependencies[function].clone(),
            };
            cache.entries.get_mut(&key).unwrap().effect_summary = Some(summary);
        }
    }

    pub(super) fn body_input(&self, function: FunctionId) -> Option<Signature> {
        let signature = &self.functions[&function];
        let parameters = signature
            .parameters
            .iter()
            .cloned()
            .map(|ty| ty.map(|ty| self.resolved(ty)))
            .collect::<Vec<_>>();
        let result = self.resolved(signature.result.clone());
        if parameters.iter().any(|p| contains_variable(&p.ty)) || contains_variable(&result) {
            return None;
        }
        Some(Signature { parameters, result })
    }

    fn body_contract(&self, function: FunctionId) -> Option<Contract> {
        Some(Contract {
            signature: self.body_input(function)?,
            effects: self.hir.functions[function].effects.clone(),
            suspends: self.hir.functions[function].suspends,
        })
    }

    pub(super) fn reuse_body(&mut self, function: FunctionId, input: &Option<Signature>) -> bool {
        let miss = |reason| {
            if !self.hir.functions[function].body.is_empty() {
                crate::compiler::profile::count(reason);
            }
            false
        };
        let Some(cache) = self.body_cache.clone() else {
            return miss("body.miss.no_session");
        };
        let (shape, result) = {
            let cache = cache.borrow();
            let Some(shape) = cache.shapes.get(&function).filter(|shape| shape.eligible) else {
                return miss("body.miss.ineligible");
            };
            let Some(result) = cache.entries.get(&shape.key) else {
                return miss("body.miss.no_entry");
            };
            if Some(&result.input) != input.as_ref() {
                return miss("body.miss.signature");
            }
            if result.source != shape.source
                || result.locals.len() != shape.locals.len()
                || result.expressions.len() != shape.expressions.len()
            {
                return miss("body.miss.source_or_shape");
            }
            if result
                .dependencies
                .iter()
                .any(|(id, contract)| self.body_contract(*id).as_ref() != Some(contract))
            {
                return miss("body.miss.dependency");
            }
            (shape.clone(), result.clone())
        };
        if let Some(summary) = result.effect_summary {
            self.effect_seeds.insert(function, summary);
        }
        for (id, value) in shape.locals.iter().zip(result.locals) {
            if let Some((ty, group)) = value {
                self.locals.insert(*id, ty);
                if let Some(group) = group {
                    self.local_groups.insert(*id, group);
                }
            }
        }
        for (id, value) in shape.expressions.iter().zip(result.expressions) {
            if let Some(ty) = value {
                self.expressions.insert(*id, ty);
            }
        }
        for index in result.promotions {
            self.integer_promotions.insert(shape.expressions[index]);
        }
        for (index, kind) in result.members {
            self.member_kinds.insert(shape.expressions[index], kind);
        }
        for index in result.bare_methods {
            self.bare_method_members.insert(shape.expressions[index]);
        }
        for (index, mut call, key) in result.calls {
            if let (ResolvedCall::ContractMethod { slot, .. }, Some(key)) = (&mut call, key) {
                *slot = self.dispatch_slot(key);
            }
            self.resolved_calls.insert(shape.expressions[index], call);
        }
        *cache
            .borrow_mut()
            .stats
            .reused
            .entry(shape.key)
            .or_default() += 1;
        true
    }

    pub(super) fn record_body(
        &mut self,
        function: FunctionId,
        input: Option<Signature>,
        constraints_before: usize,
    ) {
        let Some(cache) = self.body_cache.clone() else {
            return;
        };
        if self.hir.functions[function].body.is_empty() {
            return;
        }
        let shape = cache.borrow().shapes[&function].clone();
        *cache
            .borrow_mut()
            .stats
            .checked
            .entry(shape.key.clone())
            .or_default() += 1;
        let Some(input) = input else {
            crate::compiler::profile::count("body.store_skip.unresolved_input");
            return;
        };
        if !shape.eligible
            || !self.body_cacheable
            || self.member_constraints.len() != constraints_before
        {
            crate::compiler::profile::count("body.store_skip.ineligible_or_shared_constraints");
            return;
        }
        let mut dependencies = shape.dependencies.clone();
        let mut expressions = Vec::new();
        let mut calls = Vec::new();
        let mut promotions = Vec::new();
        let mut members = Vec::new();
        let mut bare_methods = Vec::new();
        for (index, id) in shape.expressions.iter().enumerate() {
            if let hir::Expr::Name(ResolvedName::Function(target)) = self.hir.expressions[*id] {
                dependencies.insert(target);
            }
            let ty = self
                .expressions
                .get(id)
                .cloned()
                .map(|ty| self.resolved(ty));
            if ty.as_ref().is_some_and(contains_variable) {
                return;
            }
            expressions.push(ty);
            if self.integer_promotions.contains(id) {
                promotions.push(index);
            }
            if let Some(kind) = self.member_kinds.get(id) {
                members.push((index, *kind));
            }
            if self.bare_method_members.contains(id) {
                bare_methods.push(index);
            }
            if let Some(call) = self.resolved_calls.get(id) {
                if let Some(target) = call.function() {
                    dependencies.insert(target);
                }
                let key = match call {
                    ResolvedCall::ContractMethod { slot, .. } => {
                        self.dispatch_keys.get(slot.0 as usize).cloned()
                    }
                    _ => None,
                };
                calls.push((index, call.clone(), key));
            }
        }
        let mut locals = Vec::new();
        for id in &shape.locals {
            let ty = self.locals.get(id).cloned().map(|ty| self.resolved(ty));
            if ty.as_ref().is_some_and(contains_variable) {
                return;
            }
            locals.push(ty.map(|ty| (ty, self.local_groups.get(id).cloned())));
        }
        let Some(dependencies) = dependencies
            .into_iter()
            .map(|id| self.body_contract(id).map(|contract| (id, contract)))
            .collect::<Option<Vec<_>>>()
        else {
            return;
        };
        cache.borrow_mut().entries.insert(
            shape.key,
            BodyResult {
                effect_summary: None,
                source: shape.source,
                input,
                dependencies,
                locals,
                expressions,
                calls,
                promotions,
                members,
                bare_methods,
            },
        );
    }
}
