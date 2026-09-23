use serde::Deserialize;
use std::collections::HashSet;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Reviewed native declarations, separate from ordinary Foster source. Unknown
/// fields are errors: misspelling an ownership contract must never default it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub abi: u64,
    pub headers: Vec<String>,
    #[serde(default)]
    pub sources: Vec<PathBuf>,
    #[serde(default)]
    pub include_directories: Vec<PathBuf>,
    #[serde(default)]
    pub libraries: Vec<PathBuf>,
    #[serde(default)]
    pub resources: Vec<Resource>,
    pub operations: Vec<Operation>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resource {
    pub name: String,
    pub c_type: String,
    /// Must unconditionally destroy the object; must not unwind/longjmp/callback.
    pub destroy: String,
    /// Optional fallible `int close(T*)`; zero is success.
    pub close: Option<String>,
    #[serde(default)]
    pub close_consumes_on_error: bool,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub name: String,
    pub symbol: String,
    #[serde(default)]
    pub parameters: Vec<Scalar>,
    pub result: Scalar,
    pub receiver: Option<String>,
    pub creates: Option<String>,
    /// Bytes results use `uint8_t *fn(..., size_t *length)` and this deallocator.
    pub release: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scalar {
    Void,
    Bool,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
    Bytes,
    CString,
}
impl Scalar {
    fn c(self) -> &'static str {
        match self {
            Self::Void => "void",
            Self::Bool => "_Bool",
            Self::I8 => "int8_t",
            Self::U8 => "uint8_t",
            Self::I16 => "int16_t",
            Self::U16 => "uint16_t",
            Self::I32 => "int32_t",
            Self::U32 => "uint32_t",
            Self::I64 => "int64_t",
            Self::U64 => "uint64_t",
            Self::F32 => "float",
            Self::F64 => "double",
            Self::Bytes => "uint8_t *",
            Self::CString => "const char*",
        }
    }
}
fn identifier(value: &str) -> bool {
    let mut chars = value.bytes();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}
impl Manifest {
    /// Ordinary Foster wrappers keep marshalling and resource tokens private.
    pub fn foster_source(&self, library: &Path) -> Result<String, String> {
        self.validate()?;
        let path = library.to_string_lossy().replace('\\', "/");
        let path = serde_json::to_string(&path).map_err(|e| e.to_string())?;
        let mut source = String::from(
            "//! Generated C bindings. Rebuild with the native bridge.\nimport std.ffi\nimport core.bytes\nimport core.string\nimport core.result\n\n",
        );
        writeln!(
            source,
            "func bridge() -> CBridge {{ CBridge.at({path}, \"{}\") }}\n",
            self.identity()
        )
        .unwrap();
        for resource in &self.resources {
            writeln!(source,"/// Owns a native {} allocation.\npub type Native{} = {{ value: CResource }}\nimpl Native{} {{\n    /// Attempts close, retaining ownership when the C close contract requires it.\n    pub func close(self) -> Result<(), CError> {{ self.value.close() }}\n}}\n",resource.name,resource.name,resource.name).unwrap();
        }
        for (id, op) in self.operations.iter().enumerate() {
            let mut parameters = Vec::new();
            if let Some(receiver) = &op.receiver {
                parameters.push(format!("resource: Native{receiver}"));
            }
            for (i, ty) in op.parameters.iter().enumerate() {
                parameters.push(format!(
                    "p{i}: {}",
                    match ty {
                        Scalar::CString => "String",
                        Scalar::Bytes => "Bytes",
                        Scalar::F32 | Scalar::F64 => "Float",
                        Scalar::Bool => "Bool",
                        _ => "Int",
                    }
                ));
            }
            let result = op
                .creates
                .as_ref()
                .map(|r| format!("Native{r}"))
                .unwrap_or_else(|| {
                    match op.result {
                        Scalar::Void => "()",
                        Scalar::Bytes => "Bytes",
                        Scalar::F32 | Scalar::F64 => "Float",
                        Scalar::Bool => "Bool",
                        _ => "Int",
                    }
                    .into()
                });
            writeln!(source,"/// Calls the reviewed C symbol `{}`.\npub func c_{}({}) -> Result<{result}, CError> {{\n    let arguments = CArguments.empty()",op.symbol,op.name,parameters.join(", ")).unwrap();
            for (i, ty) in op.parameters.iter().enumerate() {
                match ty {
                    Scalar::Bool => writeln!(
                        source,
                        "    arguments.integer(branch {{\n        p{i} -> 1\n        _ -> 0\n    }})"
                    )
                    .unwrap(),
                    Scalar::F32 | Scalar::F64 => {
                        writeln!(source, "    arguments.floating(p{i})").unwrap()
                    }
                    Scalar::Bytes => writeln!(source, "    arguments.bytes(p{i})").unwrap(),
                    Scalar::CString => writeln!(source, "    arguments.c_string(p{i})").unwrap(),
                    _ => writeln!(source, "    arguments.integer(p{i})").unwrap(),
                }
            }
            if let Some(resource) = &op.creates {
                writeln!(source,"    let value = try bridge().create({id}, arguments)\n    Result.Ok(Native{resource} {{ value: move value }})").unwrap();
            } else {
                let receiver = if op.receiver.is_some() {
                    "resource.value"
                } else {
                    "bridge()"
                };
                writeln!(
                    source,
                    "    let value = try {receiver}.call({id}, arguments)"
                )
                .unwrap();
                source.push_str(match op.result {
                    Scalar::Void => "    Result.Ok(())\n",
                    Scalar::Bytes => "    Result.Ok(value.bytes())\n",
                    Scalar::F32 | Scalar::F64 => "    value.floating()\n",
                    Scalar::Bool => "    Result.Ok((try value.integer()) != 0)\n",
                    _ => "    value.integer()\n",
                });
            }
            source.push_str("}\n\n");
        }
        Ok(source)
    }
    /// Deterministic contract identity, not an authenticity/security signature.
    pub fn identity(&self) -> String {
        format!("{:016x}", self.identity_bits())
    }
    fn identity_bits(&self) -> u64 {
        format!("{self:?}")
            .bytes()
            .fold(0xcbf29ce484222325, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
            })
    }
    pub fn parse(source: &str) -> Result<Self, String> {
        let manifest: Self = serde_json::from_str(source).map_err(|e| e.to_string())?;
        manifest.validate()?;
        Ok(manifest)
    }
    fn kind(&self, name: &str) -> Result<usize, String> {
        self.resources
            .iter()
            .position(|r| r.name == name)
            .map(|n| n + 1)
            .ok_or_else(|| format!("unknown C resource `{name}`"))
    }
    fn validate(&self) -> Result<(), String> {
        if self.abi != super::runtime::ABI {
            return Err("unsupported C bridge ABI".into());
        }
        let mut names = HashSet::new();
        for resource in &self.resources {
            for name in [&resource.name, &resource.c_type, &resource.destroy]
                .into_iter()
                .chain(resource.close.iter())
            {
                if !identifier(name) {
                    return Err(format!("invalid C identifier `{name}`"));
                }
            }
            if !names.insert(&resource.name) {
                return Err("duplicate C resource".into());
            }
        }
        names.clear();
        for op in &self.operations {
            if !identifier(&op.name)
                || !identifier(&op.symbol)
                || op.release.as_ref().is_some_and(|s| !identifier(s))
            {
                return Err("invalid C operation identifier".into());
            }
            if !names.insert(&op.name) {
                return Err("duplicate C operation".into());
            }
            if op.parameters.contains(&Scalar::Void) {
                return Err("void is not a C parameter".into());
            }
            if op.result == Scalar::CString {
                return Err("C string results require a length-delimited bytes adapter".into());
            }
            if op.receiver.is_some() && op.creates.is_some() {
                return Err("resource-returning receiver calls are not supported yet".into());
            }
            for name in op.receiver.iter().chain(op.creates.iter()) {
                self.kind(name)?;
            }
            if op.creates.is_some() && op.result != Scalar::Void {
                return Err("constructors use result void plus creates".into());
            }
            if (op.result == Scalar::Bytes) != op.release.is_some() {
                return Err("a copied bytes result requires exactly one release function".into());
            }
        }
        for header in &self.headers {
            if header.is_empty()
                || header
                    .bytes()
                    .any(|c| matches!(c, b'"' | b'\n' | b'\r' | 0))
            {
                return Err("invalid C header name".into());
            }
        }
        Ok(())
    }
    pub fn generate(&self) -> Result<String, String> {
        self.validate()?;
        let mut c = String::from(
            r#"/* Generated Foster C bridge. Do not edit. */
#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>
#include <limits.h>
#include <float.h>
#include <math.h>
#define EXPORT __declspec(dllexport)
_Static_assert(sizeof(void*) == 8, "Foster requires x86-64");
_Static_assert(sizeof(float) == 4 && sizeof(double) == 8, "unsupported float ABI");
static int read64(const uint8_t **p, uint64_t *n, uint64_t *v) {
    if (*n < 8) return 0;
    *v = 0; for (unsigned i=0;i<8;i++) *v |= (uint64_t)(*p)[i] << (8*i);
    *p += 8; *n -= 8; return 1;
}
static void write64(uint8_t *p, uint64_t v) { for(unsigned i=0;i<8;i++) p[i]=(uint8_t)(v >> (8*i)); }
static int c_string_valid(const uint8_t *p, uint64_t n) {
    if(!n || p[n-1] != 0) return 0;
    n--;
    while(n) {
        uint8_t first=*p++; n--;
        if(!first) return 0;
        if(first < 0x80) continue;
        unsigned count; uint32_t value, minimum;
        if(first >= 0xc2 && first <= 0xdf) { count=1; value=first&31; minimum=0x80; }
        else if(first >= 0xe0 && first <= 0xef) { count=2; value=first&15; minimum=0x800; }
        else if(first >= 0xf0 && first <= 0xf4) { count=3; value=first&7; minimum=0x10000; }
        else return 0;
        if(n < count) return 0;
        n -= count;
        while(count--) { uint8_t next=*p++; if((next&0xc0)!=0x80) return 0; value=(value<<6)|(next&63); }
        if(value < minimum || value > 0x10ffff || (value >= 0xd800 && value <= 0xdfff)) return 0;
    }
    return 1;
}
EXPORT uint64_t foster_c_abi(void) { return 1; }
"#,
        );
        for header in &self.headers {
            writeln!(c, "#include \"{header}\"").unwrap();
        }
        writeln!(
            c,
            "EXPORT uint64_t foster_c_schema(void) {{ return UINT64_C({}); }}",
            self.identity_bits()
        )
        .unwrap();
        c.push_str("EXPORT uint64_t foster_c_describe(uint32_t op) { switch(op) {\n");
        for (index, op) in self.operations.iter().enumerate() {
            let (mode, kind) = if let Some(name) = &op.creates {
                (2, self.kind(name)?)
            } else if let Some(name) = &op.receiver {
                (3, self.kind(name)?)
            } else {
                (1, 0)
            };
            let mode = mode | if op.result == Scalar::Bytes { 256 } else { 0 };
            writeln!(c, "case {index}: return ((uint64_t){kind} << 32) | {mode};").unwrap();
        }
        c.push_str("default: return 0; } }\nEXPORT void foster_c_destroy(uint32_t kind, void *p) { switch(kind) {\n");
        for (i, r) in self.resources.iter().enumerate() {
            writeln!(
                c,
                "case {}: {{ void (*destroy)({}*) = &{}; destroy(({}*)p); return; }}",
                i + 1,
                r.c_type,
                r.destroy,
                r.c_type
            )
            .unwrap();
        }
        c.push_str("default: abort(); } }\nEXPORT int64_t foster_c_close(uint32_t kind, void *p, uint8_t *consumed) { switch(kind) {\n");
        for (i, r) in self.resources.iter().enumerate() {
            if let Some(close) = &r.close {
                writeln!(c, "case {}: {{ int (*close)({}*) = &{}; int64_t status = close(({}*)p); *consumed = (status == 0 || {}); return status; }}", i+1, r.c_type, close, r.c_type, u8::from(r.close_consumes_on_error)).unwrap();
            } else {
                writeln!(
                    c,
                    "case {}: {{ void (*destroy)({}*) = &{}; destroy(({}*)p); *consumed=1; return 0; }}",
                    i + 1,
                    r.c_type,
                    r.destroy,
                    r.c_type
                )
                .unwrap();
            }
        }
        c.push_str("default: *consumed=0; return -1; } }\nEXPORT int32_t foster_c_call(uint32_t op, void *resource, const uint8_t *input, uint64_t remaining, uint8_t *out, uint64_t capacity, uint64_t *length, void **created) {\n*length=0; *created=NULL; if(capacity < 8) return 1; switch(op) {\n");
        for (index, op) in self.operations.iter().enumerate() {
            writeln!(c, "case {index}: {{").unwrap();
            let mut types = Vec::new();
            if let Some(name) = &op.receiver {
                types.push(format!("{}*", self.resources[self.kind(name)? - 1].c_type));
            }
            for ty in &op.parameters {
                if *ty == Scalar::Bytes {
                    types.extend(["const uint8_t*".into(), "size_t".into()]);
                } else {
                    types.push(ty.c().into());
                }
            }
            if op.result == Scalar::Bytes {
                types.push("size_t*".into());
            }
            if types.is_empty() {
                types.push("void".into());
            }
            let return_type = if let Some(name) = &op.creates {
                format!("{}*", self.resources[self.kind(name)? - 1].c_type)
            } else {
                op.result.c().into()
            };
            writeln!(
                c,
                "{return_type} (*target)({}) = &{};",
                types.join(","),
                op.symbol
            )
            .unwrap();
            let mut args = Vec::new();
            if let Some(name) = &op.receiver {
                args.push(format!(
                    "({}*)resource",
                    self.resources[self.kind(name)? - 1].c_type
                ));
            }
            for (i, ty) in op.parameters.iter().enumerate() {
                writeln!(
                    c,
                    "uint64_t w{i}; if(!read64(&input,&remaining,&w{i})) return 2;"
                )
                .unwrap();
                match ty {
                    Scalar::CString => {
                        writeln!(c, "if(w{i} > remaining || !c_string_valid(input,w{i})) return 2; const char *a{i}=(const char*)input; input += w{i}; remaining -= w{i};").unwrap();
                        args.push(format!("a{i}"));
                        continue;
                    }
                    Scalar::Bytes => {
                        writeln!(c, "if(w{i} > remaining || w{i} > SIZE_MAX) return 2; const uint8_t *a{i}=input; input += w{i}; remaining -= w{i};").unwrap();
                        args.push(format!("a{i}"));
                        args.push(format!("(size_t)w{i}"));
                        continue;
                    }
                    Scalar::F32 | Scalar::F64 => {
                        writeln!(c, "double d{i}; memcpy(&d{i},&w{i},8);").unwrap();
                        if *ty == Scalar::F32 {
                            writeln!(c,"if(isfinite(d{i}) && (d{i} > FLT_MAX || d{i} < -FLT_MAX)) return 3;").unwrap();
                        }
                        writeln!(c, "{} a{i} = ({})d{i};", ty.c(), ty.c()).unwrap();
                    }
                    Scalar::I8 | Scalar::I16 | Scalar::I32 | Scalar::I64 => {
                        writeln!(c, "int64_t s{i}; memcpy(&s{i},&w{i},8);").unwrap();
                        let range = match ty {
                            Scalar::I8 => Some(("INT8_MIN", "INT8_MAX")),
                            Scalar::I16 => Some(("INT16_MIN", "INT16_MAX")),
                            Scalar::I32 => Some(("INT32_MIN", "INT32_MAX")),
                            _ => None,
                        };
                        if let Some((min, max)) = range {
                            writeln!(c, "if(s{i} < {min} || s{i} > {max}) return 3;").unwrap();
                        }
                        writeln!(c, "{} a{i}=({})s{i};", ty.c(), ty.c()).unwrap();
                    }
                    _ => {
                        let max = match ty {
                            Scalar::Bool => "1",
                            Scalar::U8 => "UINT8_MAX",
                            Scalar::U16 => "UINT16_MAX",
                            Scalar::U32 => "UINT32_MAX",
                            _ => "UINT64_MAX",
                        };
                        writeln!(
                            c,
                            "if(w{i} > {max}) return 3; {} a{i}=({})w{i};",
                            ty.c(),
                            ty.c()
                        )
                        .unwrap();
                    }
                }
                args.push(format!("a{i}"));
            }
            c.push_str("if(remaining != 0) return 2;\n");
            if op.result == Scalar::Bytes {
                c.push_str("size_t size=0;\n");
                args.push("&size".into());
            }
            let call = format!("target({})", args.join(","));
            if let Some(name) = &op.creates {
                writeln!(
                    c,
                    "{} *result={call}; *created=result; return result ? 0 : 4;",
                    self.resources[self.kind(name)? - 1].c_type
                )
                .unwrap();
            } else {
                match op.result {
                    Scalar::Void => {
                        writeln!(c, "{call}; return 0;").unwrap();
                    }
                    Scalar::Bytes => {
                        writeln!(c,"uint8_t *result={call}; if(!result && size) return 4; if(size > capacity) {{ if(result) {}(result); return 5; }} if(size) memcpy(out,result,size); if(result) {}(result); *length=size; return 0;",op.release.as_ref().unwrap(),op.release.as_ref().unwrap()).unwrap();
                    }
                    Scalar::F32 | Scalar::F64 => {
                        writeln!(c,"double result={call}; uint64_t bits; memcpy(&bits,&result,8); write64(out,bits); *length=8; return 0;").unwrap();
                    }
                    _ => {
                        writeln!(
                            c,
                            "{} result={call}; write64(out,(uint64_t)result); *length=8; return 0;",
                            op.result.c()
                        )
                        .unwrap();
                    }
                }
            }
            c.push_str("}\n");
        }
        c.push_str("default: return 6; } }\n");
        Ok(c)
    }
}

/// Always rebuilds, deliberately avoiding a stale cache when transitive headers,
/// compiler options or linked libraries change. Invocation never uses a shell.
pub fn build(manifest_path: &Path, output: &Path, compiler: &Path) -> Result<PathBuf, String> {
    if !cfg!(all(windows, target_arch = "x86_64")) {
        return Err("C bridges currently require Windows x86-64".into());
    }
    let manifest_path = manifest_path.canonicalize().map_err(|e| e.to_string())?;
    let root = manifest_path.parent().unwrap();
    let manifest =
        Manifest::parse(&std::fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?)?;
    let output = std::path::absolute(output).map_err(|e| e.to_string())?;
    if output.extension().and_then(|value| value.to_str()) != Some("dll") {
        return Err("C bridge output must have a .dll extension".into());
    }
    std::fs::create_dir_all(output.parent().unwrap()).map_err(|e| e.to_string())?;
    let source = output.with_extension("bridge.c");
    std::fs::write(&source, manifest.generate()?).map_err(|e| e.to_string())?;
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
    for include in &manifest.include_directories {
        command.arg("-I").arg(root.join(include));
    }
    for path in manifest.sources.iter().chain(&manifest.libraries) {
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
    std::fs::write(
        output.with_extension("fos"),
        manifest.foster_source(&output)?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(output.with_extension("schema"), manifest.identity())
        .map_err(|e| e.to_string())?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
