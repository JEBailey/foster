use std::collections::HashSet;

use super::*;

impl Checker<'_> {
    pub(super) fn declare_constants(&mut self) -> Result<(), FosterError> {
        let constants = self
            .hir
            .constants
            .iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        let mut visiting = HashSet::new();
        for constant in constants {
            self.constant_type(constant, &mut visiting)?;
        }
        Ok(())
    }

    fn constant_type(
        &mut self,
        constant: ConstantId,
        visiting: &mut HashSet<ConstantId>,
    ) -> Result<Ty, FosterError> {
        if let Some(ty) = self.constants.get(&constant) {
            return Ok(ty.clone());
        }
        if !visiting.insert(constant) {
            return Err(FosterError::runtime(format!(
                "constant `{}` has a cyclic initializer",
                self.hir.constants[constant].name
            )));
        }
        let ty = self.constant_value_type(&self.hir.constants[constant].value.clone(), visiting)?;
        visiting.remove(&constant);
        self.constants.insert(constant, ty.clone());
        Ok(ty)
    }

    fn constant_value_type(
        &mut self,
        value: &hir::ConstantValue,
        visiting: &mut HashSet<ConstantId>,
    ) -> Result<Ty, FosterError> {
        Ok(match value {
            hir::ConstantValue::Unit => Ty::Unit,
            hir::ConstantValue::Bool(_) => Ty::Bool,
            hir::ConstantValue::Integer(_) => Ty::Int,
            hir::ConstantValue::Float(_) => Ty::Float,
            hir::ConstantValue::String(_) => self.string_type(),
            hir::ConstantValue::CodePoint(_) => Ty::CodePoint,
            hir::ConstantValue::Symbol(_) => self.symbol_type(),
            hir::ConstantValue::Constant(constant) => self.constant_type(*constant, visiting)?,
            hir::ConstantValue::List(values) => {
                let Some((first, rest)) = values.split_first() else {
                    return Err(FosterError::runtime(
                        "cannot infer the element type of an empty constant list",
                    ));
                };
                let element = self.constant_value_type(first, visiting)?;
                for value in rest {
                    let found = self.constant_value_type(value, visiting)?;
                    if found != element {
                        return Err(FosterError::runtime(format!(
                            "constant list mixes `{}` and `{}` values",
                            self.describe(&element),
                            self.describe(&found)
                        )));
                    }
                }
                Ty::List(Box::new(element))
            }
        })
    }
}
