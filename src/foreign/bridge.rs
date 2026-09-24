//! Host services for the Foster-written bridge generator.
//! JSON syntax decoding is deliberately independent of the C manifest schema.
use serde_json::Value;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

// serde_json::Value normally overwrites duplicate keys. Reject them at every
// depth so a repeated ownership field cannot silently replace an earlier one.
struct Json(Value);
impl<'de> serde::Deserialize<'de> for Json {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Json;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON with unique object keys")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Json, E> {
                Ok(Json(Value::Null))
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Json, E> {
                Ok(Json(v.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Json, E> {
                Ok(Json(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Json, E> {
                Ok(Json(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Json, E> {
                serde_json::Number::from_f64(v)
                    .map(|v| Json(Value::Number(v)))
                    .ok_or_else(|| E::custom("nonfinite JSON number"))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Json, E> {
                Ok(Json(v.into()))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Json, E> {
                Ok(Json(v.into()))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
                let mut values = Vec::new();
                while let Some(Json(value)) = seq.next_element()? {
                    values.push(value);
                }
                Ok(Json(Value::Array(values)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some((key, Json(value))) = map.next_entry::<String, Json>()? {
                    if values.insert(key.clone(), value).is_some() {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate JSON field `{key}`"
                        )));
                    }
                }
                Ok(Json(Value::Object(values)))
            }
        }
        decoder.deserialize_any(Visitor)
    }
}

/// A contract validated by tools/cbind/src/bridge.fos, with generated artifacts.
pub struct Manifest {
    document: Value,
    tree: String,
    schema: String,
    c_source: String,
    foster_source: String,
}

// Hex preserves arbitrary UTF-8 and delimiters without injecting Foster source.
// Object keys have deterministic order; array order remains significant.
fn flatten(value: &Value, parent: i64, key: &str, next: &mut i64, out: &mut String) {
    let id = *next;
    *next += 1;
    let (kind, text) = match value {
        Value::Null => ("null", String::new()),
        Value::Bool(value) => ("bool", value.to_string()),
        Value::Number(value) => ("number", value.to_string()),
        Value::String(value) => ("string", value.clone()),
        Value::Array(_) => ("array", String::new()),
        Value::Object(_) => ("object", String::new()),
    };
    writeln!(
        out,
        "{parent}\t{}\t{kind}\t{}",
        super::runtime::encode(key.as_bytes()),
        super::runtime::encode(text.as_bytes())
    )
    .unwrap();
    match value {
        Value::Array(values) => {
            for value in values {
                flatten(value, id, "", next, out);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                flatten(value, id, key, next, out);
            }
        }
        _ => {}
    }
}

fn generator() -> Result<&'static crate::vm::Program, String> {
    static PROGRAM: OnceLock<Result<crate::vm::Program, String>> = OnceLock::new();
    PROGRAM.get_or_init(|| {
        let source = concat!(
            include_str!("../../tools/cbind/src/bridge.fos"),
            "\nfunc main(args: Arguments) -> List<String> { generate_bridge(args.values[0], args.values[1]) }\n"
        );
        let compilation = crate::compile(source).map_err(|e| e.to_string())?;
        crate::vm::compile(&compilation).map_err(|e| e.to_string())
    }).as_ref().map_err(Clone::clone)
}

fn generate(tree: &str, path: &Path) -> Result<(String, String, String), String> {
    let args = crate::entry::CommandArguments::new(
        "foster bridge",
        [tree.to_owned(), path.to_string_lossy().replace('\\', "/")],
    );
    let value = crate::vm::Machine::new(generator()?)
        .run_main_with_arguments(&args)
        .map_err(|e| e.to_string())?;
    let parts = value
        .as_list()
        .ok_or("bridge generator returned a non-list")?;
    if parts.len() != 3 {
        return Err("invalid bridge generator response".into());
    }
    let text = |index: usize| {
        parts[index]
            .as_string()
            .map(str::to_owned)
            .ok_or_else(|| "bridge generator returned a non-string artifact".to_owned())
    };
    Ok((text(0)?, text(1)?, text(2)?))
}

impl Manifest {
    pub fn parse(source: &str) -> Result<Self, String> {
        Self::for_library(source, Path::new(""))
    }
    fn for_library(source: &str, path: &Path) -> Result<Self, String> {
        let Json(document) = serde_json::from_str(source).map_err(|e| e.to_string())?;
        let mut tree = String::new();
        flatten(&document, -1, "", &mut 0, &mut tree);
        let (schema, c_source, foster_source) = generate(&tree, path)?;
        Ok(Self {
            document,
            tree,
            schema,
            c_source,
            foster_source,
        })
    }
    /// Deterministic contract identity, computed in Foster; not a security signature.
    pub fn identity(&self) -> String {
        self.schema.clone()
    }
    pub fn generate(&self) -> Result<String, String> {
        Ok(self.c_source.clone())
    }
    pub fn foster_source(&self, path: &Path) -> Result<String, String> {
        Ok(generate(&self.tree, path)?.2)
    }
    // Only used after Foster has checked the entire contract, including build paths.
    fn paths(&self, key: &str) -> impl Iterator<Item = &str> {
        self.document
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|value| value.as_str().expect("Foster validated build path"))
    }
}

/// Rebuild explicitly; loading an artifact never invokes a compiler or generator.
pub fn build(manifest_path: &Path, output: &Path, compiler: &Path) -> Result<PathBuf, String> {
    if !cfg!(all(windows, target_arch = "x86_64")) {
        return Err("C bridges currently require Windows x86-64".into());
    }
    let manifest_path = manifest_path.canonicalize().map_err(|e| e.to_string())?;
    let root = manifest_path.parent().unwrap();
    let output = std::path::absolute(output).map_err(|e| e.to_string())?;
    if output.extension().and_then(|value| value.to_str()) != Some("dll") {
        return Err("C bridge output must have a .dll extension".into());
    }
    let manifest = Manifest::for_library(
        &std::fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?,
        &output,
    )?;
    std::fs::create_dir_all(output.parent().unwrap()).map_err(|e| e.to_string())?;
    let source = output.with_extension("bridge.c");
    std::fs::write(&source, &manifest.c_source).map_err(|e| e.to_string())?;
    let mut command = Command::new(compiler);
    command
        .current_dir(root)
        .args([
            "--target=x86_64-pc-windows-msvc",
            "-std=c11",
            "-shared",
            "-Werror=implicit-function-declaration",
            "-Werror=incompatible-pointer-types",
            "-Werror=incompatible-function-pointer-types",
            "-o",
        ])
        .arg(&output)
        .arg(&source)
        .arg("-I")
        .arg(root);
    for include in manifest.paths("include_directories") {
        command.arg("-I").arg(root.join(include));
    }
    for path in manifest.paths("sources").chain(manifest.paths("libraries")) {
        command.arg(root.join(path));
    }
    let result = command
        .output()
        .map_err(|e| format!("cannot run C compiler `{}`: {e}", compiler.display()))?;
    if !result.status.success() {
        return Err(format!(
            "C bridge compilation failed:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        ));
    }
    std::fs::write(output.with_extension("fos"), &manifest.foster_source)
        .map_err(|e| e.to_string())?;
    std::fs::write(output.with_extension("schema"), manifest.identity())
        .map_err(|e| e.to_string())?;
    Ok(output)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn json_adapter_preserves_contract_data_and_rejects_duplicate_keys() {
        for source in [
            r#"{"abi":1,"abi":2}"#,
            r#"{"operations":[{"name":"a","name":"b"}]}"#,
        ] {
            let error = Manifest::parse(source).err().expect("duplicate JSON key");
            assert!(error.contains("duplicate JSON field"), "{error}");
        }
        let first = Manifest::parse(r#"{"abi":1,"headers":[],"operations":[]}"#).unwrap();
        let reordered =
            Manifest::parse("{\n\"operations\":[], \"headers\":[], \"abi\":1}").unwrap();
        assert_eq!(first.identity(), reordered.identity());
        let unicode = Manifest::parse(r#"{"abi":1,"headers":["λ.h"],"operations":[]}"#).unwrap();
        assert!(unicode.generate().unwrap().contains("#include \"λ.h\""));
        assert_ne!(first.identity(), unicode.identity());
        let bindings = first
            .foster_source(Path::new("C:/quoted\"path/λ.dll"))
            .unwrap();
        crate::compile(&(bindings + "\nfunc main() -> Int { 42 }")).unwrap();
    }
    #[test]
    fn foster_checks_field_shapes_and_ownership_contracts() {
        for (source, expected) in [
            (
                r#"{"abi":1,"headers":[],"operations":[],"owenrship":true}"#,
                "unknown manifest field",
            ),
            (
                r#"{"abi":1,"headers":[],"operations":[],"sources":[4]}"#,
                "expected string",
            ),
            (
                r#"{"abi":1,"headers":[],"operations":[{"name":"a","symbol":"a","result":"i32","parameters":["void"]}]}"#,
                "void is not a C parameter",
            ),
            (
                r#"{"abi":1,"headers":[],"operations":[{"name":"a","symbol":"a","result":"i32","receiver":"Missing"}]}"#,
                "unknown C resource",
            ),
            (
                r#"{"abi":1,"headers":[],"operations":[{"name":"a","symbol":"a","result":"bytes"}]}"#,
                "release function",
            ),
            (
                r#"{"abi":1,"headers":[],"resources":[{"name":"R","c_type":"R","destroy":"destroy","close_consumes_on_error":"yes"}],"operations":[]}"#,
                "expected boolean",
            ),
            (
                r#"{"abi":1,"headers":[],"records":[{"name":"A","c_type":"A","fields":[{"name":"x","type":{"record":"A"}}]}],"operations":[]}"#,
                "cyclic or excessively nested",
            ),
        ] {
            let error = Manifest::parse(source).err().expect("invalid contract");
            assert!(error.contains(expected), "{expected}: {error}");
        }
    }
    #[test]
    fn generated_boolean_binding_is_valid_foster() {
        let manifest = Manifest::parse(r#"{"abi":1,"headers":[],"operations":[{"name":"toggle","symbol":"toggle","parameters":["bool"],"result":"bool"}]}"#).unwrap();
        let source = manifest.foster_source(Path::new("C:/fixture.dll")).unwrap();
        crate::compile(&(source + "\nfunc main() -> Int { 42 }\n")).unwrap();
    }
    #[test]
    fn validates_contract_before_generating_code() {
        for source in [
            r#"{"abi":2,"headers":[],"operations":[]}"#,
            r#"{"abi":1,"headers":[],"operations":[],"owenrship":true}"#,
            r#"{"abi":1,"headers":[],"operations":[{"name":"x","symbol":"x","result":"bytes"}]}"#,
        ] {
            assert!(Manifest::parse(source).is_err());
        }
        let manifest=Manifest::parse(r#"{"abi":1,"headers":["fixture.h"],"operations":[{"name":"add","symbol":"add","parameters":["i32","i32"],"result":"i32"}]}"#).unwrap();
        let c = manifest.generate().unwrap();
        assert!(c.contains("INT32_MIN"));
        assert!(c.contains("remaining != 0"));
        assert!(c.contains("target(a0,a1)"));
    }

    #[test]
    fn invalid_record_contracts_fail_before_generation() {
        for records in [
            r#"[{"name":"A","c_type":"struct A","fields":[]}]"#,
            r#"[{"name":"A","c_type":"A","fields":[{"name":"x","type":{"record":"A"}}]}]"#,
            r#"[{"name":"A","c_type":"A","fields":[{"name":"x","type":{"record":"Missing"}}]}]"#,
            r#"[{"name":"A","c_type":"A","fields":[{"name":"x","type":"c_string"}]}]"#,
            r#"[{"name":"A","c_type":"union A","fields":[{"name":"x","type":"i32"}]}]"#,
            r#"[{"name":"A","c_type":"A","fields":[{"name":"x","type":"i32"},{"name":"x","type":"i32"}]}]"#,
        ] {
            let source = format!(r#"{{"abi":1,"headers":[],"records":{records},"operations":[]}}"#);
            assert!(Manifest::parse(&source).is_err(), "{source}");
        }
    }
}
