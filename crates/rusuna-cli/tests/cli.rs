use std::process::{Command, Output};

const SDL: &str = "type A @table(name: \"a\") { id: Int! } type Query { rows: [A!]! @list }";

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rusuna"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn rest_roots_expand_and_report_that_they_have_no_sql_statement() {
    let schema =
        r#"type Query { time: Int @rest(source: "clock", path: "/time", roles: ["reader"]) }"#;
    let output = run(&[
        "schema",
        "expand-sdl",
        "--role",
        "reader",
        "--schema",
        schema,
    ]);
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        value["Sdl"]
            .as_str()
            .unwrap()
            .contains(r#"time: Int @rest(source: "clock""#)
    );
    let output = run(&[
        "query",
        "compile-sql",
        "--role",
        "reader",
        "--schema",
        schema,
        "--query",
        "{ time }",
    ]);
    assert_eq!(output.status.code(), Some(254));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("the REST root field `time` has no SQL statement")
    );
}

#[test]
fn syntax_and_target_errors_have_distinct_exit_codes() {
    for args in [
        vec![],
        vec!["unknown"],
        vec!["schema", "expand-sdl"],
        vec!["schema", "expand-sdl", "schema.graphql"],
        vec![
            "query",
            "compile-sql",
            "--role",
            "admin",
            "--schema",
            SDL,
            "--query",
            "{}",
            "--variables",
            "[]",
        ],
    ] {
        assert_eq!(run(&args).status.code(), Some(252), "{args:?}");
    }
    for args in [
        vec![
            "schema",
            "expand-sdl",
            "--role",
            "admin",
            "--schema",
            "invalid",
        ],
        vec![
            "query",
            "compile-sql",
            "--role",
            "admin",
            "--schema",
            SDL,
            "--query",
            "mutation { rows { id } }",
        ],
    ] {
        assert_eq!(run(&args).status.code(), Some(254), "{args:?}");
    }
}

#[test]
fn all_help_levels_and_version_succeed() {
    for args in [
        vec!["help"],
        vec!["schema", "help"],
        vec!["schema", "expand-sdl", "help"],
        vec!["schema", "import-openapi", "help"],
        vec!["query", "compile-sql", "help"],
        vec!["events", "help"],
        vec!["events", "generate-migration", "help"],
        vec!["--version"],
    ] {
        let result = run(&args);
        assert!(result.status.success(), "{args:?}: {:?}", result.stderr);
        assert!(!result.stdout.is_empty());
    }
}

#[test]
fn expansion_and_compilation_support_json_text_and_files() {
    let expanded = run(&["schema", "expand-sdl", "--role", "admin", "--schema", SDL]);
    assert!(expanded.status.success());
    let value: serde_json::Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert!(value["Sdl"].as_str().unwrap().contains("A_bool_exp"));
    let query = "query Q($id: Int!) { result: rows(where: {id: {_eq: $id}}) { id } }";
    let compiled = run(&[
        "query",
        "compile-sql",
        "--role",
        "admin",
        "--schema",
        SDL,
        "--query",
        query,
        "--operation-name",
        "Q",
        "--variables",
        "{\"id\":2}",
    ]);
    assert!(compiled.status.success(), "{:?}", compiled.stderr);
    let value: serde_json::Value = serde_json::from_slice(&compiled.stdout).unwrap();
    assert_eq!(value["Fields"][0]["ResponseKey"], "result");
    assert_eq!(value["Fields"][0]["Params"], serde_json::json!([2]));
    assert!(value["Fields"][0]["Sql"].as_str().unwrap().contains("$1"));
    let file = format!(
        "file://{}/../../tests/fixtures/schema.graphql",
        env!("CARGO_MANIFEST_DIR")
    );
    for output in ["json", "text", "table"] {
        let result = run(&[
            "schema",
            "expand-sdl",
            "--role",
            "admin",
            "--schema",
            &file,
            "--output",
            output,
        ]);
        assert!(result.status.success());
        let result = run(&[
            "query",
            "compile-sql",
            "--role",
            "admin",
            "--schema",
            SDL,
            "--query",
            "{ rows { id } }",
            "--output",
            output,
        ]);
        assert!(result.status.success());
    }
}

#[test]
fn permissions_accept_session_json_and_require_the_selected_role() {
    let schema = r#"type A @table(name: "a") @permission(role: "user", columns: ["id"], where: {id: {_eq: "$id"}}) { id: Int! } type Query { rows: [A!]! @list }"#;
    let args = [
        "query",
        "compile-sql",
        "--schema",
        schema,
        "--query",
        "{ rows { id } }",
        "--role",
        "user",
    ];
    assert_eq!(run(&args).status.code(), Some(254));
    let mut with_session = args.to_vec();
    with_session.extend(["--session", r#"{"id":3}"#]);
    let compiled = run(&with_session);
    assert!(compiled.status.success(), "{:?}", compiled.stderr);
    let value: serde_json::Value = serde_json::from_slice(&compiled.stdout).unwrap();
    assert_eq!(value["Fields"][0]["Params"], serde_json::json!([3]));
}

#[test]
fn mutations_compile_in_root_field_order() {
    let output = run(&[
        "query",
        "compile-sql",
        "--role",
        "admin",
        "--schema",
        SDL,
        "--query",
        "mutation { first: insert_A_one(object: {id: 1}) { id } second: delete_A(where: {id: {_eq: 1}}) { affected_rows } }",
    ]);
    assert!(output.status.success(), "{:?}", output.stderr);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["Fields"][0]["ResponseKey"], "first");
    assert_eq!(value["Fields"][1]["ResponseKey"], "second");
    assert!(
        value["Fields"][0]["Sql"]
            .as_str()
            .unwrap()
            .contains("INSERT INTO")
    );
    assert!(
        value["Fields"][1]["Sql"]
            .as_str()
            .unwrap()
            .contains("DELETE FROM")
    );
}

#[test]
fn event_migrations_match_the_committed_sql() {
    let directory = format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"));
    let triggers = format!("file://{directory}/events.json");
    let text = run(&[
        "events",
        "generate-migration",
        "--triggers",
        &triggers,
        "--output",
        "text",
    ]);
    assert!(text.status.success(), "{:?}", text.stderr);
    let expected = format!("{directory}/events.sql");
    if std::env::var_os("RUSUNA_UPDATE_EXPECTED").is_some() {
        std::fs::write(&expected, &text.stdout).unwrap();
    }
    assert_eq!(
        String::from_utf8(text.stdout.clone()).unwrap(),
        std::fs::read_to_string(&expected).unwrap()
    );
    let json = run(&["events", "generate-migration", "--triggers", &triggers]);
    assert!(json.status.success());
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(
        value["Sql"].as_str().unwrap().trim_end(),
        String::from_utf8(text.stdout).unwrap().trim_end()
    );
    let empty = run(&["events", "generate-migration", "--output", "text"]);
    assert!(empty.status.success());
    assert!(
        !String::from_utf8(empty.stdout)
            .unwrap()
            .contains("CREATE OR REPLACE TRIGGER")
    );
}

#[test]
fn event_trigger_errors_have_distinct_exit_codes() {
    for triggers in [
        "{}",
        "[{\"Name\": \"a\"}]",
        "[{\"Name\": \"a\", \"Table\": \"t\", \"Operations\": [\"TRUNCATE\"]}]",
    ] {
        let result = run(&["events", "generate-migration", "--triggers", triggers]);
        assert_eq!(result.status.code(), Some(252), "{triggers}");
    }
    for triggers in [
        "[{\"Name\": \"a b\", \"Table\": \"t\", \"Operations\": [\"INSERT\"]}]",
        "[{\"Name\": \"a\", \"Table\": \"t\", \"Operations\": []}]",
        "[{\"Name\": \"a\", \"Table\": \"t\", \"Operations\": [\"INSERT\"], \"Columns\": [\"c\"]}]",
    ] {
        let result = run(&["events", "generate-migration", "--triggers", triggers]);
        assert_eq!(result.status.code(), Some(254), "{triggers}");
        let error: serde_json::Value = serde_json::from_slice(&result.stderr).unwrap();
        assert!(error["Error"].is_string());
    }
}

fn import_openapi(file: &str, source: &str, options: &[&str]) -> Output {
    let path = format!(
        "{}/tests/fixtures/openapi/{file}",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut args = vec!["schema", "import-openapi", &path, "--source", source];
    args.extend(options);
    run(&args)
}

#[test]
fn openapi_imports_match_the_committed_sdl() {
    for (file, source, roles, expected) in [
        (
            "petstore.yaml",
            "petstore",
            &[][..],
            include_str!("fixtures/openapi/petstore.graphql"),
        ),
        (
            "inventory.json",
            "inventory",
            &["--role", "user", "--role", "viewer"],
            include_str!("fixtures/openapi/inventory.graphql"),
        ),
    ] {
        let result = import_openapi(file, source, roles);
        assert!(result.status.success(), "{file}: {:?}", result.stderr);
        assert_eq!(std::str::from_utf8(&result.stdout).unwrap(), expected);
        assert!(result.stderr.is_empty());
        rusuna::Schema::from_sdl(expected).unwrap();
    }
}

#[test]
fn openapi_import_keeps_operations_with_any_requested_tag() {
    let result = import_openapi(
        "inventory.json",
        "inventory",
        &["--include-tag", "write", "--include-tag", "devices"],
    );
    assert!(result.status.success(), "{:?}", result.stderr);
    let sdl = std::str::from_utf8(&result.stdout).unwrap();
    assert!(sdl.contains("get_devices_by_device_id"));
    assert!(sdl.contains("record_inventory"));
    assert!(!sdl.contains("load_inventory"));
    assert!(!sdl.contains("Skipped HEAD"));
    assert!(!sdl.contains("LoadInventoryResponse"));

    let result = import_openapi("inventory.json", "inventory", &["--include-tag", "write"]);
    assert!(result.status.success(), "{:?}", result.stderr);
    let sdl = std::str::from_utf8(&result.stdout).unwrap();
    assert!(sdl.contains("type Query\n"));
    assert!(sdl.contains("input DetailsInput"));
    assert!(!sdl.contains("type Details "));
}

#[test]
fn openapi_import_errors_leave_stdout_empty() {
    for (file, expected) in [
        ("external.json", "external reference"),
        ("invalid-sdl.yaml", "invalid generated SDL"),
    ] {
        let result = import_openapi(file, "test", &[]);
        assert_eq!(result.status.code(), Some(2));
        assert!(result.stdout.is_empty());
        let error = std::str::from_utf8(&result.stderr).unwrap();
        assert!(error.contains(expected), "{file}: {error}");
    }
}
