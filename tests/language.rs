use foster::vm::Value;
use std::path::Path;

#[test]
fn guard_return_and_implicit_result() {
    let source = r#"
func first(characters: List<String>) -> String {
    return "" if characters.empty?
    characters.head
}

func main() {
    first(["F", "o"])
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::String("F".into()));
}

#[test]
fn postfix_guards_conditionally_transfer_control() {
    let source = r#"
func choose(early: Bool) -> Int {
    return 10 if early
    20
}

func main() -> Int {
    choose(true) + choose(false)
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(30));
}

#[test]
fn postfix_guard_falls_through_to_a_parameter_result() {
    let source = r#"
func either(left: Bool, right: Bool) -> Bool {
    return true if left
    right
}

func main() -> Bool {
    either(false, false)
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Bool(false));
}

#[test]
fn postfix_guards_require_boolean_conditions() {
    let error = foster::compile("func main() -> Int { return 1 if 42\n0 }").unwrap_err();
    assert!(error.message.contains("Bool"), "{}", error.message);
}

#[test]
fn postfix_guards_only_apply_to_control_statements() {
    let expression = foster::compile("func main() { println() if true }").unwrap_err();
    assert_eq!(
        expression.message,
        "postfix `if` may only guard a control statement"
    );

    let binding = foster::compile("func main() { value = 1 if true\nvalue }").unwrap_err();
    assert_eq!(
        binding.message,
        "postfix `if` may only guard a control statement"
    );
}

#[test]
fn branch_and_recursion() {
    let source = include_str!("../examples/whitespace.fos");
    assert_eq!(foster::run(source).unwrap(), Value::String("Foster".into()));
}

#[test]
fn symbols_and_arithmetic() {
    let source = r#"
func choose(value: Int) {
    branch {
        value > 10 -> :large
        _ -> :small
    }
}

func main() { choose(6 * 2) }
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Symbol("large".into()));
}

#[test]
fn conditional_branches_require_a_wildcard_arm() {
    let missing = foster::compile("func main() { branch { true -> 1 } }").unwrap_err();
    assert!(missing.message.contains("requires a `_` arm"));

    let legacy = foster::compile("func main() { branch { true -> 1 else -> 0 } }").unwrap_err();
    assert!(legacy.message.contains("expected expression"));
}

#[test]
fn remote_objects_process_methods_on_virtual_threads() {
    let source = r#"
type Counter {
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
type Counter {
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
type Pair {
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
type Counter { value: Int }

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
type Document { value: Int }
type Inspector {}

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
type Pair { left: Int, right: Int }
type Inspector {}

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
type Document { value: Int }
type Worker {}

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
type Box { value: Int }

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
type Worker {}

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
type Worker {}
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
type Worker {}
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
type Box { value: Int }
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
type Box { value: Int }
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
    assert_eq!(package.modules.len(), 6);
    assert_eq!(package.explicit_module_count(), 4);
    assert_eq!(package.implicit_module_count(), 2);
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
    assert_eq!(foster::run(source).unwrap(), Value::Integer(20));
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

#[test]
fn runs_newly_unblocked_pima_ports() {
    assert_eq!(
        foster::run(include_str!("../examples/pima/curried_example.fos")).unwrap(),
        Value::Integer(19)
    );
    let Value::Float(root) = foster::run(include_str!("../examples/pima/newton.fos")).unwrap()
    else {
        panic!("Newton example should return Float")
    };
    assert!((root - 4.0).abs() < 0.001);
    assert_eq!(
        foster::run(include_str!("../examples/pima/birthday_paradox.fos")).unwrap(),
        Value::Float(23.0)
    );
}

#[test]
fn declarations_are_private_by_default_across_public_modules() {
    let error = foster::check_package("tests/fixtures/private_function").unwrap_err();
    assert!(
        error
            .message
            .contains("function `library.hidden` is private")
    );
}

#[test]
fn constructs_reads_and_mutates_nominal_records() {
    let source = r#"
pub type Person {
    pub name: String
    pub age: Int
    internal_id: Int
}

func main() -> Int {
    name = "Ada"
    person = Person {
        name
        age: 37
        internal_id: 104
    }
    person.age = person.age + 1
    person.age
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(38));
}

#[test]
fn infers_generic_record_arguments() {
    let source = r#"
type Parsed<T> {
    value: T
    remaining: String
}

func parse() -> Parsed<Int> {
    Parsed {
        value: 42
        remaining: ""
    }
}

func main() -> Int {
    parse().value
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn calls_functions_associated_with_record_types() {
    let source = r#"
type Box<T> { value: T }

func Box.create<T>(value: T) -> Box<T> {
    Box { value }
}

func main() -> Int {
    Box.create(42).value
}
"#;
    let compilation = foster::compile(source).unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    assert!(
        compilation
            .hir
            .function_named(module, "Box.create")
            .is_some()
    );
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));

    let unknown = foster::compile("func Missing.create() { 1 }\nfunc main() { 0 }").unwrap_err();
    assert!(unknown.message.contains("unknown record type `Missing`"));

    let receiver = foster::compile(
        "type Box { value: Int }\nfunc Box.read(self: Box) { self.value }\nfunc main() { 0 }",
    )
    .unwrap_err();
    assert!(
        receiver
            .message
            .contains("associated function `Box.read` cannot declare a `self` parameter")
    );
}

#[test]
fn associated_functions_construct_private_record_representations() {
    assert_eq!(
        foster::run_package("tests/fixtures/associated_function").unwrap(),
        Value::Integer(42)
    );
}

#[test]
fn rejects_incomplete_and_duplicate_record_initialization() {
    let missing = foster::compile(
        r#"
type Pair { left: Int, right: Int }
func main() { Pair { left: 1 } }
"#,
    )
    .unwrap_err();
    assert!(missing.message.contains("missing field(s): right"));

    let duplicate = foster::compile(
        r#"
type Pair { left: Int, right: Int }
func main() { Pair { left: 1, left: 2, right: 3 } }
"#,
    )
    .unwrap_err();
    assert!(
        duplicate
            .message
            .contains("field `left` is initialized twice")
    );
}

#[test]
fn enforces_record_and_field_visibility_across_modules() {
    let error = foster::check_package("tests/fixtures/record_privacy").unwrap_err();
    assert!(error.message.contains("field `Person.secret` is private"));
    foster::check_package("tests/fixtures/public_record").unwrap();
}

#[test]
fn rejects_private_types_in_public_signatures() {
    let source = r#"
type Secret { value: Int }
pub func expose() -> Secret { Secret { value: 1 } }
"#;
    let error = foster::compile(source).unwrap_err();
    assert!(
        error
            .message
            .contains("public function `expose` exposes private type `Secret`")
    );
}

#[test]
fn mutable_ref_capture_can_update_record_fields() {
    let source = r#"
type Counter { value: Int }

func main() -> Int {
    counter = Counter { value: 0 }
    increment = [ref counter] () -> {
        counter.value = counter.value + 1
    }
    increment()
    increment()
    counter.value
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(2));
}

#[test]
fn rejects_storing_a_borrower_into_its_own_origin() {
    let source = r#"
type Counter {
    value: Int,
    callback: func() -> Int
}

func main() -> Int {
    counter = Counter {
        value: 1,
        callback: () -> 0
    }
    callback = [ref counter] () -> counter.value
    counter.callback = callback
    counter.value
}
"#;

    let error = foster::compile(source).unwrap_err();
    assert!(
        error
            .message
            .contains("cannot store a value borrowing `counter` into its own origin")
    );
}

#[test]
fn permits_storing_a_value_derived_from_a_borrower() {
    let source = r#"
type Counter { value: Int }

func main() -> Int {
    counter = Counter { value: 1 }
    observe = [ref counter] () -> counter.value
    counter.value = observe()
    counter.value
}
"#;

    assert_eq!(foster::run(source).unwrap(), Value::Integer(1));
}

#[test]
fn constructs_and_exhaustively_matches_closed_variants() {
    let source = r#"
type Result<T> =
    | Ok(T)
    | Error(String)

func unwrap(result: Result<Int>) -> Int {
    branch result {
        Result.Ok(value) -> value
        Result.Error(message) -> 0
    }
}

func main() -> Int { unwrap(Result.Ok(42)) }
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn matches_payloadless_variants_and_wildcards() {
    let source = r#"
type Option<T> =
    | Some(T)
    | None

func present(value: Option<Int>) -> Bool {
    branch value {
        Option.Some(_) -> true
        Option.None -> false
    }
}

func main() -> Bool { present(Option.None) }
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Bool(false));
}

#[test]
fn rejects_non_exhaustive_variant_branches() {
    let error = foster::compile(
        r#"
type Choice =
    | Left(Int)
    | Right(Int)
func main() -> Int {
    value = Choice.Left(1)
    branch value { Choice.Left(number) -> number }
}
"#,
    )
    .unwrap_err();
    assert!(error.message.contains("non-exhaustive branch on `Choice`"));
}

#[test]
fn refutable_payload_patterns_do_not_cover_an_entire_alternative() {
    let error = foster::compile(
        r#"
type Option =
    | Some(Int)
    | None

func main() -> Int {
    value = Some(1)
    branch value {
        Some(0) -> 0
        Option.None -> -1
    }
}
"#,
    )
    .unwrap_err();
    assert!(error.message.contains("non-exhaustive branch on `Option`"));
}

#[test]
fn rejects_private_variants_in_public_apis() {
    let signature = foster::compile(
        r#"
type Secret = | Hidden(Int)
pub func expose() -> Secret { Hidden(1) }
"#,
    )
    .unwrap_err();
    assert!(
        signature
            .message
            .contains("public function `expose` exposes private type `Secret`")
    );

    let payload = foster::compile(
        r#"
type Secret = | Hidden(Int)
pub type Message = | Reveal(Secret)
func main() { 0 }
"#,
    )
    .unwrap_err();
    assert!(
        payload
            .message
            .contains("public variant `Message.Reveal` exposes private type `Secret`"),
        "{}",
        payload.message
    );
}

fn json_parser_with_main(expression: &str) -> String {
    let parser = include_str!("../examples/pima/json_parser/parser.fos");
    format!("{parser}\nfunc main() {{ {expression} }}")
}

#[test]
fn runs_the_foster_json_parser() {
    let value = foster::run(&json_parser_with_main(
        r#"parse_json("{\"text\":\"\\uD83D\\uDE00\",\"values\":[true,null,2.5e1]}")"#,
    ))
    .unwrap();
    assert!(
        matches!(value, Value::Variant { ref type_name, ref alternative, .. } if type_name == "ParseResult" && alternative == "Ok")
    );
    assert!(value.to_string().contains("Json.String("));
    assert!(value.to_string().contains("Json.Number(25)"));
}

#[test]
fn runs_the_json_actor_pipeline() {
    let value = foster::run_package("examples/pima/json_parser").unwrap();
    let Value::Record { name, fields, .. } = value else {
        panic!("pipeline should return a report record")
    };
    assert_eq!(name, "PipelineReport");
    assert_eq!(fields.get("processed"), Some(&Value::Integer(2)));
    assert_eq!(fields.get("failed"), Some(&Value::Integer(1)));
}

#[test]
fn json_parser_returns_typed_errors_for_malformed_input() {
    for document in [
        r#"[1,2,]"#,
        r#"{\"a\":1,}"#,
        r#"01"#,
        r#"-"#,
        r#"\"\\uDE00\""#,
    ] {
        let expression = format!("parse_json({document:?})");
        let value = foster::run(&json_parser_with_main(&expression)).unwrap();
        assert!(
            matches!(value, Value::Variant { ref type_name, ref alternative, .. } if type_name == "ParseResult" && alternative == "Error"),
            "expected typed error for {document}, got {value}"
        );
    }
}

#[test]
fn generic_functions_are_rigid_and_instantiate_per_call() {
    let source = r#"
func identity<T>(value: T) -> T [consume value] { value }

func main() -> String {
    number = identity(42)
    identity("Foster")
}
"#;
    foster::compile(source).unwrap();
    assert_eq!(foster::run(source).unwrap(), Value::String("Foster".into()));

    let error = foster::compile(
        r#"
func invalid<T>(value: T) -> T { value + 1 }
func main() { invalid(1) }
"#,
    )
    .unwrap_err();
    assert!(error.message.contains("type mismatch"));
}

#[test]
fn generic_syntax_uses_angles_while_indexing_uses_brackets() {
    let source = r#"
func first<T>(values: List<T>) -> T {
    values[0]
}

func main() -> Int {
    first([42])
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));

    let legacy = foster::compile(
        r#"
func identity[T](value: T) -> T { value }
func main() -> Int { identity(42) }
"#,
    );
    assert!(
        legacy.is_err(),
        "square-bracketed generics must be rejected"
    );
}

#[test]
fn rejects_duplicate_and_colliding_function_parameters() {
    let duplicate = foster::compile(
        r#"
func invalid<T, T>(value: T) -> T { value }
func main() { 0 }
"#,
    )
    .unwrap_err();
    assert!(
        duplicate
            .message
            .contains("declares type parameter `T` more than once")
    );

    let collision = foster::compile(
        r#"
func invalid<T>[T: group Int](value: T) -> T { value }
func main() { 0 }
"#,
    )
    .unwrap_err();
    assert!(
        collision
            .message
            .contains("uses `T` as both a type parameter and a group parameter")
    );
}

#[test]
fn checks_explicit_import_core_library_usage() {
    let compilation = foster::check_package("tests/fixtures/core_consumer").unwrap();
    assert!(compilation.package.module("core").unwrap().is_implicit());
    assert!(compilation.package.module("core.list").is_some());
    assert!(compilation.package.module("core.string").is_some());
    let string = compilation.hir.module_named("core.string").unwrap();
    let list = compilation.hir.module_named("core.list").unwrap();
    assert!(compilation.hir.function_named(string, "trim").is_some());
    assert!(compilation.hir.function_named(list, "flat_map").is_some());
}

#[test]
fn requires_qualification_for_ambiguous_imported_names() {
    let error = foster::check_package("tests/fixtures/import_ambiguity").unwrap_err();
    assert!(
        error
            .message
            .contains("imported name `map` is ambiguous; qualify it with its module")
    );
}

#[test]
fn borrows_arguments_by_default_and_requires_explicit_moves_for_consuming_calls() {
    foster::compile(
        r#"
func take(value: String) -> Unit { println() }
func main() -> String {
    value = "owned"
    take(value)
    value
}
"#,
    )
    .unwrap();

    let missing_move = foster::compile(
        r#"
func take(value: String) -> Unit [consume value] { println() }
func main() -> Unit {
    value = "owned"
    take(value)
}
"#,
    )
    .unwrap_err();
    assert!(
        missing_move
            .message
            .contains("pass this argument with `move`")
    );

    let moved = foster::compile(
        r#"
func take(value: String) -> Unit [consume value] { println() }
func main() -> String {
    value = "owned"
    take(move value)
    value
}
"#,
    )
    .unwrap_err();
    assert!(
        moved
            .message
            .contains("value `value` is used after it was moved")
    );

    foster::compile(
        r#"
func take(value: Int) -> Unit [consume value] { println() }
func main() -> Int {
    value = 42
    take(value)
    value
}
"#,
    )
    .unwrap();
}

#[test]
fn preserves_consuming_parameters_through_callable_values() {
    let missing_move = r#"
func main() -> Unit {
    action = (message: String) -> [consume message] { println(message) }
    message = "owned"
    action(message)
}
"#;
    let error = foster::compile(missing_move).unwrap_err();
    assert!(error.message.contains("pass this argument with `move`"));

    foster::compile(&missing_move.replace("action(message)", "action(move message)")).unwrap();
}

#[test]
fn preserves_consuming_parameters_through_partial_application() {
    let missing_move = r#"
func submit(message: String) -> Unit [consume message] {
    println(message)
}

func main() -> Unit {
    action = submit(_)
    message = "owned"
    action(message)
}
"#;
    let error = foster::compile(missing_move).unwrap_err();
    assert!(error.message.contains("pass this argument with `move`"));

    foster::compile(&missing_move.replace("action(message)", "action(move message)")).unwrap();

    let indirect = missing_move.replace(
        "action = submit(_)",
        "consumer = submit\n    action = consumer(_)",
    );
    let error = foster::compile(&indirect).unwrap_err();
    assert!(error.message.contains("pass this argument with `move`"));
    foster::compile(&indirect.replace("action(message)", "action(move message)")).unwrap();
}

#[test]
fn expresses_consuming_parameters_in_callable_types() {
    let source = r#"
func sink(message: String) -> Unit [consume message] {
    println(message)
}

func invoke(action: func(consume String) -> Unit, message: String) -> Unit [consume message] {
    action(move message)
}

func main() -> Unit {
    invoke(sink, "owned")
}
"#;
    let compilation = foster::compile(source).unwrap();
    let main = compilation.hir.module_named("main").unwrap();
    let sink = compilation.hir.function_named(main, "sink").unwrap();
    assert_eq!(
        compilation
            .types
            .function_type(sink)
            .unwrap()
            .parameter_modes,
        vec![foster::ast::ParameterMode::Consume]
    );

    let incompatible = source.replace("func(consume String) -> Unit", "func(String) -> Unit");
    let error = foster::compile(&incompatible).unwrap_err();
    assert!(error.message.contains("callable contract is incompatible"));
}

#[test]
fn any_is_an_ordinary_identifier_not_a_language_keyword() {
    let source = r#"
func main() -> Int {
    any = 42
    any
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn declared_type_composition_conforms_without_runtime_conversion() {
    let source = r#"
import core.string as strings

type TextSlice & Sequence<CodePoint> {
    text: String
}

func empty?(self: TextSlice) -> Bool { self.text.empty? }
func length(self: TextSlice) -> Int { self.text.length }
func head(self: TextSlice) -> CodePoint { self.text.head }
func rest(self: TextSlice) -> String { strings.slice(self.text, 1, self.text.length) }

func first(values: Sequence<CodePoint>) -> CodePoint {
    values.head
}

func main() -> CodePoint {
    value = TextSlice { text: "OK" }
    first(value)
    value.head
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::CodePoint('O'));
}

#[test]
fn callable_contract_members_dispatch_through_structural_types() {
    let source = r#"
type Identified {
    pub func id(self) -> Int [read self]
    pub func offset(self, amount: Int) -> Int [read self]
}

type User & Identified {
    value: Int
}

func id(self: User) -> Int {
    self.value
}

func offset(self: User, amount: Int) -> Int {
    self.value + amount
}

func increment_id(value: Identified) -> Int {
    value.id + value.offset(2)
}

func main() -> Int {
    increment_id(User { value: 20 })
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));

    let missing = source
        .replace("type User & Identified", "type User")
        .replace(
            "func id(self: User) -> Int {\n    self.value\n}\n\nfunc offset(self: User, amount: Int) -> Int {\n    self.value + amount\n}\n",
            "",
        );
    let error = foster::compile(&missing).unwrap_err();
    assert!(error.message.contains("missing accessible method `id`"));
}

#[test]
fn iterator_and_iterable_contracts_dispatch_stateful_iteration() {
    let source = r#"
import core.iteration
import core.option

type Counter & Iterator<Int> {
    current: Int
    end: Int
}

func next(self: Counter) -> Option<Int> {
    value = self.current
    self.current = self.current + 1
    branch {
        value >= self.end -> Option.None
        _ -> Option.Some(value)
    }
}

type Range & Iterable<Int> {
    start: Int
    end: Int
}

func iterator(self: Range) -> Iterator<Int> {
    Counter { current: self.start, end: self.end }
}

func value_or(candidate: Option<Int>, fallback: Int) -> Int {
    branch candidate {
        Option.Some(value) -> value
        Option.None -> fallback
    }
}

func main() -> Int {
    values = Range { start: 3, end: 5 }.iterator
    first = value_or(values.next(), -1)
    second = value_or(values.next(), -1)
    exhausted = value_or(values.next(), -1)
    first + second + exhausted
}
"#;

    let compilation = foster::compile(source).unwrap();
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap(),
            Value::Integer(6)
        );
    }
}

#[test]
fn core_iterator_adapts_sequences_and_advances_in_place() {
    let source = r#"
import core.iteration
import core.option

func value_or(candidate: Option<Int>, fallback: Int) -> Int {
    branch candidate {
        Option.Some(value) -> value
        Option.None -> fallback
    }
}

func main() -> Int {
    values = Iterator.from_sequence([7, 8])
    first = value_or(values.next(), -1)
    second = value_or(values.next(), -1)
    exhausted = value_or(values.next(), -1)
    first + second + exhausted
}
"#;

    let compilation = foster::compile(source).unwrap();
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap(),
            Value::Integer(14)
        );
    }
}

#[test]
fn builtin_sequences_adapt_to_collection_and_iterable() {
    let source = r#"
import core.collection
import core.option

func size<T>(values: Collection<T>) -> Int {
    values.length
}

func value_or(candidate: Option<Int>, fallback: Int) -> Int {
    branch candidate {
        Option.Some(value) -> value
        Option.None -> fallback
    }
}

func main() -> Int {
    values = [4, 5]
    cursor = values.iterator
    size(values) + size("abc") + value_or(cursor.next(), -10) + value_or(cursor.next(), -10)
}
"#;

    assert_eq!(foster::run(source).unwrap(), Value::Integer(14));
}

#[test]
fn map_is_an_iterable_collection_of_public_entries() {
    let source = r#"
import core.map
import core.option

func first_value(candidate: Option<Entry<String, Int>>) -> Int {
    branch candidate {
        Option.Some(entry) -> entry.value
        Option.None -> -1
    }
}

func main() -> Int {
    state = Map.empty()
    values = put(move state, "answer", 42)
    cursor = values.iterator
    values.length + first_value(cursor.next())
}
"#;

    assert_eq!(foster::run(source).unwrap(), Value::Integer(43));
}

#[test]
fn foster_collections_and_range_share_collection_contract() {
    let source = r#"
import core.collection
import core.range
import core.set

func size<T>(values: Collection<T>) -> Int {
    values.length
}

func main() -> Int {
    distinct = Set.from([1, 1, 2])
    span = Range.from([3, 4, 5])
    size(distinct) * 10 + size(span)
}
"#;

    assert_eq!(foster::run(source).unwrap(), Value::Integer(23));
}

#[test]
fn mutable_effect_allows_extracting_children_but_not_consuming_the_owner() {
    let source = r#"
type Resource { value: String }

func invalid(self: Resource) -> Resource [mut self] {
    move self
}

func main() -> Int { 0 }
"#;

    let error = foster::compile(source).unwrap_err();
    assert!(error.message.contains("consume self"), "{}", error.message);
}

#[test]
fn equality_ordering_and_hashing_contracts_compose_and_dispatch() {
    let source = r#"
import core.ordering

type Key & Ordered<Key> & Hashing {
    value: Int
}

func equal?(self: Key, other: Key) -> Bool {
    self.value == other.value
}

func compare(self: Key, other: Key) -> Ordering {
    branch {
        self.value < other.value -> Ordering.Less
        self.value > other.value -> Ordering.Greater
        _ -> Ordering.Equal
    }
}

func hash(self: Key) -> Int {
    self.value * 31
}

func equality_score(left: Equality<Key>, right: Key) -> Int {
    branch {
        left.equal?(right) -> 1
        _ -> 0
    }
}

func ordering_score(left: Ordered<Key>, right: Key) -> Int {
    branch left.compare(right) {
        Ordering.Less -> 10
        Ordering.Equal -> 20
        Ordering.Greater -> 30
    }
}

func hash_score(value: Hashing) -> Int {
    value.hash
}

func main() -> Int {
    key = Key { value: 7 }
    equality_score(key, Key { value: 7 }) + ordering_score(key, Key { value: 8 }) + hash_score(key)
}
"#;

    let compilation = foster::compile(source).unwrap();
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap(),
            Value::Integer(228)
        );
    }

    let missing_equality = source.replace(
        "func equal?(self: Key, other: Key) -> Bool {\n    self.value == other.value\n}\n\n",
        "",
    );
    let error = foster::compile(&missing_equality).unwrap_err();
    assert!(
        error.message.contains("missing required method `equal?`"),
        "{}",
        error.message
    );
}

#[test]
fn matching_contracts_conform_without_an_explicit_composition_clause() {
    let source = r#"
type TextSlice {
    pub empty?: Bool
    pub length: Int
    pub head: CodePoint
    pub rest: String
}

func first(values: Sequence<CodePoint>) -> CodePoint {
    values.head
}

func main() -> CodePoint {
    first(TextSlice { empty?: false, length: 2, head: 'O', rest: "K" })
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::CodePoint('O'));
}

#[test]
fn intersection_parameters_require_every_composed_contract() {
    let source = r#"
import core.string as strings

type Named {
    pub name: String
}

type TextSlice & Named & Sequence<CodePoint> {
    text: String
}

func empty?(self: TextSlice) -> Bool { self.text.empty? }
func length(self: TextSlice) -> Int { self.text.length }
func head(self: TextSlice) -> CodePoint { self.text.head }
func rest(self: TextSlice) -> String { strings.slice(self.text, 1, self.text.length) }

func describe(value: Named & Sequence<CodePoint>) -> String {
    value.name + value.head.string
}

func main() -> String {
    describe(TextSlice {
        name: "answer: "
        text: "Y"
    })
}
"#;
    assert_eq!(
        foster::run(source).unwrap(),
        Value::String("answer: Y".into())
    );
}

#[test]
fn declared_composition_requires_callable_members() {
    let error = foster::compile(
        r#"
type Broken & Sequence<CodePoint> {}

func main() { 0 }
"#,
    )
    .unwrap_err();
    assert!(error.message.contains("missing required method `empty?`"));
}

#[test]
fn declared_composition_rejects_incompatible_contract_members() {
    let error = foster::compile(
        r#"
type TextNamed {
    pub name: String
}

type NumericNamed {
    pub name: Int
}

type Broken & TextNamed & NumericNamed {}

func main() { 0 }
"#,
    )
    .unwrap_err();
    assert!(
        error
            .message
            .contains("composes incompatible definitions of field `name`")
    );
}

#[test]
fn assignment_reinitializes_a_moved_local() {
    foster::compile(
        r#"
func take(value: String) -> Unit [consume value] { println() }
func main() -> String {
    value = "first"
    take(move value)
    value = "second"
    value
}
"#,
    )
    .unwrap();
}

#[test]
fn joins_move_state_across_branch_arms() {
    let error = foster::compile(
        r#"
func take(value: String) -> Unit [consume value] { println() }
func choose(flag: Bool) -> String {
    value = "owned"
    branch {
        flag -> take(move value)
        _ -> println()
    }
    value
}
func main() -> Unit { println() }
"#,
    )
    .unwrap_err();
    assert!(
        error
            .message
            .contains("value `value` is used after it was moved")
    );
}

#[test]
fn permits_disjoint_field_use_after_a_partial_move() {
    foster::compile(
        r#"
type Pair {
    left: String
    right: String
}
func take(value: String) -> Unit [consume value] { println() }
func remaining(pair: Pair) -> String [consume pair] {
    take(move pair.left)
    pair.right
}
func main() -> Unit { println() }
"#,
    )
    .unwrap();
}

#[test]
fn runs_the_live_inventory_pipeline() {
    assert_eq!(
        foster::run(include_str!("../examples/live_inventory_pipeline.fos")).unwrap(),
        Value::Integer(1242)
    );
}

#[test]
fn supports_line_block_and_documentation_comments() {
    let source = r#"
/// A named value used by the public API.
/**
 * The second paragraph is retained as Markdown.
 */
pub type Named {
    pub value: Int
}

/// Returns the value.
///
/// This text is available to language tooling.
pub func value(named: Named) -> Int {
    /* Comments can appear between tokens and /* can be nested. */ */
    named.value // ordinary comments are discarded
}

func main() -> Int { value(Named { value: 7 }) }
"#;
    let program = foster::parse(source).unwrap();
    assert_eq!(
        program.records[0].documentation.as_deref(),
        Some(
            "A named value used by the public API.\nThe second paragraph is retained as Markdown."
        )
    );
    assert_eq!(
        program.functions[0].documentation.as_deref(),
        Some("Returns the value.\n\nThis text is available to language tooling.")
    );
    assert_eq!(foster::run(source).unwrap(), Value::Integer(7));

    let error = foster::parse("func main() { /* never closed").unwrap_err();
    assert!(error.message.contains("unterminated block comment"));
}

#[test]
fn structurally_adapts_records_with_additional_public_fields() {
    let source = r#"
type Named {
    pub name: String
}

type Located {
    pub location: String
}

type User {
    pub name: String
    pub location: String
    pub email: String
}

func label_size(value: Named & Located) -> Int {
    value.name.length + value.location.length
}

func name_size(value: Named) -> Int {
    value.name.length
}

func main() -> Int {
    user = User {
        name: "Jason"
        location: "Boston"
        email: "jason@example.com"
    }
    name_size(user) * 100 + label_size(user)
}
"#;
    let compilation = foster::compile(source).unwrap();
    let module = compilation.hir.module_named("main").unwrap();
    let label = compilation
        .hir
        .function_named(module, "label_size")
        .unwrap();
    let signature = compilation.types.function_type(label).unwrap();
    assert_eq!(
        compilation.types.display(signature.parameters[0]),
        "Named & Located"
    );
    assert_eq!(foster::run(source).unwrap(), Value::Integer(511));
}

#[test]
fn structural_adaptation_reports_missing_and_incompatible_fields() {
    let missing = foster::compile(
        r#"
type Named { pub name: String }
type Product { pub title: String }
func name(value: Named) -> String { value.name }
func main() -> String { name(Product { title: "Book" }) }
"#,
    )
    .unwrap_err();
    assert!(missing.message.contains("missing accessible field `name`"));

    let incompatible = foster::compile(
        r#"
type Named { pub name: String }
type NumericName { pub name: Int }
func name(value: Named) -> String { value.name }
func main() -> String { name(NumericName { name: 42 }) }
"#,
    )
    .unwrap_err();
    assert!(
        incompatible
            .message
            .contains("expected `String`, found `Int`")
    );
}

#[test]
fn consuming_a_structural_view_moves_the_original_value() {
    let source = r#"
type Named { pub name: String }
type User {
    pub name: String
    pub email: String
}
func take(value: Named) -> String [consume value] { value.name }
func main() -> String {
    user = User { name: "Jason", email: "jason@example.com" }
    take(move user)
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::String("Jason".into()));

    let invalid = source.replace("take(move user)\n}", "take(move user)\n    user.email\n}");
    let error = foster::compile(&invalid).unwrap_err();
    assert!(error.message.contains("used after it was moved"));
}

#[test]
fn private_record_fields_prevent_cross_module_structural_adaptation() {
    let error = foster::check_package("tests/fixtures/structural_privacy").unwrap_err();
    assert!(error.message.contains("field `value` is private"));
}

#[test]
fn structural_return_conversion_moves_and_narrows_the_value() {
    let source = r#"
type Named { pub name: String }
type User {
    pub name: String
    pub email: String
}
func as_named(user: User) -> Named [consume user] { user }
func main() -> Int {
    user = User { name: "Jason", email: "jason@example.com" }
    named = as_named(move user)
    named.name.length
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(5));
}

#[test]
fn structurally_adapts_records_containing_the_required_value() {
    let source = r#"
type Bar { pub value: Int }
type Foo { pub bar: Bar }
type Container {
    pub bar: Bar
    pub label: String
}
func extract(value: Foo) -> Int { value.bar.value }
func main() -> Int {
    extract(Container { bar: Bar { value: 42 }, label: "answer" })
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(42));
}

#[test]
fn strings_and_lists_implement_sequence_without_conversion() {
    let source = r#"
import core.sequence

func main() -> Int {
    letters = sequence.count("banana", (value: CodePoint) -> value == 'a')
    evens = sequence.count([1, 2, 3, 4], (value: Int) -> value / 2 * 2 == value)
    letters * 10 + evens
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::Integer(32));
}

#[test]
fn code_point_literals_are_distinct_copy_values() {
    let source = r#"
func main() -> String {
    value = 'λ'
    render = [copy value] () -> value.string
    branch {
        value.whitespace? -> "space"
        _ -> render()
    }
}
"#;
    assert_eq!(foster::run(source).unwrap(), Value::String("λ".into()));
}

#[test]
fn code_points_promote_through_integer_operators() {
    let source = r#"
func main() -> Int {
    digit = '9' - '0'
    branch {
        'A' == 65 -> digit * 10 + ('C' - 'A')
        _ -> 0
    }
}
"#;

    let compilation = foster::compile(source).unwrap();
    for optimize in [false, true] {
        let result =
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap();
        assert_eq!(result, Value::Integer(92));
    }
}

#[test]
fn code_points_do_not_expose_a_value_member() {
    let error = foster::compile("func main() -> Int { 'A'.value }").unwrap_err();
    assert!(error.message.contains("has no member `value`"));
}

#[test]
fn bytes_and_byte_buffers_enforce_bounds_and_round_trip_utf8() {
    let source = r#"
import core.byte
import core.byte_buffer
import core.bytes
import core.result

func byte_or(value: Result<Byte, ByteError>, fallback: Byte) -> Byte {
    branch value {
        Result.Ok(item) -> item
        Result.Error(_) -> fallback
    }
}

func text_or(value: Result<String, Utf8Error>) -> String {
    branch value {
        Result.Ok(text) -> text
        Result.Error(_) -> "invalid"
    }
}

func main() -> String {
    zero = byte_or(Byte.from(0), __byte_unchecked(0))
    capital_a = byte_or(Byte.from(65), zero)
    lower_x = byte_or(Byte.from(120), zero)

    buffer = ByteBuffer.with_capacity(4)
    buffer.push(capital_a)
    buffer.extend("BC".utf8)
    buffer[1] = lower_x

    data = buffer.snapshot
    text_or(String.from_utf8(data)) + ":" + data.hex
}
"#;

    let compilation = foster::compile(source).unwrap();
    for optimize in [false, true] {
        assert_eq!(
            foster::vm::run_with_options(&compilation, foster::vm::CompileOptions { optimize })
                .unwrap(),
            Value::String("AxC:417843".into())
        );
    }
}

#[test]
fn byte_construction_rejects_out_of_range_integers() {
    let source = r#"
import core.byte
import core.result

func main() -> Int {
    branch Byte.from(256) {
        Result.Ok(value) -> value.int
        Result.Error(error) -> error.value
    }
}

"#;

    assert_eq!(foster::run(source).unwrap(), Value::Integer(256));
}

#[test]
fn byte_bitwise_operators_preserve_byte_values() {
    let source = r#"
func main() -> Int {
    high = __byte_unchecked(240)
    low = __byte_unchecked(15)
    mixed = (high & ~low) | (low ^ __byte_unchecked(3))
    shifted = mixed >> 2
    shifted.int + (__byte_unchecked(1) << 7).int
}

"#;

    assert_eq!(foster::run(source).unwrap(), Value::Integer(191));
}

#[test]
fn bytes_decode_hex_and_report_invalid_utf8() {
    let source = r#"
import core.bytes
import core.result

func decode(value: Result<Bytes, HexError>) -> String {
    branch value {
        Result.Error(error) -> error.message
        Result.Ok(data) -> branch String.from_utf8(data) {
            Result.Ok(text) -> text
            Result.Error(_) -> data.hex
        }
    }
}

func main() -> String {
    decode(Bytes.from_hex("4869")) + ":" + decode(Bytes.from_hex("ff"))
}
"#;

    assert_eq!(foster::run(source).unwrap(), Value::String("Hi:ff".into()));
}

#[test]
fn bytes_are_iterable_collections() {
    let source = r#"
import core.bytes
import core.collection
import core.option
import core.result

func size(values: Collection<Byte>) -> Int {
    values.length
}

func first(value: Option<Byte>) -> Int {
    branch value {
        Option.Some(item) -> item.int
        Option.None -> -1
    }
}

func unpack(value: Result<Bytes, HexError>) -> Int {
    branch value {
        Result.Error(_) -> -1
        Result.Ok(data) -> size(data) * 100 + first(data.iterator.next())
    }
}

func main() -> Int {
    unpack(Bytes.from_hex("2a2b"))
}
"#;

    assert_eq!(foster::run(source).unwrap(), Value::Integer(242));
}

#[test]
fn freezing_a_byte_buffer_produces_bytes_and_consumes_the_buffer() {
    let source = r#"
import core.byte_buffer

func main() -> String {
    buffer = ByteBuffer.empty()
    buffer.push(__byte_unchecked(42))
    data = (move buffer).freeze()
    data.hex
}
"#;

    assert_eq!(foster::run(source).unwrap(), Value::String("2a".into()));

    let invalid = source.replace("    data.hex", "    buffer.length\n    data.hex");
    let error = foster::compile(&invalid).unwrap_err();
    assert!(
        error.message.contains("used after it was moved"),
        "{}",
        error.message
    );
}

#[test]
fn structural_byte_buffer_mutation_invalidates_element_loans() {
    let source = r#"
import core.byte_buffer

func main() -> Int {
    buffer = ByteBuffer.empty()
    buffer.push(__byte_unchecked(1))
    item = ref buffer[0]
    buffer.extend("more".utf8)
    item.int
}
"#;

    let error = foster::compile(source).unwrap_err();
    assert!(error.message.contains("invalidated"), "{}", error.message);
}

#[test]
fn generic_stream_contracts_handle_partial_io_and_eof() {
    let source = r#"
import core.byte_buffer
import core.bytes
import core.int
import core.result
import core.stream

type StreamError {
    message: String
}

type ChunkReader & Reader<StreamError> {
    remaining: Bytes
    chunk_size: Int
}

type CollectWriter & Writer<StreamError> {
    contents: Bytes
    chunk_size: Int
}

func read(self: ChunkReader, maximum: Int) -> Result<Bytes, StreamError> [mut self.remaining, read self.chunk_size] {
    limit = smaller(maximum, self.chunk_size)
    amount = smaller(limit, self.remaining.length)
    chunk = self.remaining.slice(0, amount)
    self.remaining = self.remaining.slice(amount, self.remaining.length)
    Result.Ok(chunk)
}

func write(self: CollectWriter, contents: Bytes) -> Result<Int, StreamError> [mut self.contents, read self.chunk_size] {
    amount = smaller(self.chunk_size, contents.length)
    self.contents = self.contents.concat(contents.slice(0, amount))
    Result.Ok(amount)
}

func flush(self: CollectWriter) -> Result<Unit, StreamError> {
    scratch = ByteBuffer.empty()
    Result.Ok(scratch.reserve(0))
}

func smaller(left: Int, right: Int) -> Int {
    branch {
        left < right -> left
        _ -> right
    }
}

func decoded(outcome: Result<Bytes, HexError>) -> Bytes {
    branch outcome {
        Result.Error(_) -> Bytes.empty()
        Result.Ok(contents) -> contents
    }
}

func rendered(outcome: Result<Bytes, StreamError>) -> String {
    branch outcome {
        Result.Error(error) -> error.message
        Result.Ok(contents) -> contents.hex
    }
}

func copied(outcome: Result<Int, StreamError>) -> String {
    branch outcome {
        Result.Error(error) -> error.message
        Result.Ok(count) -> int.to_string(count)
    }
}

func main() -> String {
    all_contents = decoded(Bytes.from_hex("00010203040506"))
    all_reader = ChunkReader { remaining: all_contents, chunk_size: 2 }
    all = rendered(read_all(all_reader))

    copy_contents = decoded(Bytes.from_hex("00010203040506"))
    copy_reader = ChunkReader { remaining: copy_contents, chunk_size: 2 }
    writer = CollectWriter { contents: Bytes.empty(), chunk_size: 3 }
    count = copied(stream.copy(copy_reader, writer))
    all + ":" + writer.contents.hex + ":" + count
}
"#;

    for optimize in [false, true] {
        assert_eq!(
            foster::run_with_options(source, foster::vm::CompileOptions { optimize }).unwrap(),
            Value::String("00010203040506:00010203040506:7".into())
        );
    }
}

#[test]
fn runs_the_generic_recursive_linked_list_example() {
    let source = include_str!("../examples/linked_list.fos");
    for optimize in [false, true] {
        assert_eq!(
            foster::run_with_options(source, foster::vm::CompileOptions { optimize }).unwrap(),
            Value::Integer(13)
        );
    }
}
