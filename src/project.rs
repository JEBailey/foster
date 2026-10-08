use crate::{error::FosterError, tooling, vm::Value};
use std::collections::BTreeMap;
impl crate::package::ProjectInputs for Project {
    fn source(&self) -> crate::package::ProjectSource {
        crate::package::ProjectSource {
            name: self.name.clone(),
            root: self.root.clone(),
            source_root: self.source_root.clone(),
            entry: self.entry.clone(),
        }
    }
    fn dependency_sources(&self) -> Result<Vec<crate::package::DependencySource>, FosterError> {
        Ok(self
            .resolve_dependencies()?
            .into_iter()
            .map(|dependency| crate::package::DependencySource {
                name: dependency.name,
                project: dependency
                    .project
                    .as_ref()
                    .map(crate::package::ProjectInputs::source),
                artifact: dependency.artifact,
            })
            .collect())
    }
}

#[cfg(test)]
use std::fs;
use std::path::{Path, PathBuf};

pub const MANIFEST_NAME: &str = "foster.toml";
pub const DEFAULT_SOURCE_DIRECTORY: &str = "src";
static TOOL: tooling::Tool =
    tooling::Tool::new(include_bytes!(concat!(env!("OUT_DIR"), "/project.fbc")));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub name: String,
    pub root: PathBuf,
    pub manifest_path: PathBuf,
    pub source_root: PathBuf,
    pub entry: String,
    pub dependencies: BTreeMap<String, ProjectDependency>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDependency {
    pub name: String,
    pub root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDependency {
    pub name: String,
    pub project: Option<Project>,
    pub artifact: Option<PathBuf>,
}

impl Project {
    pub fn load(root: impl AsRef<Path>) -> Result<Self, FosterError> {
        Self::load_manifest(root.as_ref().join(MANIFEST_NAME))
    }
    pub fn load_manifest(manifest: impl AsRef<Path>) -> Result<Self, FosterError> {
        let value = TOOL.run(vec![
            "load".into(),
            platform(),
            path_text(manifest.as_ref())?,
        ])?;
        decode_resolved(&value)?
            .into_iter()
            .next()
            .and_then(|value| value.project)
            .ok_or_else(invalid_response)
    }
    pub fn discover(
        start: impl AsRef<Path>,
        boundary: Option<&Path>,
    ) -> Result<Option<Self>, FosterError> {
        let start = start.as_ref();
        // Ancestors and prefix comparison use the host's exact lexical path rules.
        // Foster decides which manifest to select and how to load it.
        let directory = if start.is_file() {
            start.parent()
        } else {
            Some(start)
        };
        let mut arguments = vec!["discover".into(), platform()];
        if let Some(directory) = directory {
            for ancestor in directory
                .ancestors()
                .take_while(|path| boundary.is_none_or(|boundary| path.starts_with(boundary)))
            {
                arguments.push(path_text(ancestor)?);
            }
        }
        Ok(decode_resolved(&TOOL.run(arguments)?)?
            .into_iter()
            .next()
            .and_then(|value| value.project))
    }

    pub fn resolve_dependencies(&self) -> Result<Vec<ResolvedDependency>, FosterError> {
        let mut arguments = vec![
            "resolve".into(),
            platform(),
            self.name.clone(),
            path_text(&self.root)?,
            path_text(&self.manifest_path)?,
            path_text(&self.source_root)?,
            self.entry.clone(),
        ];
        for dependency in self.dependencies.values() {
            arguments.push(dependency.name.clone());
            arguments.push(path_text(&dependency.root)?);
        }
        decode_resolved(&TOOL.run(arguments)?)
    }
}

fn platform() -> String {
    if cfg!(windows) { "windows" } else { "unix" }.into()
}
fn path_text(path: &Path) -> Result<String, FosterError> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        FosterError::runtime(format!(
            "project path `{}` must be valid UTF-8",
            path.display()
        ))
    })
}
fn invalid_response() -> FosterError {
    FosterError::runtime("Foster project tool returned an invalid response")
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value, FosterError> {
    match value {
        Value::Record { fields, .. } => fields.get(key).ok_or_else(invalid_response),
        _ => Err(invalid_response()),
    }
}
fn text_field(value: &Value, key: &str) -> Result<String, FosterError> {
    tooling::string(field(value, key)?)
}
fn decode_project(value: &Value) -> Result<Project, FosterError> {
    let mut dependencies = BTreeMap::new();
    for entry in field(value, "dependencies")?
        .as_list()
        .ok_or_else(invalid_response)?
    {
        let name = text_field(entry, "name")?;
        dependencies.insert(
            name.clone(),
            ProjectDependency {
                name,
                root: text_field(entry, "root")?.into(),
            },
        );
    }
    Ok(Project {
        name: text_field(value, "name")?,
        root: text_field(value, "root")?.into(),
        manifest_path: text_field(value, "manifest")?.into(),
        source_root: text_field(value, "source")?.into(),
        entry: text_field(value, "entry")?,
        dependencies,
    })
}
fn decode_resolved(value: &Value) -> Result<Vec<ResolvedDependency>, FosterError> {
    value
        .as_list()
        .ok_or_else(invalid_response)?
        .iter()
        .map(|value| {
            let project = match field(value, "project")? {
                Value::Variant {
                    alternative,
                    payload,
                    ..
                } if alternative.as_ref() == "Some" => Some(decode_project(
                    payload.first().ok_or_else(invalid_response)?,
                )?),
                Value::Variant { alternative, .. } if alternative.as_ref() == "None" => None,
                _ => return Err(invalid_response()),
            };
            let artifact = text_field(value, "artifact")?;
            Ok(ResolvedDependency {
                name: text_field(value, "name")?,
                project,
                artifact: if artifact.is_empty() {
                    None
                } else {
                    Some(artifact.into())
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_project(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "foster-project-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("source")).unwrap();
        root
    }

    #[test]
    fn discovery_respects_lexical_workspace_boundaries() {
        let root = temporary_project("boundary");
        fs::write(
            root.join(MANIFEST_NAME),
            "[package]\nname = 'outer'\nsource = 'source'\n",
        )
        .unwrap();
        let nested = root.join("source/nested");
        fs::create_dir_all(&nested).unwrap();
        let file = nested.join("main.fos");
        fs::write(&file, "func main() { 42 }").unwrap();
        assert!(Project::discover(&file, Some(&nested)).unwrap().is_none());
        assert_eq!(
            Project::discover(&file, Some(&root)).unwrap().unwrap().name,
            "outer"
        );
        assert!(
            Project::discover(&file, Some(&root.join("source/nest")))
                .unwrap()
                .is_none()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn loads_and_discovers_a_manifest_source_root() {
        let root = temporary_project("load");
        fs::write(
            root.join(MANIFEST_NAME),
            "[package]\nname = \"sample\"\nsource = \"source\"\n",
        )
        .unwrap();
        let nested = root.join("source/nested");
        fs::create_dir(&nested).unwrap();

        let project = Project::discover(&nested, None).unwrap().unwrap();
        assert_eq!(project.name, "sample");
        assert_eq!(project.source_root, root.join("source"));
        assert_eq!(project.entry, "main.fos");
        assert!(project.dependencies.is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_package_entries_are_validated() {
        let root = temporary_project("entry");
        fs::write(
            root.join("source/lib.fos"),
            "pub func answer() -> Int { 42 }",
        )
        .unwrap();
        let manifest = root.join(MANIFEST_NAME);
        for entry in ["lib.fos", "main.fos"] {
            fs::write(root.join("source").join(entry), "").unwrap();
            fs::write(
                &manifest,
                format!("[package]\nname = 'sample'\nsource = 'source'\nentry = '{entry}'\n"),
            )
            .unwrap();
            assert_eq!(Project::load(&root).unwrap().entry, entry);
        }
        for entry in [
            "''",
            "'../lib.fos'",
            "'/lib.fos'",
            "'nested/lib.fos'",
            "'lib.txt'",
            "'missing.fos'",
            "42",
        ] {
            fs::write(
                &manifest,
                format!("[package]\nname = 'sample'\nsource = 'source'\nentry = {entry}\n"),
            )
            .unwrap();
            let error = Project::load(&root).unwrap_err().to_string();
            assert!(error.contains("entry"), "{entry}: {error}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovers_library_folders_and_respects_explicit_paths() {
        let root = temporary_project("library-discovery");
        for folder in ["vendor", "other", "vendor/nested"] {
            fs::create_dir_all(root.join(folder)).unwrap();
        }
        for file in [
            "vendor/math.flib",
            "other/text.flib",
            "vendor/ignored.fos",
            "vendor/nested/hidden.flib",
        ] {
            fs::write(root.join(file), []).unwrap();
        }
        let manifest = "[package]\nname = 'app'\nsource = 'source'\n[discovery]\nlibraries = ['vendor', 'other', './vendor']\n";
        fs::write(root.join(MANIFEST_NAME), manifest).unwrap();
        let project = Project::load(&root).unwrap();
        assert_eq!(
            project
                .dependencies
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["math", "text"]
        );
        assert_eq!(project.resolve_dependencies().unwrap().len(), 2);

        fs::write(root.join("other/MATH.flib"), []).unwrap();
        assert!(
            Project::load(&root)
                .unwrap_err()
                .message
                .contains("ambiguous discovered library")
        );
        fs::remove_file(root.join("other/MATH.flib")).unwrap();
        fs::write(root.join("other/math.flib"), []).unwrap();
        assert!(
            Project::load(&root)
                .unwrap_err()
                .message
                .contains("ambiguous discovered library `math`")
        );
        fs::write(
            root.join(MANIFEST_NAME),
            format!("{manifest}[dependencies]\nmath = {{ path = 'other/math.flib' }}\n"),
        )
        .unwrap();
        assert_eq!(
            Project::load(&root).unwrap().dependencies["math"].root,
            root.join("other/math.flib")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_invalid_library_discovery_configuration() {
        let root = temporary_project("invalid-library-discovery");
        let prefix = "[package]\nname = 'app'\nsource = 'source'\n";
        for (config, expected) in [
            ("discovery = 4\n", "unknown key `discovery`"),
            ("[discovery]\nlibraries = 'vendor'\n", "array of strings"),
            ("[discovery]\nlibraries = [4]\n", "array of strings"),
            ("[discovery]\nlibraries = ['']\n", "non-empty relative"),
            (
                "[discovery]\nlibraries = ['/absolute']\n",
                "non-empty relative",
            ),
            (
                "[discovery]\nlibraries = ['missing']\n",
                "library discovery folder",
            ),
            ("[discovery]\nrecursive = true\n", "unknown key `recursive`"),
        ] {
            fs::write(root.join(MANIFEST_NAME), format!("{prefix}{config}")).unwrap();
            let error = Project::load(&root).unwrap_err();
            assert!(error.message.contains(expected), "{config}: {error}");
        }
        fs::create_dir(root.join("vendor")).unwrap();
        fs::write(root.join("vendor/std.flib"), []).unwrap();
        fs::write(
            root.join(MANIFEST_NAME),
            format!("{prefix}[discovery]\nlibraries = ['vendor']\n"),
        )
        .unwrap();
        assert!(
            Project::load(&root)
                .unwrap_err()
                .message
                .contains("portable module name")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_manifest_paths_that_escape_the_project() {
        let root = temporary_project("escape");
        fs::write(
            root.join(MANIFEST_NAME),
            "[package]\nname = \"sample\"\nsource = \"../source\"\n",
        )
        .unwrap();

        let error = Project::load(&root).unwrap_err();
        assert!(
            error
                .message
                .contains("relative path contained by the project")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_invalid_manifest_shapes_with_specific_messages() {
        let root = temporary_project("invalid-shapes");
        fs::create_dir(root.join("src")).unwrap();
        let manifest = root.join(MANIFEST_NAME);
        let cases = [
            ("", "missing the required `[package]` table"),
            ("[package]\nname = 3\n", "`package.name`"),
            (
                "[package]\nname = \"sample\"\nextra = true\n",
                "unknown key `extra`",
            ),
            (
                "[package]\nname = \"sample\"\nsource = \"\"\n",
                "`package.source`",
            ),
            (
                "[package]\nname = \"sample\"\n[dependencies]\nmath = \"../math\"\n",
                "must be a table containing `path` or `uri`",
            ),
            (
                "[package]\nname = \"sample\"\n[dependencies]\nmath = { path = 3 }\n",
                "requires a string `path`",
            ),
            (
                "[package]\nname = \"sample\"\n[dependencies]\nmath = { path = \"../math\", version = \"1\" }\n",
                "unknown key `version`",
            ),
            (
                "[package]\nname = \"sample\"\n[dependencies]\ncore = { path = \"../core\" }\n",
                "portable module name",
            ),
            (
                "[package]\nname = 'sample'\n[dependencies]\nmath = { path = '../math', uri = 'https://example.com/math.git' }\n",
                "must choose either `path` or `uri`",
            ),
            (
                "[package]\nname = 'sample'\n[dependencies]\nmath = { uri = 'https://example.com/math.git' }\n",
                "requires `rev` or `tag`",
            ),
            (
                "[package]\nname = 'sample'\n[dependencies]\nmath = { uri = 3, rev = 'main' }\n",
                "requires a string `uri`",
            ),
            (
                "[package]\nname = 'sample'\n[dependencies]\nmath = { uri = 'https://example.com/math.git', rev = 'main' }\n",
                "full 40-character Git commit ID",
            ),
            (
                "[package]\nname = 'sample'\n[dependencies]\nmath = { path = '../math', rev = 'main' }\n",
                "permits `rev` only with `uri`",
            ),
            (
                "[package]\nname = 'sample'\n[dependencies]\nmath = { uri = 'https://example.com/math.git', tag = 'v1', rev = 'main' }\n",
                "must choose either `rev` or `tag`",
            ),
            (
                "[package]\nname = 'sample'\n[dependencies]\nmath = { uri = 'https://example.com/math.git', tag = 3 }\n",
                "requires a string `tag`",
            ),
            (
                "[package]\nname = 'sample'\n[dependencies]\nmath = { uri = 'https://example.com/math.git', tag = '' }\n",
                "`tag` must be non-empty",
            ),
            (
                "[package]\nname = 'sample'\n[dependencies]\nmath = { path = '../math', tag = 'v1' }\n",
                "permits `tag` only with `uri`",
            ),
        ];

        for (source, expected) in cases {
            fs::write(&manifest, source).unwrap();
            let error = Project::load(&root).unwrap_err();
            assert!(
                error.message.contains(expected),
                "expected `{expected}` in `{}`",
                error.message
            );
        }

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolves_diamonds_and_artifacts_once_in_name_order() {
        let root = temporary_project("diamond");
        for folder in ["left", "right", "shared"] {
            fs::create_dir_all(root.join(folder).join("src")).unwrap();
            let dependencies = if folder == "shared" {
                ""
            } else {
                "[dependencies]\nshared = { path = '../shared' }\n"
            };
            fs::write(
                root.join(folder).join(MANIFEST_NAME),
                format!("[package]\nname = '{folder}'\n{dependencies}"),
            )
            .unwrap();
        }
        fs::write(root.join("binary.flib"), []).unwrap();
        fs::write(root.join(MANIFEST_NAME), "[package]\nname = 'app'\nsource = 'source'\n[dependencies]\nright = { path = 'right' }\nleft = { path = 'left' }\nbinary = { path = 'binary.flib' }\n").unwrap();
        let dependencies = Project::load(&root)
            .unwrap()
            .resolve_dependencies()
            .unwrap();
        assert_eq!(
            dependencies
                .iter()
                .map(|value| value.name.as_str())
                .collect::<Vec<_>>(),
            ["binary", "left", "shared", "right"]
        );
        assert!(dependencies[0].artifact.is_some());
        assert!(dependencies[0].project.is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolves_transitive_path_dependencies_in_stable_order() {
        let root = temporary_project("dependencies");
        let middle = root.join("middle");
        let leaf = root.join("leaf");
        fs::create_dir_all(middle.join("source")).unwrap();
        fs::create_dir_all(leaf.join("source")).unwrap();
        fs::write(
            root.join(MANIFEST_NAME),
            "[package]\nname = \"app\"\nsource = \"source\"\n[dependencies]\nmiddle = { path = \"middle\" }\n",
        )
        .unwrap();
        fs::write(
            middle.join(MANIFEST_NAME),
            "[package]\nname = \"middle-package\"\nsource = \"source\"\n[dependencies]\nleaf = { path = \"../leaf\" }\n",
        )
        .unwrap();
        fs::write(
            leaf.join(MANIFEST_NAME),
            "[package]\nname = \"leaf-package\"\nsource = \"source\"\n",
        )
        .unwrap();

        let project = Project::load(&root).unwrap();
        let dependencies = project.resolve_dependencies().unwrap();
        assert_eq!(
            dependencies
                .iter()
                .map(|dependency| dependency.name.as_str())
                .collect::<Vec<_>>(),
            ["middle", "leaf"]
        );
        assert_eq!(
            dependencies[0].project.as_ref().unwrap().name,
            "middle-package"
        );
        assert_eq!(
            dependencies[1].project.as_ref().unwrap().name,
            "leaf-package"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_path_dependency_cycles() {
        let root = temporary_project("dependency-cycle");
        let child = root.join("child");
        fs::create_dir_all(child.join("source")).unwrap();
        fs::write(
            root.join(MANIFEST_NAME),
            "[package]\nname = \"app\"\nsource = \"source\"\n[dependencies]\nchild = { path = \"child\" }\n",
        )
        .unwrap();
        fs::write(
            child.join(MANIFEST_NAME),
            "[package]\nname = \"child\"\nsource = \"source\"\n[dependencies]\napp = { path = \"..\" }\n",
        )
        .unwrap();

        let error = Project::load(&root)
            .unwrap()
            .resolve_dependencies()
            .unwrap_err();
        assert!(error.message.contains("app -> child -> app"), "{error}");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_conflicting_transitive_dependency_names() {
        let root = temporary_project("dependency-name-conflict");
        for directory in ["left", "right", "first-shared", "second-shared"] {
            fs::create_dir_all(root.join(directory).join("source")).unwrap();
        }
        fs::write(
            root.join(MANIFEST_NAME),
            "[package]\nname = \"app\"\nsource = \"source\"\n[dependencies]\nleft = { path = \"left\" }\nright = { path = \"right\" }\n",
        )
        .unwrap();
        fs::write(
            root.join("left/foster.toml"),
            "[package]\nname = \"left\"\nsource = \"source\"\n[dependencies]\nshared = { path = \"../first-shared\" }\n",
        )
        .unwrap();
        fs::write(
            root.join("right/foster.toml"),
            "[package]\nname = \"right\"\nsource = \"source\"\n[dependencies]\nshared = { path = \"../second-shared\" }\n",
        )
        .unwrap();
        for directory in ["first-shared", "second-shared"] {
            fs::write(
                root.join(directory).join(MANIFEST_NAME),
                format!("[package]\nname = \"{directory}\"\nsource = \"source\"\n"),
            )
            .unwrap();
        }

        let error = Project::load(&root)
            .unwrap()
            .resolve_dependencies()
            .unwrap_err();
        assert!(
            error
                .message
                .contains("dependency name `shared` refers to both"),
            "{error}"
        );

        fs::remove_dir_all(root).unwrap();
    }
}
