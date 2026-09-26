//! Build-time-compiled Foster tools, decoded and verified once per process.
use std::sync::OnceLock;

use crate::{entry::CommandArguments, error::FosterError, vm};

pub(crate) struct Tool {
    bytecode: &'static [u8],
    program: OnceLock<Result<vm::Program, String>>,
}

impl Tool {
    pub(crate) const fn new(bytecode: &'static [u8]) -> Self {
        Self {
            bytecode,
            program: OnceLock::new(),
        }
    }

    pub(crate) fn run(&self, arguments: Vec<String>) -> Result<vm::Value, FosterError> {
        let program = self
            .program
            .get_or_init(|| vm::decode_program(self.bytecode).map_err(|error| error.to_string()))
            .as_ref()
            .map_err(|error| {
                FosterError::runtime(format!("cannot initialize Foster tool: {error}"))
            })?;
        let value = vm::Machine::new(program)
            .run_main_with_arguments(&CommandArguments::new("foster", arguments))?;
        match value {
            vm::Value::Variant {
                alternative,
                mut payload,
                ..
            } if alternative.as_ref() == "Ok" => payload
                .pop()
                .ok_or_else(|| FosterError::runtime("Foster tool returned an empty success")),
            vm::Value::Variant {
                alternative,
                payload,
                ..
            } if alternative.as_ref() == "Error" => Err(FosterError::runtime(
                payload
                    .first()
                    .and_then(vm::Value::as_string)
                    .unwrap_or("Foster tool returned an invalid error"),
            )),
            _ => Err(FosterError::runtime(
                "Foster tool returned an invalid result",
            )),
        }
    }
}

pub(crate) fn string(value: &vm::Value) -> Result<String, FosterError> {
    value
        .as_string()
        .map(str::to_owned)
        .ok_or_else(|| FosterError::runtime("Foster tool returned a non-string value"))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn initialization_preserves_existing_source_and_rejects_existing_manifest() {
        let root = std::env::temp_dir().join(format!(
            "foster-init-preserve-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.fos"), "func main() { 42 }\n").unwrap();
        crate::init_project(&root, Some("sample-app")).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("src/main.fos")).unwrap(),
            "func main() { 42 }\n"
        );
        let manifest = fs::read(root.join("foster.toml")).unwrap();
        assert!(
            crate::init_project(&root, Some("replacement"))
                .unwrap_err()
                .message
                .contains("already exists")
        );
        assert_eq!(fs::read(root.join("foster.toml")).unwrap(), manifest);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_project_names_do_not_write_a_manifest() {
        let root = std::env::temp_dir().join(format!(
            "foster-init-invalid-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for name in ["", "bad name", "bad/name", "λ"] {
            assert!(
                crate::init_project(&root, Some(name))
                    .unwrap_err()
                    .message
                    .contains("ASCII letters")
            );
            assert!(!root.join("foster.toml").exists());
        }
        fs::remove_dir_all(root).unwrap();
    }
}
