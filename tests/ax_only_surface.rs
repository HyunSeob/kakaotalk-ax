use std::path::Path;
use std::process::Command;

#[test]
fn obsolete_cli_routes_are_rejected_before_native_access() {
    let bin = env!("CARGO_BIN_EXE_kax");
    for cmd in ["setup", "sql", "export", "send", "prepare", "result"] {
        let output = Command::new(bin).arg(cmd).output().expect("spawn kax");
        assert_eq!(output.status.code(), Some(2), "cmd={cmd}");
        assert!(
            output.stdout.is_empty(),
            "expected empty stdout for {cmd}, got {:?}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn cargo_manifest_and_lockfile_have_no_database_dependencies() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in ["Cargo.toml", "Cargo.lock"] {
        let content = std::fs::read_to_string(root.join(file)).expect("read manifest/lock");
        for forbidden in ["sqlite", "rusqlite", "sqlcipher", "diesel", "sqlx"] {
            assert!(
                !content.to_lowercase().contains(forbidden),
                "forbidden DB dependency {forbidden} found in {file}"
            );
        }
    }
}

#[test]
fn ax_read_negative_limit_returns_exit_1_and_json_value_error() {
    let bin = env!("CARGO_BIN_EXE_kax");
    let output = Command::new(bin)
        .args(["ax-read", "--window-title", "fixture", "--limit", "-1"])
        .output()
        .expect("spawn kax");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        !output.stdout.is_empty(),
        "expected JSON error payload on stdout"
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be valid JSON");
    assert_eq!(parsed["status"], "failed");
    assert_eq!(parsed["errorType"], "ValueError");
}

#[test]
fn ax_call_missing_required_arg_returns_exit_1_and_type_error() {
    let bin = env!("CARGO_BIN_EXE_kax");
    let output = Command::new(bin)
        .args(["ax-call", "read_messages"])
        .output()
        .expect("spawn kax");
    assert_eq!(output.status.code(), Some(1));
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be valid JSON");
    assert_eq!(parsed["status"], "failed");
    assert_eq!(parsed["errorType"], "TypeError");
}

#[test]
fn cli_args_file_missing_malformed_json_and_non_utf8_match_python_error_types() {
    let bin = env!("CARGO_BIN_EXE_kax");
    let dir = tempfile::tempdir().unwrap();

    // 1. Missing --args-file -> FileNotFoundError
    let missing = dir.path().join("does-not-exist.json");
    let out1 = Command::new(bin)
        .args(["ax-call", "windows", "--args-file"])
        .arg(&missing)
        .output()
        .expect("spawn kax");
    assert_eq!(out1.status.code(), Some(1));
    let j1: serde_json::Value = serde_json::from_slice(&out1.stdout).unwrap();
    assert_eq!(j1["status"], "failed");
    assert_eq!(j1["errorType"], "FileNotFoundError");

    // 2. Malformed JSON "{" -> JSONDecodeError
    let malformed = dir.path().join("malformed.json");
    std::fs::write(&malformed, b"{").unwrap();
    let out2 = Command::new(bin)
        .args(["ax-call", "windows", "--args-file"])
        .arg(&malformed)
        .output()
        .expect("spawn kax");
    assert_eq!(out2.status.code(), Some(1));
    let j2: serde_json::Value = serde_json::from_slice(&out2.stdout).unwrap();
    assert_eq!(j2["status"], "failed");
    assert_eq!(j2["errorType"], "JSONDecodeError");

    // 3. Non-UTF8 byte 0xFF -> UnicodeDecodeError
    let bad_utf8 = dir.path().join("bad_utf8.json");
    std::fs::write(&bad_utf8, [0xFFu8]).unwrap();
    let out3 = Command::new(bin)
        .args(["ax-call", "windows", "--args-file"])
        .arg(&bad_utf8)
        .output()
        .expect("spawn kax");
    assert_eq!(out3.status.code(), Some(1));
    let j3: serde_json::Value = serde_json::from_slice(&out3.stdout).unwrap();
    assert_eq!(j3["status"], "failed");
    assert_eq!(j3["errorType"], "UnicodeDecodeError");
}

#[test]
fn removed_implementations_and_python_entrypoints_are_not_shipped() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for rel in [
        "pyproject.toml",
        "src/kakaotalk_reader.py",
        "src/kakaotalk_service.py",
        "src/kakaotalk_media.py",
        "src/kakaotalk_loco.py",
        "src/kakaotalk_tablet.py",
        "src/kakaotalk_ui.py",
        "src/kakaotalk_automation.py",
        "src/kakaotalk_native",
        "skills/kakaotalk-ax/scripts/kax.py",
    ] {
        assert!(!root.join(rel).exists(), "removed artifact still exists: {rel}");
    }
}

#[test]
fn skill_and_readme_invoke_kax_directly_without_python_entrypoints() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let skill = std::fs::read_to_string(root.join("skills/kakaotalk-ax/SKILL.md")).unwrap();
    let api_ref =
        std::fs::read_to_string(root.join("skills/kakaotalk-ax/references/ax-api.md")).unwrap();
    let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
    for doc in [&skill, &api_ref, &readme] {
        assert!(doc.contains("kax"));
        assert!(!doc.contains("scripts/kax.py"));
        assert!(!doc.contains("from kakaotalk_automation import NativeAX"));
    }
}

#[test]
fn cli_help_lists_expected_subcommands() {
    let bin = env!("CARGO_BIN_EXE_kax");
    let output = Command::new(bin).arg("--help").output().expect("spawn kax");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for sub in ["mcp", "build", "ax-call", "ax-windows", "status", "ax-read", "migrate-journal"] {
        assert!(stdout.contains(sub), "missing subcommand {sub} in --help");
    }
}
