use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::process::ExitCode;

#[derive(Debug)]
struct Reported;

impl fmt::Display for Reported {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "compilation failed")
    }
}

impl Error for Reported {}

fn main() -> ExitCode {
    match execute() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if !error.is::<Reported>() {
                eprintln!("error: {error}");
            }
            ExitCode::FAILURE
        }
    }
}

fn execute() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let command = args
        .next()
        .ok_or("usage: foster <run|check> <file> | foster lsp")?;
    if command == "lsp" {
        if args.next().is_some() {
            return Err("usage: foster lsp".into());
        }
        return foster::lsp::run();
    }
    let path = args.next().ok_or(
        "usage: foster run <file-or-directory> [--optimize|--no-optimize] | foster check <file-or-directory> | foster lsp",
    )?;
    let flags = args.collect::<Vec<_>>();
    let path = std::path::Path::new(&path);
    match command.as_str() {
        "check" => {
            if !flags.is_empty() {
                return Err("`check` does not accept optimization flags".into());
            }
            if path.is_dir() {
                let compilation = foster::check_package(path)?;
                report_warnings(&compilation, None, None)?;
                println!(
                    "ok: checked {} module(s) ({} implicit)",
                    compilation.package.modules.len(),
                    compilation.package.implicit_module_count()
                );
            } else {
                let source = fs::read_to_string(path)?;
                let program = parse_file(path, &source)?;
                let compilation = foster::hir::Compilation::new(
                    foster::package::Package::from_program_with_core("main", program.clone())?,
                )?;
                report_warnings(&compilation, Some(path), Some(&source))?;
                println!("ok: checked {} function(s)", program.functions.len());
            }
        }
        "run" => {
            let optimize = match flags.as_slice() {
                [] => true,
                [flag] if flag == "--optimize" => true,
                [flag] if flag == "--no-optimize" => false,
                [flag] => return Err(format!("unknown run flag `{flag}`").into()),
                _ => return Err("`run` accepts at most one optimization flag".into()),
            };
            let options = foster::vm::CompileOptions { optimize };
            let value = if path.is_dir() {
                let compilation = foster::check_package(path)?;
                report_warnings(&compilation, None, None)?;
                foster::vm::run_with_options(&compilation, options)?
            } else {
                let source = fs::read_to_string(path)?;
                let program = parse_file(path, &source)?;
                let compilation = foster::hir::Compilation::new(
                    foster::package::Package::from_program_with_core("main", program)?,
                )?;
                report_warnings(&compilation, Some(path), Some(&source))?;
                foster::vm::run_with_options(&compilation, options)?
            };
            if value != foster::vm::Value::Unit {
                println!("{value}");
            }
        }
        _ => {
            return Err(
                format!("unknown command `{command}`; expected `run`, `check`, or `lsp`").into(),
            );
        }
    }
    Ok(())
}

fn report_warnings(
    compilation: &foster::hir::Compilation,
    path: Option<&std::path::Path>,
    source: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    for diagnostic in &compilation.diagnostics {
        if let (Some(path), Some(source)) = (path, source) {
            if diagnostic
                .source_module
                .as_deref()
                .is_none_or(|module| module == "main")
            {
                foster::diagnostic::eprint(&path.to_string_lossy(), source, diagnostic)?;
            } else {
                let code = diagnostic.code.as_deref().unwrap_or("warning");
                eprintln!("warning[{code}]: {}", diagnostic.message);
            }
        } else if let Some(module) = diagnostic
            .source_module
            .as_deref()
            .and_then(|module| compilation.package.module(module))
            && let Some(source_path) = &module.source_path
        {
            let source = fs::read_to_string(source_path)?;
            foster::diagnostic::eprint(source_path.as_str(), &source, diagnostic)?;
        } else {
            let code = diagnostic.code.as_deref().unwrap_or("warning");
            eprintln!("warning[{code}]: {}", diagnostic.message);
        }
    }
    Ok(())
}

fn parse_file(
    path: &std::path::Path,
    source: &str,
) -> Result<foster::ast::Program, Box<dyn Error>> {
    foster::parse(source).map_err(|error| {
        let diagnostic = foster::diagnostic::Diagnostic::from_source_error(source, &error);
        let name = path.to_string_lossy();
        if let Err(render_error) = foster::diagnostic::eprint(&name, source, &diagnostic) {
            eprintln!("error: could not render diagnostic: {render_error}");
        }
        Box::new(Reported) as Box<dyn Error>
    })
}
