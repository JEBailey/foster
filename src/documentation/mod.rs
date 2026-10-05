//! Static API documentation generation and local preview serving.

mod render;
mod server;
mod type_links;

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
    let pages = render::write(compilation, output).map_err(io::Error::other)?;

    Ok(GenerationReport {
        output: output.to_path_buf(),
        modules: pages.module_count,
        declarations: pages.declaration_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_parameters_appear_in_documentation() {
        let compilation = crate::compile("pub enum Event = Created(Int, Bool) | Finished").unwrap();
        let site = render::site(&compilation).unwrap();
        assert!(
            site.modules
                .iter()
                .any(|page| page.html.contains("Created(Int, Bool)"))
        );
    }

    #[test]
    fn foster_writer_reports_counts_and_filesystem_errors() {
        let root = std::env::temp_dir().join(format!(
            "foster-documentation-writer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let compilation = crate::compile("/// A greeting.\npub func hello() {}\n").unwrap();
        let expected = render::site(&compilation).unwrap();
        let output = root.join("nested/site");
        let report = generate(&compilation, &output).unwrap();
        assert_eq!(report.modules, expected.module_count);
        assert_eq!(report.declarations, expected.declaration_count);
        assert_eq!(
            std::fs::read_to_string(output.join("index.html")).unwrap(),
            expected.index
        );
        for page in expected.modules {
            assert_eq!(
                std::fs::read_to_string(output.join("modules").join(page.file_name)).unwrap(),
                page.html
            );
        }
        assert!(
            std::fs::read_to_string(output.join("style.css"))
                .unwrap()
                .contains(":root")
        );
        let blocked = root.join("file");
        std::fs::write(&blocked, "preserve me").unwrap();
        let error = generate(&compilation, &blocked).unwrap_err();
        assert!(error.to_string().contains("file"), "{error}");
        assert_eq!(std::fs::read_to_string(&blocked).unwrap(), "preserve me");
        std::fs::remove_dir_all(root).unwrap();
    }
}
