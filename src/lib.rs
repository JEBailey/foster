//! Foster toolchain driver: projects, commands, formatting, documentation, and editor services.
#![allow(clippy::result_large_err)]
pub mod archive;
pub use foster_compiler::ast;
pub use foster_compiler::block;
pub use foster_compiler::codegen;
pub use foster_compiler::compiler;
pub mod debugger;
pub use foster_compiler::diagnostic;
pub mod documentation;
pub use foster_compiler::entry;
pub use foster_compiler::error;
pub mod foreign;
pub mod formatter;
pub use foster_compiler::hir;
pub use foster_compiler::intrinsics;
pub use foster_compiler::lexer;
pub use foster_compiler::library;
pub mod lsp;
pub use foster_compiler::native;
pub use foster_compiler::ownership;
pub use foster_compiler::package;
pub use foster_compiler::parser;
pub mod project;
pub use foster_compiler::semantics;
pub use foster_compiler::symbols;
pub use foster_compiler::typecheck;
pub use foster_compiler::types;
pub use foster_host::remote;
mod tooling;
pub mod vm;

/// Creates a project using the embedded Foster project tool.
pub fn init_project(path: &Path, name: Option<&str>) -> Result<String, FosterError> {
    static TOOL: tooling::Tool =
        tooling::Tool::new(include_bytes!(concat!(env!("OUT_DIR"), "/init.fbc")));
    let mut arguments = vec![
        path.to_str()
            .ok_or_else(|| FosterError::runtime("project path must be valid UTF-8"))?
            .to_owned(),
    ];
    if let Some(name) = name {
        arguments.push(name.to_owned());
    }
    tooling::string(&TOOL.run(arguments)?)
}
use error::FosterError;
use std::path::Path;
use vm::Value;

pub use foster_compiler::{check_package, compile, parse, parse_recovering};

pub fn run(source: &str) -> Result<Value, FosterError> {
    vm::run(&compile(source)?)
}

pub fn run_with_options(source: &str, options: vm::CompileOptions) -> Result<Value, FosterError> {
    vm::run_with_options(&compile(source)?, options)
}

pub fn run_with_arguments(
    source: &str,
    arguments: &entry::CommandArguments,
) -> Result<Value, FosterError> {
    vm::run_with_arguments(&compile(source)?, vm::CompileOptions::default(), arguments)
}

pub fn check_project(project: &project::Project) -> Result<compiler::Compilation, FosterError> {
    let package = package::Package::load_project(project)?;
    compiler::check(package)
}

pub fn run_package(source_root: impl AsRef<Path>) -> Result<Value, FosterError> {
    let compilation = check_package(source_root)?;
    vm::run(&compilation)
}

pub fn run_project(project: &project::Project) -> Result<Value, FosterError> {
    let compilation = check_project(project)?;
    vm::run(&compilation)
}

pub fn run_package_with_options(
    source_root: impl AsRef<Path>,
    options: vm::CompileOptions,
) -> Result<Value, FosterError> {
    let compilation = check_package(source_root)?;
    vm::run_with_options(&compilation, options)
}
