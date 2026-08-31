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
        Some(("init", arguments)) => init(arguments)?,
        Some(("lsp", _)) => return foster::lsp::run(),
        Some(("check", arguments)) => check(arguments)?,
        Some(("build", arguments)) => build(arguments)?,
        Some(("pack", arguments)) => pack(arguments)?,
        Some(("run", arguments)) => run(arguments)?,
        Some(("fmt", arguments)) => format_path(
            &source_target_or_current(arguments)?.source,
            arguments.get_flag("check"),
        )?,
        Some(("test", arguments)) => test(arguments)?,
        Some(("docs", arguments)) => docs(arguments)?,
        Some(("serve-docs", arguments)) => serve_docs(arguments)?,
        _ => unreachable!("clap requires a recognized subcommand"),
    }
    Ok(())
}

fn cli() -> Command {
    let path = || {
        Arg::new("path").value_parser(value_parser!(PathBuf)).help(
            "Source file, project directory, or foster.toml (defaults to the current project)",
        )
    };
    let optimizer = || {
        [
            Arg::new("optimize")
                .long("optimize")
                .help("Enable bytecode optimization (the default)")
                .action(ArgAction::SetTrue)
                .conflicts_with("no-optimize"),
            Arg::new("no-optimize")
                .long("no-optimize")
                .help("Disable bytecode optimization")
                .action(ArgAction::SetTrue),
        ]
    };
    let port = || {
        Arg::new("port")
            .long("port")
            .help("TCP port for the documentation server")
            .value_parser(value_parser!(u16))
            .default_value("8000")
    };
    let no_open = || {
        Arg::new("no-open")
            .long("no-open")
            .help("Do not open the documentation site in a browser")
            .action(ArgAction::SetTrue)
    };

    Command::new("foster")
        .about("The Foster compiler and development tools")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("init")
                .about("Create a Foster project with foster.toml and src/main.fos")
                .arg(
                    Arg::new("path")
                        .value_parser(value_parser!(PathBuf))
                        .help("Directory in which to create the project")
                        .default_value("."),
                )
                .arg(
                    Arg::new("name")
                        .long("name")
                        .value_name("NAME")
                        .help("Package name (defaults to the project directory name)"),
                ),
        )
        .subcommand(
            Command::new("run")
                .about("Compile and run a Foster program, bytecode file, or package")
                .arg(path())
                .args(optimizer())
                .arg(
                    Arg::new("command-arguments")
                        .last(true)
                        .num_args(0..)
                        .allow_hyphen_values(true)
                        .help("Arguments passed to Foster `main` after `--`"),
                ),
        )
        .subcommand(
            Command::new("build")
                .about("Compile Foster source to bytecode or a native executable")
                .arg(path())
                .args(optimizer())
                .arg(
                    Arg::new("native")
                        .long("native")
                        .help("Compile and link a host-native executable with Cranelift")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("output")
                        .short('o')
                        .long("output")
                        .help("Write the compiled artifact to this path")
                        .value_parser(value_parser!(PathBuf)),
                ),
        )
        .subcommand(
            Command::new("pack")
                .about("Build a runnable .fpk archive with optional resources")
                .arg(path())
                .args(optimizer())
                .arg(
                    Arg::new("output")
                        .short('o')
                        .long("output")
                        .help("Write the package archive to this path")
                        .value_parser(value_parser!(PathBuf)),
                )
                .arg(
                    Arg::new("resources")
                        .long("resources")
                        .value_parser(value_parser!(PathBuf))
                        .help("Resource directory (defaults to <package>/resources when present)"),
                ),
        )
        .subcommand(
            Command::new("check")
                .about("Type-check and validate Foster source without running it")
                .arg(path())
                .arg(
                    Arg::new("dump-ownership")
                        .long("dump-ownership")
                        .help(
                            "Print deterministic ownership MIR, loan ancestry, and inferred regions",
                        )
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("test")
                .about("Compile and run Foster test declarations")
                .arg(path())
                .args(optimizer()),
        )
        .subcommand(
            Command::new("fmt")
                .about("Format Foster source files")
                .arg(path())
                .arg(
                    Arg::new("check")
                        .long("check")
                        .help("Check formatting without writing files")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("docs")
                .about("Generate static API documentation for Foster source")
                .arg(path())
                .arg(
                    Arg::new("output")
                        .long("output")
                        .help("Write generated documentation to this directory")
                        .value_parser(value_parser!(PathBuf)),
                )
                .arg(
                    Arg::new("serve")
                        .long("serve")
                        .help("Serve the generated documentation after building it")
                        .action(ArgAction::SetTrue),
                )
                .arg(no_open().requires("serve"))
                .arg(port().requires("serve")),
        )
        .subcommand(
            Command::new("serve-docs")
                .about("Serve an existing generated documentation directory")
                .arg(
                    Arg::new("directory")
                        .help("Generated documentation directory to serve")
                        .value_parser(value_parser!(PathBuf))
                        .default_value("documentation"),
                )
                .arg(no_open())
                .arg(port()),
        )
        .subcommand(
            Command::new("lsp")
                .about("Start the Foster language server over standard input/output"),
        )
}

fn required_path<'a>(arguments: &'a ArgMatches, name: &str) -> &'a Path {
    arguments
        .get_one::<PathBuf>(name)
        .expect("clap supplies required and defaulted paths")
        .as_path()
}

#[derive(Debug)]
struct SourceTarget {
    source: PathBuf,
    project: Option<foster::project::Project>,
}

impl SourceTarget {
    fn explicit(path: &Path) -> Result<Self, Box<dyn Error>> {
        if path
            .file_name()
            .is_some_and(|name| name == foster::project::MANIFEST_NAME)
        {
            return Self::from_project(foster::project::Project::load_manifest(path)?);
        }
        if path.is_dir() && path.join(foster::project::MANIFEST_NAME).is_file() {
            return Self::from_project(foster::project::Project::load(path)?);
        }
        Ok(Self {
            source: path.to_path_buf(),
            project: None,
        })
    }

    fn current_project() -> Result<Self, Box<dyn Error>> {
        let current = std::env::current_dir()?;
        let project = foster::project::Project::discover(&current, None)?.ok_or_else(|| {
            format!(
                "could not find `{}` in `{}` or any parent directory; pass a Foster source path or run `foster init`",
                foster::project::MANIFEST_NAME,
                current.display()
            )
        })?;
        Self::from_project(project)
    }

    fn from_project(project: foster::project::Project) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            source: project.source_root.clone(),
            project: Some(project),
        })
    }

    fn artifact_base(&self) -> &Path {
        self.project
            .as_ref()
            .map(|project| project.root.as_path())
            .unwrap_or(&self.source)
    }
}

fn source_target(arguments: &ArgMatches) -> Result<SourceTarget, Box<dyn Error>> {
    arguments
        .get_one::<PathBuf>("path")
        .map_or_else(SourceTarget::current_project, |path| {
            SourceTarget::explicit(path)
        })
}

fn source_target_or_current(arguments: &ArgMatches) -> Result<SourceTarget, Box<dyn Error>> {
    if let Some(path) = arguments.get_one::<PathBuf>("path") {
        return SourceTarget::explicit(path);
    }
    let current = std::env::current_dir()?;
    foster::project::Project::discover(&current, None)?.map_or_else(
        || SourceTarget::explicit(&current),
        SourceTarget::from_project,
    )
}

fn init(arguments: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let requested_root = required_path(arguments, "path");
    if requested_root.exists() && !requested_root.is_dir() {
        return Err(format!(
            "project path `{}` exists and is not a directory",
            requested_root.display()
        )
        .into());
    }
    fs::create_dir_all(requested_root)?;
    let root = fs::canonicalize(requested_root)?;
    let manifest = root.join(foster::project::MANIFEST_NAME);
    if manifest.exists() {
        return Err(format!("project manifest `{}` already exists", manifest.display()).into());
    }

    let name = arguments
        .get_one::<String>("name")
        .cloned()
        .or_else(|| {
            root.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        })
        .ok_or("cannot infer a package name; pass `--name <name>`")?;
    if name.is_empty()
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(
            "package names may contain only ASCII letters, digits, hyphens, and underscores".into(),
        );
    }

    let source_root = root.join(foster::project::DEFAULT_SOURCE_DIRECTORY);
    if source_root.exists() && !source_root.is_dir() {
        return Err(format!(
            "default source path `{}` exists and is not a directory",
            source_root.display()
        )
        .into());
    }
    fs::create_dir_all(&source_root)?;
    let main = source_root.join("main.fos");
    if !main.exists() {
        fs::write(
            &main,
            "func main() -> () {\n    println(\"Hello, Foster!\")\n}\n",
        )?;
    }
    fs::write(
        &manifest,
        format!("[package]\nname = \"{name}\"\nsource = \"src\"\n"),
    )?;

    println!("created Foster project `{name}` at {}", root.display());
    println!("  {}", manifest.display());
    println!("  {}", main.display());
    Ok(())
}

fn check(arguments: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let target = source_target(arguments)?;
    let path = &target.source;
    if path.is_dir() {
        let compilation = compile_target(&target)?;
        report_warnings(&compilation, None, None)?;
        if arguments.get_flag("dump-ownership") {
            print!("{}", compilation.ownership.debug_dump(&compilation.hir));
        }
        let module_count = compilation.package.input_module_count();
        println!(
            "ok: checked {module_count} module{} ({} implicit)",
            if module_count == 1 { "" } else { "s" },
            compilation.package.input_implicit_module_count()
        );
    } else {
        let source = fs::read_to_string(path)?;
        let program = parse_file(path, &source)?;
        let function_count = program.functions.len();
        let compilation = compile_single_file(path, &source, program)?;
        report_warnings(&compilation, Some(path), Some(&source))?;
        if arguments.get_flag("dump-ownership") {
            print!("{}", compilation.ownership.debug_dump(&compilation.hir));
        }
        println!("ok: checked {function_count} function(s)");
    }
    Ok(())
}

fn build(arguments: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let target = source_target(arguments)?;
    let native = arguments.get_flag("native");
    let output = arguments
        .get_one::<PathBuf>("output")
        .cloned()
        .unwrap_or_else(|| {
            if native {
                default_native_path(target.artifact_base())
            } else {
                default_bytecode_path(target.artifact_base())
            }
        });
    let compilation = compile_target(&target)?;
    report_warnings(&compilation, None, None)?;
    if native {
        foster::native::build_executable(
            &compilation,
            &output,
            foster::native::CompileOptions {
                optimize: !arguments.get_flag("no-optimize"),
            },
        )?;
        println!("built native executable {}", output.display());
        return Ok(());
    }
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

fn pack(arguments: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let target = source_target(arguments)?;
    let output = arguments
        .get_one::<PathBuf>("output")
        .cloned()
        .unwrap_or_else(|| default_package_path(target.artifact_base()));
    let default_resources = target
        .artifact_base()
        .is_dir()
        .then(|| target.artifact_base().join("resources"));
    let resources = arguments
        .get_one::<PathBuf>("resources")
        .cloned()
        .or_else(|| default_resources.filter(|path| path.is_dir()));
    let compilation = compile_target(&target)?;
    report_warnings(&compilation, None, None)?;
    let program = foster::vm::compile_with_options(
        &compilation,
        foster::vm::CompileOptions {
            optimize: !arguments.get_flag("no-optimize"),
        },
    )?;
    let bytecode = foster::vm::encode_program(&program)?;
    foster::archive::write_package(&output, &bytecode, resources.as_deref())?;
    println!("packed {}", output.display());
    Ok(())
}

fn run(arguments: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let target = source_target(arguments)?;
    let path = &target.source;
    let command_arguments = foster::entry::CommandArguments::new(
        target.artifact_base().to_string_lossy(),
        arguments
            .get_many::<String>("command-arguments")
            .into_iter()
            .flatten()
            .cloned(),
    );
    let options = foster::vm::CompileOptions {
        optimize: !arguments.get_flag("no-optimize"),
    };
    let value = if path
        .extension()
        .is_some_and(|extension| extension == foster::archive::EXTENSION)
    {
        run_archive(path, &command_arguments)?
    } else if path.extension().is_some_and(|extension| extension == "fbc") {
        let program = foster::vm::decode_program(&fs::read(path)?)?;
        foster::vm::Machine::new(&program).run_main_with_arguments(&command_arguments)?
    } else if path.is_dir() {
        let compilation = compile_target(&target)?;
        report_warnings(&compilation, None, None)?;
        foster::vm::run_with_arguments(&compilation, options, &command_arguments)?
    } else {
        let source = fs::read_to_string(path)?;
        let program = parse_file(path, &source)?;
        let compilation = compile_single_file(path, &source, program)?;
        report_warnings(&compilation, Some(path), Some(&source))?;
        foster::vm::run_with_arguments(&compilation, options, &command_arguments)?
    };
    if value != foster::vm::Value::Unit {
        println!("{value}");
    }
    Ok(())
}

fn run_archive(
    path: &Path,
    arguments: &foster::entry::CommandArguments,
) -> Result<foster::vm::Value, Box<dyn Error>> {
    let package = foster::archive::read_package(path)?;
    let program = foster::vm::decode_program(&package.bytecode)?;
    let working_directory = PackageWorkingDirectory::create()?;
    working_directory.write_resources(&package.resources)?;
    let host = foster::vm::HostContext::new(working_directory.path());
    Ok(
        foster::vm::Machine::with_host_context(&program, host)
            .run_main_with_arguments(arguments)?,
    )
}

struct PackageWorkingDirectory {
    path: PathBuf,
}

impl PackageWorkingDirectory {
    fn create() -> Result<Self, Box<dyn Error>> {
        let unique = format!(
            "foster-package-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        fs::create_dir(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn write_resources(&self, resources: &[(PathBuf, Vec<u8>)]) -> Result<(), Box<dyn Error>> {
        let root = self.path.join("resources");
        fs::create_dir(&root)?;
        for (relative, contents) in resources {
            let destination = root.join(relative);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(destination, contents)?;
        }
        Ok(())
    }
}

impl Drop for PackageWorkingDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn test(arguments: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let target = source_target(arguments)?;
    let path = &target.source;
    if path.extension().is_some_and(|extension| extension == "fbc") {
        return Err("compiled bytecode does not retain test discovery metadata".into());
    }
    let compilation = compile_target(&target)?;
    report_warnings(&compilation, None, None)?;
    let program = foster::vm::compile_with_options(
        &compilation,
        foster::vm::CompileOptions {
            optimize: !arguments.get_flag("no-optimize"),
        },
    )?;
    let machine = foster::vm::Machine::new(&program);
    let requested_root = path.is_dir().then(|| fs::canonicalize(path)).transpose()?;
    let mut tests = compilation
        .hir
        .tests
        .iter()
        .filter(|function| {
            let definition = &compilation.hir.functions[**function];
            let module = &compilation.hir.modules[definition.module];
            if path.is_file() {
                return module.name == "main";
            }
            let Some(root) = &requested_root else {
                return false;
            };
            module
                .source_path
                .as_ref()
                .and_then(|source| fs::canonicalize(source).ok())
                .is_some_and(|source| source.starts_with(root))
        })
        .map(|function| {
            let definition = &compilation.hir.functions[*function];
            (
                compilation.hir.modules[definition.module].name.clone(),
                definition
                    .test_description
                    .clone()
                    .expect("test functions carry descriptions"),
                *function,
            )
        })
        .collect::<Vec<_>>();
    tests.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));

    println!("running {} test(s)", tests.len());
    let mut failed = Vec::new();
    for (module, description, function) in tests {
        let display = if module == "main" {
            description.clone()
        } else {
            format!("{module}: {description}")
        };
        match machine.run_function(function) {
            Ok(foster::vm::Value::Unit) => println!("test {display} ... ok"),
            Ok(value) => {
                println!("test {display} ... FAILED");
                failed.push((display, format!("returned {value:?} instead of ()")));
            }
            Err(error) => {
                println!("test {display} ... FAILED");
                failed.push((display, error.to_string()));
            }
        }
    }
    if failed.is_empty() {
        println!("test result: ok");
        return Ok(());
    }
    eprintln!("\nfailures:");
    for (name, error) in &failed {
        eprintln!("    {name}: {error}");
    }
    eprintln!("\ntest result: FAILED. {} failed", failed.len());
    Err(Box::new(Reported))
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
    let target = source_target_or_current(arguments)?;
    let output = arguments
        .get_one::<PathBuf>("output")
        .cloned()
        .unwrap_or_else(|| default_documentation_directory(target.artifact_base()));
    let compilation = compile_target(&target)?;
    report_warnings(&compilation, None, None)?;
    let report = foster::documentation::generate(&compilation, &output)?;
    println!(
        "generated {} declaration{} in {} module{} at {}",
        report.declarations,
        if report.declarations == 1 { "" } else { "s" },
        report.modules,
        if report.modules == 1 { "" } else { "s" },
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

fn default_native_path(source: &Path) -> PathBuf {
    let mut output = if source.is_dir() {
        source.join("main")
    } else {
        source.with_extension("")
    };
    if cfg!(windows) {
        output.set_extension("exe");
    }
    output
}

fn default_package_path(source: &Path) -> PathBuf {
    source.with_extension(foster::archive::EXTENSION)
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
        return compile_package(path);
    }
    let source = fs::read_to_string(path)?;
    let program = parse_file(path, &source)?;
    compile_single_file(path, &source, program)
}

fn compile_target(target: &SourceTarget) -> Result<foster::hir::Compilation, Box<dyn Error>> {
    target.project.as_ref().map_or_else(
        || compile_path(&target.source),
        |project| {
            foster::check_project(project)
                .map_err(|error| report_project_compilation_error(project, &error))
        },
    )
}

fn compile_package(path: &Path) -> Result<foster::hir::Compilation, Box<dyn Error>> {
    foster::check_package(path).map_err(|error| report_project_error(path, &error))
}

fn report_project_compilation_error(
    project: &foster::project::Project,
    error: &foster::error::FosterError,
) -> Box<dyn Error> {
    if let Some(module) = &error.source_module {
        let mut projects = vec![(None, project.clone())];
        if let Ok(dependencies) = project.resolve_dependencies() {
            projects.extend(
                dependencies
                    .into_iter()
                    .map(|dependency| (Some(dependency.name), dependency.project)),
            );
        }
        for (prefix, candidate) in projects {
            let local_module = match prefix {
                None => module.as_str(),
                Some(prefix) if module == &prefix => "main",
                Some(prefix) => {
                    let Some(local) = module.strip_prefix(&format!("{prefix}.")) else {
                        continue;
                    };
                    local
                }
            };
            let mut source_path = candidate.source_root.clone();
            source_path.extend(local_module.split('.'));
            source_path.set_extension("fos");
            if let Ok(source) = fs::read_to_string(&source_path) {
                let diagnostic = foster::diagnostic::Diagnostic::from_source_error(&source, error);
                if let Err(render_error) =
                    foster::diagnostic::eprint(&source_path.to_string_lossy(), &source, &diagnostic)
                {
                    eprintln!("error: could not render diagnostic: {render_error}");
                }
                return Box::new(Reported);
            }
        }
    }
    eprintln!("error: {error}");
    Box::new(Reported)
}

fn report_project_error(source_root: &Path, error: &foster::error::FosterError) -> Box<dyn Error> {
    if let Some(module) = &error.source_module {
        let mut source_path = source_root.to_path_buf();
        source_path.extend(module.split('.'));
        source_path.set_extension("fos");
        if let Ok(source) = fs::read_to_string(&source_path) {
            let diagnostic = foster::diagnostic::Diagnostic::from_source_error(&source, error);
            if let Err(render_error) =
                foster::diagnostic::eprint(&source_path.to_string_lossy(), &source, &diagnostic)
            {
                eprintln!("error: could not render diagnostic: {render_error}");
            }
            return Box::new(Reported);
        }
    }
    eprintln!("error: {error}");
    Box::new(Reported)
}

fn compile_single_file(
    path: &Path,
    source: &str,
    program: foster::ast::Program,
) -> Result<foster::hir::Compilation, Box<dyn Error>> {
    let package = foster::package::Package::from_program_with_core("main", program)?;
    foster::hir::Compilation::new(package).map_err(|error| {
        if error
            .source_module
            .as_deref()
            .is_none_or(|module| module == "main")
        {
            let diagnostic = foster::diagnostic::Diagnostic::from_source_error(source, &error);
            if let Err(render_error) =
                foster::diagnostic::eprint(&path.to_string_lossy(), source, &diagnostic)
            {
                eprintln!("error: could not render diagnostic: {render_error}");
            }
        } else {
            eprintln!("error: {error}");
        }
        Box::new(Reported) as Box<dyn Error>
    })
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
