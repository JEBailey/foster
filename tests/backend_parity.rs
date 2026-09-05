//! The same source and expectations must hold for both execution engines and optimization modes.
use foster::{native, vm};

#[test]
fn foster_library_loops_slices_and_builders_agree() {
    check(
        "library-algorithms",
        include_str!("fixtures/programs/library_algorithms.fos"),
        Ok("42"),
    );
}

#[test]
fn owned_list_reads_check_bounds_in_both_backends() {
    check(
        "list-at-bounds",
        "import core.list\nfunc main() -> Int { [1].at(-1) }",
        Err("index"),
    );
}

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "foster-parity-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn check(name: &str, source: &str, expected: Result<&str, &str>) {
    let compilation = foster::compile(source).unwrap();
    let prepared = native::prepare(&compilation).unwrap();
    let scratch = Scratch::new(name);
    for optimize in [false, true] {
        let result = vm::run_with_options(&compilation, vm::CompileOptions { optimize });
        match expected {
            Ok(value) => assert_eq!(
                result.unwrap().to_string(),
                value,
                "VM {name}, optimize={optimize}"
            ),
            Err(message) => assert!(
                result.unwrap_err().to_string().contains(message),
                "VM {name}"
            ),
        }
        let executable = scratch.0.join(format!(
            "program-{optimize}{}",
            std::env::consts::EXE_SUFFIX
        ));
        prepared
            .build_executable(&executable, native::CompileOptions { optimize })
            .unwrap();
        let output = std::process::Command::new(executable).output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        match expected {
            Ok(value) => {
                assert!(output.status.success(), "native {name}: {stderr}");
                assert_eq!(
                    String::from_utf8_lossy(&output.stdout).trim(),
                    value,
                    "native {name}, optimize={optimize}"
                );
            }
            Err(message) => {
                assert!(
                    !output.status.success(),
                    "native {name} unexpectedly succeeded"
                );
                assert!(stderr.contains(message), "native {name}: {stderr}");
            }
        }
    }
}

#[test]
fn aggregates_compare_by_value() {
    check(
        "equality",
        r#"
import core.list
type Point = { x: Int, label: String }
enum Choice = Number(Int) | Text(String) | Empty
func empty() -> List<Int> { [] }
func main() -> Bool {
    assert(Point { x: 1, label: "a" } == Point { x: 1, label: "a" })
    assert(Point { x: 1, label: "a" } != Point { x: 2, label: "a" })
    assert([1, 2] == [1, 2])
    assert([1, 2] != [1, 3])
    assert([1] != [1, 2])
    assert(Choice.Number(1) == Choice.Number(1))
    assert(Choice.Number(1) != Choice.Number(2))
    assert(Choice.Empty == Choice.Empty)
    assert(Choice.Number(1) != Choice.Empty)
    assert([Choice.Text("abc")] == [Choice.Text("a" + "bc")])
    assert([[1], [2]] == [[1], [2]])
    assert([0.0] == [-0.0])
    assert(empty() == empty())
    assert("abc".bytes == ("a" + "bc").bytes)
    assert("abc".bytes != "abd".bytes)
    assert([Point { x: 1, label: "a" }].contains?(Point { x: 1, label: "a" }))
    let nan = 0.0 / 0.0
    assert([nan] != [nan])
    let values = [1, 2]
    values[0] = 3
    values == [3, 2]
}
"#,
        Ok("true"),
    );
}

#[test]
fn minimum_integer_negation_is_a_language_error() {
    check(
        "negation",
        "func main() -> Int { let value = -9223372036854775807 - 1\n -value }",
        Err("overflow"),
    );
}

#[test]
fn assertions_are_language_errors() {
    check(
        "assertion",
        "func main() { assert(false, \"parity failure\") }",
        Err("parity failure"),
    );
}

#[test]
fn ownership_and_remote_ordering_agree() {
    check(
        "ownership-remote",
        r#"
type Counter = { value: Int }
func Counter.increment(self: Counter, amount: Int) -> Int [mut self] {
    self.value = self.value + amount
    self.value
}
func set[g: group Int](value: ref[g] Int, replacement: Int) -> Int [mut g] {
    value = replacement
}
func take(value: String) -> () [consume value] { () }
func main() -> Int {
    let text = "before"
    take(move text)
    text = "after"
    assert(text == "after")
    let values = [0, 2]
    set(ref values[0], 40)
    assert(values == [40, 2])
    let worker = remote Counter { value: 0 }
    let first = worker.increment(20)
    let second = worker.increment(2)
    await first + await second
}
"#,
        Ok("42"),
    );
}

#[test]
fn generic_calls_preserve_values() {
    check(
        "generics",
        r#"
type Echo<T> = { value: T }
func Echo.get<T>(self: Echo<T>) -> T { self.value }
func identity<T>(value: T) -> T { value }
func main() -> Bool { identity(Echo { value: true }.get()) }
"#,
        Ok("true"),
    );
}

#[test]
fn generic_remote_calls_preserve_logical_types() {
    check(
        "remote-generics",
        r#"
type Echo = {}
func Echo.identity<T>(self: Echo, value: T) -> T { value }
func Echo.tag<T>(self: Echo, value: T) -> Int { 1 }
func main() -> Int {
    let worker = remote Echo {}
    let number = await worker.identity(41)
    let tag = await worker.tag(:hello)
    assert(await worker.identity(:hello) == :hello)
    number + tag
}
"#,
        Ok("42"),
    );
}

#[test]
fn verifier_rejects_inconsistent_specialized_call_results() {
    for source in [
        "type Echo<T> = { value: T }\nfunc Echo.get<T>(self: Echo<T>) -> T { self.value }\nfunc main() -> Bool { Echo { value: true }.get() }",
        "func identity<T>(value: T) -> T { value }\nfunc main() -> Bool { identity(true) }",
    ] {
        let compilation = foster::compile(source).unwrap();
        let mut program =
            vm::compile_with_options(&compilation, vm::CompileOptions { optimize: false }).unwrap();
        assert!(vm::verify(&program).is_ok());
        assert!(vm::decode_program(&vm::encode_program(&program).unwrap()).is_ok());
        program
            .functions
            .get_mut(&program.main.unwrap())
            .unwrap()
            .result_type = vm::VerificationType::Integer;
        assert!(
            vm::verify(&program).is_err(),
            "{:#?}",
            program.functions[&program.main.unwrap()]
        );
        assert!(vm::encode_program(&program).is_err());
    }
}
