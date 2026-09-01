//! Static API documentation generation and local preview serving.

mod render;
mod server;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::compiler::Compilation;

pub use server::{ServeOptions, serve};

/// Summary of a generated documentation site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationReport {
    pub output: PathBuf,
    pub modules: usize,
    pub declarations: usize,
}

/// Generate a self-contained static documentation site from resolved compiler data.
pub fn generate(
    compilation: &Compilation,
    output: impl AsRef<Path>,
) -> io::Result<GenerationReport> {
    let output = output.as_ref();
    let modules = output.join("modules");
    fs::create_dir_all(&modules)?;

    let pages = render::site(compilation);
    fs::write(output.join("index.html"), pages.index)?;
    fs::write(output.join("style.css"), render::STYLE)?;
    for page in pages.modules {
        fs::write(modules.join(page.file_name), page.html)?;
    }

    Ok(GenerationReport {
        output: output.to_path_buf(),
        modules: pages.module_count,
        declarations: pages.declaration_count,
    })
}
