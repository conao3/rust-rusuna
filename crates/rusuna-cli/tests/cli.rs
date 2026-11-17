use std::process::{Command, Output};

const SDL: &str = "type A @table(name: \"a\") { id: Int! } type Query { rows: [A!]! @list }";

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rusuna"))
        .args(args)
        .output()
        .unwrap()
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
        vec!["schema", "expand-sdl", "--schema", "invalid"],
        vec![
            "query",
            "compile-sql",
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
        vec!["query", "compile-sql", "help"],
        vec!["--version"],
    ] {
        let result = run(&args);
        assert!(result.status.success(), "{args:?}: {:?}", result.stderr);
        assert!(!result.stdout.is_empty());
    }
}

#[test]
fn expansion_and_compilation_support_json_text_and_files() {
    let expanded = run(&["schema", "expand-sdl", "--schema", SDL]);
    assert!(expanded.status.success());
    let value: serde_json::Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert!(value["Sdl"].as_str().unwrap().contains("A_bool_exp"));
    let query = "query Q($id: Int!) { result: rows(where: {id: {_eq: $id}}) { id } }";
    let compiled = run(&[
        "query",
        "compile-sql",
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
            "--schema",
            &file,
            "--output",
            output,
        ]);
        assert!(result.status.success());
        let result = run(&[
            "query",
            "compile-sql",
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
