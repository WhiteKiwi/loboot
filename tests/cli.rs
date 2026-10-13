use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    process::{Command, Output},
};
use tempfile::TempDir;

fn call(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_loboot"))
        .arg("--json")
        .arg("--root")
        .arg(root)
        .arg("--platform")
        .arg("linux")
        .args(args)
        .env("PATH", "")
        .output()
        .unwrap()
}
fn value(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}
fn manifest(dir: &Path) -> std::path::PathBuf {
    let p = dir.join("example.toml");
    let exe = std::env::current_exe().unwrap();
    fs::write(&p,format!("id = \"hello\"\nexecutable = {:?}\ncwd = {:?}\nargs = [\"literal $HOME %n\", \"\"]\nmode = \"once\"\nrestart = \"never\"\n",exe.to_str().unwrap(),dir.to_str().unwrap())).unwrap();
    p
}
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, at: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        if !at.exists() {
            return;
        }
        for e in fs::read_dir(at).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            if p.is_dir() {
                out.insert(
                    format!("{}/", p.strip_prefix(base).unwrap().display()),
                    vec![],
                );
                walk(base, &p, out);
            } else {
                out.insert(
                    p.strip_prefix(base).unwrap().to_string_lossy().into_owned(),
                    fs::read(p).unwrap(),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result);
    result
}
#[test]
fn offline_crud_and_all_dryruns_never_spawn_or_change_files() {
    let t = TempDir::new().unwrap();
    let root = t.path().canonicalize().unwrap().join("sandbox");
    let m = manifest(&t.path().canonicalize().unwrap());
    let m = m.to_str().unwrap();
    let out = call(&root, &["add", m, "--dry-run"]);
    assert!(out.status.success(), "{}", value(&out));
    assert!(!root.exists());
    for command in ["list", "discover", "doctor"] {
        let out = call(&root, &[command]);
        assert!(out.status.success(), "{}", value(&out));
        assert!(!root.exists());
    }
    let out = call(&root, &["add", m]);
    assert!(out.status.success(), "{}", value(&out));
    assert_eq!(value(&out)["schema_version"], "loboot.cli/v1");
    let before = snapshot(&root);
    for args in [
        vec!["add", m, "--dry-run"],
        vec!["update", m, "--dry-run"],
        vec!["remove", "hello", "--dry-run"],
        vec!["start", "hello", "--dry-run"],
        vec!["stop", "hello", "--dry-run"],
        vec!["restart", "hello", "--dry-run"],
        vec!["enable", "hello", "--dry-run"],
        vec!["disable", "hello", "--dry-run"],
    ] {
        let out = call(&root, &args);
        assert!(out.status.success(), "{args:?}: {}", value(&out));
        assert_eq!(snapshot(&root), before, "{args:?}");
    }
    for command in ["start", "stop", "restart", "enable", "disable", "status"] {
        let out = call(&root, &[command, "hello"]);
        assert!(!out.status.success());
        assert_eq!(value(&out)["error"]["code"], "offline_only");
        assert_eq!(snapshot(&root), before);
    }
    let out = call(&root, &["remove", "hello"]);
    assert!(out.status.success(), "{}", value(&out));
}
#[test]
fn drift_blocks_mutation_and_json_errors_are_machine_readable() {
    let t = TempDir::new().unwrap();
    let root = t.path().canonicalize().unwrap().join("sandbox");
    let m = manifest(&t.path().canonicalize().unwrap());
    assert!(call(&root, &["add", m.to_str().unwrap()]).status.success());
    fs::write(root.join("native/loboot-hello.service"), "foreign").unwrap();
    let before = snapshot(&root);
    for args in [
        vec!["remove", "hello"],
        vec!["update", m.to_str().unwrap()],
        vec!["start", "hello", "--dry-run"],
    ] {
        let out = call(&root, &args);
        assert!(!out.status.success());
        assert_eq!(value(&out)["ok"], false);
        assert_eq!(snapshot(&root), before);
    }
}
#[test]
fn parser_errors_keep_json_envelope() {
    let t = TempDir::new().unwrap();
    let out = call(t.path(), &["not-a-command"]);
    assert!(!out.status.success());
    assert_eq!(value(&out)["error"]["code"], "usage");
}
#[test]
fn platform_override_is_offline_only() {
    let out = Command::new(env!("CARGO_BIN_EXE_loboot"))
        .args(["--json", "--platform", "macos", "list"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(value(&out)["error"]["code"], "usage");
}

#[test]
fn macos_dryrun_allows_conditional_disable_and_never_touches_manager() {
    let t = TempDir::new().unwrap();
    let root = t.path().canonicalize().unwrap().join("mac-sandbox");
    let m = manifest(&t.path().canonicalize().unwrap());
    let mac = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_loboot"))
            .arg("--json")
            .arg("--root")
            .arg(&root)
            .args(["--platform", "macos"])
            .args(args)
            .env("PATH", "")
            .output()
            .unwrap()
    };
    let out = mac(&["add", m.to_str().unwrap()]);
    assert!(out.status.success(), "{}", value(&out));
    let before = snapshot(&root);
    for command in ["start", "stop", "restart", "enable", "disable", "status"] {
        let out = mac(&[command, "hello", "--dry-run"]);
        assert!(out.status.success(), "{command}: {}", value(&out));
        assert_eq!(snapshot(&root), before);
        if command == "disable" {
            assert_eq!(
                value(&out)["data"]["plans"][0]["blocked"]["code"],
                "stop_first"
            );
        }
    }
}
#[test]
fn ergonomic_argv_add_preserves_literal_arguments() {
    let t = TempDir::new().unwrap();
    let root = t.path().canonicalize().unwrap().join("sandbox");
    let exe = std::env::current_exe().unwrap();
    let cwd = t.path().canonicalize().unwrap();
    let out = call(
        &root,
        &[
            "add",
            "argv-worker",
            "--cwd",
            cwd.to_str().unwrap(),
            "--mode",
            "once",
            "--",
            exe.to_str().unwrap(),
            "--json",
            "$HOME",
            "",
        ],
    );
    assert!(out.status.success(), "{}", value(&out));
    let out = call(&root, &["show", "argv-worker"]);
    assert_eq!(
        value(&out)["data"]["service"]["manifest"]["args"],
        serde_json::json!(["--json", "$HOME", ""])
    );
}
#[test]
fn json_flag_after_separator_is_not_a_global_flag() {
    let out = Command::new(env!("CARGO_BIN_EXE_loboot"))
        .args(["show", "--", "--json", "extra"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(!out.stderr.is_empty());
}
