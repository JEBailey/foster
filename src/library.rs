//! Independently compiled libraries: declaration interfaces plus portable generic code.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;

use crate::{
    ast,
    compiler::Compilation,
    error::FosterError,
    hir,
    package::{ModuleOrigin, Package},
    symbols, vm,
};
use serde::{Deserialize, Serialize};

mod dispatch;
mod interface;
mod linking;

const MAGIC: &[u8; 8] = b"FOSTERLB";
pub const FORMAT_VERSION: u16 = 1;
const MAX_SECTION: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Library {
    pub interface: Interface,
    pub code: vm::Program,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interface {
    pub language_version: u16,
    pub ownership_version: u16,
    pub package: String,
    pub modules: Vec<Module>,
    pub embedded: Vec<String>,
    pub slots: Vec<Slot>,
    pub contexts: BTreeMap<u32, FunctionContext>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionContext {
    pub composition_owner: Option<String>,
    pub dispatch: bool,
    pub registrations: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Module {
    pub path: String,
    pub identity: symbols::ModuleName,
    pub declarations: ast::Program,
    /// One binding per declaration, in exactly the same order.
    pub functions: Vec<symbols::Definition>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MethodKey {
    pub name: String,
    pub parameters: Vec<symbols::Parameter>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Slot {
    pub id: u32,
    pub key: MethodKey,
}

#[derive(Debug, Clone)]
pub struct ExternalFunction {
    pub library: usize,
    pub definition: symbols::Definition,
    pub context: FunctionContext,
}

fn error(message: impl Into<String>) -> FosterError {
    FosterError::runtime(format!("compiled library: {}", message.into()))
}

pub fn build(compilation: &Compilation) -> Result<Library, FosterError> {
    let code = vm::compile_library(compilation)?;
    let interface = interface::build(compilation, &code)?;
    let library = Library { interface, code };
    library.validate()?;
    Ok(library)
}

impl Library {
    pub fn validate(&self) -> Result<(), FosterError> {
        if self.interface.language_version != crate::ownership::LANGUAGE_VERSION
            || self.interface.ownership_version != crate::ownership::MODEL_VERSION
        {
            return Err(error("incompatible language or ownership model version"));
        }
        if self.interface.package.is_empty() || self.code.drops_inserted {
            return Err(error("invalid package identity or finalized library code"));
        }
        vm::verify(&self.code)?;
        self.code.symbols.validate(&self.code)?;
        let definitions = self
            .code
            .symbols
            .modules
            .iter()
            .flat_map(|m| &m.definitions)
            .map(|d| (d.function, d))
            .collect::<BTreeMap<_, _>>();
        if definitions.len() != self.code.functions.len() || self.code.main.is_some() {
            return Err(error(
                "incomplete function bindings or executable entry point",
            ));
        }
        let mut names = self
            .code
            .symbols
            .modules
            .iter()
            .map(|m| (m.name.clone(), m.name.path.clone()))
            .collect::<BTreeMap<_, _>>();
        for module in &self.interface.modules {
            names.insert(module.identity.clone(), module.path.clone());
        }
        let type_ids = self
            .code
            .symbols
            .modules
            .iter()
            .flat_map(|m| &m.types)
            .map(|t| (t.variant, t.id))
            .collect::<BTreeSet<_>>();
        if self
            .code
            .records
            .keys()
            .any(|id| !type_ids.contains(&(false, id.into_raw().into_u32())))
            || self
                .code
                .variants
                .values()
                .any(|v| !type_ids.contains(&(true, v.parent.into_raw().into_u32())))
        {
            return Err(error("incomplete nominal type bindings"));
        }
        let mut slots = BTreeSet::new();
        for slot in &self.interface.slots {
            if slot.id >= u32::MAX - 2 || !slots.insert(slot.id) {
                return Err(error("invalid dispatch slot table"));
            }
        }
        for reserved in [u32::MAX, u32::MAX - 1, u32::MAX - 2] {
            slots.insert(reserved);
        }
        let missing_dispatch = self
            .code
            .dispatch
            .keys()
            .any(|(_, slot)| !slots.contains(&slot.0));
        let missing_call = self.code.functions.values().flat_map(|function| &function.instructions).any(|instruction| {
            matches!(instruction, vm::Instruction::CallContractMethod { slot, .. } if !slots.contains(&slot.0))
        });
        if missing_dispatch || missing_call {
            return Err(error("missing dispatch slot binding"));
        }
        let mut modules = BTreeSet::new();
        let mut bindings = BTreeSet::new();
        for module in &self.interface.modules {
            if !modules.insert(&module.path)
                || module.declarations.functions.len() != module.functions.len()
                || !module.declarations.tests.is_empty()
            {
                return Err(error("duplicate module or inconsistent declaration table"));
            }
            for (declaration, binding) in
                module.declarations.functions.iter().zip(&module.functions)
            {
                interface::validate_function(module, declaration, binding, &names)?;
                if !declaration.body.is_empty()
                    || !declaration.body_is_recovery_stub
                    || !declaration.effects_explicit
                    || declaration.return_type.is_none()
                    || declaration.parameters.iter().any(|p| p.ty.is_none())
                    || definition_mismatch(definitions.get(&binding.function).copied(), binding)
                    || binding.symbol.name.module != module.identity
                    || !bindings.insert(binding.function)
                {
                    return Err(error("invalid compiled function interface"));
                }
            }
        }
        if definitions
            .values()
            .any(|d| d.symbol.name.module.package != "foster" && !bindings.contains(&d.function))
        {
            return Err(error("missing library function declaration"));
        }
        if self.interface.contexts.len() != bindings.len() {
            return Err(error("incomplete function declaration contexts"));
        }
        for (function, context) in &self.interface.contexts {
            if !bindings.contains(function)
                || context
                    .composition_owner
                    .as_ref()
                    .is_some_and(|m| !modules.contains(m))
                || context
                    .registrations
                    .iter()
                    .any(|(m, _)| !modules.contains(m))
            {
                return Err(error("invalid function declaration context"));
            }
        }
        Ok(())
    }
}

fn definition_mismatch(
    actual: Option<&symbols::Definition>,
    expected: &symbols::Definition,
) -> bool {
    actual != Some(expected)
}

pub fn encode(library: &Library) -> Result<Vec<u8>, FosterError> {
    library.validate()?;
    let interface = serde_json::to_vec(&library.interface).map_err(|e| error(e.to_string()))?;
    let code = vm::encode_program(&library.code).map_err(|e| error(e.to_string()))?;
    if interface.len() > MAX_SECTION || code.len() > MAX_SECTION {
        return Err(error("library exceeds size limit"));
    }
    let mut bytes = MAGIC.to_vec();
    bytes.extend(FORMAT_VERSION.to_le_bytes());
    bytes.extend((interface.len() as u32).to_le_bytes());
    bytes.extend(interface);
    bytes.extend((code.len() as u32).to_le_bytes());
    bytes.extend(code);
    Ok(bytes)
}

pub fn decode(bytes: &[u8]) -> Result<Library, FosterError> {
    if bytes.get(..8) != Some(MAGIC.as_slice())
        || bytes.get(8..10) != Some(&FORMAT_VERSION.to_le_bytes())
    {
        return Err(error("invalid header or unsupported library version"));
    }
    let mut offset = 10;
    let mut section = || -> Result<&[u8], FosterError> {
        let size = bytes
            .get(offset..offset + 4)
            .ok_or_else(|| error("truncated section length"))?;
        let size = u32::from_le_bytes(size.try_into().unwrap()) as usize;
        offset += 4;
        if size > MAX_SECTION {
            return Err(error("library section exceeds size limit"));
        }
        let section = bytes
            .get(offset..offset + size)
            .ok_or_else(|| error("truncated library section"))?;
        offset += size;
        Ok(section)
    };
    let interface =
        serde_json::from_slice(section()?).map_err(|e| error(format!("invalid interface: {e}")))?;
    let code = vm::decode_program(section()?).map_err(|e| error(format!("invalid code: {e}")))?;
    if offset != bytes.len() {
        return Err(error("trailing library data"));
    }
    let library = Library { interface, code };
    library.validate()?;
    Ok(library)
}

pub fn read(path: impl AsRef<Path>) -> Result<Library, FosterError> {
    let path = path.as_ref();
    let metadata = std::fs::metadata(path)
        .map_err(|e| error(format!("cannot read {}: {e}", path.display())))?;
    if metadata.len() > (2 * MAX_SECTION + 18) as u64 {
        return Err(error("library exceeds size limit"));
    }
    decode(&std::fs::read(path).map_err(|e| error(e.to_string()))?)
}

pub(crate) use interface::mount;
pub(crate) use linking::link;
