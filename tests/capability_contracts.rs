//! Narrow capability views do not imply the rest of a combined contract.

#[test]
fn narrow_lookup_keeps_storage_loans_live() {
    let declarations = include_str!("fixtures/programs/membership_contracts.fos")
        .split_once("func main()")
        .unwrap()
        .0;
    let source = format!(
        r#"{declarations}
func main() -> Int {{
    let provider = LookupOnly {{ item: Item {{ value: 40, text: "original" }} }}
    branch lookup(provider, "answer") {{
        Option.Some(item) -> {{
            provider.item = Item {{ value: 42, text: "replacement" }}
            item.value
        }}
        Option.None -> 0
    }}
}}
"#
    );
    assert!(
        foster::compile(&source).is_err(),
        "replacing borrowed storage must fail through a narrow lookup view"
    );
}

#[test]
fn lookup_and_membership_do_not_imply_full_collections() {
    let declarations = include_str!("fixtures/programs/membership_contracts.fos")
        .split_once("func main()")
        .unwrap()
        .0;
    for expression in [
        "full_map(LookupOnly { item: Item { value: 42, text: \"owned\" } })",
        "full_map(KeysOnly {})",
        "full_set(MembersOnly {})",
    ] {
        let source = format!("{declarations}\nfunc main() -> Int {{ {expression} }}");
        assert!(
            foster::compile(&source).is_err(),
            "unexpected collection conformance: {expression}"
        );
    }
}

#[test]
fn partial_capabilities_do_not_satisfy_combined_contracts() {
    let declarations = include_str!("fixtures/programs/capability_contracts.fos")
        .split_once("func main()")
        .unwrap()
        .0;
    for expression in [
        "collection_size(CountOnly { count: 42 })",
        "connected(ClientOnly {})",
        "listening(ServerOnly {})",
    ] {
        let source = format!("{declarations}\nfunc main() -> Int {{ {expression} }}");
        assert!(
            foster::compile(&source).is_err(),
            "a partial capability must not satisfy its combined contract: {expression}"
        );
    }
}
