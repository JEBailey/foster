pub mod ast;
pub mod diagnostic;
pub mod error;
pub mod hir;
pub mod lexer;
pub mod lsp;
pub mod ownership;
pub mod package;
pub mod parser;
pub mod typecheck;
pub mod types;
pub mod vm;

use error::FosterError;
use std::path::Path;
use vm::Value;

pub fn parse(source: &str) -> Result<ast::Program, FosterError> {
    let tokens = lexer::lex(source)?;
    parser::parse(tokens)
}

pub fn run(source: &str) -> Result<Value, FosterError> {
    vm::run(&compile(source)?)
}

pub fn run_with_options(source: &str, options: vm::CompileOptions) -> Result<Value, FosterError> {
    vm::run_with_options(&compile(source)?, options)
}

pub fn compile(source: &str) -> Result<hir::Compilation, FosterError> {
    let program = parse(source)?;
    hir::Compilation::new(package::Package::from_program_with_core("main", program)?)
}

pub fn check_package(source_root: impl AsRef<Path>) -> Result<hir::Compilation, FosterError> {
    let package = package::Package::load(source_root)?;
    hir::Compilation::new(package)
}

pub fn run_package(source_root: impl AsRef<Path>) -> Result<Value, FosterError> {
    let compilation = check_package(source_root)?;
    vm::run(&compilation)
}

pub fn run_package_with_options(
    source_root: impl AsRef<Path>,
    options: vm::CompileOptions,
) -> Result<Value, FosterError> {
    let compilation = check_package(source_root)?;
    vm::run_with_options(&compilation, options)
}
