use crate::compiler::Compilation;
use crate::error::CompileError;
use crate::hir::PackageHir;
use crate::package::Package;

pub(super) fn check(package: Package) -> Result<Compilation, CompileError> {
    let mut hir = PackageHir::lower(&package).map_err(CompileError::lowering)?;
    crate::hir::ownership::infer_ref_capture_effects(&mut hir);
    crate::hir::ownership::validate_groups_and_effects(&hir).map_err(CompileError::effects)?;
    let (initial_types, _) = crate::typecheck::check(&mut hir).map_err(CompileError::types)?;
    crate::hir::ownership::infer_capture_modes(&mut hir, &initial_types)
        .map_err(CompileError::ownership)?;
    let (types, diagnostics) = crate::typecheck::check(&mut hir).map_err(CompileError::types)?;
    crate::hir::ownership::validate_groups_and_effects(&hir).map_err(CompileError::effects)?;
    crate::hir::ownership::check_closure_ownership(&hir).map_err(CompileError::ownership)?;
    let ownership =
        crate::ownership::build_and_check(&hir, &types).map_err(CompileError::ownership)?;
    let compilation = Compilation {
        package,
        hir,
        types,
        diagnostics,
        ownership,
    };
    if let Some(main) = compilation
        .hir
        .module_named("main")
        .and_then(|module| compilation.hir.function_named(module, "main"))
    {
        crate::entry::accepts_arguments(&compilation.hir, &compilation.types, main)
            .map_err(CompileError::types)?;
    }
    Ok(compilation)
}
