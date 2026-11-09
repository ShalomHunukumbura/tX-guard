use std::process::Command;

fn tx_guard(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_tx-guard"))
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("the binary runs");
    (
        out.status.code().expect("exited normally"),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn text_report_on_the_fixture_shop() {
    let (code, out, _) = tx_guard(&["tests/fixtures/shop"]);
    assert_eq!(code, 1);
    let expected = "\
tests/fixtures/shop/orders.py:11:5  TG003 error  requests.post() sends a POST request while the transaction opened on line 9 is still open.
tests/fixtures/shop/orders.py:12:5  TG002 error  send_mail() sends an email while the transaction opened on line 9 is still open.
tests/fixtures/shop/orders.py:13:5  TG001 error  ship_order.delay() queues a Celery task while the transaction opened on line 9 is still open.
tests/fixtures/shop/orders.py:19:17  TG004 warning  requests.get() waits on another service while the transaction opened on line 18 is still open.
Found 3 errors and 1 warning in 2 files.
";
    let got: String = out
        .lines()
        .filter(|l| !l.starts_with("    fix:"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn every_finding_has_a_fix_line() {
    let (_, out, _) = tx_guard(&["tests/fixtures/shop/orders.py"]);
    assert_eq!(out.matches("\n    fix: ").count(), 4, "{out}");
    assert!(out.contains("fix: Queue it after the commit"), "{out}");
}

#[test]
fn clean_file_exits_zero() {
    let (code, out, _) = tx_guard(&["tests/fixtures/shop/clean.py"]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "All clear: checked 1 file, nothing runs before a commit.\n"
    );
}

#[test]
fn warnings_fail_only_with_strict() {
    let args = ["--ignore", "TG001,TG002,TG003", "tests/fixtures/shop"];
    assert_eq!(tx_guard(&args).0, 0);
    let strict = [
        "--strict",
        "--ignore",
        "TG001,TG002,TG003",
        "tests/fixtures/shop",
    ];
    assert_eq!(tx_guard(&strict).0, 1);
}

#[test]
fn github_annotations() {
    let (_, out, _) = tx_guard(&["--format", "github", "tests/fixtures/shop/orders.py"]);
    let first = out.lines().next().unwrap();
    assert!(
        first.starts_with(
            "::error file=tests/fixtures/shop/orders.py,line=11,col=5,title=TG003 http-write-in-transaction::"
        ),
        "{first}"
    );
    assert!(out.contains("::warning "), "{out}");
}

#[test]
fn json_report() {
    let (_, out, _) = tx_guard(&["--format", "json", "tests/fixtures/shop/orders.py"]);
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 4);
    assert_eq!(rows[0]["rule"], "TG003");
    assert_eq!(rows[0]["severity"], "error");
    assert_eq!(rows[2]["name"], "task-before-commit");
}

#[test]
fn explain_and_list_rules() {
    let (code, out, _) = tx_guard(&["--explain", "tg001"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("TG001 task-before-commit (error)"), "{out}");
    assert!(out.contains("Why it matters:"), "{out}");
    let (_, list, _) = tx_guard(&["--list-rules"]);
    assert_eq!(list.lines().count(), 4, "{list}");
}

#[test]
fn usage_errors_exit_two() {
    let (code, _, err) = tx_guard(&["--ignore", "TG999", "tests/fixtures/shop"]);
    assert_eq!(code, 2);
    assert!(err.contains("unknown rule TG999"), "{err}");
    assert_eq!(tx_guard(&["--explain", "XX1"]).0, 2);
    assert_eq!(tx_guard(&["does/not/exist"]).0, 2);
}
