use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

#[test]
fn execution_packages_build_without_compiler_dependencies() {
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let packages = metadata["packages"].as_array().unwrap();
    let names: BTreeSet<_> = packages
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        BTreeSet::from([
            "foster",
            "foster-bootstrap",
            "foster-compiler",
            "foster-vm",
            "foster-bytecode",
            "foster-native-runtime",
            "foster-host",
        ])
    );
    let dependencies: BTreeMap<_, Vec<_>> = packages
        .iter()
        .map(|package| {
            (
                package["name"].as_str().unwrap(),
                package["dependencies"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|d| d["kind"] != "dev")
                    .map(|d| d["name"].as_str().unwrap())
                    .collect(),
            )
        })
        .collect();
    for root in [
        "foster-vm",
        "foster-bytecode",
        "foster-native-runtime",
        "foster-host",
    ] {
        let mut pending = vec![root];
        let mut visited = BTreeSet::new();
        while let Some(package) = pending.pop() {
            if !visited.insert(package) {
                continue;
            }
            assert!(
                !matches!(package, "foster" | "foster-compiler" | "foster-bootstrap"),
                "{root} depends on {package}"
            );
            if let Some(children) = dependencies.get(package) {
                pending.extend(children.iter().copied());
            }
        }
    }
}
