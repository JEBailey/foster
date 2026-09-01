//! Command-entry conventions shared by the VM and native backend.

use crate::error::FosterError;
use crate::hir::{FunctionId, PackageHir};
use crate::types::{Type, TypeInformation};

pub const ARGUMENTS_MODULE: &str = "std.process";
pub const ARGUMENTS_TYPE: &str = "Arguments";

/// The host command line supplied to a Foster `main` function.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandArguments {
    /// The path or command name used to invoke the Foster program.
    pub executable: String,
    /// Arguments after the executable name.
    pub values: Vec<String>,
}

impl CommandArguments {
    pub fn new(
        executable: impl Into<String>,
        values: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            executable: executable.into(),
            values: values.into_iter().map(Into::into).collect(),
        }
    }
}

/// Validate the language-level entry signature and report whether it accepts command arguments.
pub(crate) fn accepts_arguments(
    hir: &PackageHir,
    types: &TypeInformation,
    main: FunctionId,
) -> Result<bool, FosterError> {
    let definition = &hir.functions[main];
    let signature = types
        .function_type(main)
        .ok_or_else(|| FosterError::runtime("`main` is missing type information"))?;
    match signature.parameters.as_slice() {
        [] => Ok(false),
        [parameter] if is_arguments_type(hir, types, *parameter) => Ok(true),
        _ => Err(FosterError::new(
            "`main` must take no parameters or one `std.process.Arguments` parameter",
            0,
            0,
        )
        .with_code("E0901")
        .with_primary_label(definition.span.clone(), "invalid command entry signature")
        .with_help(
            "use `func main() { ... }` or import `std.process` and use \
             `func main(arguments: Arguments) { ... }`",
        )),
    }
}

pub(crate) fn is_arguments_type(
    hir: &PackageHir,
    types: &TypeInformation,
    ty: crate::types::TypeId,
) -> bool {
    let Type::Record { record, arguments } = &types.types[ty] else {
        return false;
    };
    arguments.is_empty()
        && hir.records[*record].name == ARGUMENTS_TYPE
        && hir.modules[hir.records[*record].module].name == ARGUMENTS_MODULE
}
