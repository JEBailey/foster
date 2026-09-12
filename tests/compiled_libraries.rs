#![allow(clippy::result_large_err)]

use foster::{library, package::Package, vm::Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/flib-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn library(&self, source: &str) -> library::Library {
        let mut package =
            Package::from_program_with_core("main", foster::parse(source).unwrap()).unwrap();
        package
            .symbol_modules
            .insert("main".into(), ("example".into(), "main".into()));
        let compilation = foster::compiler::check(package).unwrap();
        let compiled = library::build(&compilation).unwrap();
        fs::write(
            self.0.join("example.flib"),
            library::encode(&compiled).unwrap(),
        )
        .unwrap();
        compiled
    }
    fn consumer(
        &self,
        source: &str,
    ) -> Result<foster::compiler::Compilation, foster::error::FosterError> {
        fs::create_dir_all(self.0.join("src")).unwrap();
        fs::write(
            self.0.join("foster.toml"),
            "[package]\nname = 'consumer'\n[dependencies]\napi = { path = 'example.flib' }\n",
        )
        .unwrap();
        fs::write(self.0.join("src/main.fos"), source).unwrap();
        foster::check_project(&foster::project::Project::load(&self.0).unwrap())
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn run(compilation: &foster::compiler::Compilation) -> Value {
    let code = foster::vm::compile(compilation).unwrap();
    let bytes = foster::vm::encode_program(&code).unwrap();
    let code = foster::vm::decode_program(&bytes).unwrap();
    foster::vm::Machine::new(&code).run_main().unwrap()
}

#[test]
fn consumers_inherit_compiled_defaults_for_new_receivers() {
    let w = Workspace::new();
    w.library(
        r#"
pub type Sized = { pub func length(self) -> Int }
func adjustment(value: Int) -> Int { value + 2 }
impl Sized {
    pub func score(self) -> Int { adjustment(self.length()) }
    pub func twice(self) -> Int { self.score() * 2 }
    pub func closure(self) -> func(Int) -> Int {
        let offset = self.length()
        (value: Int) -> adjustment(value) + offset
    }
}
pub type Provider<T> = {}
impl Provider {
    pub func echo<T>(self: Provider<T>, value: T) -> T [consume value] { value }
    pub func number<T>(self: Provider<T>, value: Int) -> Int { value + 1 }
    pub func number<T>(self: Provider<T>, value: String) -> Int { 9 }
}
pub type Middle<U> = & Provider<U> & {}
pub type Other = {}
impl Other { pub func score(self) -> Int { 21 } }
"#,
    );
    let app = w
        .consumer(
            r#"
import api
pub type Local = & Sized & { count: Int }
impl Local { pub func length(self) -> Int { self.count } }
pub type Ordered = & Sized & Other & {}
impl Ordered { pub func length(self) -> Int { 100 } }
pub type Override = & Sized & Other & {}
impl Override {
    pub func length(self) -> Int { 100 }
    pub func score(self) -> Int { 7 }
}
pub type Generic<V> = & Middle<V> & {}
type Concrete = & Generic<Int> & {}
func adjustment(value: Int) -> Int { 999 }
func main() -> Int {
    let local = Local { count: 40 }
    assert(local.twice() == 84)
    assert(local.closure()(0) == 42)
    assert(Ordered {}.twice() == 42)
    assert(Override {}.twice() == 14)
    let generic = Concrete {}
    assert(generic.echo(42) == 42)
    assert(generic.number(10) == 11)
    assert(generic.number("text") == 9)
    local.score()
}
"#,
        )
        .unwrap();
    assert_eq!(run(&app), Value::Integer(42));
    let prepared = foster::native::prepare(&app).unwrap();
    for optimize in [false, true] {
        let executable = w.0.join(format!(
            "defaults-{optimize}{}",
            std::env::consts::EXE_SUFFIX
        ));
        prepared
            .build_executable(&executable, foster::native::CompileOptions { optimize })
            .unwrap();
        let output = Command::new(executable).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
    }
    // A second compiled boundary must retain the original donor templates and imports.
    fs::write(
        w.0.join("example.flib"),
        library::encode(&library::build(&app).unwrap()).unwrap(),
    )
    .unwrap();
    fs::write(
        w.0.join("foster.toml"),
        "[package]\nname = 'final_consumer'\n[dependencies]\napi = { path = 'example.flib' }\n",
    )
    .unwrap();
    fs::write(
        w.0.join("src/main.fos"),
        r#"
import api
type Final = & Generic<Int> & {}
func main() -> Int { Final {}.echo(42) }
"#,
    )
    .unwrap();
    let final_app = foster::check_project(&foster::project::Project::load(&w.0).unwrap()).unwrap();
    assert_eq!(run(&final_app), Value::Integer(42));
}

#[test]
fn generic_records_closures_and_private_calls_link_without_bodies() {
    let w = Workspace::new();
    let lib = w.library(
        r#"
pub type Box<T> = { pub value: T }
func offset(x: Int) -> Int { x + 1 }
pub func identity<T>(x: T) -> T [consume x] { x }
pub func multiplier(factor: Int) -> func(Int) -> Int { (x: Int) -> factor * offset(x) }
pub func boxed(x: Int) -> Box<Int> { Box { value: x } }
"#,
    );
    assert!(
        lib.interface
            .modules
            .iter()
            .flat_map(|m| &m.declarations.functions)
            .all(|f| f.body.is_empty())
    );
    let app=w.consumer("import api\nfunc main() -> Int { let f = multiplier(2)\nlet b = identity(boxed(20))\nf(b.value) }").unwrap();
    assert_eq!(run(&app), Value::Integer(42));
}

#[test]
fn structural_calls_find_client_implementations() {
    let w = Workspace::new();
    w.library(
        r#"
pub type Scored = { pub func score(self, value: Int) -> Int }
pub func evaluate(value: Scored) -> Int { value.score(40) }
"#,
    );
    let app = w
        .consumer(
            r#"
import api
type Local = { pub func score(self, value: Int) -> Int }
impl Local { func score(self: Local, value: Int) -> Int { value + 2 } }
func main() -> Int { evaluate(Local {}) }
"#,
        )
        .unwrap();
    assert_eq!(run(&app), Value::Integer(42));
}

#[test]
fn malformed_library_is_rejected() {
    let w = Workspace::new();
    let lib = w.library("pub func answer() -> Int { 42 }");
    let bytes = library::encode(&lib).unwrap();
    for length in [0, 8, 10, 14, bytes.len() - 1] {
        assert!(library::decode(&bytes[..length]).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(library::decode(&trailing).is_err());
    let mut old_format = bytes.clone();
    old_format[8..10].copy_from_slice(&1u16.to_le_bytes());
    assert!(library::decode(&old_format).is_err());
    let mut lib = lib;
    lib.interface.modules[0].declarations.functions[0].body_is_recovery_stub = false;
    assert!(library::encode(&lib).is_err());
    lib.interface.modules[0].declarations.functions[0].body_is_recovery_stub = true;
    lib.interface.modules[0].declarations.functions[0].return_type =
        Some(foster::ast::TypeExpr::Unit);
    assert!(library::encode(&lib).is_err());
    let mut lib = library::decode(&bytes).unwrap();
    lib.interface.ownership_version += 1;
    assert!(library::encode(&lib).is_err());
}

#[test]
fn default_templates_preserve_explicit_ownership_and_reject_invalid_metadata() {
    let w = Workspace::new();
    let mut lib = w.library(
        r#"
pub type Provider = {}
impl Provider { pub func take(self, value: String) -> String [consume value] { value } }
"#,
    );
    let error = w
        .consumer(
            r#"
import api
type Local = & Provider & {}
func main() -> String { let text = "owned"
    Local {}.take(text)
}
"#,
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("move"), "{error}");
    let template = lib
        .interface
        .contexts
        .values_mut()
        .find_map(|context| context.default_template.as_mut())
        .unwrap();
    template.parameters.clear();
    assert!(
        library::encode(&lib)
            .unwrap_err()
            .to_string()
            .contains("default adaptation template")
    );
}

fn cli(args: &[&str], path: &Path) -> std::process::Output {
    let output = Command::new(env!("CARGO_BIN_EXE_foster"))
        .args(args)
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn cli_builds_and_runs_after_library_source_is_removed() {
    let w = Workspace::new();
    let source = w.0.join("math.fos");
    fs::write(&source, "pub func twice(x: Int) -> Int { x * 2 }").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_foster"))
        .arg("build")
        .arg(&source)
        .args(["--library", "--package-name", "arithmetic", "-o"])
        .arg(w.0.join("example.flib"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_file(&source).unwrap();
    w.consumer("import api\nfunc main() -> Int { twice(21) }")
        .unwrap();
    cli(&["check"], &w.0);
    assert_eq!(
        String::from_utf8(cli(&["run"], &w.0).stdout)
            .unwrap()
            .trim(),
        "42"
    );
    cli(&["build"], &w.0);
    let bytecode = w.0.join("main.fbc");
    assert_eq!(
        String::from_utf8(cli(&["run"], &bytecode).stdout)
            .unwrap()
            .trim(),
        "42"
    );
    fs::remove_file(bytecode).unwrap();
    cli(&["pack"], &w.0);
    let archive = w.0.with_extension("fpk");
    assert_eq!(
        String::from_utf8(cli(&["run"], &archive).stdout)
            .unwrap()
            .trim(),
        "42"
    );
    fs::remove_file(archive).unwrap();
}

#[test]
fn ownership_and_effect_contracts_survive_import() {
    let w = Workspace::new();
    w.library(
        r#"
pub func preserve[g: group Int](value: ref[g] Int) -> ref[g] Int { ref value }
pub func observe[g: group Int](value: ref[g] Int) -> Int { value }
pub func append(values: List<Int>) -> () { values.push(30) }
"#,
    );
    let valid=w.consumer("import api\nfunc main() -> Int { let values = [42]\nlet selected = preserve(ref values[0])\nobserve(selected) }").unwrap();
    assert_eq!(run(&valid), Value::Integer(42));
    let invalid=w.consumer("import api\nfunc main() -> Int { let values = [42]\nlet selected = preserve(ref values[0])\nappend(values)\nobserve(selected) }");
    assert_eq!(
        invalid.unwrap_err().code.as_deref(),
        Some(foster::ownership::diagnostics::INVALIDATED_LOAN)
    );
    let invalid=w.consumer("import api\nfunc pure(values: List<Int>) -> () [read values] { append(values) }\nfunc main() -> Int { 0 }");
    assert!(invalid.unwrap_err().message.contains("undeclared effect"));
}

#[test]
fn enum_payloads_are_relocated() {
    let w = Workspace::new();
    w.library("pub enum Choice<T> = Value(T) | Empty\npub func boxed(x: Int) -> Choice<Int> { Choice.Value(x) }");
    let app=w.consumer("import api\nenum Earlier = A | B\nfunc main() -> Int { branch boxed(42) { Choice.Value(v) -> v\nChoice.Empty -> 0 } }").unwrap();
    assert_eq!(run(&app), Value::Integer(42));
}

#[test]
fn imported_code_builds_as_native() {
    let w = Workspace::new();
    w.library("func bump(x: Int) -> Int { x + 1 }\npub func identity<T>(x: T) -> T [consume x] { x }\npub func multiplier(factor: Int) -> func(Int) -> Int { (x: Int) -> factor * bump(x) }");
    w.consumer("import api\nfunc main() -> Int { let f = multiplier(2)\nf(identity(20)) }")
        .unwrap();
    cli(&["build", "--native"], &w.0);
    let output = Command::new(w.0.join(if cfg!(windows) { "main.exe" } else { "main" }))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "42");
}

#[test]
fn libraries_can_bundle_compiled_dependencies() {
    let w = Workspace::new();
    w.library(
        "pub type Box = { pub value: Int }\npub func boxed(x: Int) -> Box { Box { value: x } }",
    );
    let middle = w
        .consumer("import api\npub func answer() -> api::Box { boxed(42) }")
        .unwrap();
    let artifact = library::build(&middle).unwrap();
    fs::write(
        w.0.join("example.flib"),
        library::encode(&artifact).unwrap(),
    )
    .unwrap();
    // The consumer's own package identity must differ from the bundled middle package.
    fs::write(
        w.0.join("foster.toml"),
        "[package]\nname = 'application'\n[dependencies]\napi = { path = 'example.flib' }\n",
    )
    .unwrap();
    fs::write(
        w.0.join("src/main.fos"),
        "import api\nfunc main() -> Int { answer().value }",
    )
    .unwrap();
    let app = foster::check_project(&foster::project::Project::load(&w.0).unwrap()).unwrap();
    assert_eq!(run(&app), Value::Integer(42));
}

#[test]
fn composition_materialized_inside_a_library_is_retained() {
    let w = Workspace::new();
    w.library("pub type Base = { pub value: Int, pub func score(self) -> Int }\nimpl Base { pub func score(self: Base) -> Int { self.value } }\npub type Derived = & Base & {}\npub func derived() -> Derived { Derived { value: 42 } }");
    let app = w
        .consumer("import api\ntype New = & Derived & { pub extra: Int }\nfunc main() -> Int { assert(derived().score() == 42)\nNew { extra: 99, value: 42 }.score() }")
        .unwrap();
    assert_eq!(run(&app), Value::Integer(42));
}

#[test]
fn nested_modules_and_constants_survive_rebasing() {
    let w = Workspace::new();
    let root = w.0.join("library");
    fs::create_dir_all(root.join("src/models")).unwrap();
    fs::write(root.join("foster.toml"), "[package]\nname = 'models'\n").unwrap();
    fs::write(
        root.join("src/models/deep.fos"),
        "pub const answer = 42\npub type Box = { pub value: Int }",
    )
    .unwrap();
    fs::write(root.join("src/main.fos"),"import models.deep as forms\npub const answer = forms::answer\npub func boxed() -> forms::Box { Box { value: answer } }").unwrap();
    let compilation =
        foster::check_project(&foster::project::Project::load(&root).unwrap()).unwrap();
    fs::write(
        w.0.join("example.flib"),
        library::encode(&library::build(&compilation).unwrap()).unwrap(),
    )
    .unwrap();
    fs::remove_dir_all(&root).unwrap();
    let app = w
        .consumer("import api\nfunc main() -> Int { boxed().value }")
        .unwrap();
    assert_eq!(run(&app), Value::Integer(42));
}

#[test]
fn embedded_dependencies_are_linked_with_checked_descriptors() {
    let w = Workspace::new();
    w.library("import std.fs\npub func answer() -> Int { 42 }");
    let app = w
        .consumer("import api\nfunc main() -> Int { answer() }")
        .unwrap();
    assert_eq!(run(&app), Value::Integer(42));
}

#[test]
fn private_helpers_are_not_importable() {
    let w = Workspace::new();
    w.library("func secret() -> Int { 42 }\npub func answer() -> Int { secret() }");
    assert!(
        w.consumer("import api\nfunc main() -> Int { secret() }")
            .is_err()
    );
    assert!(
        w.consumer("import api\nfunc main() -> Int { api::secret() }")
            .is_err()
    );
}

#[test]
fn materialized_methods_keep_cross_module_receiver_types() {
    let w = Workspace::new();
    let root = w.0.join("library");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("foster.toml"), "[package]\nname = 'composed'\n").unwrap();
    fs::write(
        root.join("src/tokens.fos"),
        "pub type Token = { pub value: Int }",
    )
    .unwrap();
    fs::write(
        root.join("src/base.fos"),
        r#"
import tokens
pub type Base = { pub value: Int, pub func score(self) -> Int }
impl Base {
    pub func score(self: Base) -> Int { self.value }
    pub func closure(self) -> func(tokens::Token) -> Int {
        let offset = self.value
        (token: tokens::Token) -> token.value + offset
    }
}
"#,
    )
    .unwrap();
    fs::write(root.join("src/main.fos"),"import base\npub type Derived = & Base & {}\npub func derived() -> Derived { Derived { value: 42 } }").unwrap();
    let compilation =
        foster::check_project(&foster::project::Project::load(&root).unwrap()).unwrap();
    fs::write(
        w.0.join("example.flib"),
        library::encode(&library::build(&compilation).unwrap()).unwrap(),
    )
    .unwrap();
    fs::remove_dir_all(&root).unwrap();
    let app = w
        .consumer("import api\nimport api.tokens\ntype New = & Derived & { pub extra: Int }\nfunc main() -> Int { assert(derived().score() == 42)\nlet value = New { extra: 99, value: 42 }\nassert(value.closure()(Token { value: 0 }) == 42)\nvalue.score() }")
        .unwrap();
    assert_eq!(run(&app), Value::Integer(42));
}
