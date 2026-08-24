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
            &["package"],
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

        Ok(Self {
            name: name.to_owned(),
            root,
            manifest_path: manifest_path.to_path_buf(),
            source_root,
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
                "[package]\nname = \"sample\"\n[dependencies]\n",
                "unknown key `dependencies`",
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
}
