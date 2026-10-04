//! Compiler command adapter for the Foster-written C binding tool.
//! JSON, validation, generation, file I/O, and Clang invocation live in bridge.fos.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub struct Manifest {
    source: String,
    schema: String,
    c_source: String,
}
fn generator() -> Result<&'static crate::vm::VerifiedProgram, String> {
    static PROGRAM: OnceLock<Result<crate::vm::VerifiedProgram, String>> = OnceLock::new();
    PROGRAM.get_or_init(|| {
        let source = concat!(
            include_str!("../../tools/cbind/src/bridge.fos"),
            "\nfunc main(args: Arguments) -> List<String> {\nbranch args.values[0] {\n\"build\" -> [build(args.values[1], args.values[2], args.values[3], args.values[4])]\n_ -> generate_bridge(args.values[1], args.values[2])\n}\n}\n"
        );
        let compilation = crate::compile(source).map_err(|e| e.to_string())?;
        crate::vm::compile(&compilation).and_then(|program| program.into_verified()).map_err(|e| e.to_string())
    }).as_ref().map_err(Clone::clone)
}
fn invoke(arguments: Vec<String>) -> Result<Vec<String>, String> {
    let args = crate::entry::CommandArguments::new("foster bridge", arguments);
    let value = crate::vm::Machine::new(generator()?)
        .run_main_with_arguments(&args)
        .map_err(|e| e.to_string())?;
    value
        .as_list()
        .ok_or("bridge generator returned a non-list")?
        .iter()
        .map(|value| {
            value
                .as_string()
                .map(str::to_owned)
                .ok_or_else(|| "bridge generator returned a non-string artifact".to_owned())
        })
        .collect()
}
fn generate(source: &str, path: &Path) -> Result<Vec<String>, String> {
    let parts = invoke(vec![
        "generate".into(),
        source.into(),
        path.to_string_lossy().replace('\\', "/"),
    ])?;
    if parts.len() != 3 {
        return Err("invalid bridge generator response".into());
    }
    Ok(parts)
}
impl Manifest {
    pub fn parse(source: &str) -> Result<Self, String> {
        let parts = generate(source, Path::new(""))?;
        Ok(Self {
            source: source.into(),
            schema: parts[0].clone(),
            c_source: parts[1].clone(),
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
        Ok(generate(&self.source, path)?[2].clone())
    }
}
/// Dispatch the existing compiler command to the same Foster implementation as cbind.
/// `module_path` names the bridge for the runtime loader; relative names resolve
/// at run time against `FOSTER_BRIDGE_DIR`, the current directory, and the
/// executable directory.
pub fn build(
    manifest_path: &Path,
    output: &Path,
    compiler: &Path,
    module_path: &str,
) -> Result<PathBuf, String> {
    if !cfg!(all(windows, target_arch = "x86_64")) {
        return Err("C bridges currently require Windows x86-64".into());
    }
    let parts = invoke(vec![
        "build".into(),
        manifest_path.to_string_lossy().into_owned(),
        output.to_string_lossy().into_owned(),
        compiler.to_string_lossy().into_owned(),
        module_path.to_owned(),
    ])?;
    if parts.len() != 1 {
        return Err("invalid bridge build response".into());
    }
    Ok(PathBuf::from(&parts[0]))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_names_adjust_initial_case_and_escape_reserved_names() {
        let manifest = Manifest::parse(
            r#"{
            "abi":1,"headers":[],
            "enums":[{"name":"cMode","c_type":"CMode"}],
            "records":[{"name":"color","c_type":"Color","fields":[
                {"name":"type","type":"i32"},
                {"name":"type_","type":"i32"},
                {"name":"c_type","type":"i32"},
                {"name":"copy","type":"i32"}
            ]}],
            "constants":[{"name":"C_ANSWER","type":"i32","value":42}],
            "operations":[
                {"name":"c_original","symbol":"c_original","parameters":[],"result":"i32"},
                {"name":"Type","symbol":"Type","parameters":[],"result":"i32"},
                {"name":"RoundTrip","symbol":"RoundTrip","parameters":[{"record":"color"}],"result":{"record":"color"}}
            ]
        }"#,
        )
        .unwrap();
        let source = manifest.foster_source(Path::new("unused.dll")).unwrap();
        assert!(source.contains("pub type Color ="));
        assert!(source.contains("pub type CMode = Int"));
        assert!(source.contains("pub const C_ANSWER = 42"));
        assert!(source.contains("pub func c_original()"));
        assert!(source.contains("pub func type_()"));
        assert!(source.contains("pub func roundTrip(p0: Color) -> Result<Color, CError>"));
        assert!(source.contains("Calls the C symbol `RoundTrip`"));
        let program = crate::compile(&(source + "\nfunc main() -> Int {\nlet color = Color { type_: 1, type__: 2, c_type: 3, copy_: 4 }\nlet copied = color.copy()\nassert(copied.type_ + copied.type__ + copied.c_type + copied.copy_ == 10)\nC_ANSWER\n}\n")).unwrap();
        assert_eq!(crate::vm::run(&program).unwrap().to_string(), "42");
    }
    #[test]
    fn generated_names_reject_initial_case_collisions() {
        for input in [
            r#"{"abi":1,"headers":[],"enums":[{"name":"mode","c_type":"mode"},{"name":"Mode","c_type":"Mode"}],"operations":[]}"#,
            r#"{"abi":1,"headers":[],"operations":[{"name":"Foo","symbol":"Foo","parameters":[],"result":"void"},{"name":"foo","symbol":"foo","parameters":[],"result":"void"}]}"#,
            r#"{"abi":1,"headers":[],"records":[{"name":"Point","c_type":"Point","fields":[{"name":"x","type":"i32"}]}],"constants":[{"name":"ORIGIN","type":{"record":"Point"},"value":{"x":0}}],"operations":[{"name":"oRIGIN","symbol":"oRIGIN","parameters":[],"result":"void"}]}"#,
        ] {
            assert!(
                Manifest::parse(input)
                    .err()
                    .expect("case collision")
                    .contains("duplicate generated Foster")
            );
        }
    }
    #[test]
    fn large_array_results_decode_in_separate_statements() {
        let manifest = Manifest::parse(r#"{"abi":1,"headers":[],"records":[{"name":"Large","c_type":"Large","fields":[{"name":"items","type":{"array":"f32","length":80}}]}],"operations":[{"name":"large","symbol":"large","parameters":["i32"],"parameter_names":["slot0"],"result":{"record":"Large"}}]}"#).unwrap();
        let source = manifest.foster_source(Path::new("large.dll")).unwrap();
        assert!(source.contains("large(slot0_: Int)"));
        assert!(source.contains("let slot79 = (try (try value.slot(79)).floating())"));
        assert!(source.contains("Result.Ok(Large { items: [slot0, slot1"));
        crate::compile(&(source + "\nfunc main() -> Int { 42 }")).unwrap();
    }
    #[test]
    fn fixed_arrays_and_enum_aliases_generate_checked_value_bindings() {
        let manifest = Manifest::parse(r#"{"abi":1,"headers":[],"enums":[{"name":"Mode","c_type":"enum Mode"}],"records":[{"name":"Array","c_type":"Array","fields":[{"name":"grid","type":{"array":{"array":"f32","length":2},"length":2}},{"name":"mode","type":{"enum":"Mode"}}]}],"operations":[{"name":"roundtrip","symbol":"roundtrip","parameters":[{"record":"Array"}],"result":{"record":"Array"}}]}"#).unwrap();
        let source = manifest.foster_source(Path::new("sample.dll")).unwrap();
        let compiled = crate::compile(&(source.clone() + "\nfunc main() -> Int { let a = Array { grid: [[1.0,2.0],[3.0,4.0]], mode: 3 }\nlet b = a.copy()\nassert(b.grid[1][0] == 3.0)\n42 }")).unwrap();
        assert_eq!(crate::vm::run(&compiled).unwrap().to_string(), "42");
        assert!(source.contains("pub type Mode = Int"));
        assert!(source.contains("C array length mismatch"));
        assert!(manifest.generate().unwrap().contains("float (*)[2][2]"));
        for ty in [
            r#"{"array":"i32","length":0}"#,
            r#"{"array":"i32","length":-1}"#,
            r#"{"array":"i32","length":8192}"#,
            r#"{"array":"void","length":2}"#,
            r#"{"array":{"array":"i32","length":100},"length":100}"#,
            r#"{"enum":"Missing"}"#,
        ] {
            let input = format!(
                r#"{{"abi":1,"headers":[],"records":[{{"name":"A","c_type":"A","fields":[{{"name":"x","type":{ty}}}]}}],"operations":[]}}"#
            );
            assert!(Manifest::parse(&input).is_err(), "{ty}");
        }
    }
    #[test]
    fn json_adapter_preserves_contract_data_and_rejects_duplicate_keys() {
        for source in [
            r#"{"abi":1,"abi":2}"#,
            r#"{"operations":[{"name":"a","name":"b"}]}"#,
        ] {
            let error = Manifest::parse(source).err().expect("duplicate JSON key");
            assert!(error.contains("duplicate object key"), "{error}");
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
    fn generated_string_literals_preserve_controls_and_combining_marks() {
        let text = (0..32).map(char::from).collect::<String>() + "\"\u{301}\\\u{301}λ🙂";
        let input = serde_json::json!({
            "abi": 1, "headers": [], "operations": [],
            "constants": [{"name": "TEXT", "type": "string", "value": text}]
        })
        .to_string();
        let manifest = Manifest::parse(&input).unwrap();
        let source = manifest
            .foster_source(Path::new("quote\"\u{301}.dll"))
            .unwrap();
        let compilation = crate::compile(&(source + "\nfunc main() -> String { TEXT }\n")).unwrap();
        for optimize in [false, true] {
            let value =
                crate::vm::run_with_options(&compilation, crate::vm::CompileOptions { optimize })
                    .unwrap();
            assert_eq!(value.as_string(), Some(text.as_str()));
        }
    }
    #[test]
    fn constants_and_parameter_metadata_generate_valid_foster() {
        let manifest = Manifest::parse(r#"{
            "abi":1,"headers":[],
            "records":[{"name":"Point","c_type":"Point","docs2":"A point.","fields":[{"name":"x","type":"f64","docs2":"Horizontal coordinate."}]}],
            "constants":[
                {"name":"ORIGIN","type":{"record":"Point"},"value":{"x":0}},
                {"name":"ANSWER","type":"i64","value":42},
                {"name":"TEXT","type":"string","value":"quote \" and newline\n"}
            ],
            "operations":[{"name":"names","symbol":"names","parameters":["i32","i32","i32","i32","i32"],"parameter_names":["arguments","value","result","type","c_type"],"result":"void","docs2":"First line.\nSecond line."}]
        }"#).unwrap();
        let source = manifest.foster_source(Path::new("unused.dll")).unwrap();
        assert!(
            source.contains("arguments_: Int, value_: Int, result_: Int, type_: Int, c_type: Int")
        );
        assert!(source.contains("/// First line.\n/// Second line."));
        let program = crate::compile(
            &(source + "\nfunc main() -> Int { assert(oRIGIN().x == 0.0)\nANSWER }\n"),
        )
        .unwrap();
        assert_eq!(crate::vm::run(&program).unwrap().to_string(), "42");
        for extra in [
            r#""constants":[{"name":"N","type":"i64","value":1.5}]"#,
            r#""constants":[{"name":"N","type":"i64","value":"42"}]"#,
            r#""constants":[{"name":"N","type":"bytes","value":42}]"#,
            r#""constants":[{"name":"N","type":"i64","value":42,"docs2":0}]"#,
        ] {
            let input = format!(r#"{{"abi":1,"headers":[],"operations":[],{extra}}}"#);
            assert!(Manifest::parse(&input).is_err(), "{input}");
        }
        assert!(Manifest::parse(r#"{"abi":1,"headers":[],"operations":[{"name":"x","symbol":"x","parameters":["i32"],"parameter_names":["x","y"],"result":"void"}]}"#).is_err());
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
