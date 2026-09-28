use super::*;

// Specialize rows together, rather than unioning coverage independently for each
// field: independent unions would incorrectly accept (true, true)/(false, false).
#[derive(Clone)]
enum Constructor {
    Bool(bool),
    Variant(hir::VariantId, String),
    Record(Vec<String>),
}

fn wildcard(pattern: &hir::Pattern) -> bool {
    matches!(
        pattern.unspanned(),
        hir::Pattern::Wildcard | hir::Pattern::Binding(_)
    )
}

impl Constructor {
    fn specialize(&self, pattern: &hir::Pattern, arity: usize) -> Option<Vec<hir::Pattern>> {
        if wildcard(pattern) {
            return Some(vec![hir::Pattern::Wildcard; arity]);
        }
        match (self, pattern.unspanned()) {
            (Self::Bool(a), hir::Pattern::Bool(b)) if a == b => Some(vec![]),
            (Self::Variant(a, _), hir::Pattern::Variant { variant, fields }) if a == variant => {
                Some(fields.clone())
            }
            (Self::Record(names), hir::Pattern::Record { fields }) => Some(
                names
                    .iter()
                    .map(|name| {
                        fields
                            .iter()
                            .find(|(field, _)| field == name)
                            .map(|(_, pattern)| pattern.clone())
                            .unwrap_or(hir::Pattern::Wildcard)
                    })
                    .collect(),
            ),
            _ => None,
        }
    }

    fn witness(&self, fields: &[String]) -> String {
        match self {
            Self::Bool(value) => value.to_string(),
            Self::Variant(_, name) if fields.is_empty() => name.clone(),
            Self::Variant(_, name) => format!("{name}({})", fields.join(", ")),
            Self::Record(names) => format!(
                "{{ {} }}",
                names
                    .iter()
                    .zip(fields)
                    .map(|(name, value)| format!("{name}: {value}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

impl Checker<'_> {
    pub(super) fn check_branch_coverage(
        &mut self,
        function: FunctionId,
        subject: Ty,
        arms: &[hir::BranchArm],
    ) -> Result<(), FosterError> {
        let rows = arms
            .iter()
            .filter_map(|arm| match &arm.test {
                hir::BranchTest::Pattern(pattern) => Some(vec![pattern.clone()]),
                hir::BranchTest::Wildcard => Some(vec![hir::Pattern::Wildcard]),
                _ => None,
            })
            .collect();
        // A bounded proof is conservative: running out of work never establishes
        // coverage. Wildcard rows terminate immediately, including recursive types.
        let mut budget = 100_000;
        let missing =
            self.uncovered_patterns(function, vec![subject.clone()], rows, &mut budget)?;
        if let Some(missing) = missing {
            let mut subject = self.resolved(subject);
            while let Ty::Reference(_, inner) = subject {
                subject = self.resolved(*inner);
            }
            let message = match subject {
                Ty::Variant(parent, _) => format!(
                    "non-exhaustive branch on `{}`",
                    self.hir.variant_types[parent].name
                ),
                _ => "pattern branch fails exhaustiveness checking".to_owned(),
            };
            return Err(self.error(
                function,
                format!("{message}; missing pattern: {}", missing.join(", ")),
            ));
        }
        Ok(())
    }

    fn uncovered_patterns(
        &mut self,
        function: FunctionId,
        types: Vec<Ty>,
        rows: Vec<Vec<hir::Pattern>>,
        budget: &mut usize,
    ) -> Result<Option<Vec<String>>, FosterError> {
        if rows.iter().any(|row| row.iter().all(wildcard)) {
            return Ok(None);
        }
        if rows.is_empty() {
            return Ok(Some(vec!["_".into(); types.len()]));
        }
        if *budget == 0 {
            return Err(self.error(function, "branch coverage proof is too complex; add a covering arm or simplify nested patterns"));
        }
        *budget -= 1;
        if rows.iter().all(|row| wildcard(&row[0])) {
            let tails = rows
                .into_iter()
                .map(|row| row.into_iter().skip(1).collect())
                .collect();
            return Ok(self
                .uncovered_patterns(function, types[1..].to_vec(), tails, budget)?
                .map(|mut tail| {
                    tail.insert(0, "_".into());
                    tail
                }));
        }
        let mut ty = self.resolved(types[0].clone());
        while let Ty::Reference(_, inner) = ty {
            ty = self.resolved(*inner);
        }
        let mut constructors = Vec::new();
        match ty {
            Ty::Bool => {
                constructors.push((Constructor::Bool(false), vec![]));
                constructors.push((Constructor::Bool(true), vec![]));
            }
            Ty::Variant(parent, arguments) => {
                let definition = self.hir.variant_types[parent].clone();
                let generics = definition
                    .parameters
                    .iter()
                    .cloned()
                    .zip(arguments)
                    .collect();
                for variant in definition.alternatives {
                    let case = self.hir.variants[variant].clone();
                    let payload = case
                        .payload
                        .iter()
                        .map(|payload| self.annotation_type(definition.module, payload, &generics))
                        .collect::<Result<Vec<_>, _>>()?;
                    constructors.push((
                        Constructor::Variant(variant, format!("{}.{}", definition.name, case.name)),
                        payload,
                    ));
                }
            }
            Ty::Record(record, arguments)
                if self.structural_pattern_type(&Ty::Record(record, arguments.clone())) =>
            {
                // Only inspect fields actually tested. Omitted/private fields are
                // unconstrained, and must not be exposed in missing witnesses.
                let mut names = std::collections::BTreeSet::new();
                for row in &rows {
                    if let hir::Pattern::Record { fields } = row[0].unspanned() {
                        names.extend(fields.iter().map(|(name, _)| name.clone()));
                    }
                }
                let names: Vec<_> = names.into_iter().collect();
                let fields = names
                    .iter()
                    .map(|name| self.record_field_type(function, record, &arguments, name))
                    .collect::<Result<Vec<_>, _>>()?;
                constructors.push((Constructor::Record(names), fields));
            }
            _ => {}
        }
        if constructors.is_empty() {
            // Literals cannot exhaust an open domain. Structural type tests do
            // not establish coverage either; these still require a catch-all.
            let defaults = rows
                .into_iter()
                .filter(|row| wildcard(&row[0]))
                .map(|row| row.into_iter().skip(1).collect())
                .collect();
            return Ok(self
                .uncovered_patterns(function, types[1..].to_vec(), defaults, budget)?
                .map(|mut tail| {
                    tail.insert(0, "_".into());
                    tail
                }));
        }
        for (constructor, fields) in constructors {
            let specialized = rows
                .iter()
                .filter_map(|row| {
                    let mut head = constructor.specialize(&row[0], fields.len())?;
                    head.extend_from_slice(&row[1..]);
                    Some(head)
                })
                .collect();
            let arity = fields.len();
            let mut expanded = fields;
            expanded.extend_from_slice(&types[1..]);
            if let Some(witness) =
                self.uncovered_patterns(function, expanded, specialized, budget)?
            {
                let head = constructor.witness(&witness[..arity]);
                let mut result = vec![head];
                result.extend_from_slice(&witness[arity..]);
                return Ok(Some(result));
            }
        }
        Ok(None)
    }
}
