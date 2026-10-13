//! Explicit filesystem recovery. A saved plan is data, never instructions.
use super::*;

const PLAN_SCHEMA: &str = "loboot.recovery/v1";
pub const RECOVERY_PLAN_LIMIT: u64 = 64 * 1024 * 1024;

/// Read an explicit local plan without following symlinks or blocking on special
/// files. Deserialization errors must not echo the potentially sensitive bytes.
pub fn read_recovery_plan_file(path: &Path) -> Result<Vec<u8>> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let file = open_input_file(&absolute)?;
    let mut bytes = Vec::new();
    file.take(RECOVERY_PLAN_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > RECOVERY_PLAN_LIMIT {
        return Err(Error::new("invalid_recovery_plan", "saved recovery plan exceeds 64 MiB"));
    }
    Ok(bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecoveryTarget {
    Before,
    After,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}
impl Identity {
    fn of(md: &std::fs::Metadata) -> Self {
        Self {
            device: md.dev(),
            inode: md.ino(),
            mode: md.mode(),
            uid: md.uid(),
            gid: md.gid(),
            links: md.nlink(),
            size: md.len(),
            modified_seconds: md.mtime(),
            modified_nanoseconds: md.mtime_nsec(),
            changed_seconds: md.ctime(),
            changed_nanoseconds: md.ctime_nsec(),
        }
    }
}

// Directory timestamps change when the lock is first created. Only identity and
// access metadata belong in the plan; every managed entry is checked separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
}
impl DirectoryIdentity {
    fn of(dir: &Dir) -> Result<Self> {
        let md = dir.0.metadata()?;
        Ok(Self {
            device: md.dev(),
            inode: md.ino(),
            mode: md.mode(),
            uid: md.uid(),
            gid: md.gid(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    contents: String,
    identity: Identity,
}
fn snapshot(dir: Option<&Dir>, name: &str) -> Result<Option<Snapshot>> {
    let Some(file) = dir
        .map(|dir| dir.file(name, libc::O_RDONLY, 0, true))
        .transpose()?
        .flatten()
    else {
        return Ok(None);
    };
    let md = file.metadata()?;
    // SAFETY: geteuid only reads this process's effective identity.
    if md.uid() != unsafe { libc::geteuid() } || md.mode() & 0o022 != 0 {
        return Err(Error::new(
            "unsafe_recovery_file",
            "recovery evidence must belong to the current user and not be group/world writable",
        ));
    }
    let identity = Identity::of(&md);
    let mut bytes = Vec::new();
    let mut reader = file.take(LIMIT + 1);
    reader.read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(Error::new("invalid_state", "recovery evidence exceeds 4 MiB"));
    }
    if Identity::of(&reader.get_ref().metadata()?) != identity {
        return Err(Error::new("stale_plan", "recovery evidence changed while reading"));
    }
    let contents = String::from_utf8(bytes)
        .map_err(|_| Error::new("invalid_state", "recovery evidence is not UTF-8"))?;
    Ok(Some(Snapshot { contents, identity }))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureSnapshot {
    directory: PathBuf,
    root: DirectoryIdentity,
    root_marker: Snapshot,
    generation: DirectoryIdentity,
    generation_marker: Snapshot,
    stdout: Identity,
    stderr: Identity,
}

/// Contains plaintext service definitions and raw evidence. Keep saved plans
/// private. Every field is compared with a freshly observed plan on apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryPlan {
    schema_version: String,
    pub target: RecoveryTarget,
    recommended_target: RecoveryTarget,
    pub id: String,
    operation: String,
    state_dir: PathBuf,
    native_dir: PathBuf,
    state_identity: DirectoryIdentity,
    #[serde(deserialize_with = "required_option")]
    native_identity: Option<DirectoryIdentity>,
    journal: Journal,
    journal_snapshot: Snapshot,
    #[serde(deserialize_with = "required_option")]
    current_record: Option<Snapshot>,
    #[serde(deserialize_with = "required_option")]
    current_native: Option<Snapshot>,
    record_state: String,
    native_state: String,
    #[serde(deserialize_with = "required_option")]
    selected_capture: Option<CaptureSnapshot>,
    retained_logs: Vec<PathBuf>,
    pub changes: Vec<String>,
    note: String,
}
impl RecoveryPlan {
    fn selected(&self) -> Option<&Record> {
        match self.target {
            RecoveryTarget::Before => self.journal.before.as_ref(),
            RecoveryTarget::After => self.journal.after.as_ref(),
        }
    }
    pub fn removes_definition(&self) -> bool {
        self.selected().is_none()
    }
    pub fn changes_pair(&self) -> bool {
        self.changes.len() > 1
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RecoveryOutcome {
    pub id: String,
    pub target: RecoveryTarget,
    pub pair_verified: bool,
    pub journal_removed: bool,
    pub retained_logs: Vec<PathBuf>,
    pub runtime_restored: bool,
}

fn classified<T: PartialEq>(current: Option<&T>, before: Option<&T>, after: Option<&T>) -> Result<String> {
    let matches_before = current == before;
    let matches_after = current == after;
    if !matches_before && !matches_after {
        return Err(Error::new(
            "recovery_conflict",
            "current file matches neither recorded endpoint; external or missing update state will not be adopted",
        ));
    }
    Ok(if current.is_none() {
        "absent"
    } else if matches_before && matches_after {
        "both"
    } else if matches_before {
        "before"
    } else {
        "after"
    }
    .into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryStep {
    Native,
    Record,
    Verified,
}

fn validate_recovery_directory(path: &Path) -> Result<()> {
    validate_path(path)?;
    // A root-owned sticky ancestor such as /tmp does not let another account
    // rename this user's children. The managed leaf itself must be private to
    // the effective user. ACLs and the account owner remain outside this guard.
    let uid = unsafe { libc::geteuid() };
    let mut current = PathBuf::from("/");
    for part in path.components() {
        if let Component::Normal(part) = part {
            current.push(part);
        }
        let Some(dir) = Dir::open(&current, false)? else {
            break;
        };
        let md = dir.0.metadata()?;
        let writable = md.mode() & 0o022 != 0;
        let sticky_root = md.uid() == 0 && md.mode() & 0o1000 != 0;
        let leaf = current == path;
        if (leaf && (md.uid() != uid || writable))
            || (!leaf && ((md.uid() != 0 && md.uid() != uid) || (writable && !sticky_root)))
        {
            return Err(Error::new("unsafe_recovery_path", "recovery requires current-user managed directories and trusted non-writable ancestors; no permission repair was attempted"));
        }
    }
    Ok(())
}

impl Store {
    fn validate_recovery_record(&self, id: &str, record: &Record) -> Result<()> {
        if record.manifest.id != id
            || self.expected(
                &record.manifest,
                record.schema_version,
                record.log_generation.as_deref(),
            ).map_err(|_| Error::new("invalid_journal", "record has invalid manifest, schema or backend fields"))? != *record
        {
            return Err(Error::new("invalid_journal", "record does not match the configured backend and ID"));
        }
        Ok(())
    }
    fn validate_journal(&self, journal: &Journal) -> Result<()> {
        validate_id(&journal.id)?;
        if journal.schema_version != 1
            || journal.state_dir != self.state_dir
            || journal.native_dir != self.native_dir
            || journal.before == journal.after
            || !matches!(
                (journal.operation.as_str(), journal.before.is_some(), journal.after.is_some()),
                ("add", false, true) | ("update", true, true) | ("remove", true, false)
            )
        {
            return Err(Error::new("invalid_journal", "journal schema, store paths or operation endpoints are inconsistent"));
        }
        for record in [&journal.before, &journal.after].into_iter().flatten() {
            self.validate_recovery_record(&journal.id, record)?;
        }
        Ok(())
    }
    fn recovery_capture(&self, record: &Record) -> Result<Option<CaptureSnapshot>> {
        let Some(paths) = self.log_paths(record)? else {
            return Ok(None);
        };
        self.verify_capture(record)?;
        let root = self.log_root(false)?.ok_or_else(|| Error::new("log_capture_missing", "capture root disappeared"))?;
        let dir = self.capture_dir(record)?.ok_or_else(|| Error::new("log_capture_missing", "capture generation disappeared"))?;
        let missing = || Error::new("log_capture_missing", "capture file disappeared");
        let stdout = dir.private_file("stdout.log", false)?.ok_or_else(missing)?;
        let stderr = dir.private_file("stderr.log", false)?.ok_or_else(missing)?;
        Ok(Some(CaptureSnapshot {
            directory: paths.directory,
            root: DirectoryIdentity::of(&root)?,
            root_marker: snapshot(Some(&root), ".owner")?.ok_or_else(missing)?,
            generation: DirectoryIdentity::of(&dir)?,
            generation_marker: snapshot(Some(&dir), ".owner")?.ok_or_else(missing)?,
            stdout: Identity::of(&stdout.metadata()?),
            stderr: Identity::of(&stderr.metadata()?),
        }))
    }
    /// Read-only, including when there is no pending journal. Never creates a
    /// lock, directory, capture file, or service-manager query.
    pub fn plan_recovery(&self, target: RecoveryTarget) -> Result<Option<RecoveryPlan>> {
        validate_recovery_directory(&self.state_dir)?;
        validate_recovery_directory(&self.native_dir)?;
        let (state, native) = self.dirs()?;
        let Some(journal_snapshot) = snapshot(state.as_ref(), JOURNAL)? else {
            return Ok(None);
        };
        let journal: Journal = serde_json::from_str(&journal_snapshot.contents)
            .map_err(|_| Error::new("invalid_journal", "pending journal is not strict valid JSON"))?;
        self.validate_journal(&journal)?;
        if native.is_none() {
            return Err(Error::new("recovery_conflict", "native directory is missing; transactions create it before recording a journal"));
        }
        let state = state.as_ref().expect("journal has a state directory");
        let current_record = snapshot(Some(state), &format!("{}.toml", journal.id))?;
        let current_native = snapshot(native.as_ref(), &self.platform.native_name(&journal.id))?;
        let record: Option<Record> = current_record
            .as_ref()
            .map(|s| toml::from_str(&s.contents).map_err(|_| Error::new("invalid_state", "current ownership record is not strict valid TOML")))
            .transpose()?;
        if let Some(record) = &record {
            self.validate_recovery_record(&journal.id, record)?;
        }
        let record_state = classified(record.as_ref(), journal.before.as_ref(), journal.after.as_ref())?;
        let native_state = classified(
            current_native.as_ref().map(|s| &s.contents),
            journal.before.as_ref().map(|r| &r.native_bytes),
            journal.after.as_ref().map(|r| &r.native_bytes),
        )?;
        let selected = match target {
            RecoveryTarget::Before => journal.before.as_ref(),
            RecoveryTarget::After => journal.after.as_ref(),
        };
        let selected_capture = selected.map(|r| self.recovery_capture(r)).transpose()?.flatten();
        let mut retained_logs = Vec::new();
        for record in [&journal.before, &journal.after].into_iter().flatten() {
            if let Some(paths) = self.log_paths(record)? {
                if !retained_logs.contains(&paths.directory) {
                    retained_logs.push(paths.directory);
                }
            }
        }
        let mut changes = Vec::new();
        if current_native.as_ref().map(|s| &s.contents) != selected.map(|r| &r.native_bytes) {
            changes.push(if selected.is_some() { "restore native definition" } else { "remove recorded native definition" }.into());
        }
        if record.as_ref() != selected {
            changes.push(if selected.is_some() { "restore ownership record" } else { "remove recorded ownership record" }.into());
        }
        changes.push("verify selected pair, then remove pending journal".into());
        let plan = RecoveryPlan {
            schema_version: PLAN_SCHEMA.into(),
            target,
            recommended_target: if record.as_ref() == journal.after.as_ref()
                && current_native.as_ref().map(|s| &s.contents)
                    == journal.after.as_ref().map(|r| &r.native_bytes)
                && journal.after.as_ref().is_none_or(|record| self.recovery_capture(record).is_ok())
            {
                RecoveryTarget::After
            } else {
                RecoveryTarget::Before
            },
            id: journal.id.clone(),
            operation: journal.operation.clone(),
            state_dir: self.state_dir.clone(),
            native_dir: self.native_dir.clone(),
            state_identity: DirectoryIdentity::of(state)?,
            native_identity: native.as_ref().map(DirectoryIdentity::of).transpose()?,
            journal,
            journal_snapshot,
            current_record,
            current_native,
            record_state,
            native_state,
            selected_capture,
            retained_logs,
            changes,
            note: "Filesystem recovery only. Runtime and enablement are not restored. Log remnants are retained, never adopted or deleted. This plan contains private plaintext definitions; save it outside managed directories.".into(),
        };
        if snapshot(Some(state), JOURNAL)?.as_ref() != Some(&plan.journal_snapshot) {
            return Err(Error::new("stale_plan", "journal changed while planning"));
        }
        Ok(Some(plan))
    }
    pub fn validate_recovery_plan(&self, plan: &RecoveryPlan) -> Result<()> {
        if plan.state_dir != self.state_dir || plan.native_dir != self.native_dir {
            return Err(Error::new("plan_mismatch", "recovery plan belongs to another store"));
        }
        let fresh = self.plan_recovery(plan.target)?.ok_or_else(|| Error::new("no_pending_recovery", "no pending journal; no recovery was performed"))?;
        if fresh != *plan {
            return Err(Error::new("stale_plan", "recovery evidence changed; save and review a fresh plan"));
        }
        Ok(())
    }
    pub fn apply_recovery_checked(&self, plan: &RecoveryPlan, check: impl FnOnce() -> Result<()>) -> Result<RecoveryOutcome> {
        // Fail before even creating the lock for an invalid/stale submitted plan.
        self.validate_recovery_plan(plan)?;
        let state = Dir::open(&self.state_dir, false)?.ok_or_else(|| Error::new("stale_plan", "state directory disappeared"))?;
        let lock = state.lock()?;
        lock.try_lock_exclusive().map_err(|e| Error::new("store_busy", format!("cannot acquire mutation lock: {e}")))?;
        self.validate_recovery_plan(plan)?;
        check()?;
        self.validate_recovery_plan(plan)?;
        let result = self.commit_recovery(plan, &state);
        FileExt::unlock(&lock).map_err(|e| Error::new("lock_error", e.to_string()))?;
        result
    }
    fn commit_recovery(&self, plan: &RecoveryPlan, state: &Dir) -> Result<RecoveryOutcome> {
        self.commit_recovery_steps(plan, state, |_| Ok(()))
    }
    fn commit_recovery_steps(
        &self,
        plan: &RecoveryPlan,
        state: &Dir,
        mut step: impl FnMut(RecoveryStep) -> Result<()>,
    ) -> Result<RecoveryOutcome> {
        let selected = plan.selected();
        let record_name = format!("{}.toml", plan.id);
        let native_name = self.platform.native_name(&plan.id);
        let target_native = selected.map(|r| r.native_bytes.as_str());
        let current_native = plan.current_native.as_ref().map(|s| s.contents.as_str());
        let current_record: Option<Record> = plan.current_record.as_ref()
            .map(|s| toml::from_str(&s.contents).map_err(|_| Error::new("invalid_state", "validated ownership record changed")))
            .transpose()?;
        let pair = (|| -> Result<()> {
            if current_native != target_native {
                let native = Dir::open(&self.native_dir, false)?.ok_or_else(|| Error::new("stale_plan", "native directory disappeared"))?;
                if Some(DirectoryIdentity::of(&native)?) != plan.native_identity {
                    return Err(Error::new("stale_plan", "native directory identity changed"));
                }
                if let Some(bytes) = target_native {
                    native.atomic(&native_name, bytes.as_bytes(), current_native.is_some())?;
                } else {
                    native.remove(&native_name)?;
                }
            }
            step(RecoveryStep::Native)?;
            if current_record.as_ref() != selected {
                if let Some(record) = selected {
                    let text = toml::to_string(record).map_err(|e| Error::new("serialization", e.to_string()))?;
                    state.atomic(&record_name, text.as_bytes(), current_record.is_some())?;
                } else {
                    state.remove(&record_name)?;
                }
            }
            step(RecoveryStep::Record)?;
            let native = Dir::open(&self.native_dir, false)?;
            if snapshot(Some(state), JOURNAL)?.as_ref() != Some(&plan.journal_snapshot)
                || self.record(&plan.id, Some(state), native.as_ref())
                    .map_err(|_| Error::new("verification_failed", "recovered ownership pair could not be verified"))?.as_ref() != selected
                || snapshot(native.as_ref(), &native_name)?.as_ref().map(|s| s.contents.as_str()) != target_native
            {
                return Err(Error::new("verification_failed", "recovered pair or journal changed before final verification"));
            }
            if let Some(record) = selected {
                self.verify_capture(record)?;
            }
            step(RecoveryStep::Verified)?;
            Ok(())
        })();
        pair.map_err(|e| Error::new("partial_failure", format!("Recovery may be incomplete: {e}. The journal was not removed; inspect and save a fresh plan before retrying.")))?;
        state.remove(JOURNAL).map_err(|e| Error::new("recovery_cleanup_uncertain", format!("Selected filesystem pair verified, but journal removal/durability could not be confirmed: {e}. Inspect a fresh recovery plan; runtime was not restored.")))?;
        Ok(RecoveryOutcome {
            id: plan.id.clone(),
            target: plan.target,
            pair_verified: true,
            journal_removed: true,
            retained_logs: plan.retained_logs.clone(),
            runtime_restored: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Mode, Restart};
    use std::{fs, os::unix::fs::{PermissionsExt, symlink}};

    struct Fixture {
        _temp: tempfile::TempDir,
        store: Store,
        journal: Journal,
    }
    impl Fixture {
        fn new(platform: Platform, operation: &str, record_after: bool, native_after: bool) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let store = Store::new(root.join("state"), root.join("native"), platform);
            let state = Dir::open(&store.state_dir, true).unwrap().unwrap();
            let native = Dir::open(&store.native_dir, true).unwrap().unwrap();
            let mut manifest = Manifest {
                id: "recover-job".into(),
                executable: root.join("program-no-longer-present"),
                args: vec!["old".into()],
                cwd: root.clone(),
                mode: Mode::Service,
                restart: Restart::Never,
            };
            let before = if operation == "add" { None } else { Some(store.expected(&manifest, 1, None).unwrap()) };
            manifest.args = vec!["new".into()];
            let after = if operation == "remove" { None } else { Some(store.expected(&manifest, 1, None).unwrap()) };
            let journal = Journal {
                schema_version: 1,
                operation: operation.into(),
                id: manifest.id,
                state_dir: store.state_dir.clone(),
                native_dir: store.native_dir.clone(),
                before,
                after,
            };
            state.atomic(JOURNAL, &serde_json::to_vec_pretty(&journal).unwrap(), false).unwrap();
            if let Some(record) = if record_after { &journal.after } else { &journal.before } {
                state.atomic("recover-job.toml", toml::to_string(record).unwrap().as_bytes(), false).unwrap();
            }
            if let Some(record) = if native_after { &journal.after } else { &journal.before } {
                native.atomic(&record.native_name, record.native_bytes.as_bytes(), false).unwrap();
            }
            Self { _temp: temp, store, journal }
        }
        fn rewrite_journal(&self, value: &serde_json::Value) {
            let state = Dir::open(&self.store.state_dir, false).unwrap().unwrap();
            state.atomic(JOURNAL, &serde_json::to_vec(value).unwrap(), true).unwrap();
        }
        fn assert_target(&self, target: RecoveryTarget) {
            let expected = match target {
                RecoveryTarget::Before => &self.journal.before,
                RecoveryTarget::After => &self.journal.after,
            };
            let (state, native) = self.store.dirs().unwrap();
            assert_eq!(&self.store.record(&self.journal.id, state.as_ref(), native.as_ref()).unwrap(), expected);
            assert_eq!(native.as_ref().unwrap().read(&self.store.platform.native_name(&self.journal.id)).unwrap(), expected.as_ref().map(|r| r.native_bytes.as_bytes().to_vec()));
        }
    }

    #[test]
    fn recovery_covers_every_endpoint_combination_in_both_directions() {
        for platform in [Platform::Linux, Platform::Macos] {
            for operation in ["add", "update", "remove"] {
                for record_after in [false, true] {
                    for native_after in [false, true] {
                        for target in [RecoveryTarget::Before, RecoveryTarget::After] {
                            let f = Fixture::new(platform, operation, record_after, native_after);
                            let plan = f.store.plan_recovery(target).unwrap().unwrap();
                            let saved = serde_json::to_vec(&plan).unwrap();
                            let plan = serde_json::from_slice(&saved).unwrap();
                            let outcome = f.store.apply_recovery_checked(&plan, || Ok(())).unwrap();
                            assert!(outcome.pair_verified && outcome.journal_removed);
                            assert!(!outcome.runtime_restored);
                            f.assert_target(target);
                            assert!(f.store.plan_recovery(target).unwrap().is_none());
                            assert_eq!(f.store.apply_recovery_checked(&plan, || Ok(())).unwrap_err().code, "no_pending_recovery");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn planning_absent_journal_creates_nothing() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let store = Store::new(root.join("missing/state"), root.join("missing/native"), Platform::Linux);
        assert!(store.plan_recovery(RecoveryTarget::Before).unwrap().is_none());
        assert!(!root.join("missing").exists());
    }

    #[test]
    fn plan_is_read_only_and_normal_operations_remain_blocked() {
        let f = Fixture::new(Platform::Linux, "update", false, true);
        let journal = fs::read(f.store.state_dir.join(JOURNAL)).unwrap();
        let record = fs::read(f.store.state_dir.join("recover-job.toml")).unwrap();
        let native = fs::read(f.store.definition_path(&f.journal.id).unwrap()).unwrap();
        f.store.plan_recovery(RecoveryTarget::Before).unwrap();
        assert!(!f.store.state_dir.join(".lock").exists());
        assert_eq!(fs::read(f.store.state_dir.join(JOURNAL)).unwrap(), journal);
        assert_eq!(fs::read(f.store.state_dir.join("recover-job.toml")).unwrap(), record);
        assert_eq!(fs::read(f.store.definition_path(&f.journal.id).unwrap()).unwrap(), native);
        assert_eq!(f.store.read(&f.journal.id).unwrap_err().code, "recovery_required");
        assert_eq!(f.store.plan_remove(&f.journal.id).unwrap_err().code, "recovery_required");
    }

    #[test]
    fn interruption_preserves_journal_and_fresh_retry_is_idempotent() {
        for operation in ["add", "update", "remove"] {
            for target in [RecoveryTarget::Before, RecoveryTarget::After] {
                for failed_step in [RecoveryStep::Native, RecoveryStep::Record, RecoveryStep::Verified] {
                    let f = Fixture::new(Platform::Linux, operation, target == RecoveryTarget::Before, target == RecoveryTarget::Before);
                    let plan = f.store.plan_recovery(target).unwrap().unwrap();
                    let state = Dir::open(&f.store.state_dir, false).unwrap().unwrap();
                    let original = state.read(JOURNAL).unwrap();
                    let err = f.store.commit_recovery_steps(&plan, &state, |step| {
                        if step == failed_step { Err(Error::new("injected_io", "simulated interruption")) } else { Ok(()) }
                    }).unwrap_err();
                    assert_eq!(err.code, "partial_failure");
                    assert_eq!(state.read(JOURNAL).unwrap(), original);
                    let fresh = f.store.plan_recovery(target).unwrap().unwrap();
                    f.store.apply_recovery_checked(&fresh, || Ok(())).unwrap();
                    f.assert_target(target);
                }
            }
        }
    }

    #[test]
    fn stale_snapshots_and_modified_plan_are_never_applied() {
        for changed in ["journal", "record", "native", "mode", "plan", "target"] {
            let f = Fixture::new(Platform::Linux, "update", false, true);
            let mut plan = f.store.plan_recovery(RecoveryTarget::Before).unwrap().unwrap();
            match changed {
                "journal" => { let path = f.store.state_dir.join(JOURNAL); let bytes = fs::read_to_string(&path).unwrap(); fs::write(path, format!("{bytes}\n")).unwrap(); }
                "record" => { let path = f.store.state_dir.join("recover-job.toml"); let bytes = fs::read_to_string(&path).unwrap(); fs::write(path, format!("# changed\n{bytes}")).unwrap(); }
                "native" => { fs::write(f.store.definition_path(&f.journal.id).unwrap(), &f.journal.before.as_ref().unwrap().native_bytes).unwrap(); }
                "mode" => fs::set_permissions(f.store.state_dir.join("recover-job.toml"), fs::Permissions::from_mode(0o640)).unwrap(),
                "target" => plan.target = RecoveryTarget::After,
                _ => plan.changes.clear(),
            }
            assert_eq!(f.store.apply_recovery_checked(&plan, || Ok(())).unwrap_err().code, "stale_plan", "{changed}");
            assert!(f.store.state_dir.join(JOURNAL).exists());
            assert!(!f.store.state_dir.join(".lock").exists());
        }
    }

    #[test]
    fn malformed_or_mismatched_journals_are_refused() {
        for changed in ["schema", "operation", "state", "native", "id", "unknown", "name", "bytes", "record_schema", "shape"] {
            let f = Fixture::new(Platform::Linux, "update", false, true);
            let mut value = serde_json::to_value(&f.journal).unwrap();
            match changed {
                "schema" => value["schema_version"] = 9.into(),
                "operation" => value["operation"] = "repair-anything".into(),
                "state" => value["state_dir"] = "/different/state".into(),
                "native" => value["native_dir"] = "/different/native".into(),
                "id" => value["id"] = "../escape".into(),
                "unknown" => value["unknown"] = true.into(),
                "name" => value["before"]["native_name"] = "other.service".into(),
                "bytes" => value["after"]["native_bytes"] = "untrusted command".into(),
                "record_schema" => value["after"]["schema_version"] = 9.into(),
                _ => value["before"] = serde_json::Value::Null,
            }
            f.rewrite_journal(&value);
            assert!(f.store.plan_recovery(RecoveryTarget::Before).is_err(), "{changed}");
            assert!(f.store.state_dir.join(JOURNAL).exists());
        }
    }

    #[test]
    fn foreign_and_missing_update_files_are_refused() {
        for changed in ["missing_record", "missing_native", "foreign_record", "foreign_native", "invalid_utf8"] {
            let f = Fixture::new(Platform::Linux, "update", false, true);
            let record = f.store.state_dir.join("recover-job.toml");
            let native = f.store.definition_path(&f.journal.id).unwrap();
            match changed {
                "missing_record" => fs::remove_file(record).unwrap(),
                "missing_native" => fs::remove_file(native).unwrap(),
                "foreign_record" => fs::write(record, "not a record").unwrap(),
                "invalid_utf8" => fs::write(native, [0xff]).unwrap(),
                _ => fs::write(native, "external bytes").unwrap(),
            }
            assert!(f.store.plan_recovery(RecoveryTarget::Before).is_err(), "{changed}");
        }
    }

    #[test]
    fn symlinks_hardlinks_special_files_and_writable_evidence_are_refused() {
        for entry in [JOURNAL, "recover-job.toml", "native"] {
            for kind in ["symlink", "hardlink", "directory", "public_write"] {
                let f = Fixture::new(Platform::Linux, "update", false, true);
                let path = if entry == "native" { f.store.definition_path(&f.journal.id).unwrap() } else { f.store.state_dir.join(entry) };
                let outside = f.store.state_dir.parent().unwrap().join("outside");
                let bytes = fs::read(&path).unwrap();
                fs::write(&outside, &bytes).unwrap();
                if kind != "public_write" { fs::remove_file(&path).unwrap(); }
                match kind {
                    "symlink" => symlink(&outside, &path).unwrap(),
                    "hardlink" => fs::hard_link(&outside, &path).unwrap(),
                    "directory" => fs::create_dir(&path).unwrap(),
                    _ => fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap(),
                }
                assert!(f.store.plan_recovery(RecoveryTarget::Before).is_err(), "{entry}/{kind}");
                assert_eq!(fs::read(outside).unwrap(), bytes);
            }
        }
    }

    #[test]
    fn guarded_apply_rechecks_and_lock_is_exclusive() {
        let f = Fixture::new(Platform::Linux, "update", false, true);
        let plan = f.store.plan_recovery(RecoveryTarget::Before).unwrap().unwrap();
        let err = f.store.apply_recovery_checked(&plan, || {
            assert_eq!(f.store.apply_recovery_checked(&plan, || Ok(())).unwrap_err().code, "store_busy");
            Err(Error::new("native_conflict", "blocked"))
        }).unwrap_err();
        assert_eq!(err.code, "native_conflict");
        f.store.validate_recovery_plan(&plan).unwrap();
        let err = f.store.apply_recovery_checked(&plan, || {
            fs::write(f.store.definition_path(&f.journal.id)?, "changed during check")?;
            Ok(())
        }).unwrap_err();
        assert_eq!(err.code, "recovery_conflict");
        assert!(f.store.state_dir.join(JOURNAL).exists());
    }

    #[test]
    fn symlink_lock_and_ancestor_replacement_are_refused() {
        let f = Fixture::new(Platform::Linux, "update", false, true);
        let plan = f.store.plan_recovery(RecoveryTarget::Before).unwrap().unwrap();
        let outside = f.store.state_dir.parent().unwrap().join("outside");
        fs::write(&outside, "untouched").unwrap();
        symlink(&outside, f.store.state_dir.join(".lock")).unwrap();
        assert!(f.store.apply_recovery_checked(&plan, || Ok(())).is_err());
        assert_eq!(fs::read_to_string(&outside).unwrap(), "untouched");
        let previous = f.store.state_dir.parent().unwrap().join("previous-state");
        fs::rename(&f.store.state_dir, &previous).unwrap();
        symlink(&previous, &f.store.state_dir).unwrap();
        assert!(f.store.plan_recovery(RecoveryTarget::Before).is_err());
    }

    #[test]
    fn partial_mac_capture_can_roll_back_without_adopting_or_removing_logs() {
        for operation in ["add", "update"] {
        for point in 0..=5 {
            let mut f = Fixture::new(Platform::Macos, operation, false, false);
            let manifest = f.journal.after.as_ref().unwrap().manifest.clone();
            let after = f.store.expected(&manifest, 2, Some("0123456789abcdef0123456789abcdef")).unwrap();
            f.journal.after = Some(after.clone());
            f.rewrite_journal(&serde_json::to_value(&f.journal).unwrap());
            let paths = f.store.log_paths(&after).unwrap().unwrap();
            if point >= 1 {
                let root = f.store.log_root(true).unwrap().unwrap();
                if point >= 2 {
                    let name = paths.directory.file_name().unwrap().to_str().unwrap();
                    let dir = root.private_child(name, true, true).unwrap().unwrap();
                    if point >= 3 { dir.create_private(".owner", &Store::log_marker(&after)).unwrap(); }
                    if point >= 4 { dir.create_private("stdout.log", b"kept output").unwrap(); }
                    if point >= 5 { dir.create_private("stderr.log", b"").unwrap(); }
                }
            }
            let completion = f.store.plan_recovery(RecoveryTarget::After);
            assert_eq!(completion.is_ok(), point == 5);
            let before = f.store.plan_recovery(RecoveryTarget::Before).unwrap().unwrap();
            f.store.apply_recovery_checked(&before, || Ok(())).unwrap();
            if point >= 2 { assert!(paths.directory.exists()); }
            if point >= 4 { assert_eq!(fs::read_to_string(paths.stdout).unwrap(), "kept output"); }
        }
        }
    }

    #[test]
    fn mac_completion_requires_unchanged_verified_capture() {
        let mut f = Fixture::new(Platform::Macos, "add", false, false);
        let manifest = f.journal.after.as_ref().unwrap().manifest.clone();
        let after = f.store.expected(&manifest, 2, Some("0123456789abcdef0123456789abcdef")).unwrap();
        f.journal.after = Some(after.clone());
        f.rewrite_journal(&serde_json::to_value(&f.journal).unwrap());
        f.store.initialize_logs(&after).unwrap();
        let paths = f.store.log_paths(&after).unwrap().unwrap();
        let plan = f.store.plan_recovery(RecoveryTarget::After).unwrap().unwrap();
        fs::write(&paths.stdout, "changed after planning").unwrap();
        assert_eq!(f.store.apply_recovery_checked(&plan, || Ok(())).unwrap_err().code, "stale_plan");
        let fresh = f.store.plan_recovery(RecoveryTarget::After).unwrap().unwrap();
        f.store.apply_recovery_checked(&fresh, || Ok(())).unwrap();
        f.assert_target(RecoveryTarget::After);
        assert_eq!(fs::read_to_string(paths.stdout).unwrap(), "changed after planning");
    }


    #[test]
    fn schema_two_update_and_remove_preserve_existing_capture() {
        for operation in ["update", "remove"] {
            for target in [RecoveryTarget::Before, RecoveryTarget::After] {
                let mut f = Fixture::new(Platform::Macos, operation, false, false);
                let generation = "0123456789abcdef0123456789abcdef";
                let before = f.store.expected(&f.journal.before.as_ref().unwrap().manifest, 2, Some(generation)).unwrap();
                f.journal.before = Some(before.clone());
                if let Some(after) = &f.journal.after {
                    f.journal.after = Some(f.store.expected(&after.manifest, 2, Some(generation)).unwrap());
                }
                f.rewrite_journal(&serde_json::to_value(&f.journal).unwrap());
                let state = Dir::open(&f.store.state_dir, false).unwrap().unwrap();
                let native = Dir::open(&f.store.native_dir, false).unwrap().unwrap();
                state.atomic("recover-job.toml", toml::to_string(&before).unwrap().as_bytes(), true).unwrap();
                native.atomic(&before.native_name, before.native_bytes.as_bytes(), true).unwrap();
                f.store.initialize_logs(&before).unwrap();
                let paths = f.store.log_paths(&before).unwrap().unwrap();
                fs::write(&paths.stdout, "retained data").unwrap();
                let plan = f.store.plan_recovery(target).unwrap().unwrap();
                f.store.apply_recovery_checked(&plan, || Ok(())).unwrap();
                f.assert_target(target);
                assert_eq!(fs::read_to_string(paths.stdout).unwrap(), "retained data");
            }
        }
    }

    #[test]
    fn untrusted_writable_directories_and_final_parse_errors_are_refused() {
        for directory in ["state", "native", "ancestor"] {
            let f = Fixture::new(Platform::Linux, "update", false, true);
            let path = match directory {
                "state" => f.store.state_dir.clone(),
                "native" => f.store.native_dir.clone(),
                _ => f.store.state_dir.parent().unwrap().to_path_buf(),
            };
            fs::set_permissions(&path, fs::Permissions::from_mode(0o777)).unwrap();
            assert_eq!(f.store.plan_recovery(RecoveryTarget::Before).unwrap_err().code, "unsafe_recovery_path");
        }
        let f = Fixture::new(Platform::Linux, "update", false, true);
        let plan = f.store.plan_recovery(RecoveryTarget::Before).unwrap().unwrap();
        let state = Dir::open(&f.store.state_dir, false).unwrap().unwrap();
        let error = f.store.commit_recovery_steps(&plan, &state, |step| {
            if step == RecoveryStep::Record {
                fs::write(f.store.state_dir.join("recover-job.toml"), "secret-final-value invalid TOML")?;
            }
            Ok(())
        }).unwrap_err();
        assert_eq!(error.code, "partial_failure");
        assert!(!error.message.contains("secret-final-value"));
        assert!(f.store.state_dir.join(JOURNAL).exists());
    }

    #[test]
    fn a_missing_native_directory_is_drift_not_permission_to_recreate_it() {
        let f = Fixture::new(Platform::Linux, "add", false, false);
        fs::remove_dir(&f.store.native_dir).unwrap();
        for target in [RecoveryTarget::Before, RecoveryTarget::After] {
            assert_eq!(f.store.plan_recovery(target).unwrap_err().code, "recovery_conflict");
        }
        assert!(!f.store.native_dir.exists());
        assert!(f.store.state_dir.join(JOURNAL).exists());
    }
    #[test]
    fn omitted_null_fields_are_not_treated_as_recorded_absence() {
        let f = Fixture::new(Platform::Linux, "add", false, false);
        let plan = f.store.plan_recovery(RecoveryTarget::Before).unwrap().unwrap();
        for field in ["current_record", "current_native", "selected_capture"] {
            let mut value = serde_json::to_value(&plan).unwrap();
            value.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<RecoveryPlan>(value).is_err());
        }
        let mut value = serde_json::to_value(&f.journal).unwrap();
        value.as_object_mut().unwrap().remove("before");
        f.rewrite_journal(&value);
        assert_eq!(f.store.plan_recovery(RecoveryTarget::Before).unwrap_err().code, "invalid_journal");
    }

    #[test]
    fn malformed_json_and_oversized_journal_fail_without_echoing_content() {
        for bytes in [b"secret=not-json".to_vec(), vec![b'x'; LIMIT as usize + 1]] {
            let f = Fixture::new(Platform::Linux, "add", false, false);
            fs::write(f.store.state_dir.join(JOURNAL), bytes).unwrap();
            let err = f.store.plan_recovery(RecoveryTarget::Before).unwrap_err();
            assert!(!err.message.contains("secret"));
        }
    }
}
