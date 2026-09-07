const PRELUDE: &str = r#"
import core.result
import core.remote_error
type Worker = {}
impl Worker { func work(self) -> Int { 42 } }
"#;

#[test]
fn pending_requests_are_not_discharged_by_discard_moves_or_partial_awaits() {
    for body in [
        "let worker = remote Worker {}\nworker.work()\n0",
        "let worker = remote Worker {}\nlet pending = worker.work()\n0",
        "let worker = remote Worker {}\nlet pending = worker.work()\nlet moved = move pending\n0",
        "let worker = remote Worker {}\nlet pending = worker.work()\nbranch flag { true -> { await pending }\n_ -> Result.Ok(0) }\n0",
        "let worker = remote Worker {}\nloop { worker.work()\nbreak }\n0",
        "loop { let worker = remote Worker {}\nworker.work()\nbreak }\n0",
        "let pending = branch { _ -> { let worker = remote Worker {}\nworker.work() } }\nawait pending\n0",
        "let worker = remote Worker {}\nlet pending = worker.work()\nreturn 0 if flag\nawait pending\n0",
        "let worker = remote Worker {}\nlet pending = worker.work()\nworker = remote Worker {}\nawait pending\n0",
    ] {
        let source = format!(
            "{PRELUDE}\nfunc check(flag: Bool) -> Int {{ {body} }}\nfunc main() -> Int {{ check(true) }}"
        );
        let error = foster::compile(&source).expect_err("pending request was accepted");
        assert_eq!(error.code.as_deref(), Some("E0730"), "{body}: {error:?}");
        assert!(error.labels.len() >= 2);
    }
}

#[test]
fn returning_a_future_does_not_keep_its_local_owner_alive() {
    let source = format!(
        "{PRELUDE}\nfunc pending() -> Future<Result<Int, RemoteError>> {{ let worker = remote Worker {{}}\nworker.work() }}\nfunc main() -> Int {{ 0 }}"
    );
    let error = foster::compile(&source).expect_err("escaping pending future was accepted");
    assert_eq!(error.code.as_deref(), Some("E0730"), "{error:?}");
}

#[test]
fn completed_requests_and_owner_transfers_are_accepted() {
    for body in [
        "let worker = remote Worker {}\nlet pending = worker.work()\nawait pending\n0",
        "let worker = remote Worker {}\nlet pending = worker.work()\nlet moved = move worker\nawait pending\n0",
        "let worker = remote Worker {}\nlet pending = worker.work()\nlet moved = move pending\nawait moved\n0",
        "let worker = remote Worker {}\nlet pending = worker.work()\nbranch flag { true -> { await pending }\n_ -> { await pending } }\n0",
        "let worker = remote Worker {}\nlet index = 0\nloop { break if index == 3\nawait worker.work()\nindex = index + 1 }\n0",
        "let worker = remote Worker {}\nworker.work()\nawait worker.work()\n0",
        "let worker = branch { _ -> remote Worker {} }\nawait worker.work()\n0",
    ] {
        let source = format!(
            "{PRELUDE}\nfunc check(flag: Bool) -> Int {{ {body} }}\nfunc main() -> Int {{ check(true) }}"
        );
        foster::compile(&source).unwrap_or_else(|error| panic!("{body}: {error:?}"));
    }
}

#[test]
fn transferring_only_an_owner_cannot_hide_outstanding_work() {
    let source = format!(
        "{PRELUDE}\nfunc finish(worker: Remote<Worker>) -> () [consume worker] {{ () }}\nfunc main() -> Int {{ let worker = remote Worker {{}}\nlet pending = worker.work()\nfinish(move worker)\nawait pending\n0 }}"
    );
    let error = foster::compile(&source).expect_err("owner transfer hid a pending request");
    assert_eq!(error.code.as_deref(), Some("E0730"), "{error:?}");
}

#[test]
fn pending_aggregate_transfers_are_rejected_and_record_factories_keep_owners() {
    for declaration in [
        "func make() -> Remote<Worker> { let worker = remote Worker {}\nworker.work()\nmove worker }",
        "type Job = { owner: Remote<Worker>, pending: Future<Result<Int, RemoteError>> }\nfunc make() -> Job { let worker = remote Worker {}\nlet pending = worker.work()\nJob { owner: move worker, pending: move pending } }",
    ] {
        let source = format!("{PRELUDE}\n{declaration}\nfunc main() -> Int {{ 0 }}");
        let error =
            foster::compile(&source).expect_err("pending responsibility was lost at return");
        assert_eq!(error.code.as_deref(), Some("E0730"), "{error:?}");
    }
    for (expression, valid) in [
        ("held.owner.work()", false),
        ("await held.owner.work()", true),
    ] {
        let source = format!(
            "{PRELUDE}\ntype Holder = {{ owner: Remote<Worker> }}\nfunc make() -> Holder {{ Holder {{ owner: remote Worker {{}} }} }}\nfunc main() -> Int {{ let held = make()\n{expression}\n0 }}"
        );
        let result = foster::compile(&source);
        assert_eq!(result.is_ok(), valid, "{result:?}");
    }
}

#[test]
fn direct_future_helpers_preserve_the_callers_obligation() {
    for (expression, valid) in [("issue(worker)", false), ("await issue(worker)", true)] {
        let source = format!(
            "{PRELUDE}\nfunc issue(worker: Remote<Worker>) -> Future<Result<Int, RemoteError>> {{ worker.work() }}\nfunc main() -> Int {{ let worker = remote Worker {{}}\n{expression}\n0 }}"
        );
        let result = foster::compile(&source);
        assert_eq!(result.is_ok(), valid, "{result:?}");
    }
}

#[test]
fn futures_stored_in_records_retain_request_identity() {
    let prefix =
        format!("{PRELUDE}\ntype Pending = {{ value: Future<Result<Int, RemoteError>> }}\n");
    for (ending, valid) in [("await held.value\n0", true), ("0", false)] {
        let source = format!(
            "{prefix}\nfunc main() -> Int {{ let worker = remote Worker {{}}\nlet held = Pending {{ value: worker.work() }}\n{ending} }}"
        );
        let result = foster::compile(&source);
        assert_eq!(result.is_ok(), valid, "{result:?}");
    }
}

#[test]
fn remote_message_arguments_do_not_own_the_receivers_request() {
    let source = format!(
        "{PRELUDE}\nimpl Worker {{ func accept(self, other: Remote<Worker>) -> Int [consume other] {{ 42 }} }}\nfunc main() -> Int {{ let worker = remote Worker {{}}\nlet other = remote Worker {{}}\nawait worker.accept(move other)\n0 }}"
    );
    foster::compile(&source).unwrap();
}

#[test]
fn consumed_owner_parameters_cannot_return_pending_futures() {
    let source = format!(
        "{PRELUDE}\nfunc issue(worker: Remote<Worker>) -> Future<Result<Int, RemoteError>> [consume worker] {{ worker.work() }}\nfunc main() -> Int {{ 0 }}"
    );
    let error = foster::compile(&source).expect_err("consumed owner outlived its function");
    assert_eq!(error.code.as_deref(), Some("E0730"));
}
