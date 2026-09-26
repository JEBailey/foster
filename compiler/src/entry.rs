//! Command-entry conventions shared by the VM and native backend.

use crate::error::FosterError;
use crate::hir::{FunctionId, PackageHir};
use crate::types::{Type, TypeInformation};

pub use foster_bytecode::entry::{ARGUMENTS_MODULE, ARGUMENTS_TYPE};

pub use foster_bytecode::entry::CommandArguments;

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
        [parameter] if is_arguments_type(hir, types, parameter.ty) => Ok(true),
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
