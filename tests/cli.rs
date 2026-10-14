use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    process::{Command, Output},
};
use tempfile::TempDir;

fn call(root: &Path, args: &[&str]) -> Output {
    call_platform(root, "linux", args)
}
fn call_platform(root: &Path, platform: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_loboot"))
        .arg("--json")
        .arg("--root")
        .arg(root)
        .arg("--platform")
        .arg(platform)
        .args(args)
        .env("PATH", "")
        .output()
        .unwrap()
}

fn stored_record(root: &Path) -> loboot::store::Record {
    toml::from_str(&fs::read_to_string(root.join("state/hello.toml")).unwrap()).unwrap()
}

fn set_environment(path: &Path, environment: &[(&str, &str)]) {
    let mut manifest: loboot::model::Manifest =
        toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    manifest.environment = environment
        .iter()
        .map(|(name, value)| ((*name).into(), (*value).into()))
        .collect();
    fs::write(path, toml::to_string(&manifest).unwrap()).unwrap();
}

fn assert_not_exposed(output: &Output, sentinel: &str) {
    assert!(!String::from_utf8_lossy(&output.stdout).contains(sentinel));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(sentinel));
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
        vec!["logs", "hello", "--dry-run"],
    ] {
        let out = call(&root, &args);
        assert!(out.status.success(), "{args:?}: {}", value(&out));
        assert_eq!(snapshot(&root), before, "{args:?}");
    }
    for command in [
        "start", "stop", "restart", "enable", "disable", "status", "logs",
    ] {
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
    for command in [
        "start", "stop", "restart", "enable", "disable", "status", "logs",
    ] {
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

#[test]
fn mac_logs_are_bounded_offline_and_dryrun_is_read_only() {
    let t = TempDir::new().unwrap();
    let base = t.path().canonicalize().unwrap();
    let root = base.join("logs-root");
    let m = manifest(&base);
    let mac = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_loboot"))
            .args(["--json", "--root"])
            .arg(&root)
            .args(["--platform", "macos"])
            .args(args)
            .env("PATH", "")
            .output()
            .unwrap()
    };
    assert!(mac(&["add", m.to_str().unwrap()]).status.success());
    let shown = value(&mac(&["show", "hello"]));
    let generation = shown["data"]["service"]["log_generation"].as_str().unwrap();
    let capture = root.join("state/logs").join(format!("hello-{generation}"));
    fs::write(capture.join("stdout.log"), "old\npassword=hidden\nlast\n").unwrap();
    let before = snapshot(&root);
    let dry = mac(&["logs", "hello", "--dry-run"]);
    assert!(dry.status.success());
    assert_eq!(snapshot(&root), before);
    let logs = mac(&["logs", "hello", "--lines", "2", "--max-bytes", "64"]);
    assert!(logs.status.success(), "{}", value(&logs));
    let data = value(&logs);
    assert_eq!(
        data["data"]["sources"][0]["text"],
        "password=[REDACTED]\nlast\n"
    );
    assert_eq!(data["data"]["sources"][1]["empty"], true);
    assert_eq!(snapshot(&root), before);
    let invalid = mac(&["logs", "hello", "--max-bytes", "0"]);
    assert_eq!(value(&invalid)["error"]["code"], "invalid_log_limit");
    assert_eq!(snapshot(&root), before);
    fs::remove_file(capture.join("stderr.log")).unwrap();
    assert_eq!(
        value(&mac(&["logs", "hello"]))["data"]["sources"][1]["missing"],
        true
    );
}

struct RecoveryFixture {
    _temp: TempDir,
    root: std::path::PathBuf,
    saved: std::path::PathBuf,
    before: serde_json::Value,
    after: serde_json::Value,
}
impl RecoveryFixture {
    fn new(operation: &str) -> Self {
        Self::with_environment(operation, None)
    }
    fn with_environment(operation: &str, environment: Option<&str>) -> Self {
        let temp = TempDir::new().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let root = base.join("recovery-root");
        let manifest_path = manifest(&base);
        if let Some(value) = environment {
            set_environment(&manifest_path, &[("APPLICATION_SETTING", value)]);
        }
        let installed = call(&root, &["add", manifest_path.to_str().unwrap()]);
        assert!(installed.status.success(), "{}", value(&installed));
        let installed = serde_json::to_value(stored_record(&root)).unwrap();
        let (before, after) = match operation {
            "add" => {
                // Native publication happened, but the ownership write did not.
                fs::remove_file(root.join("state/hello.toml")).unwrap();
                (serde_json::Value::Null, installed)
            }
            "remove" => {
                // Native deletion happened, but the ownership deletion did not.
                fs::remove_file(root.join("native/loboot-hello.service")).unwrap();
                (installed, serde_json::Value::Null)
            }
            "update" => {
                let text = fs::read_to_string(&manifest_path).unwrap();
                fs::write(
                    &manifest_path,
                    text.replace("literal $HOME %n", "new literal $HOME %n"),
                )
                .unwrap();
                let updated = call(&root, &["update", manifest_path.to_str().unwrap()]);
                assert!(updated.status.success(), "{}", value(&updated));
                let after = serde_json::to_value(stored_record(&root)).unwrap();
                // Ownership is after while the native definition is still before.
                fs::write(
                    root.join("native/loboot-hello.service"),
                    installed["native_bytes"].as_str().unwrap(),
                )
                .unwrap();
                (installed, after)
            }
            _ => panic!("unsupported fixture operation"),
        };
        fs::write(
            root.join("state/.pending.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version":1,"operation":operation,"id":"hello",
                "state_dir":root.join("state"),"native_dir":root.join("native"),
                "before":before,"after":after,
            }))
            .unwrap(),
        )
        .unwrap();
        fs::remove_file(root.join("state/.lock")).unwrap();
        Self {
            _temp: temp,
            root,
            saved: base.join("recovery.json"),
            before,
            after,
        }
    }
    fn save_plan(&self, target: &str) -> serde_json::Value {
        let output = call(&self.root, &["recover", "plan", "--to", target]);
        assert!(output.status.success(), "{}", value(&output));
        assert!(output.stderr.is_empty());
        fs::write(&self.saved, &output.stdout).unwrap();
        value(&output)
    }
    fn apply(&self, dry_run: bool) -> Output {
        let mut args = vec!["recover", "apply", self.saved.to_str().unwrap()];
        if dry_run {
            args.push("--dry-run");
        }
        call(&self.root, &args)
    }
}

#[test]
fn recovery_plan_without_journal_never_creates_directories_or_a_lock() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().canonicalize().unwrap().join("missing");
    for args in [
        vec!["recover", "plan"],
        vec!["recover", "plan", "--to", "before"],
        vec!["recover", "plan", "--to", "after", "--dry-run"],
    ] {
        let output = call(&root, &args);
        assert!(output.status.success(), "{}", value(&output));
        assert_eq!(
            value(&output)["data"],
            serde_json::json!({"pending":false,"plan":null})
        );
        assert!(!root.exists());
    }
}

#[test]
fn recovery_offline_plan_preview_apply_and_replay_cover_both_targets() {
    for operation in ["add", "update", "remove"] {
        for target in ["before", "after"] {
            let fixture = RecoveryFixture::new(operation);
            let before = snapshot(&fixture.root);
            let saved = fixture.save_plan(target);
            assert_eq!(saved["data"]["pending"], true);
            assert_eq!(saved["data"]["plan"]["target"], target);
            assert_eq!(
                saved["data"]["plan"]["schema_version"],
                "loboot.recovery/v1"
            );
            assert_eq!(snapshot(&fixture.root), before);
            let preview = fixture.apply(true);
            assert!(
                preview.status.success(),
                "{operation}/{target}: {}",
                value(&preview)
            );
            assert_eq!(value(&preview)["data"]["validated"], true);
            assert_eq!(value(&preview)["data"]["native_checked"], false);
            assert_eq!(snapshot(&fixture.root), before);
            assert!(!fixture.root.join("state/.lock").exists());
            let applied = fixture.apply(false);
            assert!(
                applied.status.success(),
                "{operation}/{target}: {}",
                value(&applied)
            );
            assert!(applied.stderr.is_empty());
            assert_eq!(value(&applied)["data"]["runtime_restored"], false);
            assert_eq!(value(&applied)["data"]["recovered"]["pair_verified"], true);
            assert_eq!(
                value(&applied)["data"]["recovered"]["journal_removed"],
                true
            );
            assert!(!fixture.root.join("state/.pending.json").exists());
            let selected = if target == "before" {
                &fixture.before
            } else {
                &fixture.after
            };
            if selected.is_null() {
                assert!(!fixture.root.join("state/hello.toml").exists());
                assert!(!fixture.root.join("native/loboot-hello.service").exists());
            } else {
                let shown = call(&fixture.root, &["show", "hello"]);
                assert!(shown.status.success(), "{}", value(&shown));
                assert_eq!(&value(&shown)["data"]["service"], selected);
            }
            let recovered = snapshot(&fixture.root);
            let replay = fixture.apply(false);
            assert!(!replay.status.success());
            assert_eq!(value(&replay)["error"]["code"], "no_pending_recovery");
            assert_eq!(snapshot(&fixture.root), recovered);
            let fresh = call(&fixture.root, &["recover", "plan"]);
            assert_eq!(
                value(&fresh)["data"],
                serde_json::json!({"pending":false,"plan":null})
            );
        }
    }
}

#[test]
fn recovery_default_target_is_before_and_saved_filename_can_be_relative() {
    let fixture = RecoveryFixture::new("add");
    let output = call(&fixture.root, &["recover", "plan"]);
    assert_eq!(value(&output)["data"]["plan"]["target"], "before");
    fs::write(&fixture.saved, output.stdout).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_loboot"))
        .args(["--json", "--root"])
        .arg(&fixture.root)
        .args([
            "--platform",
            "linux",
            "recover",
            "apply",
            "recovery.json",
            "--dry-run",
        ])
        .current_dir(fixture.saved.parent().unwrap())
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", value(&output));
    assert!(!fixture.root.join("state/.lock").exists());
}

#[test]
fn recovery_rejects_non_envelopes_unknown_fields_and_secret_bearing_parse_errors() {
    let fixture = RecoveryFixture::new("update");
    let saved = fixture.save_plan("before");
    let before = snapshot(&fixture.root);
    let mut invalid = vec![
        saved["data"].clone(),
        saved["data"]["plan"].clone(),
        serde_json::json!({"schema_version":"loboot.cli/v1","ok":false,"error":{"message":"private-plan-secret"}}),
    ];
    for pointer in [
        "",
        "/data",
        "/data/plan",
        "/data/plan/journal",
        "/data/plan/journal/before",
        "/data/plan/journal/before/manifest",
        "/data/plan/current_native",
        "/data/plan/current_native/identity",
        "/data/plan/journal_snapshot/identity",
    ] {
        let mut edited = saved.clone();
        edited
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(
                "private-plan-secret".into(),
                serde_json::json!("private-plan-secret"),
            );
        invalid.push(edited);
    }
    for (pointer, replacement) in [
        ("/schema_version", serde_json::json!("loboot.cli/v999")),
        ("/ok", serde_json::json!(false)),
        ("/data/pending", serde_json::json!(false)),
        ("/data/plan", serde_json::Value::Null),
        (
            "/data/plan/target",
            serde_json::json!("private-plan-secret"),
        ),
        (
            "/data/plan/id",
            serde_json::json!({"private-plan-secret":true}),
        ),
    ] {
        let mut edited = saved.clone();
        *edited.pointer_mut(pointer).unwrap() = replacement;
        invalid.push(edited);
    }
    for (pointer, field) in [
        ("", "schema_version"),
        ("/data", "pending"),
        ("/data", "plan"),
    ] {
        let mut edited = saved.clone();
        edited
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(field);
        invalid.push(edited);
    }
    for edited in invalid {
        fs::write(&fixture.saved, serde_json::to_vec(&edited).unwrap()).unwrap();
        let output = fixture.apply(true);
        assert!(!output.status.success());
        assert_eq!(value(&output)["error"]["code"], "invalid_recovery_plan");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("private-plan-secret"));
        assert!(output.stderr.is_empty());
        assert_eq!(snapshot(&fixture.root), before);
    }
    let encoded = serde_json::to_string(&saved).unwrap();
    for malformed in [
        b"{private-plan-secret".to_vec(),
        [encoded.as_bytes(), b" private-plan-secret"].concat(),
        format!("{{\"schema_version\":\"loboot.cli/v1\",{}", &encoded[1..]).into_bytes(),
        vec![0xff, 0xfe],
    ] {
        fs::write(&fixture.saved, malformed).unwrap();
        let output = fixture.apply(false);
        assert_eq!(value(&output)["error"]["code"], "invalid_recovery_plan");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("private-plan-secret"));
        assert_eq!(snapshot(&fixture.root), before);
    }
}

#[test]
fn recovery_stale_and_edited_plans_preserve_pending_evidence() {
    for altered in ["target", "journal", "native", "metadata", "root"] {
        let fixture = RecoveryFixture::new("update");
        let mut saved = fixture.save_plan("before");
        let expected = match altered {
            "target" => {
                saved["data"]["plan"]["target"] = serde_json::json!("after");
                fs::write(&fixture.saved, serde_json::to_vec(&saved).unwrap()).unwrap();
                "stale_plan"
            }
            "journal" => {
                let path = fixture.root.join("state/.pending.json");
                let mut bytes = fs::read(&path).unwrap();
                bytes.push(b'\n');
                fs::write(path, bytes).unwrap();
                "stale_plan"
            }
            "native" => {
                // A different valid endpoint is still stale evidence.
                fs::write(
                    fixture.root.join("native/loboot-hello.service"),
                    fixture.after["native_bytes"].as_str().unwrap(),
                )
                .unwrap();
                "stale_plan"
            }
            "metadata" => {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    fixture.root.join("native/loboot-hello.service"),
                    fs::Permissions::from_mode(0o400),
                )
                .unwrap();
                "stale_plan"
            }
            "root" => {
                saved["data"]["plan"]["state_dir"] = serde_json::json!("/different/state");
                fs::write(&fixture.saved, serde_json::to_vec(&saved).unwrap()).unwrap();
                "plan_mismatch"
            }
            _ => unreachable!(),
        };
        let before = snapshot(&fixture.root);
        for dry_run in [true, false] {
            let output = fixture.apply(dry_run);
            assert!(!output.status.success());
            assert_eq!(value(&output)["error"]["code"], expected, "{altered}");
            assert_eq!(snapshot(&fixture.root), before);
            assert!(fixture.root.join("state/.pending.json").exists());
            assert!(!fixture.root.join("state/.lock").exists());
        }
    }
}

#[test]
fn recovery_missing_update_target_refuses_without_adopting_absence() {
    let fixture = RecoveryFixture::new("update");
    fixture.save_plan("before");
    fs::remove_file(fixture.root.join("native/loboot-hello.service")).unwrap();
    let before = snapshot(&fixture.root);
    for args in [
        vec!["recover", "plan"],
        vec![
            "recover",
            "apply",
            fixture.saved.to_str().unwrap(),
            "--dry-run",
        ],
        vec!["recover", "apply", fixture.saved.to_str().unwrap()],
    ] {
        let output = call(&fixture.root, &args);
        assert!(!output.status.success());
        assert_eq!(value(&output)["error"]["code"], "recovery_conflict");
        assert_eq!(snapshot(&fixture.root), before);
    }
}

#[test]
fn recovery_plan_input_refuses_symlinks_special_files_and_oversize_data() {
    let fixture = RecoveryFixture::new("add");
    fixture.save_plan("after");
    let before = snapshot(&fixture.root);
    let linked = fixture.saved.with_file_name("linked.json");
    std::os::unix::fs::symlink(&fixture.saved, &linked).unwrap();
    let directory = fixture.saved.with_file_name("directory.json");
    fs::create_dir(&directory).unwrap();
    let oversized = fixture.saved.with_file_name("oversized.json");
    fs::File::create(&oversized)
        .unwrap()
        .set_len(64 * 1024 * 1024 + 1)
        .unwrap();
    for path in [&linked, &directory, &oversized] {
        let output = call(&fixture.root, &["recover", "apply", path.to_str().unwrap()]);
        assert!(!output.status.success());
        assert_eq!(snapshot(&fixture.root), before);
        assert!(!fixture.root.join("state/.lock").exists());
    }
}

#[test]
fn environment_values_round_trip_but_normal_views_and_plans_are_redacted() {
    const SENTINEL: &str = "private-environment-value-19037";
    const PRIVATE_PATH: &str = "/opt/private-environment-path-19037:/usr/bin";
    for platform in ["linux", "macos"] {
        let temp = TempDir::new().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let root = base.join("environment-root");
        let path = manifest(&base);
        set_environment(
            &path,
            &[
                ("APPLICATION_SETTING", SENTINEL),
                ("EMPTY", ""),
                ("PATH", PRIVATE_PATH),
            ],
        );
        let filename = path.to_str().unwrap();
        let preview = call_platform(&root, platform, &["add", filename, "--dry-run"]);
        assert!(preview.status.success(), "{}", value(&preview));
        assert_not_exposed(&preview, SENTINEL);
        assert_not_exposed(&preview, PRIVATE_PATH);
        assert!(!root.exists());
        let added = call_platform(&root, platform, &["add", filename]);
        assert!(added.status.success(), "{}", value(&added));
        assert_not_exposed(&added, SENTINEL);
        let original = stored_record(&root);
        assert_eq!(
            original.manifest.environment["APPLICATION_SETTING"],
            SENTINEL
        );
        assert_eq!(original.manifest.environment["PATH"], PRIVATE_PATH);
        assert!(original.native_bytes.contains(SENTINEL));
        assert_eq!(
            fs::read_to_string(root.join("native").join(&original.native_name)).unwrap(),
            original.native_bytes
        );
        let before = snapshot(&root);
        for args in [
            vec!["list"],
            vec!["show", "hello"],
            vec!["explain", filename],
            vec!["doctor"],
            vec!["discover"],
            vec!["add", filename, "--dry-run"],
            vec!["update", filename, "--dry-run"],
            vec!["start", "hello", "--dry-run"],
            vec!["logs", "hello", "--dry-run"],
        ] {
            let output = call_platform(&root, platform, &args);
            assert!(
                output.status.success(),
                "{platform}/{args:?}: {}",
                value(&output)
            );
            assert_not_exposed(&output, SENTINEL);
            assert_not_exposed(&output, PRIVATE_PATH);
            assert!(output.stderr.is_empty());
            assert_eq!(snapshot(&root), before);
        }
        for args in [
            vec!["list"],
            vec!["show", "hello"],
            vec!["explain", filename],
        ] {
            let output = call_platform(&root, platform, &args);
            assert_eq!(value(&output)["data"]["environment_redacted"], true);
            assert!(String::from_utf8_lossy(&output.stdout).contains("[REDACTED]"));
            let human = Command::new(env!("CARGO_BIN_EXE_loboot"))
                .arg("--root")
                .arg(&root)
                .args(["--platform", platform])
                .args(&args)
                .env("PATH", "")
                .output()
                .unwrap();
            assert!(human.status.success());
            assert_not_exposed(&human, SENTINEL);
            assert_not_exposed(&human, PRIVATE_PATH);
            assert_eq!(snapshot(&root), before);
        }
        let noop = call_platform(&root, platform, &["update", filename, "--dry-run"]);
        assert_eq!(value(&noop)["data"]["plan"]["changes"], serde_json::json!([]));
        set_environment(
            &path,
            &[("APPLICATION_SETTING", "changed-private-environment-19037")],
        );
        let changed = call_platform(&root, platform, &["update", filename, "--dry-run"]);
        assert!(changed.status.success(), "{}", value(&changed));
        assert!(
            !value(&changed)["data"]["plan"]["changes"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_not_exposed(&changed, "changed-private-environment-19037");
        assert_eq!(snapshot(&root), before);
        assert!(call_platform(&root, platform, &["update", filename]).status.success());
        assert_eq!(stored_record(&root).log_generation, original.log_generation);
        set_environment(&path, &[]);
        assert!(call_platform(&root, platform, &["update", filename]).status.success());
        assert!(stored_record(&root).manifest.environment.is_empty());
        let shown = call_platform(&root, platform, &["show", "hello"]);
        assert_eq!(value(&shown)["data"]["environment_redacted"], false);
        assert!(
            value(&shown)["data"]["service"]["manifest"]
                .get("environment")
                .is_none()
        );
    }
}

#[test]
fn malformed_environment_errors_never_echo_values_or_write_state() {
    const SENTINEL: &str = "private-invalid-environment-28051";
    let temp = TempDir::new().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let root = base.join("invalid-environment-root");
    let path = manifest(&base);
    let original = fs::read_to_string(&path).unwrap();
    for suffix in [
        format!("[environment]\nTOKEN = ['{SENTINEL}']\n"),
        format!("[environment]\nTOKEN = '{SENTINEL}\n"),
        format!("[environment]\nTOKEN = '{SENTINEL}'\nTOKEN = 'duplicate'\n"),
        format!("[environment]\n'{SENTINEL}' = '{SENTINEL}'\n"),
        format!("[environment]\nPATH = '{SENTINEL}'\n"),
        format!("[environment]\nTOKEN = \"{SENTINEL}\\n\"\n"),
    ] {
        fs::write(&path, format!("{original}{suffix}")).unwrap();
        for command in ["add", "explain"] {
            let output = call(&root, &[command, path.to_str().unwrap()]);
            assert!(!output.status.success());
            assert_not_exposed(&output, SENTINEL);
            assert!(output.stderr.is_empty());
            assert!(!root.exists());
        }
    }
}

#[test]
fn malformed_stored_environment_is_not_printed_by_diagnostics() {
    const SENTINEL: &str = "private-invalid-state-90217";
    let temp = TempDir::new().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let root = base.join("invalid-state-root");
    let path = manifest(&base);
    set_environment(&path, &[("APPLICATION_SETTING", SENTINEL)]);
    assert!(call(&root, &["add", path.to_str().unwrap()]).status.success());
    let record = root.join("state/hello.toml");
    let mut text = fs::read_to_string(&record).unwrap();
    text.push_str(&format!("\n[manifest.environment]\nTOKEN = ['{SENTINEL}']\n"));
    fs::write(record, text).unwrap();
    let before = snapshot(&root);
    for args in [
        vec!["list"],
        vec!["show", "hello"],
        vec!["doctor"],
        vec!["update", path.to_str().unwrap(), "--dry-run"],
    ] {
        let output = call(&root, &args);
        assert_not_exposed(&output, SENTINEL);
        assert!(output.stderr.is_empty());
        let result = value(&output);
        if args[0] == "doctor" {
            assert_eq!(result["data"]["state"]["code"], "invalid_state");
        } else {
            assert_eq!(result["error"]["code"], "invalid_state");
        }
        assert_eq!(snapshot(&root), before);
    }
}

#[test]
fn recovery_environment_export_stays_exact_while_summaries_are_redacted() {
    const SENTINEL: &str = "private-recovery-environment-37901";
    let fixture = RecoveryFixture::with_environment("update", Some(SENTINEL));
    let before = snapshot(&fixture.root);
    let saved = fixture.save_plan("after");
    assert_eq!(
        saved["data"]["plan"]["journal"]["after"]["manifest"]["environment"]["APPLICATION_SETTING"],
        SENTINEL
    );
    assert!(fs::read_to_string(&fixture.saved).unwrap().contains(SENTINEL));
    let summary = Command::new(env!("CARGO_BIN_EXE_loboot"))
        .arg("--root")
        .arg(&fixture.root)
        .args([
            "--platform",
            "linux",
            "recover",
            "plan",
            "--to",
            "after",
            "--dry-run",
        ])
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(summary.status.success());
    assert_not_exposed(&summary, SENTINEL);
    let summary: serde_json::Value = serde_json::from_slice(&summary.stdout).unwrap();
    assert_eq!(summary["plan"]["id"], "hello");
    assert!(summary["plan"].get("journal").is_none());
    let dry = fixture.apply(true);
    assert!(dry.status.success(), "{}", value(&dry));
    assert_not_exposed(&dry, SENTINEL);
    assert_eq!(snapshot(&fixture.root), before);
    let applied = fixture.apply(false);
    assert!(applied.status.success(), "{}", value(&applied));
    assert_not_exposed(&applied, SENTINEL);
    assert_eq!(
        stored_record(&fixture.root).manifest.environment["APPLICATION_SETTING"],
        SENTINEL
    );
    assert_not_exposed(&call(&fixture.root, &["show", "hello"]), SENTINEL);
}
