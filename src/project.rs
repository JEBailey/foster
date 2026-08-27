use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use crate::error::FosterError;
use crate::vm::Value;

pub const MANIFEST_NAME: &str = "foster.toml";
pub const DEFAULT_SOURCE_DIRECTORY: &str = "src";

const MANIFEST_PARSER: &str = r#"
import core.result
import std.process
import std.toml

func main(arguments: Arguments) -> Result<TomlDocument, TomlError> {
    parse(arguments.values.head)
}
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub name: String,
    pub root: PathBuf,
    pub manifest_path: PathBuf,
    pub source_root: PathBuf,
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
    pub project: Project,
}

impl Project {
    pub fn load(root: impl AsRef<Path>) -> Result<Self, FosterError> {
        Self::load_manifest(root.as_ref().join(MANIFEST_NAME))
    }

    pub fn load_manifest(manifest_path: impl AsRef<Path>) -> Result<Self, FosterError> {
        let manifest_path = manifest_path.as_ref();
        let source = fs::read_to_string(manifest_path).map_err(|error| {
            FosterError::runtime(format!(
                "cannot read project manifest `{}`: {error}",
                manifest_path.display()
            ))
        })?;
        let document = parse_manifest(&source, manifest_path)?;
        let table = document_entries(&document).ok_or_else(|| {
            FosterError::runtime("embedded Foster TOML parser returned an invalid document")
        })?;
        reject_unknown_keys(
            table,
            &["package", "dependencies"],
            &format!("project manifest `{}`", manifest_path.display()),
        )?;

        let package = find_entry(table, "package").ok_or_else(|| {
            FosterError::runtime(format!(
                "project manifest `{}` is missing the required `[package]` table",
                manifest_path.display()
            ))
        })?;
        let package = value_table(package).ok_or_else(|| {
            FosterError::runtime(format!(
                "`package` in `{}` must be a TOML table",
                manifest_path.display()
            ))
        })?;
        reject_unknown_keys(
            package,
            &["name", "source"],
            &format!("`[package]` in `{}`", manifest_path.display()),
        )?;

        let name = required_string(package, "name", manifest_path)?;
        if name.trim().is_empty() {
            return Err(FosterError::runtime(format!(
                "`package.name` in `{}` cannot be empty",
                manifest_path.display()
            )));
        }

        let source_directory = find_entry(package, "source")
            .map(|value| {
                value_string(value).ok_or_else(|| {
                    FosterError::runtime(format!(
                        "`package.source` in `{}` must be a string",
                        manifest_path.display()
                    ))
                })
            })
            .transpose()?
            .unwrap_or(DEFAULT_SOURCE_DIRECTORY);
        validate_source_directory(source_directory, manifest_path)?;

        let root = manifest_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let source_root = root.join(source_directory);
        if !source_root.is_dir() {
            return Err(FosterError::runtime(format!(
                "project source root `{}` from `{}` is not a directory",
                source_root.display(),
                manifest_path.display()
            )));
        }

        let dependencies = find_entry(table, "dependencies")
            .map(|value| parse_dependencies(value, &root, manifest_path))
            .transpose()?
            .unwrap_or_default();

        Ok(Self {
            name: name.to_owned(),
            root,
            manifest_path: manifest_path.to_path_buf(),
            source_root,
            dependencies,
        })
    }

    pub fn discover(
        start: impl AsRef<Path>,
        boundary: Option<&Path>,
    ) -> Result<Option<Self>, FosterError> {
        let start = start.as_ref();
        let mut candidate = if start.is_file() {
            start.parent()
        } else {
            Some(start)
        };
        while let Some(directory) = candidate {
            if boundary.is_some_and(|boundary| !directory.starts_with(boundary)) {
                break;
            }
            let manifest = directory.join(MANIFEST_NAME);
            if manifest.is_file() {
                return Self::load_manifest(manifest).map(Some);
            }
            if boundary.is_some_and(|boundary| directory == boundary) {
                break;
            }
            candidate = directory.parent();
        }
        Ok(None)
    }

    pub fn resolve_dependencies(&self) -> Result<Vec<ResolvedDependency>, FosterError> {
        let root_manifest = canonical_manifest(self)?;
        let mut state = DependencyResolution {
            visiting: vec![(self.name.clone(), root_manifest)],
            aliases: HashMap::new(),
            resolved: HashSet::new(),
            output: Vec::new(),
        };
        state.visit_dependencies(self)?;
        Ok(state.output)
    }
}

struct DependencyResolution {
    visiting: Vec<(String, PathBuf)>,
    aliases: HashMap<String, PathBuf>,
    resolved: HashSet<(String, PathBuf)>,
    output: Vec<ResolvedDependency>,
}

impl DependencyResolution {
    fn visit_dependencies(&mut self, project: &Project) -> Result<(), FosterError> {
        for dependency in project.dependencies.values() {
            let dependency_project = Project::load(&dependency.root).map_err(|error| {
                FosterError::runtime(format!(
                    "cannot load dependency `{}` of package `{}`: {error}",
                    dependency.name, project.name
                ))
            })?;
            let manifest = canonical_manifest(&dependency_project)?;

            if let Some((position, _)) = self
                .visiting
                .iter()
                .enumerate()
                .find(|(_, (_, candidate))| *candidate == manifest)
            {
                let mut cycle = self.visiting[position..]
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>();
                cycle.push(dependency.name.as_str());
                return Err(FosterError::runtime(format!(
                    "path dependency cycle: {}",
                    cycle.join(" -> ")
                )));
            }

            if let Some(existing) = self.aliases.get(&dependency.name) {
                if existing != &manifest {
                    return Err(FosterError::runtime(format!(
                        "dependency name `{}` refers to both `{}` and `{}`",
                        dependency.name,
                        existing.display(),
                        manifest.display()
                    )));
                }
            } else {
                self.aliases
                    .insert(dependency.name.clone(), manifest.clone());
            }

            let identity = (dependency.name.clone(), manifest.clone());
            if !self.resolved.insert(identity) {
                continue;
            }

            self.output.push(ResolvedDependency {
                name: dependency.name.clone(),
                project: dependency_project.clone(),
            });
            self.visiting
                .push((dependency.name.clone(), manifest.clone()));
            self.visit_dependencies(&dependency_project)?;
            self.visiting.pop();
        }
        Ok(())
    }
}

fn canonical_manifest(project: &Project) -> Result<PathBuf, FosterError> {
    fs::canonicalize(&project.manifest_path).map_err(|error| {
        FosterError::runtime(format!(
            "cannot resolve project manifest `{}`: {error}",
            project.manifest_path.display()
        ))
    })
}

fn parse_dependencies(
    value: &Value,
    project_root: &Path,
    manifest_path: &Path,
) -> Result<BTreeMap<String, ProjectDependency>, FosterError> {
    let entries = value_table(value).ok_or_else(|| {
        FosterError::runtime(format!(
            "`dependencies` in `{}` must be a TOML table",
            manifest_path.display()
        ))
    })?;
    let mut dependencies = BTreeMap::new();
    for entry in entries {
        let name = entry_key(entry).ok_or_else(|| {
            FosterError::runtime(format!(
                "`dependencies` in `{}` contains an invalid entry",
                manifest_path.display()
            ))
        })?;
        validate_dependency_name(name, manifest_path)?;
        let value = entry_value(entry).expect("a TOML entry has a value");
        let table = value_table(value).ok_or_else(|| {
            FosterError::runtime(format!(
                "dependency `{name}` in `{}` must be a table containing `path`",
                manifest_path.display()
            ))
        })?;
        reject_unknown_keys(
            table,
            &["path"],
            &format!("dependency `{name}` in `{}`", manifest_path.display()),
        )?;
        let path = find_entry(table, "path")
            .and_then(value_string)
            .ok_or_else(|| {
                FosterError::runtime(format!(
                    "dependency `{name}` in `{}` requires a string `path`",
                    manifest_path.display()
                ))
            })?;
        validate_dependency_path(path, name, manifest_path)?;
        dependencies.insert(
            name.to_owned(),
            ProjectDependency {
                name: name.to_owned(),
                root: project_root.join(path),
            },
        );
    }
    Ok(dependencies)
}

fn validate_dependency_name(name: &str, manifest_path: &Path) -> Result<(), FosterError> {
    let mut characters = name.chars();
    let valid = characters
        .next()
        .is_some_and(|character| character == '_' || character.is_alphabetic())
        && characters.all(|character| character == '_' || character.is_alphanumeric());
    if valid && !matches!(name, "core" | "std") {
        return Ok(());
    }
    Err(FosterError::runtime(format!(
        "dependency name `{name}` in `{}` must be a portable module name other than `core` or `std`",
        manifest_path.display()
    )))
}

fn validate_dependency_path(
    source: &str,
    name: &str,
    manifest_path: &Path,
) -> Result<(), FosterError> {
    let path = Path::new(source);
    if source.trim().is_empty() || path.is_absolute() {
        return Err(FosterError::runtime(format!(
            "dependency `{name}` path in `{}` must be a non-empty relative path",
            manifest_path.display()
        )));
    }
    Ok(())
}

fn required_string<'a>(
    table: &'a [Value],
    key: &str,
    manifest_path: &Path,
) -> Result<&'a str, FosterError> {
    let value = find_entry(table, key).ok_or_else(|| {
        FosterError::runtime(format!(
            "project manifest `{}` is missing required `package.{key}`",
            manifest_path.display()
        ))
    })?;
    value_string(value).ok_or_else(|| {
        FosterError::runtime(format!(
            "`package.{key}` in `{}` must be a string",
            manifest_path.display()
        ))
    })
}

fn reject_unknown_keys(
    table: &[Value],
    allowed: &[&str],
    location: &str,
) -> Result<(), FosterError> {
    if let Some(key) = table
        .iter()
        .filter_map(entry_key)
        .find(|key| !allowed.contains(key))
    {
        return Err(FosterError::runtime(format!(
            "unknown key `{key}` in {location}; expected {}",
            allowed
                .iter()
                .map(|key| format!("`{key}`"))
                .collect::<Vec<_>>()
                .join(" or ")
        )));
    }
    Ok(())
}

fn parse_manifest(source: &str, manifest_path: &Path) -> Result<Value, FosterError> {
    static PROGRAM: OnceLock<Result<crate::vm::Program, String>> = OnceLock::new();
    let program = PROGRAM
        .get_or_init(|| {
            crate::compile(MANIFEST_PARSER)
                .and_then(|compilation| crate::vm::compile(&compilation))
                .map_err(|error| error.message)
        })
        .as_ref()
        .map_err(|error| {
            FosterError::runtime(format!(
                "cannot initialize embedded Foster TOML parser: {error}"
            ))
        })?;
    let arguments = crate::entry::CommandArguments::new(
        manifest_path.display().to_string(),
        [source.to_owned()],
    );
    let outcome = crate::vm::Machine::new(program).run_main_with_arguments(&arguments)?;
    match outcome {
        Value::Variant {
            alternative,
            mut payload,
            ..
        } if alternative.as_ref() == "Ok" => payload.pop().ok_or_else(|| {
            FosterError::runtime("embedded Foster TOML parser returned an empty Result.Ok")
        }),
        Value::Variant {
            alternative,
            mut payload,
            ..
        } if alternative.as_ref() == "Error" => {
            let error = payload.pop().ok_or_else(|| {
                FosterError::runtime("embedded Foster TOML parser returned an empty Result.Error")
            })?;
            Err(manifest_parse_error(&error, manifest_path))
        }
        _ => Err(FosterError::runtime(
            "embedded Foster TOML parser returned an invalid result",
        )),
    }
}

fn manifest_parse_error(value: &Value, manifest_path: &Path) -> FosterError {
    let (message, line, column) = match value {
        Value::Record { fields, .. } => (
            fields
                .get("message")
                .and_then(Value::as_string)
                .unwrap_or("invalid TOML"),
            fields
                .get("line")
                .and_then(|value| match value {
                    Value::Integer(value) => Some(*value),
                    _ => None,
                })
                .unwrap_or(0),
            fields
                .get("column")
                .and_then(|value| match value {
                    Value::Integer(value) => Some(*value),
                    _ => None,
                })
                .unwrap_or(0),
        ),
        _ => ("invalid TOML", 0, 0),
    };
    FosterError::runtime(format!(
        "invalid project manifest `{}` at {line}:{column}: {message}",
        manifest_path.display()
    ))
}

fn document_entries(value: &Value) -> Option<&[Value]> {
    let Value::Record { name, fields, .. } = value else {
        return None;
    };
    (name == "TomlDocument")
        .then(|| fields.get("entries")?.as_list())
        .flatten()
}

fn entry_key(value: &Value) -> Option<&str> {
    let Value::Record { name, fields, .. } = value else {
        return None;
    };
    (name == "TomlEntry")
        .then(|| fields.get("key")?.as_string())
        .flatten()
}

fn entry_value(value: &Value) -> Option<&Value> {
    let Value::Record { name, fields, .. } = value else {
        return None;
    };
    (name == "TomlEntry").then(|| fields.get("value")).flatten()
}

fn find_entry<'a>(entries: &'a [Value], key: &str) -> Option<&'a Value> {
    entries
        .iter()
        .find(|entry| entry_key(entry) == Some(key))
        .and_then(entry_value)
}

fn value_table(value: &Value) -> Option<&[Value]> {
    let Value::Variant {
        alternative,
        payload,
        ..
    } = value
    else {
        return None;
    };
    (alternative.as_ref() == "Table")
        .then(|| payload.first()?.as_list())
        .flatten()
}

fn value_string(value: &Value) -> Option<&str> {
    let Value::Variant {
        alternative,
        payload,
        ..
    } = value
    else {
        return None;
    };
    (alternative.as_ref() == "String")
        .then(|| payload.first()?.as_string())
        .flatten()
}

fn validate_source_directory(source: &str, manifest_path: &Path) -> Result<(), FosterError> {
    if source.trim().is_empty() {
        return Err(FosterError::runtime(format!(
            "`package.source` in `{}` cannot be empty",
            manifest_path.display()
        )));
    }
    let path = Path::new(source);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(FosterError::runtime(format!(
            "`package.source` in `{}` must be a relative path contained by the project",
            manifest_path.display()
        )));
    }
    Ok(())
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
        assert!(project.dependencies.is_empty());

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
                "must be a table containing `path`",
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
        assert_eq!(dependencies[0].project.name, "middle-package");
        assert_eq!(dependencies[1].project.name, "leaf-package");

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
