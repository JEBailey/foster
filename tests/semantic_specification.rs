//! Editorial integrity checks; behavioral conformance lives in the specification's witness suites.

const SPECIFICATION: &str = include_str!("../docs/semantics.md");

#[test]
fn semantic_specification_tracks_the_language_and_ownership_baseline() {
    let baseline = format!(
        "language version {}, ownership-model version {}",
        foster::ownership::LANGUAGE_VERSION,
        foster::ownership::MODEL_VERSION,
    );
    assert!(SPECIFICATION.contains(&baseline));
}

#[test]
fn semantic_specification_has_unique_ordered_rule_ids() {
    let identifiers = SPECIFICATION
        .lines()
        .filter_map(|line| line.strip_prefix("**S-"))
        .map(|line| line.split_whitespace().next().unwrap())
        .collect::<Vec<_>>();
    let expected = (1..=22).map(|id| format!("{id:02}")).collect::<Vec<_>>();
    assert_eq!(identifiers, expected);
}

#[test]
fn semantic_specification_link_paths_exist() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs");
    for (index, marker) in SPECIFICATION.match_indices("](") {
        let after = &SPECIFICATION[index + marker.len()..];
        let target = after.split_once(')').unwrap().0;
        let path = target.split('#').next().unwrap();
        assert!(
            directory.join(path).exists(),
            "missing specification link: {target}"
        );
    }
}
