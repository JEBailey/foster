use foster::vm::Value;
use std::path::Path;

#[test]
fn remote_objects_process_methods_on_virtual_threads() {
    let source = r#"
type Counter = {
    value: Int
}

func increment(self: Counter, amount: Int) -> Int [mut self] {
    self.value = self.value + amount
    self.value
}

func main() -> Int {
    counter = remote Counter { value: 0 }
    first = counter.increment(2)
    second = counter.increment(3)
    await first + await second
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(7));
}

#[test]
fn remote_read_loans_observe_owner_mutation() {
    let source = r#"
type Counter = {
    value: Int
}

func snapshot(self: Counter) -> Int [read self.value] {
    self.value
}

func assign(self: Counter, value: Int) -> Int [mut self] {
    self.value = value
    self.value
}

func main() -> Int {
    counter = Counter { value: 0 }
    reader = remote ref counter
    before = await reader.snapshot()
    counter.assign(42)
    after = await reader.snapshot()
    before + after
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn remote_read_loans_serialize_reads_with_owner_methods() {
    let source = r#"
type Pair = {
    left: Int
    right: Int
}

func total(self: Pair) -> Int [read self.left, read self.right] {
    self.left + self.right
}

func replace(self: Pair, value: Int) -> Int [mut self] {
    self.left = value
    self.right = value
    self.left + self.right
}

func main() -> Int {
    pair = Pair { left: 0, right: 0 }
    reader = remote ref pair
    pending = reader.total()
    pair.replace(21)
    observed = await pending
    after = await reader.total()
    branch observed {
        0 -> after
        42 -> after
        _ -> -1
    }
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn remote_read_loans_reject_mutating_methods() {
    let source = r#"
type Counter = { value: Int }

func increment(self: Counter) -> Int [mut self] {
    self.value = self.value + 1
    self.value
}

func main() {
    counter = Counter { value: 0 }
    reader = remote ref counter
    reader.increment()
}
"#;
    let error = foster::compile(source).unwrap_err();
    assert!(
        error
            .message
            .contains("read-only remote loan cannot call mutating method `increment`")
    );
}

#[test]
fn remote_borrowed_arguments_are_live_read_only_loans() {
    let source = r#"
type Document = { value: Int }
type Inspector = {}

func inspect(self: Inspector, document: Document) -> Int [read document.value] {
    document.value
}

func assign(self: Document, value: Int) -> Int [mut self] {
    self.value = value
    self.value
}

func main() -> Int {
    document = Document { value: 0 }
    inspector = remote Inspector {}
    before = await inspector.inspect(document)
    document.assign(42)
    after = await inspector.inspect(document)
    before + after
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn remote_borrowed_arguments_serialize_with_owner_mutation() {
    let source = r#"
type Pair = { left: Int, right: Int }
type Inspector = {}

func total(self: Inspector, pair: Pair) -> Int [read pair.left, read pair.right] {
    pair.left + pair.right
}

func replace(self: Pair, value: Int) -> Int [mut self] {
    self.left = value
    self.right = value
    self.left + self.right
}

func main() -> Int {
    pair = Pair { left: 0, right: 0 }
    inspector = remote Inspector {}
    pending = inspector.total(pair)
    pair.replace(21)
    observed = await pending
    branch observed {
        0 -> 42
        42 -> 42
        _ -> -1
    }
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn remote_borrowed_arguments_reject_mutation() {
    let source = r#"
type Document = { value: Int }
type Worker = {}

func rewrite(self: Worker, document: Document) -> Int [mut document] {
    document.value = 42
    document.value
}

func main() {
    document = Document { value: 0 }
    worker = remote Worker {}
    worker.rewrite(document)
}
"#;
    let error = foster::compile(source).unwrap_err();
    assert!(
        error
            .message
            .contains("remote borrowed parameter `document` may only have read effects")
    );
}

#[test]
fn remote_calls_reject_borrowed_messages() {
    let source = r#"
type Box = { value: Int }

func read[g: group Int](self: Box, value: ref[g] Int) -> Int {
    value
}

func main() {
    box = remote Box { value: 0 }
    values = [1]
    box.read(ref values[0])
}
"#;
    let error = foster::compile(source).unwrap_err();
    assert!(
        error
            .message
            .contains("cannot cross a remote-object boundary")
    );
}

#[test]
fn remote_calls_require_moves_for_consumed_messages() {
    let source = r#"
type Worker = {}

func submit(self: Worker, message: String) -> Unit [consume message] {
    println(message)
}

func main() {
    worker = remote Worker {}
    message = "owned"
    worker.submit(message)
}
"#;
    let error = foster::compile(source).unwrap_err();
    assert!(error.message.contains("pass this argument with `move`"));

    foster::compile(&source.replace("submit(message)", "submit(move message)")).unwrap();
}

#[test]
fn derives_and_checks_group_mutation_effects() {
    let source = r#"
func replace[g: group Int](value: ref[g] Int) -> Int {
    value = 2
    value
}
func main() { 0 }
"#;
    let compilation = foster::compile(source).unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    let replace = compilation.hir.function_named(module, "replace").unwrap();
    assert!(
        compilation.hir.functions[replace]
            .effects
            .iter()
            .any(|effect| effect.kind == foster::ast::EffectKind::Mut)
    );
}

#[test]
fn propagates_declared_effects_through_calls() {
    let source = r#"
func replace[g: group Int](value: ref[g] Int) -> Int [mut g] {
    value = 2
    value
}
func wrapper[g: group Int](value: ref[g] Int) -> Int {
    replace(ref value)
}
func main() { 0 }
"#;
    let compilation = foster::compile(source).unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    let wrapper = compilation.hir.function_named(module, "wrapper").unwrap();
    assert!(
        compilation.hir.functions[wrapper]
            .effects
            .iter()
            .any(|effect| effect.kind == foster::ast::EffectKind::Mut)
    );
}

#[test]
fn derives_suspend_from_await_and_callee_contracts() {
    let source = r#"
type Worker = {}
func value(self: Worker) -> Int { 1 }
func wait(worker: Remote<Worker>) -> Int {
    await worker.value()
}
func main() { 0 }
"#;
    let compilation = foster::compile(source).unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    let wait = compilation.hir.function_named(module, "wait").unwrap();
    assert!(compilation.hir.functions[wait].suspends);
}

#[test]
fn accepts_declared_suspension() {
    let source = r#"
type Worker = {}
func value(self: Worker) -> Int { 1 }
func wait(worker: Remote<Worker>) -> Int [suspend] {
    await worker.value()
}
func main() { 0 }
"#;
    foster::compile(source).unwrap();
}

#[test]
fn supports_dotted_effects_and_rejects_non_method_self_effects() {
    foster::compile(
        r#"
type Box = { value: Int }
func update[g: group Box](box: ref[g] Box) -> Int [mut g.value] {
    box.value = box.value + 1
    box.value
}
func main() { 0 }
"#,
    )
    .unwrap();

    let non_method = foster::compile("func f() -> Unit [mut self] { }").unwrap_err();
    assert!(
        non_method
            .message
            .contains("undeclared effect group `self`")
    );
}

#[test]
fn instantiates_multiple_groups_independently() {
    let source = r#"
func transfer[source: group Int, destination: group Int](from: ref[source] Int, to: ref[destination] Int) -> Int [read source, mut destination] {
    to = from
    to
}

func wrapper[left: group Int, right: group Int](from: ref[left] Int, to: ref[right] Int) -> Int [read left, mut right] {
    transfer(ref from, ref to)
}
func main() { 0 }
"#;
    foster::compile(source).unwrap();

    let missing = source.replace("[read left, mut right]", "[read left]");
    let error = foster::compile(&missing).unwrap_err();
    assert!(error.message.contains("mut right"));
}

#[test]
fn supports_explicit_closure_effect_contracts() {
    let source = r#"
func make[g: group Int](value: ref[g] Int) -> func() -> Int [mut g] {
    [ref value] () -> [mut g] {
        value = value + 1
        value
    }
}
func main() { 0 }
"#;
    foster::compile(source).unwrap();

    let too_narrow = source.replace("-> [mut g] {", "-> [read g] {");
    let error = foster::compile(&too_narrow).unwrap_err();
    assert!(error.message.contains("mut g"));
}

#[test]
fn returned_ref_capture_uses_the_original_projected_place() {
    let source = r#"
func incrementer[g: group Int](value: ref[g] Int) -> func() -> Int [mut g] {
    [ref value] () -> [mut g] {
        value = value + 1
        value
    }
}

func main() -> Int {
    values = [40]
    increment = incrementer(ref values[0])
    increment()
    increment()
    values.head
}
"#;
    let compilation = foster::compile(source).unwrap();
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap(),
            Value::Integer(42)
        );
    }
}

#[test]
fn derives_consume_from_move_out() {
    let missing = r#"
func take[g: group Int](value: ref[g] Int) -> Int { move value }
func main() { 0 }
"#;
    let compilation = foster::compile(missing).unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    let take = compilation.hir.function_named(module, "take").unwrap();
    assert_eq!(
        compilation
            .types
            .function_type(take)
            .unwrap()
            .parameter_modes[0],
        foster::ast::ParameterMode::Consume
    );

    let declared = missing.replace("-> Int { move", "-> Int [consume g] { move");
    foster::compile(&declared).unwrap();

    let reused = r#"
func take[g: group Int](value: ref[g] Int) -> Int [read g, consume g] {
    result = move value
    value
}
func main() { 0 }
"#;
    let error = foster::compile(reused).unwrap_err();
    assert!(
        error.message.contains("used after it was moved"),
        "{}",
        error.message
    );
}

#[test]
fn reports_overdeclared_effect_and_suspend_warnings() {
    let source = r#"
// λ keeps token ranges byte-accurate
type Box = { value: Int }
func inspect(self: Box) -> Int [mut self, suspend] { self.value }
func main() { 0 }
"#;
    let compilation = foster::compile(source).unwrap();
    let codes = compilation
        .diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.code.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(codes, vec!["unused-effect", "unused-suspend"]);
    assert_eq!(
        &source[compilation.diagnostics[0].labels[0].range.clone()],
        "mut self"
    );
    assert_eq!(
        &source[compilation.diagnostics[1].labels[0].range.clone()],
        "suspend"
    );
}

#[test]
fn discovers_implicit_and_companion_modules() {
    let compilation = foster::check_package(Path::new("tests/fixtures/modules")).unwrap();
    let package = &compilation.package;
    assert_eq!(package.modules.len(), 12);
    assert_eq!(package.explicit_module_count(), 9);
    assert_eq!(package.implicit_module_count(), 3);
    assert!(!package.module("json").unwrap().is_implicit());
    assert!(package.module("tools").unwrap().is_implicit());
    assert!(package.module("tools.text").unwrap().is_implicit());
    assert!(package.module("tools.text.trim").is_some());
}

#[test]
fn carries_source_spans_into_package_hir() {
    let compilation = foster::check_package("tests/fixtures/modules").unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    let decode = compilation.hir.function_named(module, "decode").unwrap();
    assert!(
        compilation.hir.functions[decode].span.start < compilation.hir.functions[decode].span.end
    );
    assert_eq!(
        compilation.hir.functions[decode].body.len(),
        compilation.hir.functions[decode].statement_spans.len()
    );
    assert!(
        compilation.hir.functions[decode].statement_spans[0].start
            > compilation.hir.functions[decode].span.start
    );
    assert_eq!(
        compilation.hir.expressions.len(),
        compilation.hir.expression_spans.len()
    );
    assert_eq!(
        compilation.hir.expressions.len(),
        compilation.hir.expression_functions.len()
    );
    let source = compilation
        .package
        .module("main")
        .unwrap()
        .source
        .as_deref()
        .unwrap();
    let qualified_parse = compilation
        .hir
        .expressions
        .iter()
        .find_map(|(expression, value)| {
            (matches!(
                value,
                foster::hir::Expr::Name(foster::hir::ResolvedName::Function(_))
            ) && compilation.hir.expression_functions[&expression] == decode)
                .then_some(expression)
        })
        .unwrap();
    assert_eq!(
        &source[compilation.hir.expression_spans[&qualified_parse].clone()],
        "parser.parse"
    );
    assert_eq!(
        compilation.hir.expression_functions[&qualified_parse],
        decode
    );
    assert!(
        compilation.hir.modules[module]
            .imports_with_spans
            .iter()
            .all(|import| import.span.start < import.span.end)
    );
}

#[test]
fn carries_exact_nested_pattern_binding_spans_into_hir() {
    let source = r#"type Choice =
    | Some(String)
    | None

func select(value: Choice) -> String {
    branch value {
        Choice.Some(payload) -> payload
        Choice.None -> ""
    }
}
"#;
    let compilation = foster::compile(source).unwrap();
    let (local, definition) = compilation
        .hir
        .locals
        .iter()
        .find(|(_, local)| local.name == "payload" && local.kind == foster::hir::LocalKind::Binding)
        .unwrap();

    assert_eq!(&source[definition.span.clone()], "payload");
    assert_eq!(
        compilation
            .types
            .display(compilation.types.local_type(local).unwrap()),
        "String"
    );
}

#[test]
fn runs_a_package_main_module() {
    assert_eq!(
        foster::run_package("tests/fixtures/modules").unwrap(),
        Value::Integer(42)
    );
}

#[test]
fn module_constants_run_with_and_without_optimization() {
    let source = r#"
/// The answer.
const ANSWER = 42
const VALUES = [ANSWER, 43]

func main() -> Int {
    VALUES[0]
}
"#;
    let compilation = foster::compile(source).unwrap();
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize },)
                .unwrap(),
            Value::Integer(42)
        );
    }
}

#[test]
fn imports_public_constants_directly_and_by_qualifier() {
    assert_eq!(
        foster::run_package("tests/fixtures/constants").unwrap(),
        Value::Integer(42)
    );
}

#[test]
fn module_constants_are_private_by_default() {
    let error = foster::check_package("tests/fixtures/constants_private").unwrap_err();
    assert!(error.message.contains("unknown name `HIDDEN`"));
}

#[test]
fn rejects_non_constant_initializers() {
    let error = foster::compile(
        r#"
func answer() -> Int { 42 }
const BAD = answer()
func main() { BAD }
"#,
    )
    .unwrap_err();
    assert!(error.message.contains("constant initializer"));
}

#[test]
fn rejects_cyclic_constants() {
    let error = foster::compile(
        r#"
const FIRST = SECOND
const SECOND = FIRST
func main() { FIRST }
"#,
    )
    .unwrap_err();
    assert!(error.message.contains("cyclic initializer"));
}

#[test]
fn rejects_assignment_to_a_module_constant() {
    let error = foster::compile(
        r#"
const ANSWER = 42
func main() {
    ANSWER = 43
}
"#,
    )
    .unwrap_err();
    assert!(error.message.contains("cannot assign to constant `ANSWER`"));
}

#[test]
fn rejects_unknown_imports() {
    let error = foster::check_package("tests/fixtures/invalid_import").unwrap_err();
    assert!(
        error
            .message
            .contains("imports unknown module `missing.module`")
    );
}

#[test]
fn resolves_qualified_function_names_into_hir_ids() {
    use foster::hir::{Expr, ResolvedName};

    let compilation = foster::check_package("tests/fixtures/modules").unwrap();
    let target_module = compilation.hir.module_named("json.parser").unwrap();
    let target_function = compilation
        .hir
        .function_named(target_module, "parse")
        .unwrap();
    assert!(compilation.hir.expressions.iter().any(|(_, expression)| {
        matches!(expression, Expr::Name(ResolvedName::Function(found)) if *found == target_function)
    }));
}

#[test]
fn rejects_unknown_qualified_members() {
    let error = foster::check_package("tests/fixtures/invalid_qualified").unwrap_err();
    assert!(
        error
            .message
            .contains("module `json.parser` has no member `missing`")
    );
}

#[test]
fn infers_types_across_function_calls() {
    use foster::types::Type;

    let compilation = foster::check_package("tests/fixtures/inference").unwrap();
    let main_module = compilation.hir.module_named("main").unwrap();
    let main = compilation.hir.function_named(main_module, "main").unwrap();
    let identity = compilation
        .hir
        .function_named(main_module, "identity")
        .unwrap();

    let main_type = compilation.types.function_type(main).unwrap();
    assert_eq!(compilation.types.types[main_type.result], Type::Int);
    let identity_type = compilation.types.function_type(identity).unwrap();
    assert_eq!(
        compilation.types.types[identity_type.parameters[0]],
        Type::Int
    );
    assert_eq!(compilation.types.types[identity_type.result], Type::Int);
}

#[test]
fn rejects_return_type_mismatches() {
    let error = foster::check_package("tests/fixtures/invalid_type").unwrap_err();
    assert!(error.message.contains("type mismatch"));
    assert!(error.message.contains("Int"));
    assert!(error.message.contains("String"));
}

#[test]
fn rejects_unknown_type_names() {
    let error = foster::check_package("tests/fixtures/unknown_type").unwrap_err();
    assert!(error.message.contains("unknown type `Missing`"));
}

#[test]
fn checks_qualified_call_arguments() {
    let error = foster::check_package("tests/fixtures/invalid_call").unwrap_err();
    assert!(error.message.contains("type mismatch"));
    assert!(error.message.contains("String"));
    assert!(error.message.contains("Int"));
}

#[test]
fn executes_nested_and_anonymous_closures() {
    assert_eq!(
        foster::run(include_str!("../examples/pima/closure.fos")).unwrap(),
        Value::Integer(36)
    );
}

#[test]
fn classifies_copy_and_move_captures() {
    use foster::hir::{CaptureMode, Expr};

    let compilation = foster::compile(include_str!("../examples/pima/closure.fos")).unwrap();
    let mut modes = compilation
        .hir
        .expressions
        .iter()
        .filter_map(|(_, expression)| match expression {
            Expr::Closure { captures, .. } => Some(
                captures
                    .iter()
                    .map(|capture| {
                        (
                            compilation.hir.locals[capture.local].name.as_str(),
                            capture.mode,
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>();
    modes.sort_by_key(|(name, _)| *name);
    assert!(modes.contains(&("factor", CaptureMode::Copy)));
    assert!(modes.contains(&("prefix", CaptureMode::Move)));
}

#[test]
fn supports_float_literals_types_arithmetic_and_closures() {
    let source = r#"
func scale(value: Float, factor: Float) -> Float {
    value * factor
}

func main() -> Float {
    halve = (value: Float) -> value / 2.0
    halve(scale(1.25e1, 2.0))
}
"#;

    assert_eq!(foster::run(source).unwrap(), Value::Float(12.5));
}

#[test]
fn rejects_mixed_int_and_float_arithmetic() {
    let error = foster::compile("func main() { 1 + 1.5 }").unwrap_err();
    assert!(error.message.contains("type mismatch"));
    assert!(error.message.contains("Float"));
    assert!(error.message.contains("Int"));
}

#[test]
fn honors_explicit_copy_and_move_captures() {
    use foster::hir::{CaptureMode, Expr};

    let source = r#"
func make(scale: Int, prefix: String) [consume prefix] {
    [copy scale, move prefix] (value: Int) -> {
        println(prefix)
        scale * value
    }
}

func main() -> Int {
    apply = make(3, "triple")
    apply(14)
}
"#;
    let compilation = foster::compile(source).unwrap();
    let modes = compilation
        .hir
        .expressions
        .iter()
        .filter_map(|(_, expression)| match expression {
            Expr::Closure { captures, .. } if captures.len() == 2 => Some(captures),
            _ => None,
        })
        .next()
        .unwrap();
    assert!(
        modes
            .iter()
            .any(|capture| capture.mode == CaptureMode::Copy)
    );
    assert!(
        modes
            .iter()
            .any(|capture| capture.mode == CaptureMode::Move)
    );
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn rejects_explicit_copy_of_non_copy_value() {
    let source = r#"
func make(text: String) {
    [copy text] () -> text
}
"#;
    let error = foster::compile(source).unwrap_err();
    assert!(error.message.contains("`text` is not Copy"));
}

#[test]
fn supports_placeholder_partial_application() {
    let source = r#"
func combine(a: Int, b: Int, c: Int) -> Int {
    a * 100 + b * 10 + c
}

func main() -> Int {
    with_middle = combine(_, 2, _)
    with_middle(4, 7)
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(427));
    foster::compile(source).unwrap();
}

#[test]
fn mutable_ref_capture_updates_the_original_place() {
    use foster::hir::{CaptureMode, Expr};

    let source = r#"
func main() -> Int {
    count = 0
    increment = [ref count] () -> {
        count = count + 1
    }
    increment()
    increment()
    count
}
"#;
    let compilation = foster::compile(source).unwrap();
    assert!(compilation.hir.expressions.iter().any(|(_, expression)| {
        matches!(expression, Expr::Closure { captures, .. }
            if captures.iter().any(|capture| capture.mode == CaptureMode::Ref))
    }));
    assert_eq!(foster::run(source).unwrap(), Value::Integer(2));
}

#[test]
fn mutable_ref_capture_can_reshape_a_list() {
    let source = r#"
func main() -> Int {
    values = [1]
    append = [ref values] (value: Int) -> values.push(value)
    append(2)
    append(3)
    values.length
}
"#;
    foster::compile(source).unwrap();
    assert_eq!(foster::run(source).unwrap(), Value::Integer(3));
}

#[test]
fn preserves_group_effects_when_callable_representation_is_inferred() {
    use foster::types::Type;

    let source = r#"
func make[people: group Int](person: ref[people] Int)
    -> func(Int) -> Int [mut people]
{
    [ref person] (value: Int) -> [mut people] {
        person = value
        person
    }
}
"#;
    let compilation = foster::compile(source).unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    let make = compilation.hir.function_named(module, "make").unwrap();
    let result = compilation.types.function_type(make).unwrap().result;
    let Type::Function(callable) = &compilation.types.types[result] else {
        panic!("expected callable result")
    };
    assert!(callable.erased);
    assert_eq!(callable.effects.len(), 1);
    assert_eq!(callable.effects[0].target, "people");
    assert!(compilation.hir.functions.iter().any(|(_, function)| {
        function.name.contains("closure")
            && function
                .effects
                .iter()
                .any(|effect| effect.target == "people")
    }));
}

#[test]
fn rejects_call_after_structural_capture_invalidation() {
    let source = r#"
func main() -> Int {
    values = [10, 20]
    selected = ref values[0]
    show = [ref selected] () -> selected
    values.push(30)
    show()
}
"#;
    let error = foster::compile(source).unwrap_err();
    assert!(
        error
            .message
            .contains("closure `show` is no longer callable")
    );
    assert!(error.message.contains("structural mutation"));
    assert!(error.message.contains("`values`"));
}

#[test]
fn projected_reference_capture_works_before_invalidation() {
    let source = r#"
func main() -> Int {
    values = [10, 20]
    selected = ref values[1]
    show = [ref selected] () -> selected
    show()
}
"#;
    let compilation = foster::compile(source).unwrap();
    assert_eq!(
        foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize: false })
            .unwrap(),
        Value::Integer(20)
    );
}

#[test]
fn rejects_direct_reference_use_after_structural_invalidation() {
    let source = r#"
func main() -> Int {
    values = [10, 20]
    selected = ref values[0]
    values.push(30)
    selected
}
"#;
    let error = foster::compile(source).unwrap_err();
    assert!(error.message.contains("borrowed value `selected`"));
    assert!(
        error
            .message
            .contains("reference into `values` was invalidated")
    );
    assert_eq!(error.code.as_deref(), Some("E0401"));
    assert_eq!(error.source_module.as_deref(), Some("main"));
    assert_eq!(error.labels.len(), 3);
    assert!(error.labels[0].primary);
    assert!(error.labels[0].message.contains("used here"));
    assert!(error.labels[2].message.contains("reshaped `values`"));
    assert!(
        error
            .help
            .as_deref()
            .is_some_and(|help| help.contains("reacquire"))
    );
}

#[test]
fn permits_structural_mutation_after_a_borrows_last_use() {
    let source = r#"
func main() -> Int {
    values = [10, 20]
    selected = ref values[0]
    selected
    values.push(30)
    values.length
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(3));
}

#[test]
fn reacquiring_a_projected_reference_replaces_invalid_loan_state() {
    let source = r#"
type Selection = {
    item: Int
}

func main() -> Int {
    values = [10, 20]
    first = ref values[0]
    selected = Selection { item: first }
    values.push(30)
    second = ref values[1]
    selected = Selection { item: second }
    selected.item
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(20));
}

#[test]
fn conservatively_joins_branch_invalidation() {
    let source = r#"
func main() -> Int {
    values = [10, 20]
    selected = ref values[0]
    branch {
        true -> values.push(30)
        _ -> values.push(40)
    }
    selected
}
"#;
    let error = foster::compile(source).unwrap_err();
    assert!(error.message.contains("borrowed value `selected`"));
}

#[test]
fn propagates_projected_borrow_invalidation_through_records() {
    let source = r#"
type Selection = {
    item: Int
}

func main() -> Int {
    values = [10, 20]
    selected = ref values[0]
    saved = Selection { item: selected }
    values.push(30)
    saved.item
}
"#;
    let error = foster::compile(source).unwrap_err();
    assert!(
        error.message.contains("borrowed value `saved`"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("reference into `values`"),
        "{}",
        error.message
    );
}

#[test]
fn direct_owned_call_results_do_not_inherit_argument_provenance() {
    let source = r#"
func describe(value: Int) -> String {
    "number"
}

func main() -> Int {
    values = [10, 20]
    selected = ref values[0]
    description = describe(selected)
    values.push(30)
    description.length
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(6));
}

#[test]
fn ownership_mir_records_explicit_result_provenance_summaries() {
    let source = r#"
func preserve[g: group Int](value: ref[g] Int) -> ref[g] Int {
    ref value
}

func describe(value: Int) -> String {
    "number"
}

func main() { 0 }
"#;
    let compilation = foster::compile(source).unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    let preserve = compilation.hir.function_named(module, "preserve").unwrap();
    let describe = compilation.hir.function_named(module, "describe").unwrap();
    assert_eq!(
        compilation.ownership.functions[&preserve]
            .result_provenance
            .parameters,
        vec![0]
    );
    assert!(
        !compilation.ownership.functions[&preserve]
            .result_provenance
            .fresh_owned
    );
    assert!(
        compilation.ownership.functions[&describe]
            .result_provenance
            .fresh_owned
    );
}

#[test]
fn ownership_mir_records_loan_identity_and_forward_provenance() {
    let source = r#"
func describe(value: Int) -> String { "number" }

func main() -> Int {
    values = [10, 20]
    selected = ref values[0]
    description = describe(selected)
    selected
}
"#;
    let compilation = foster::compile(source).unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    let main = compilation.hir.function_named(module, "main").unwrap();
    let mir = &compilation.ownership.functions[&main];
    assert_eq!(mir.loans.len(), 1);
    assert_eq!(mir.loans[0].id, foster::ownership::LoanId(0));

    let selected = compilation
        .hir
        .locals
        .iter()
        .find_map(|(id, local)| (local.function == main && local.name == "selected").then_some(id))
        .unwrap();
    let description = compilation
        .hir
        .locals
        .iter()
        .find_map(|(id, local)| {
            (local.function == main && local.name == "description").then_some(id)
        })
        .unwrap();
    let analysis = &compilation.ownership.provenance[&main];
    let exit = analysis.exits.iter().flatten().last().unwrap();
    assert_eq!(
        exit.contents
            .get(&foster::hir::Place {
                root: selected,
                projections: Vec::new(),
            })
            .unwrap(),
        &std::collections::HashSet::from([foster::ownership::LoanId(0)])
    );
    assert!(!exit.contents.contains_key(&foster::hir::Place {
        root: description,
        projections: Vec::new(),
    }));
}

#[test]
fn ownership_mir_emits_typed_reshape_invalidations() {
    let source = r#"
func main() -> Int {
    values = [10, 20]
    selected = ref values[0]
    selected
    values.push(30)
    values.length
}
"#;
    let compilation = foster::compile(source).unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    let main = compilation.hir.function_named(module, "main").unwrap();
    let invalidations = compilation.ownership.functions[&main]
        .blocks
        .iter()
        .flat_map(|block| &block.operations)
        .filter(|operation| {
            matches!(
                operation,
                foster::ownership::Operation::Invalidate {
                    kind: foster::ownership::InvalidationKind::Reshape,
                    ..
                }
            )
        })
        .count();
    assert_eq!(invalidations, 1);
}

#[test]
fn ownership_mir_preserves_dotted_invalidation_paths() {
    let source = r#"
type Pair = {
    left: List<Int>
    right: List<Int>
}

func grow[g: group Pair](pair: ref[g] Pair) -> Unit [reshape g.left.items] {
    pair.left.push(30)
}

func main() -> Int {
    pair = Pair { left: [10], right: [20] }
    grow(ref pair)
    pair.left.length
}
"#;
    let compilation = foster::compile(source).unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    let main = compilation.hir.function_named(module, "main").unwrap();
    let place = compilation.ownership.functions[&main]
        .blocks
        .iter()
        .flat_map(|block| &block.operations)
        .find_map(|operation| match operation {
            foster::ownership::Operation::Invalidate {
                place,
                kind: foster::ownership::InvalidationKind::Reshape,
                ..
            } => Some(place),
            _ => None,
        })
        .unwrap();
    assert!(place.projections.ends_with(&[
        foster::hir::Projection::Field("left".into()),
        foster::hir::Projection::Field("items".into()),
    ]));
}

#[test]
fn ownership_mir_tracks_disjoint_fields_and_move_transfer() {
    let source = r#"
type Saved = {
    left: Int
    right: Int
}

func main() -> Int {
    values = [10, 20]
    left = ref values[0]
    right = ref values[1]
    saved = Saved { left: left, right: right }
    saved.left = 0
    moved = move saved
    moved.right
}
"#;
    let compilation = foster::compile(source).unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    let main = compilation.hir.function_named(module, "main").unwrap();
    let local = |name: &str| {
        compilation
            .hir
            .locals
            .iter()
            .find_map(|(id, local)| (local.function == main && local.name == name).then_some(id))
            .unwrap()
    };
    let saved = local("saved");
    let moved = local("moved");
    let exit = compilation.ownership.provenance[&main]
        .exits
        .iter()
        .flatten()
        .last()
        .unwrap();
    assert!(!exit.contents.keys().any(|place| place.root == saved));
    let moved_loans = exit
        .contents
        .iter()
        .filter(|(place, _)| place.root == moved)
        .flat_map(|(_, loans)| loans)
        .copied()
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(moved_loans.len(), 1);
}

#[test]
fn direct_borrowed_call_results_substitute_parameter_provenance() {
    let source = r#"
func preserve[g: group Int](value: ref[g] Int) -> ref[g] Int {
    ref value
}

func main() -> Int {
    values = [10, 20]
    selected = preserve(ref values[0])
    show = [ref selected] () -> {
        println(selected)
        0
    }
    values.push(30)
    show()
}
"#;
    let error = foster::compile(source).unwrap_err();
    assert!(
        error
            .message
            .contains("closure `show` is no longer callable"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("reference into `values`"),
        "{}",
        error.message
    );
}

#[test]
fn permits_reshape_of_a_disjoint_record_field() {
    let source = r#"
type Pair = {
    left: List<Int>
    right: List<Int>
}

func main() -> Int {
    pair = Pair { left: [10], right: [20] }
    selected = ref pair.left[0]
    pair.right.push(30)
    selected
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(10));
}

#[test]
fn guarded_return_value_reshape_does_not_invalidate_the_continuation() {
    let source = r#"
func reshape_and_return(values: List<Int>) -> Int [reshape values.items] {
    values.push(30)
    0
}

func main() -> Int {
    values = [10, 20]
    selected = ref values[0]
    return reshape_and_return(values) if false
    selected
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(10));
}

#[test]
fn rejects_returning_a_reference_into_a_frame_local() {
    let source = r#"
func invalid() {
    values = [10]
    ref values[0]
}
"#;
    let error = foster::compile(source).unwrap_err();
    assert!(
        error
            .message
            .contains("returned reference borrows local `values`")
    );
}

#[test]
fn rejects_use_after_move_capture() {
    let source = r#"
func main() -> String {
    text = "owned"
    get = [move text] () -> text
    text
}
"#;
    let error = foster::compile(source).unwrap_err();
    assert!(error.message.contains("`text` was already moved"));
}

#[test]
fn rejects_escaping_borrow_of_a_local() {
    let source = r#"
func invalid() {
    value = 1
    [ref value] () -> value
}
"#;
    let error = foster::compile(source).unwrap_err();
    assert!(
        error
            .message
            .contains("returned closure borrows local `value`")
    );
}

#[test]
fn checks_callable_effect_bounds() {
    let source = r#"
func invalid[state: group Int](value: ref[state] Int)
    -> func(Int) -> Int [read state]
{
    [ref value] (next: Int) -> {
        value = next
        value
    }
}
"#;
    let error = foster::compile(source).unwrap_err();
    assert!(error.message.contains("callable contract is incompatible"));
}

#[test]
fn passes_projected_references_to_group_parameterized_functions() {
    let source = r#"
func set[state: group Int](value: ref[state] Int, next: Int) -> Int [mut state] {
    value = next
    value
}

func main() -> Int {
    values = [1, 2]
    set(ref values[0], 7)
    values.head
}
"#;
    foster::compile(source).unwrap();
    assert_eq!(foster::run(source).unwrap(), Value::Integer(7));
}
