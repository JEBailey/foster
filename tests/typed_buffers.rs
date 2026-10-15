#[test]
fn buffer_views_cannot_escape_the_owner() {
    let source = r#"
import core.buffer
import core.buffer.Buffer
import core.buffer.BufferView
func escaped(owner: ref[owner] Buffer<Int>) -> BufferView<ref[owner] Buffer<Int>> {
    let local = buffer::integers(4, 0)
    local.view(0, 4)
}
func main() -> Int {
    let owner = buffer::integers(4, 0)
    escaped(ref owner).read(0)
}
"#;
    assert!(foster::compile(source).is_err());
}

#[test]
fn buffer_ownership_and_element_types_are_checked() {
    for body in [
        "let b = buffer::integers(4, 0)\nlet v = b.view(0, 4)\nlet moved = move b\nv.read(0)",
        "let b = buffer::integers(4, 0)\nb.write(0, 1.5)\n0",
        "let b = buffer::integers(4, 0)\nb.copy()\n0",
        "let b = buffer::integers(4, 0)\nb.storage.clear()\n0",
    ] {
        let source = format!("import core.buffer\nfunc main() -> Int {{\n{body}\n}}");
        assert!(
            foster::compile(&source).is_err(),
            "unexpectedly compiled {body}"
        );
    }
}
#[test]
fn scalar_buffer_access_lowers_to_checked_indexing() {
    let compilation = foster::compile(
        r#"
import core.buffer
func main() -> Int {
    let values = buffer::integers(4, 0)
    values.write(3, 42)
    values.read(3)
}

"#,
    )
    .unwrap();
    let ir = foster::native::emit_ir(&compilation).unwrap();
    assert!(
        ir.contains("borrowed_field_index"),
        "missing borrowed checked read: {ir}"
    );
    assert!(ir.contains("StoreIndex {"), "missing checked write: {ir}");
    assert!(
        !ir.contains("function Buffer.read("),
        "read wrapper remains: {ir}"
    );
    assert!(
        !ir.contains("function Buffer.write("),
        "write wrapper remains: {ir}"
    );
    assert!(
        !ir.contains("CallContractMethod"),
        "scalar copy dispatch remains: {ir}"
    );
}

#[test]
fn borrowed_buffer_reads_do_not_create_owned_backing_temporaries() {
    let compilation = foster::compile(include_str!(
        "fixtures/programs/typed_buffer_read_cleanup.fos"
    ))
    .unwrap();
    let ir = foster::native::emit_ir(&compilation).unwrap();
    assert!(
        ir.contains("borrowed_field_index"),
        "missing borrowed checked read: {ir}"
    );
    assert!(
        !ir.contains("field: \"storage\", by_reference: false"),
        "owned backing temporary remains: {ir}"
    );
}
