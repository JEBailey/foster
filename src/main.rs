use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
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
        .ok_or("usage: foster <run|build|check|docs|serve-docs> [path] | foster lsp")?;
    if command == "lsp" {
        if args.next().is_some() {
            return Err("usage: foster lsp".into());
        }
        return foster::lsp::run();
    }
    if command == "docs" {
        return docs(args.collect());
    }
    if command == "serve-docs" {
        return serve_docs(args.collect());
    }
    let path = args.next().ok_or(
        "usage: foster run <file-or-directory> [--optimize|--no-optimize] | foster check <file-or-directory> | foster lsp",
    )?;
    let flags = args.collect::<Vec<_>>();
    let path = Path::new(&path);
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
        "build" => {
            let (optimize, output) = parse_build_flags(&flags, path)?;
            let compilation = compile_path(path)?;
            report_warnings(&compilation, None, None)?;
            let program = foster::vm::compile_with_options(
                &compilation,
                foster::vm::CompileOptions { optimize },
            )?;
            let bytes = foster::vm::encode_program(&program)?;
            fs::write(&output, bytes)?;
            println!("built {}", output.display());
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
            let value = if path.extension().is_some_and(|extension| extension == "fbc") {
                let bytes = fs::read(path)?;
                let program = foster::vm::decode_program(&bytes)?;
                foster::vm::Machine::new(&program).run_main()?
            } else if path.is_dir() {
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
                format!(
                    "unknown command `{command}`; expected `run`, `build`, `check`, `docs`, `serve-docs`, or `lsp`"
                )
                .into(),
            );
        }
    }
    Ok(())
}

fn parse_build_flags(flags: &[String], source: &Path) -> Result<(bool, PathBuf), Box<dyn Error>> {
    let mut optimize = true;
    let mut output = None;
    let mut index = 0;
    while index < flags.len() {
        match flags[index].as_str() {
            "--optimize" => optimize = true,
            "--no-optimize" => optimize = false,
            "--output" | "-o" => {
                index += 1;
                output = Some(PathBuf::from(
                    flags.get(index).ok_or("`--output` requires a path")?,
                ));
            }
            flag => return Err(format!("unknown build flag `{flag}`").into()),
        }
        index += 1;
    }
    let output = output.unwrap_or_else(|| {
        if source.is_dir() {
            source.join("main.fbc")
        } else {
            source.with_extension("fbc")
        }
    });
    Ok((optimize, output))
}

fn docs(args: Vec<String>) -> Result<(), Box<dyn Error>> {
    let mut source = None;
    let mut output = None;
    let mut serve = false;
    let mut open_browser = true;
    let mut port = 8000;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--output" => {
                index += 1;
                output = Some(PathBuf::from(
                    args.get(index).ok_or("`--output` requires a path")?,
                ));
            }
            "--serve" => serve = true,
            "--no-open" => open_browser = false,
            "--port" => {
                index += 1;
                port = parse_port(args.get(index).ok_or("`--port` requires a number")?)?;
            }
            flag if flag.starts_with('-') => {
                return Err(format!("unknown docs flag `{flag}`").into());
            }
            path if source.is_none() => source = Some(PathBuf::from(path)),
            path => return Err(format!("unexpected docs argument `{path}`").into()),
        }
        index += 1;
    }
    if !serve && (!open_browser || port != 8000) {
        return Err("`--port` and `--no-open` require `--serve`".into());
    }

    let source = source.unwrap_or_else(|| PathBuf::from("."));
    let compilation = compile_path(&source)?;
    report_warnings(&compilation, None, None)?;
    let output = output.unwrap_or_else(|| default_documentation_directory(&source));
    let report = foster::documentation::generate(&compilation, &output)?;
    println!(
        "generated {} declaration(s) in {} module(s) at {}",
        report.declarations,
        report.modules,
        report.output.display()
    );
    if serve {
        foster::documentation::serve(
            &output,
            foster::documentation::ServeOptions { port, open_browser },
        )?;
    }
    Ok(())
}

fn serve_docs(args: Vec<String>) -> Result<(), Box<dyn Error>> {
    let mut directory = None;
    let mut open_browser = true;
    let mut port = 8000;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--no-open" => open_browser = false,
            "--port" => {
                index += 1;
                port = parse_port(args.get(index).ok_or("`--port` requires a number")?)?;
            }
            flag if flag.starts_with('-') => {
                return Err(format!("unknown serve-docs flag `{flag}`").into());
            }
            path if directory.is_none() => directory = Some(PathBuf::from(path)),
            path => return Err(format!("unexpected serve-docs argument `{path}`").into()),
        }
        index += 1;
    }
    foster::documentation::serve(
        directory.unwrap_or_else(|| PathBuf::from("documentation")),
        foster::documentation::ServeOptions { port, open_browser },
    )?;
    Ok(())
}

fn parse_port(value: &str) -> Result<u16, Box<dyn Error>> {
    value
        .parse::<u16>()
        .map_err(|_| format!("invalid TCP port `{value}`").into())
}

fn default_documentation_directory(source: &Path) -> PathBuf {
    if source.is_dir() {
        source.join("documentation")
    } else {
        source
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("documentation")
    }
}

fn compile_path(path: &Path) -> Result<foster::hir::Compilation, Box<dyn Error>> {
    if path.is_dir() {
        return Ok(foster::check_package(path)?);
    }
    let source = fs::read_to_string(path)?;
    let program = parse_file(path, &source)?;
    Ok(foster::hir::Compilation::new(
        foster::package::Package::from_program_with_core("main", program)?,
    )?)
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
