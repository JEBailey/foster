use std::{fs, path::PathBuf, process::Command};

#[test]
fn independent_tzdata_library_runs_in_vm_and_native() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let package = foster::project::Project::load(root.join("packages/tzdata")).unwrap();
    eprintln!("checking tzdata source");
    let compiled = foster::check_project(&package).unwrap();
    let library = foster::library::build(&compiled).unwrap();
    eprintln!("built tzdata library");
    let encoded = foster::library::encode(&library).unwrap();
    foster::library::decode(&encoded).unwrap();
    let directory = root
        .join("target/tzdata-tests")
        .join(std::process::id().to_string());
    fs::create_dir_all(directory.join("src")).unwrap();
    fs::write(directory.join("tzdata.flib"), &encoded).unwrap();
    fs::write(
        directory.join("foster.toml"),
        "[package]\nname = 'tzdata_consumer'\n[dependencies]\ntzdata = { path = 'tzdata.flib' }\n",
    )
    .unwrap();
    fs::write(
        directory.join("src/main.fos"),
        include_str!("fixtures/programs/tzdata.fos"),
    )
    .unwrap();
    // The consumer's sole dependency is the artifact. It has no source dependency
    // or run-time path to the IANA archive, generator, TZif files, or library source.
    let consumer =
        foster::check_project(&foster::project::Project::load(&directory).unwrap()).unwrap();
    eprintln!("checked compiled-library consumer");
    for optimize in [false, true] {
        eprintln!("compiling VM optimize={optimize}");
        let program =
            foster::vm::compile_with_options(&consumer, foster::vm::CompileOptions { optimize })
                .unwrap();
        let program =
            foster::vm::decode_program(&foster::vm::encode_program(&program).unwrap()).unwrap();
        eprintln!("running VM optimize={optimize}");
        assert_eq!(
            foster::vm::Machine::new(&program).run_main().unwrap(),
            foster::vm::Value::Integer(42)
        );
        let executable =
            directory.join(format!("tzdata-{optimize}{}", std::env::consts::EXE_SUFFIX));
        eprintln!("building native optimize={optimize}");
        foster::native::build_executable(
            &consumer,
            &executable,
            foster::native::CompileOptions { optimize },
        )
        .unwrap();
        let output = Command::new(executable)
            .current_dir(&directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "optimize={optimize}, status={}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
    }
    let vectors = include_str!("fixtures/tzdata-offsets.txt")
        .lines()
        .collect::<String>();
    let reference = include_str!("fixtures/programs/tzdata_reference.fos")
        .replace("REFERENCE_DATA", &serde_json::to_string(&vectors).unwrap());
    for (mode, stride) in [("vm", 59), ("native", 1)] {
        fs::write(
            directory.join("src/main.fos"),
            reference.replace("REFERENCE_STRIDE", &stride.to_string()),
        )
        .unwrap();
        let reference_compilation =
            foster::check_project(&foster::project::Project::load(&directory).unwrap()).unwrap();
        eprintln!("reference comparison: {mode}");
        if mode == "vm" {
            assert_eq!(
                foster::vm::run(&reference_compilation).unwrap(),
                foster::vm::Value::Integer(42)
            );
        } else {
            let executable = directory.join(format!("reference{}", std::env::consts::EXE_SUFFIX));
            foster::native::build_executable(
                &reference_compilation,
                &executable,
                foster::native::CompileOptions::default(),
            )
            .unwrap();
            let output = Command::new(executable)
                .current_dir(&directory)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "status={}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
        }
    }
}
