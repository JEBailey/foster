use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};
use walkdir::WalkDir;

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

fn execute() -> Result<(), Box<dyn Error>> {
    let matches = cli().get_matches();
    match matches.subcommand() {
        Some(("lsp", _)) => return foster::lsp::run(),
        Some(("check", arguments)) => check(required_path(arguments, "path"))?,
        Some(("build", arguments)) => build(arguments)?,
        Some(("run", arguments)) => run(arguments)?,
        Some(("fmt", arguments)) => format_path(
            required_path(arguments, "path"),
            arguments.get_flag("check"),
        )?,
        Some(("docs", arguments)) => docs(arguments)?,
        Some(("serve-docs", arguments)) => serve_docs(arguments)?,
        _ => unreachable!("clap requires a recognized subcommand"),
    }
    Ok(())
}

fn cli() -> Command {
    let path = || {
        Arg::new("path")
            .value_parser(value_parser!(PathBuf))
            .required(true)
    };
    let optimizer = || {
        [
            Arg::new("optimize")
                .long("optimize")
                .action(ArgAction::SetTrue)
                .conflicts_with("no-optimize"),
            Arg::new("no-optimize")
                .long("no-optimize")
                .action(ArgAction::SetTrue),
        ]
    };
    let port = || {
        Arg::new("port")
            .long("port")
            .value_parser(value_parser!(u16))
            .default_value("8000")
    };
    let no_open = || {
        Arg::new("no-open")
            .long("no-open")
            .action(ArgAction::SetTrue)
    };

    Command::new("foster")
        .about("The Foster compiler and development tools")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(Command::new("run").arg(path()).args(optimizer()))
        .subcommand(
            Command::new("build").arg(path()).args(optimizer()).arg(
                Arg::new("output")
                    .short('o')
                    .long("output")
                    .value_parser(value_parser!(PathBuf)),
            ),
        )
        .subcommand(Command::new("check").arg(path()))
        .subcommand(
            Command::new("fmt")
                .about("Format Foster source files")
                .arg(
                    Arg::new("path")
                        .value_parser(value_parser!(PathBuf))
                        .default_value("."),
                )
                .arg(
                    Arg::new("check")
                        .long("check")
                        .help("Check formatting without writing files")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("docs")
                .arg(
                    Arg::new("path")
                        .value_parser(value_parser!(PathBuf))
                        .default_value("."),
                )
                .arg(
                    Arg::new("output")
                        .long("output")
                        .value_parser(value_parser!(PathBuf)),
                )
                .arg(Arg::new("serve").long("serve").action(ArgAction::SetTrue))
                .arg(no_open().requires("serve"))
                .arg(port().requires("serve")),
        )
        .subcommand(
            Command::new("serve-docs")
                .arg(
                    Arg::new("directory")
                        .value_parser(value_parser!(PathBuf))
                        .default_value("documentation"),
                )
                .arg(no_open())
                .arg(port()),
        )
        .subcommand(Command::new("lsp"))
}

fn required_path<'a>(arguments: &'a ArgMatches, name: &str) -> &'a Path {
    arguments
        .get_one::<PathBuf>(name)
        .expect("clap supplies required and defaulted paths")
        .as_path()
}

fn check(path: &Path) -> Result<(), Box<dyn Error>> {
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
    Ok(())
}

fn build(arguments: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let path = required_path(arguments, "path");
    let output = arguments
        .get_one::<PathBuf>("output")
        .cloned()
        .unwrap_or_else(|| default_bytecode_path(path));
    let compilation = compile_path(path)?;
    report_warnings(&compilation, None, None)?;
    let program = foster::vm::compile_with_options(
        &compilation,
        foster::vm::CompileOptions {
            optimize: !arguments.get_flag("no-optimize"),
        },
    )?;
    fs::write(&output, foster::vm::encode_program(&program)?)?;
    println!("built {}", output.display());
    Ok(())
}

fn run(arguments: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let path = required_path(arguments, "path");
    let options = foster::vm::CompileOptions {
        optimize: !arguments.get_flag("no-optimize"),
    };
    let value = if path.extension().is_some_and(|extension| extension == "fbc") {
        let program = foster::vm::decode_program(&fs::read(path)?)?;
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
    Ok(())
}

fn format_path(path: &Path, check: bool) -> Result<(), Box<dyn Error>> {
    let paths = if path.is_dir() {
        WalkDir::new(path)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|entry| {
                entry.depth() == 0
                    || !entry.file_type().is_dir()
                    || !matches!(
                        entry.file_name().to_str(),
                        Some("target" | ".git" | ".foster" | "documentation")
                    )
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|entry| entry.path().extension().is_some_and(|value| value == "fos"))
            .map(|entry| entry.into_path())
            .collect::<Vec<_>>()
    } else {
        vec![path.to_path_buf()]
    };

    let mut changed = Vec::new();
    for path in paths {
        let source = fs::read_to_string(&path)?;
        let formatted = foster::formatter::format(&source)
            .map_err(|error| format!("cannot format `{}`: {error}", path.display()))?;
        if formatted != source {
            changed.push((path, formatted));
        }
    }

    if check && !changed.is_empty() {
        for (path, _) in &changed {
            eprintln!("needs formatting: {}", path.display());
        }
        return Err(format!("{} file(s) need formatting", changed.len()).into());
    }
    for (path, source) in &changed {
        fs::write(path, source)?;
    }
    if !check {
        println!("formatted {} file(s)", changed.len());
    }
    Ok(())
}

fn docs(arguments: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let source = required_path(arguments, "path");
    let output = arguments
        .get_one::<PathBuf>("output")
        .cloned()
        .unwrap_or_else(|| default_documentation_directory(source));
    let compilation = compile_path(source)?;
    report_warnings(&compilation, None, None)?;
    let report = foster::documentation::generate(&compilation, &output)?;
    println!(
        "generated {} declaration(s) in {} module(s) at {}",
        report.declarations,
        report.modules,
        report.output.display()
    );
    if arguments.get_flag("serve") {
        foster::documentation::serve(
            &output,
            foster::documentation::ServeOptions {
                port: *arguments.get_one::<u16>("port").unwrap(),
                open_browser: !arguments.get_flag("no-open"),
            },
        )?;
    }
    Ok(())
}

fn serve_docs(arguments: &ArgMatches) -> Result<(), Box<dyn Error>> {
    foster::documentation::serve(
        required_path(arguments, "directory"),
        foster::documentation::ServeOptions {
            port: *arguments.get_one::<u16>("port").unwrap(),
            open_browser: !arguments.get_flag("no-open"),
        },
    )?;
    Ok(())
}

fn default_bytecode_path(source: &Path) -> PathBuf {
    if source.is_dir() {
        source.join("main.fbc")
    } else {
        source.with_extension("fbc")
    }
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
    path: Option<&Path>,
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

fn parse_file(path: &Path, source: &str) -> Result<foster::ast::Program, Box<dyn Error>> {
    foster::parse(source).map_err(|error| {
        let diagnostic = foster::diagnostic::Diagnostic::from_source_error(source, &error);
        if let Err(render_error) =
            foster::diagnostic::eprint(&path.to_string_lossy(), source, &diagnostic)
        {
            eprintln!("error: could not render diagnostic: {render_error}");
        }
        Box::new(Reported) as Box<dyn Error>
    })
}
