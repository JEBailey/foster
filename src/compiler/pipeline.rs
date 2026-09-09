use crate::compiler::Compilation;
use crate::error::CompileError;
use crate::hir::PackageHir;
use crate::package::Package;

pub(super) fn check(package: Package) -> Result<Compilation, CompileError> {
    check_impl(package, false, None).map_err(|errors| errors.into_iter().next().unwrap())
}

pub(super) fn check_collecting(
    package: Package,
    cache: Option<crate::typecheck::incremental::SharedBodyCache>,
) -> Result<Compilation, Vec<CompileError>> {
    check_impl(package, true, cache)
}

fn check_impl(
    package: Package,
    recover: bool,
    cache: Option<crate::typecheck::incremental::SharedBodyCache>,
) -> Result<Compilation, Vec<CompileError>> {
    fn types(
        hir: &mut PackageHir,
        recover: bool,
        recoverable: &std::collections::HashSet<crate::hir::ModuleId>,
        cache: Option<crate::typecheck::incremental::SharedBodyCache>,
    ) -> Result<
        (
            crate::types::TypeInformation,
            Vec<crate::diagnostic::Diagnostic>,
        ),
        Vec<CompileError>,
    > {
        if recover {
            crate::typecheck::check_collecting(hir, recoverable, cache)
                .map_err(|errors| errors.into_iter().map(CompileError::types).collect())
        } else {
            crate::typecheck::check(hir).map_err(|error| vec![CompileError::types(error)])
        }
    }
    crate::compiler::cancellation::check().map_err(|e| vec![CompileError::lowering(e)])?;
    let mut hir = PackageHir::lower(&package).map_err(|e| vec![CompileError::lowering(e)])?;
    if let Some(cache) = &cache {
        cache.borrow_mut().prepare(&mut hir, &package);
    }
    let recoverable = hir
        .modules
        .iter()
        .filter_map(|(id, module)| {
            package
                .module(&module.name)
                .filter(|module| module.origin == crate::package::ModuleOrigin::Input)
                .map(|_| id)
        })
        .collect();
    crate::hir::ownership::infer_ref_capture_effects(&mut hir);
    crate::hir::ownership::validate_groups_and_effects(&hir)
        .map_err(|e| vec![CompileError::effects(e)])?;
    let (initial_types, _) = types(&mut hir, recover, &recoverable, cache.clone())?;
    crate::compiler::cancellation::check().map_err(|e| vec![CompileError::types(e)])?;
    crate::hir::ownership::infer_capture_modes(&mut hir, &initial_types)
        .map_err(|e| vec![CompileError::ownership(e)])?;
    let (types, diagnostics) = types(&mut hir, recover, &recoverable, cache.clone())?;
    crate::compiler::cancellation::check().map_err(|e| vec![CompileError::types(e)])?;
    crate::hir::ownership::validate_groups_and_effects(&hir)
        .map_err(|e| vec![CompileError::effects(e)])?;
    crate::hir::ownership::check_closure_ownership(&hir)
        .map_err(|e| vec![CompileError::ownership(e)])?;
    let ownership = if recover {
        crate::ownership::build_and_check_collecting(&hir, &types, true).map_err(|errors| {
            errors
                .into_iter()
                .map(CompileError::ownership)
                .collect::<Vec<_>>()
        })?
    } else {
        crate::ownership::build_and_check(&hir, &types)
            .map_err(|e| vec![CompileError::ownership(e)])?
    };
    if let Some(cache) = &cache {
        cache.borrow_mut().complete(&hir);
    }
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
            .map_err(|e| vec![CompileError::types(e)])?;
    }
    Ok(compilation)
}
