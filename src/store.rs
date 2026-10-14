//! Managed TOML ownership records, pure planning, and journaled native-definition changes.
//! Every filesystem operation is anchored to an O_NOFOLLOW directory descriptor.
use crate::{
    Error, Result,
    backend::Platform,
    model::{Manifest, validate_id, validate_path},
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    ffi::{CStr, CString, OsStr},
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[path = "recovery.rs"]
mod recovery;
pub use recovery::{
    RECOVERY_PLAN_LIMIT, RecoveryOutcome, RecoveryPlan, RecoveryTarget, read_recovery_plan_file,
};

const LIMIT: u64 = 4 * 1024 * 1024;
const JOURNAL: &str = ".pending.json";
static NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_generation: Option<String>,
    pub manifest: Manifest,
    pub native_name: String,
    pub native_bytes: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub operation: String,
    pub id: String,
    pub changes: Vec<String>,
    #[serde(skip)]
    before: Option<Record>,
    #[serde(skip)]
    after: Option<Record>,
    #[serde(skip)]
    state_dir: PathBuf,
    #[serde(skip)]
    native_dir: PathBuf,
}
impl Plan {
    pub fn is_noop(&self) -> bool {
        self.before == self.after
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema_version: u32,
    operation: String,
    id: String,
    state_dir: PathBuf,
    native_dir: PathBuf,
    #[serde(deserialize_with = "required_option")]
    before: Option<Record>,
    #[serde(deserialize_with = "required_option")]
    after: Option<Record>,
}

// Unlike serde's default Option behavior, journal/plan endpoints must be
// explicitly present even when null. Missing evidence is not recorded absence.
fn required_option<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Debug, Clone)]
pub struct Store {
    state_dir: PathBuf,
    native_dir: PathBuf,
    platform: Platform,
}
impl Store {
    pub fn new(state_dir: PathBuf, native_dir: PathBuf, platform: Platform) -> Self {
        Self {
            state_dir,
            native_dir,
            platform,
        }
    }
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }
    pub fn native_dir(&self) -> &Path {
        &self.native_dir
    }
    pub fn definition_path(&self, id: &str) -> Result<PathBuf> {
        validate_id(id)?;
        let name = self.platform.native_name(id);
        entry_name(&name)?;
        Ok(self.native_dir.join(name))
    }
    fn dirs(&self) -> Result<(Option<Dir>, Option<Dir>)> {
        Ok((
            Dir::open(&self.state_dir, false)?,
            Dir::open(&self.native_dir, false)?,
        ))
    }
    fn pending(&self, state: Option<&Dir>) -> Result<()> {
        if state
            .map(|d| d.read(JOURNAL))
            .transpose()?
            .flatten()
            .is_some()
        {
            return Err(Error::new(
                "recovery_required",
                format!(
                    "unfinished filesystem transaction: {}. Stop runtime actions. This journal contains before/after records and exact native bytes; inspect both paths, restore the before pair or complete the after pair, then remove the journal only after verifying both. No automatic repair was attempted",
                    self.state_dir.join(JOURNAL).display()
                ),
            ));
        }
        Ok(())
    }
    /// A prospective record; does not create files or change existing records.
    pub fn preview(&self, manifest: &Manifest) -> Result<Record> {
        let generation = if self.platform == Platform::Macos {
            Some(new_generation()?)
        } else {
            None
        };
        self.expected(
            manifest,
            if generation.is_some() { 2 } else { 1 },
            generation.as_deref(),
        )
    }
    fn expected(
        &self,
        manifest: &Manifest,
        schema: u32,
        generation: Option<&str>,
    ) -> Result<Record> {
        manifest.validate_syntax()?;
        let native_name = self.platform.native_name(&manifest.id);
        entry_name(&native_name)?;
        let native_bytes = match (schema, self.platform, generation) {
            (1, _, None) => self.platform.render(manifest)?,
            (2, Platform::Macos, Some(generation)) => {
                let paths = self.capture_paths(&manifest.id, generation)?;
                self.platform
                    .render_with_logs(manifest, &paths.stdout, &paths.stderr)?
            }
            _ => {
                return Err(Error::new(
                    "invalid_state",
                    "unsupported record schema or log generation",
                ));
            }
        };
        Ok(Record {
            schema_version: schema,
            log_generation: generation.map(str::to_owned),
            manifest: manifest.clone(),
            native_name,
            native_bytes,
        })
    }
    fn record(
        &self,
        id: &str,
        state: Option<&Dir>,
        native: Option<&Dir>,
    ) -> Result<Option<Record>> {
        validate_id(id)?;
        let bytes = state
            .map(|d| d.read(&format!("{id}.toml")))
            .transpose()?
            .flatten();
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        let text = std::str::from_utf8(&bytes)
            .map_err(|e| Error::new("invalid_state", format!("{id}: {e}")))?;
        let record: Record =
            toml::from_str(text).map_err(|e| Error::new("invalid_state", format!("{id}: {e}")))?;
        if record.manifest.id != id
            || self.expected(
                &record.manifest,
                record.schema_version,
                record.log_generation.as_deref(),
            )? != record
        {
            return Err(Error::new(
                "invalid_state",
                format!(
                    "stored ownership record for {id} does not match its manifest and generated definition"
                ),
            ));
        }
        let actual = native
            .map(|d| d.read(&record.native_name))
            .transpose()?
            .flatten();
        if actual.as_deref() != Some(record.native_bytes.as_bytes()) {
            return Err(Error::new(
                "definition_drift",
                format!(
                    "{} is missing or differs from the exact managed definition; refusing to overwrite or adopt it",
                    self.native_dir.join(&record.native_name).display()
                ),
            ));
        }
        Ok(Some(record))
    }
    pub fn read(&self, id: &str) -> Result<Record> {
        let (state, native) = self.dirs()?;
        self.pending(state.as_ref())?;
        self.record(id, state.as_ref(), native.as_ref())?
            .ok_or_else(|| Error::new("not_managed", format!("{id} is not managed by loboot")))
    }
    pub fn list(&self) -> Result<Vec<Record>> {
        let (state, native) = self.dirs()?;
        self.pending(state.as_ref())?;
        let Some(state) = state else {
            return Ok(Vec::new());
        };
        let mut records = Vec::new();
        for name in state.names()? {
            if name == "logs" {
                self.log_root(false)?;
                continue;
            }
            if name == ".lock" || name.starts_with(".tmp-") {
                continue;
            }
            let Some(id) = name.strip_suffix(".toml") else {
                return Err(Error::new(
                    "invalid_state",
                    format!("unexpected state entry: {name}"),
                ));
            };
            let record = self
                .record(id, Some(&state), native.as_ref())?
                .ok_or_else(|| {
                    Error::new("state_changed", "state entry disappeared during listing")
                })?;
            records.push(record);
        }
        records.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
        Ok(records)
    }
    pub fn check(&self) -> Result<()> {
        self.list().map(|_| ())
    }
    pub fn plan_add(&self, manifest: &Manifest) -> Result<Plan> {
        manifest.validate()?;
        let (state, native) = self.dirs()?;
        self.pending(state.as_ref())?;
        let before = self.record(&manifest.id, state.as_ref(), native.as_ref())?;
        if let Some(before) = before {
            if before.manifest == *manifest {
                return Ok(self.plan("add", Some(before.clone()), Some(before), Vec::new()));
            }
            return Err(Error::new(
                "already_managed",
                format!("{} already exists; use update", manifest.id),
            ));
        }
        let after = self.preview(manifest)?;
        self.check_log_plan(None, &after)?;
        if native
            .as_ref()
            .map(|d| d.read(&after.native_name))
            .transpose()?
            .flatten()
            .is_some()
        {
            return Err(Error::new(
                "foreign_definition",
                format!(
                    "{} exists without a matching loboot ownership record",
                    self.native_dir.join(&after.native_name).display()
                ),
            ));
        }
        let changes = vec![
            format!(
                "install {}",
                self.native_dir.join(&after.native_name).display()
            ),
            format!(
                "record {}",
                self.state_dir
                    .join(format!("{}.toml", manifest.id))
                    .display()
            ),
        ];
        Ok(self.plan("add", None, Some(after), changes))
    }
    pub fn plan_update(&self, manifest: &Manifest) -> Result<Plan> {
        manifest.validate()?;
        let before = self.read(&manifest.id)?;
        let after = if before.log_generation.is_some() {
            self.expected(
                manifest,
                before.schema_version,
                before.log_generation.as_deref(),
            )?
        } else {
            self.preview(manifest)?
        };
        self.check_log_plan(Some(&before), &after)?;
        let changes = if before == after {
            vec![]
        } else {
            vec![
                format!(
                    "replace {}",
                    self.native_dir.join(&after.native_name).display()
                ),
                format!("update ownership record for {}", manifest.id),
            ]
        };
        Ok(self.plan("update", Some(before), Some(after), changes))
    }
    pub fn plan_remove(&self, id: &str) -> Result<Plan> {
        validate_id(id)?;
        let (state, native) = self.dirs()?;
        self.pending(state.as_ref())?;
        let before = self.record(id, state.as_ref(), native.as_ref())?;
        if before.is_none() {
            let name = self.platform.native_name(id);
            if native
                .as_ref()
                .map(|d| d.read(&name))
                .transpose()?
                .flatten()
                .is_some()
            {
                return Err(Error::new(
                    "foreign_definition",
                    "native definition exists without a loboot ownership record",
                ));
            }
            return Ok(Plan {
                operation: "remove".into(),
                id: id.into(),
                changes: vec![],
                before: None,
                after: None,
                state_dir: self.state_dir.clone(),
                native_dir: self.native_dir.clone(),
            });
        }
        let changes = vec![
            format!("remove {}", self.definition_path(id)?.display()),
            format!("remove ownership record for {id}"),
        ];
        Ok(self.plan("remove", before, None, changes))
    }
    fn plan(
        &self,
        operation: &str,
        before: Option<Record>,
        after: Option<Record>,
        changes: Vec<String>,
    ) -> Plan {
        let id = after
            .as_ref()
            .or(before.as_ref())
            .expect("plan has a record")
            .manifest
            .id
            .clone();
        Plan {
            operation: operation.into(),
            id,
            changes,
            before,
            after,
            state_dir: self.state_dir.clone(),
            native_dir: self.native_dir.clone(),
        }
    }
    /// Creates a lock only for actual execution. Never call from a dry-run path.
    pub fn with_lock<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        // Check both configured paths before creating anything.
        self.dirs()?;
        let state = Dir::open(&self.state_dir, true)?.expect("created directory");
        let lock = state.lock()?;
        lock.try_lock_exclusive()
            .map_err(|e| Error::new("store_busy", format!("cannot acquire mutation lock: {e}")))?;
        self.pending(Some(&state))?;
        let result = f();
        FileExt::unlock(&lock).map_err(|e| Error::new("lock_error", e.to_string()))?;
        result
    }
    pub fn apply(&self, plan: &Plan) -> Result<()> {
        self.apply_checked(plan, || Ok(()))
    }
    pub fn apply_checked(&self, plan: &Plan, check: impl FnOnce() -> Result<()>) -> Result<()> {
        if plan.state_dir != self.state_dir || plan.native_dir != self.native_dir {
            return Err(Error::new("plan_mismatch", "plan belongs to another store"));
        }
        self.with_lock(|| {
            self.revalidate(plan)?;
            check()?;
            self.revalidate(plan)?;
            if plan.before == plan.after { return Ok(()); }
            let state = Dir::open(&self.state_dir, false)?.expect("locked state directory");
            let native = Dir::open(&self.native_dir, true)?.expect("created native directory");
            let journal = Journal { schema_version:1, operation:plan.operation.clone(), id:plan.id.clone(), state_dir:self.state_dir.clone(), native_dir:self.native_dir.clone(), before:plan.before.clone(), after:plan.after.clone() };
            let bytes = serde_json::to_vec_pretty(&journal).map_err(|e| Error::new("serialization", e.to_string()))?;
            state.atomic(JOURNAL, &bytes, false)?;
            let commit = (|| -> Result<()> {
                if let Some(after) = &plan.after {
                    if after.log_generation != plan.before.as_ref().and_then(|r| r.log_generation.clone()) {
                        self.initialize_logs(after)?;
                    }
                    native.atomic(&after.native_name, after.native_bytes.as_bytes(), plan.before.is_some())?;
                    let text = toml::to_string(after).map_err(|e| Error::new("serialization", e.to_string()))?;
                    state.atomic(&format!("{}.toml", plan.id), text.as_bytes(), plan.before.is_some())?;
                } else if let Some(before) = &plan.before {
                    native.remove(&before.native_name)?;
                    state.remove(&format!("{}.toml", plan.id))?;
                }
                state.remove(JOURNAL)?;
                Ok(())
            })();
            commit.map_err(|e| Error::new("partial_failure", format!("filesystem transaction may be incomplete: {e}. Inspect {} before any further mutation", self.state_dir.join(JOURNAL).display())))
        })
    }
    fn revalidate(&self, plan: &Plan) -> Result<()> {
        validate_id(&plan.id)?;
        if plan
            .before
            .as_ref()
            .is_some_and(|r| r.manifest.id != plan.id)
            || plan
                .after
                .as_ref()
                .is_some_and(|r| r.manifest.id != plan.id)
            || !matches!(plan.operation.as_str(), "add" | "update" | "remove")
            || (plan.operation == "remove" && plan.after.is_some())
            || (plan.operation != "remove" && plan.after.is_none())
        {
            return Err(Error::new("plan_mismatch", "plan fields are inconsistent"));
        }
        let (state, native) = self.dirs()?;
        self.pending(state.as_ref())?;
        let current = self.record(&plan.id, state.as_ref(), native.as_ref())?;
        if current != plan.before {
            return Err(Error::new(
                "stale_plan",
                "managed state changed after planning; create a fresh plan",
            ));
        }
        if current.is_none() {
            let name = self.platform.native_name(&plan.id);
            if native
                .as_ref()
                .map(|d| d.read(&name))
                .transpose()?
                .flatten()
                .is_some()
            {
                return Err(Error::new(
                    "foreign_definition",
                    "native definition appeared after planning",
                ));
            }
        }
        if let Some(after) = &plan.after {
            after.manifest.validate()?;
            if self.expected(
                &after.manifest,
                after.schema_version,
                after.log_generation.as_deref(),
            )? != *after
            {
                return Err(Error::new(
                    "plan_mismatch",
                    "plan does not match this backend",
                ));
            }
            self.check_log_plan(plan.before.as_ref(), after)?;
        }
        Ok(())
    }
}

const LOG_ROOT_MARKER: &[u8] = b"loboot.private-log-root/v1\n";

#[derive(Debug, Clone, Serialize)]
pub struct CapturePaths {
    pub directory: PathBuf,
    pub stdout: PathBuf,
    pub stderr: PathBuf,
}

fn new_generation() -> Result<String> {
    let mut bytes = [0u8; 16];
    // SAFETY: getentropy writes at most the supplied 16-byte buffer, below its
    // 256-byte API limit. Randomness names resources; it grants no authority.
    if unsafe { libc::getentropy(bytes.as_mut_ptr().cast(), bytes.len()) } != 0 {
        return Err(Error::new(
            "log_initialization",
            "cannot allocate a log generation",
        ));
    }
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn private_metadata(md: &std::fs::Metadata, directory: bool) -> Result<()> {
    // SAFETY: geteuid is a read-only process identity query.
    let uid = unsafe { libc::geteuid() };
    let expected = if directory { 0o700 } else { 0o600 };
    if md.uid() != uid
        || md.mode() & 0o7777 != expected
        || (directory && !md.is_dir())
        || (!directory && (!md.is_file() || md.nlink() != 1))
    {
        return Err(Error::new(
            "unsafe_log_path",
            "log storage must be owned by the current effective UID, directories mode 0700 and regular non-hardlinked files mode 0600; no automatic permission repair",
        ));
    }
    Ok(())
}

impl Dir {
    fn private_child(&self, name: &str, create: bool, exclusive: bool) -> Result<Option<Self>> {
        private_metadata(&self.0.metadata()?, true)?;
        let name_c = entry_name(name)?;
        if create {
            let rc = unsafe { libc::mkdirat(self.0.as_raw_fd(), name_c.as_ptr(), 0o700) };
            if rc < 0 {
                let e = errno();
                if exclusive || e.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(Error::new(
                        "log_collision",
                        format!("cannot exclusively create private log directory: {e}"),
                    ));
                }
            } else {
                self.0.sync_all()?;
            }
        }
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name_c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            let e = errno();
            if e.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(Error::new(
                "unsafe_log_path",
                format!("cannot open private log directory: {e}"),
            ));
        }
        let child = Self(unsafe { File::from_raw_fd(fd) });
        private_metadata(&child.0.metadata()?, true)?;
        Ok(Some(child))
    }
    fn private_file(&self, name: &str, create: bool) -> Result<Option<File>> {
        private_metadata(&self.0.metadata()?, true)?;
        let flags = if create {
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL
        } else {
            libc::O_RDONLY
        };
        let file = self.file(name, flags, 0o600, true)?;
        if let Some(file) = &file {
            private_metadata(&file.metadata()?, false)?;
        }
        Ok(file)
    }
    fn check_marker(&self, expected: &[u8]) -> Result<()> {
        let Some(mut file) = self.private_file(".owner", false)? else {
            return Err(Error::new(
                "log_ownership",
                "private log directory has no ownership marker; refusing to adopt existing files",
            ));
        };
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(4097)
            .read_to_end(&mut bytes)?;
        if bytes != expected {
            return Err(Error::new(
                "log_ownership",
                "private log ownership marker does not match the exact record",
            ));
        }
        Ok(())
    }
    fn create_private(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let mut file = self
            .private_file(name, true)?
            .ok_or_else(|| Error::new("log_initialization", "private file could not be created"))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        self.0.sync_all()?;
        Ok(())
    }
}

impl Store {
    fn capture_paths(&self, id: &str, generation: &str) -> Result<CapturePaths> {
        validate_id(id)?;
        if generation.len() != 32
            || !generation
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::new(
                "invalid_state",
                "log generation must be 32 lowercase hexadecimal characters",
            ));
        }
        let directory = self
            .state_dir
            .join("logs")
            .join(format!("{id}-{generation}"));
        Ok(CapturePaths {
            stdout: directory.join("stdout.log"),
            stderr: directory.join("stderr.log"),
            directory,
        })
    }
    pub fn log_paths(&self, record: &Record) -> Result<Option<CapturePaths>> {
        record
            .log_generation
            .as_deref()
            .map(|generation| self.capture_paths(&record.manifest.id, generation))
            .transpose()
    }
    fn log_marker(record: &Record) -> Vec<u8> {
        format!(
            "loboot.private-log-generation/v1\n{}\n{}\n",
            record.manifest.id,
            record.log_generation.as_deref().unwrap_or("")
        )
        .into_bytes()
    }
    fn log_root(&self, create: bool) -> Result<Option<Dir>> {
        let Some(state) = Dir::open(&self.state_dir, create)? else {
            return Ok(None);
        };
        private_metadata(&state.0.metadata()?, true)?;
        let Some(root) = state.private_child("logs", create, false)? else {
            return Ok(None);
        };
        // Empty private roots have no data to adopt. This makes interrupted
        // mkdir-before-marker initialization retryable without changing files.
        if root.names()?.is_empty() {
            if create {
                root.create_private(".owner", LOG_ROOT_MARKER)?;
            }
        } else {
            root.check_marker(LOG_ROOT_MARKER)?;
        }
        Ok(Some(root))
    }
    fn capture_dir(&self, record: &Record) -> Result<Option<Dir>> {
        let Some(paths) = self.log_paths(record)? else {
            return Ok(None);
        };
        let Some(root) = self.log_root(false)? else {
            return Ok(None);
        };
        let name = paths
            .directory
            .file_name()
            .and_then(OsStr::to_str)
            .expect("derived name");
        let Some(dir) = root.private_child(name, false, false)? else {
            return Ok(None);
        };
        dir.check_marker(&Self::log_marker(record))?;
        Ok(Some(dir))
    }
    fn check_log_plan(&self, before: Option<&Record>, after: &Record) -> Result<()> {
        let Some(paths) = self.log_paths(after)? else {
            return Ok(());
        };
        if before.is_some_and(|before| before.log_generation == after.log_generation) {
            return self.verify_capture(after);
        }
        if let Some(root) = self.log_root(false)? {
            let name = paths
                .directory
                .file_name()
                .and_then(OsStr::to_str)
                .expect("derived name");
            if root.private_child(name, false, false)?.is_some() {
                return Err(Error::new(
                    "log_collision",
                    "new log generation already exists; no files were adopted or truncated",
                ));
            }
        }
        Ok(())
    }
    fn initialize_logs(&self, record: &Record) -> Result<()> {
        let Some(paths) = self.log_paths(record)? else {
            return Ok(());
        };
        let root = self.log_root(true)?.expect("created log root");
        let name = paths
            .directory
            .file_name()
            .and_then(OsStr::to_str)
            .expect("derived name");
        let dir = root
            .private_child(name, true, true)?
            .expect("created generation");
        dir.create_private(".owner", &Self::log_marker(record))?;
        dir.create_private("stdout.log", b"")?;
        dir.create_private("stderr.log", b"")?;
        Ok(())
    }
    /// Validate capture paths before any action that can launch a job. Never
    /// repair, create, truncate or follow a path during a runtime operation.
    pub fn verify_capture(&self, record: &Record) -> Result<()> {
        if record.log_generation.is_none() {
            return Ok(());
        }
        let missing = || {
            Error::new(
                "log_capture_missing",
                "owned log capture is missing; restore its private files before enabling or starting the service",
            )
        };
        let dir = self.capture_dir(record)?.ok_or_else(missing)?;
        for name in ["stdout.log", "stderr.log"] {
            dir.private_file(name, false)?.ok_or_else(missing)?;
        }
        Ok(())
    }
    pub fn read_logs(
        &self,
        record: &Record,
        limits: crate::logs::LogLimits,
    ) -> Result<Vec<crate::logs::LogSource>> {
        let paths = self.log_paths(record)?.ok_or_else(|| Error::new("log_capture_unavailable", "legacy macOS definition has no output capture; stop/unload it and explicitly update its original manifest to opt in"))?;
        let dir = self.capture_dir(record)?;
        [
            ("stdout", "stdout.log", paths.stdout),
            ("stderr", "stderr.log", paths.stderr),
        ]
        .into_iter()
        .map(|(stream, name, path)| {
            let file = dir
                .as_ref()
                .map(|dir| dir.private_file(name, false))
                .transpose()?
                .flatten();
            crate::logs::read_file(stream, path, file, limits)
        })
        .collect()
    }
}

fn entry_name(name: &str) -> Result<CString> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.chars().any(char::is_control)
    {
        return Err(Error::new("invalid_path", "invalid directory entry name"));
    }
    CString::new(name).map_err(|_| Error::new("invalid_path", "NUL in filename"))
}
fn errno() -> std::io::Error {
    std::io::Error::last_os_error()
}
fn path_error(path: &Path, e: std::io::Error) -> Error {
    Error::new(
        "unsafe_or_inaccessible_path",
        format!(
            "{}: {e}; symlinks and non-directory ancestors are forbidden",
            path.display()
        ),
    )
}
struct Dir(File);
impl Dir {
    fn open(path: &Path, create: bool) -> Result<Option<Self>> {
        validate_path(path)?;
        let root = CString::new("/").unwrap();
        // SAFETY: C strings are valid, returned descriptors are owned exactly once.
        let fd = unsafe {
            libc::open(
                root.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(errno().into());
        }
        let mut dir = Self(unsafe { File::from_raw_fd(fd) });
        for part in path.components() {
            let Component::Normal(part) = part else {
                continue;
            };
            let part = CString::new(part.as_bytes())
                .map_err(|_| Error::new("invalid_path", "NUL in path"))?;
            let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
            let mut fd = unsafe { libc::openat(dir.0.as_raw_fd(), part.as_ptr(), flags) };
            if fd < 0 && errno().kind() == std::io::ErrorKind::NotFound {
                if !create {
                    return Ok(None);
                }
                let rc = unsafe { libc::mkdirat(dir.0.as_raw_fd(), part.as_ptr(), 0o700) };
                if rc < 0 && errno().kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(path_error(path, errno()));
                }
                dir.0.sync_all()?;
                fd = unsafe { libc::openat(dir.0.as_raw_fd(), part.as_ptr(), flags) };
            }
            if fd < 0 {
                return Err(path_error(path, errno()));
            }
            dir = Self(unsafe { File::from_raw_fd(fd) });
        }
        Ok(Some(dir))
    }
    fn file(
        &self,
        name: &str,
        extra_flags: i32,
        mode: libc::mode_t,
        managed: bool,
    ) -> Result<Option<File>> {
        let name = entry_name(name)?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                extra_flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                // C varargs promote Darwin's u16 mode_t to int. All supplied
                // permission values also fit int on Linux (where mode_t is u32).
                mode as libc::c_int,
            )
        };
        if fd < 0 {
            let e = errno();
            if e.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(Error::new(
                "unsafe_or_inaccessible_file",
                format!("{}: {e}", name.to_string_lossy()),
            ));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let md = file.metadata()?;
        if !md.is_file() || (managed && md.nlink() != 1) {
            return Err(Error::new(
                "unsafe_file",
                "expected a regular, non-hardlinked file",
            ));
        }
        Ok(Some(file))
    }
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let Some(file) = self.file(name, libc::O_RDONLY, 0, true)? else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > LIMIT {
            return Err(Error::new("invalid_state", "managed file exceeds 4 MiB"));
        }
        Ok(Some(bytes))
    }
    fn lock(&self) -> Result<File> {
        self.file(".lock", libc::O_RDWR | libc::O_CREAT, 0o600, true)?
            .ok_or_else(|| Error::new("lock_error", "lock could not be created"))
    }
    fn atomic(&self, name: &str, bytes: &[u8], replace: bool) -> Result<()> {
        if bytes.len() as u64 > LIMIT {
            return Err(Error::new("invalid_state", "managed file exceeds 4 MiB"));
        }
        let target = entry_name(name)?;
        let temp = format!(
            ".tmp-{}-{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        );
        let temp_c = entry_name(&temp)?;
        let mut f = self
            .file(
                &temp,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                0o600,
                true,
            )?
            .ok_or_else(|| Error::new("io", "temporary file creation failed"))?;
        let result = (|| -> Result<()> {
            f.write_all(bytes)?;
            f.sync_all()?;
            if replace {
                // renameat replaces the entry itself, never follows its target.
                let rc = unsafe {
                    libc::renameat(
                        self.0.as_raw_fd(),
                        temp_c.as_ptr(),
                        self.0.as_raw_fd(),
                        target.as_ptr(),
                    )
                };
                if rc < 0 {
                    return Err(errno().into());
                }
            } else {
                // linkat gives portable, atomic NOREPLACE semantics.
                let rc = unsafe {
                    libc::linkat(
                        self.0.as_raw_fd(),
                        temp_c.as_ptr(),
                        self.0.as_raw_fd(),
                        target.as_ptr(),
                        0,
                    )
                };
                if rc < 0 {
                    return Err(Error::new("file_collision", errno().to_string()));
                }
                let rc = unsafe { libc::unlinkat(self.0.as_raw_fd(), temp_c.as_ptr(), 0) };
                if rc < 0 {
                    return Err(errno().into());
                }
            }
            self.0.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            unsafe {
                libc::unlinkat(self.0.as_raw_fd(), temp_c.as_ptr(), 0);
            }
        }
        result
    }
    fn remove(&self, name: &str) -> Result<()> {
        let name = entry_name(name)?;
        let rc = unsafe { libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), 0) };
        if rc < 0 {
            return Err(errno().into());
        }
        self.0.sync_all()?;
        Ok(())
    }
    fn names(&self) -> Result<Vec<String>> {
        let fd = unsafe { libc::dup(self.0.as_raw_fd()) };
        if fd < 0 {
            return Err(errno().into());
        }
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            unsafe {
                libc::close(fd);
            }
            return Err(errno().into());
        }
        let mut names = Vec::new();
        loop {
            // readdir uses null for both EOF and failure; clear errno to distinguish them.
            #[cfg(target_os = "linux")]
            unsafe {
                *libc::__errno_location() = 0;
            }
            #[cfg(target_os = "macos")]
            unsafe {
                *libc::__error() = 0;
            }
            let entry = unsafe { libc::readdir(stream) };
            if entry.is_null() {
                let error = errno();
                if error.raw_os_error() != Some(0) {
                    unsafe {
                        libc::closedir(stream);
                    }
                    return Err(error.into());
                }
                break;
            }
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            match std::str::from_utf8(bytes) {
                Ok(s) => names.push(s.to_owned()),
                Err(_) => {
                    unsafe {
                        libc::closedir(stream);
                    }
                    return Err(Error::new("invalid_state", "non-UTF-8 state entry"));
                }
            }
        }
        unsafe {
            libc::closedir(stream);
        }
        Ok(names)
    }
}

pub(crate) fn is_safe_directory(path: &Path) -> Result<bool> {
    Ok(Dir::open(path, false)?.is_some())
}
pub(crate) fn open_input_file(path: &Path) -> Result<File> {
    validate_path(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::new("invalid_path", "file has no parent directory"))?;
    let dir = Dir::open(parent, false)?
        .ok_or_else(|| Error::new("missing_file", format!("{} does not exist", path.display())))?;
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| Error::new("invalid_path", "file has no valid filename"))?;
    dir.file(name, libc::O_RDONLY, 0, false)?
        .ok_or_else(|| Error::new("missing_file", format!("{} does not exist", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Mode, Restart};
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };
    struct Fixture {
        _tmp: tempfile::TempDir,
        store: Store,
        manifest: Manifest,
    }
    impl Fixture {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().canonicalize().unwrap();
            let executable = root.join("executable");
            fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            let manifest = Manifest {
                id: "test-job".into(),
                executable,
                args: vec!["a b".into()],
                cwd: root.clone(),
                mode: Mode::Service,
                restart: Restart::Never,
            };
            let store = Store::new(root.join("state"), root.join("native"), Platform::Linux);
            Self {
                _tmp: tmp,
                store,
                manifest,
            }
        }
        fn add(&self) {
            self.store
                .apply(&self.store.plan_add(&self.manifest).unwrap())
                .unwrap();
        }
    }
    fn mac_fixture() -> Fixture {
        let mut fixture = Fixture::new();
        fixture.store.platform = Platform::Macos;
        fixture
    }
    #[test]
    fn mac_capture_is_private_bounded_and_retained_on_remove() {
        let f = mac_fixture();
        let plan = f.store.plan_add(&f.manifest).unwrap();
        assert!(!f.store.state_dir.exists());
        f.store.apply(&plan).unwrap();
        let record = f.store.read(&f.manifest.id).unwrap();
        assert_eq!(record.schema_version, 2);
        let paths = f.store.log_paths(&record).unwrap().unwrap();
        for path in [&paths.stdout, &paths.stderr] {
            assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o600);
        }
        assert_eq!(
            fs::metadata(&paths.directory).unwrap().mode() & 0o777,
            0o700
        );
        f.store.verify_capture(&record).unwrap();
        fs::write(&paths.stdout, "old\npassword=hidden\nlast\n").unwrap();
        let logs = f
            .store
            .read_logs(&record, crate::logs::LogLimits::new(2, 64).unwrap())
            .unwrap();
        assert!(logs[0].redacted && logs[0].lines_truncated);
        assert!(!logs[0].text.contains("hidden"));
        assert!(logs[1].empty && !logs[1].missing);
        f.store
            .apply(&f.store.plan_remove(&f.manifest.id).unwrap())
            .unwrap();
        assert!(paths.stdout.exists());
        assert!(f.store.list().unwrap().is_empty());
        f.add();
        let new_paths = f
            .store
            .log_paths(&f.store.read(&f.manifest.id).unwrap())
            .unwrap()
            .unwrap();
        assert_ne!(new_paths.directory, paths.directory);
        assert!(fs::read_to_string(paths.stdout).unwrap().contains("hidden"));
    }
    #[test]
    fn legacy_mac_records_remain_readable_until_explicit_update() {
        let f = mac_fixture();
        fs::create_dir(&f.store.state_dir).unwrap();
        fs::set_permissions(&f.store.state_dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(&f.store.native_dir).unwrap();
        let legacy = f.store.expected(&f.manifest, 1, None).unwrap();
        let text = toml::to_string(&legacy).unwrap();
        assert!(!text.contains("log_generation"));
        fs::write(
            f.store.definition_path(&f.manifest.id).unwrap(),
            &legacy.native_bytes,
        )
        .unwrap();
        fs::write(f.store.state_dir.join("test-job.toml"), &text).unwrap();
        assert_eq!(f.store.read(&f.manifest.id).unwrap(), legacy);
        assert!(f.store.plan_add(&f.manifest).unwrap().is_noop());
        assert!(!f.store.state_dir.join("logs").exists());
        assert_eq!(
            f.store
                .read_logs(&legacy, crate::logs::LogLimits::new(10, 64).unwrap())
                .unwrap_err()
                .code,
            "log_capture_unavailable"
        );
        let plan = f.store.plan_update(&f.manifest).unwrap();
        assert!(!plan.is_noop());
        assert!(!f.store.state_dir.join("logs").exists());
        f.store.apply(&plan).unwrap();
        let current = f.store.read(&f.manifest.id).unwrap();
        assert_eq!(current.schema_version, 2);
        assert!(f.store.plan_update(&f.manifest).unwrap().is_noop());
        let mut changed = f.manifest.clone();
        changed.args.push("new".into());
        f.store
            .apply(&f.store.plan_update(&changed).unwrap())
            .unwrap();
        assert_eq!(
            f.store.read(&f.manifest.id).unwrap().log_generation,
            current.log_generation
        );
    }
    #[test]
    fn unsafe_or_missing_capture_is_never_repaired_by_reading() {
        for kind in ["symlink", "hardlink", "public", "missing", "marker"] {
            let f = mac_fixture();
            f.add();
            let record = f.store.read(&f.manifest.id).unwrap();
            let paths = f.store.log_paths(&record).unwrap().unwrap();
            let outside = f.manifest.cwd.join("outside");
            fs::write(&outside, "untouched").unwrap();
            match kind {
                "symlink" => {
                    fs::remove_file(&paths.stdout).unwrap();
                    symlink(&outside, &paths.stdout).unwrap();
                }
                "hardlink" => {
                    fs::remove_file(&paths.stdout).unwrap();
                    fs::hard_link(&outside, &paths.stdout).unwrap();
                }
                "public" => {
                    fs::set_permissions(&paths.stdout, fs::Permissions::from_mode(0o644)).unwrap()
                }
                "missing" => fs::remove_file(&paths.stdout).unwrap(),
                _ => fs::write(paths.directory.join(".owner"), "foreign").unwrap(),
            }
            assert!(f.store.verify_capture(&record).is_err(), "{kind}");
            let logs = f
                .store
                .read_logs(&record, crate::logs::LogLimits::new(10, 64).unwrap());
            if kind == "missing" {
                assert!(logs.unwrap()[0].missing);
            } else {
                assert!(logs.is_err(), "{kind}");
            }
            assert_eq!(fs::read_to_string(&outside).unwrap(), "untouched");
        }
    }
    #[test]
    fn foreign_log_roots_and_generation_collisions_are_refused() {
        let f = mac_fixture();
        fs::create_dir(&f.store.state_dir).unwrap();
        fs::set_permissions(&f.store.state_dir, fs::Permissions::from_mode(0o700)).unwrap();
        let root = f.store.state_dir.join("logs");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(root.join("foreign"), "untouched").unwrap();
        assert_eq!(
            f.store.plan_add(&f.manifest).unwrap_err().code,
            "log_ownership"
        );
        assert_eq!(
            fs::read_to_string(root.join("foreign")).unwrap(),
            "untouched"
        );
        fs::remove_file(root.join("foreign")).unwrap();
        let plan = f.store.plan_add(&f.manifest).unwrap();
        let paths = f
            .store
            .log_paths(plan.after.as_ref().unwrap())
            .unwrap()
            .unwrap();
        fs::write(root.join(".owner"), LOG_ROOT_MARKER).unwrap();
        fs::set_permissions(root.join(".owner"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::create_dir(&paths.directory).unwrap();
        fs::set_permissions(&paths.directory, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(f.store.apply(&plan).unwrap_err().code, "log_collision");
        assert!(!f.store.state_dir.join(JOURNAL).exists());
    }
    #[test]
    fn unsafe_generation_and_public_state_are_refused_without_chmod() {
        let f = mac_fixture();
        for value in ["../outside", "ABCDEF", "", &"0".repeat(33)] {
            assert!(f.store.capture_paths(&f.manifest.id, value).is_err());
        }
        fs::create_dir(&f.store.state_dir).unwrap();
        fs::set_permissions(&f.store.state_dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            f.store.plan_add(&f.manifest).unwrap_err().code,
            "unsafe_log_path"
        );
        assert_eq!(
            fs::metadata(&f.store.state_dir).unwrap().mode() & 0o777,
            0o755
        );
    }
    #[test]
    fn planning_creates_no_directories_or_locks() {
        let f = Fixture::new();
        let plan = f.store.plan_add(&f.manifest).unwrap();
        assert!(!plan.is_noop());
        assert!(!f.store.state_dir.exists());
        assert!(!f.store.native_dir.exists());
        assert!(f.store.list().unwrap().is_empty());
        assert!(f.store.plan_remove("missing").unwrap().is_noop());
        assert!(!f.store.state_dir.exists());
    }
    #[test]
    fn add_update_remove_are_idempotent_and_exact() {
        let mut f = Fixture::new();
        f.add();
        assert_eq!(f.store.list().unwrap().len(), 1);
        let managed_text = fs::read_to_string(f.store.state_dir.join("test-job.toml")).unwrap();
        assert!(managed_text.contains("[manifest]"));
        let managed_record: Record = toml::from_str(&managed_text).unwrap();
        assert_eq!(managed_record.manifest, f.manifest);
        assert!(!f.store.state_dir.join("test-job.json").exists());
        assert!(f.store.plan_add(&f.manifest).unwrap().is_noop());
        let native = f.store.definition_path(&f.manifest.id).unwrap();
        let record = f.store.read(&f.manifest.id).unwrap();
        assert_eq!(fs::read_to_string(&native).unwrap(), record.native_bytes);
        f.manifest.args.push("new value".into());
        assert!(f.store.plan_add(&f.manifest).is_err());
        let update = f.store.plan_update(&f.manifest).unwrap();
        assert!(!update.is_noop());
        f.store.apply(&update).unwrap();
        assert!(f.store.plan_update(&f.manifest).unwrap().is_noop());
        assert_eq!(f.store.read(&f.manifest.id).unwrap().manifest, f.manifest);
        f.store
            .apply(&f.store.plan_remove(&f.manifest.id).unwrap())
            .unwrap();
        assert!(!native.exists());
        assert!(f.store.plan_remove(&f.manifest.id).unwrap().is_noop());
        assert!(f.store.list().unwrap().is_empty());
    }
    #[test]
    fn foreign_definitions_are_never_adopted_or_removed() {
        let f = Fixture::new();
        fs::create_dir(&f.store.native_dir).unwrap();
        let native = f.store.definition_path(&f.manifest.id).unwrap();
        fs::write(&native, f.store.platform.render(&f.manifest).unwrap()).unwrap();
        assert_eq!(
            f.store.plan_add(&f.manifest).unwrap_err().code,
            "foreign_definition"
        );
        assert_eq!(
            f.store.plan_remove(&f.manifest.id).unwrap_err().code,
            "foreign_definition"
        );
        assert!(native.exists());
        assert!(!f.store.state_dir.exists());
    }
    #[test]
    fn native_and_record_drift_are_rejected() {
        let f = Fixture::new();
        f.add();
        let native = f.store.definition_path(&f.manifest.id).unwrap();
        fs::write(&native, "externally edited").unwrap();
        assert_eq!(
            f.store.read(&f.manifest.id).unwrap_err().code,
            "definition_drift"
        );
        assert!(f.store.plan_remove(&f.manifest.id).is_err());
        assert_eq!(fs::read_to_string(native).unwrap(), "externally edited");
    }
    #[test]
    fn native_symlinks_and_ancestor_symlinks_are_rejected() {
        let f = Fixture::new();
        let outside = f.manifest.cwd.join("outside");
        fs::write(&outside, "untouched").unwrap();
        fs::create_dir(&f.store.native_dir).unwrap();
        symlink(&outside, f.store.definition_path(&f.manifest.id).unwrap()).unwrap();
        assert!(f.store.plan_add(&f.manifest).is_err());
        assert_eq!(fs::read_to_string(&outside).unwrap(), "untouched");
        let actual = f.manifest.cwd.join("actual");
        fs::create_dir(&actual).unwrap();
        let linked = f.manifest.cwd.join("linked");
        symlink(&actual, &linked).unwrap();
        let store = Store::new(linked.join("state"), actual.join("native"), Platform::Linux);
        assert!(store.plan_add(&f.manifest).is_err());
        assert!(!actual.join("state").exists());
    }
    #[test]
    fn state_and_lock_symlinks_are_rejected() {
        let f = Fixture::new();
        f.add();
        let record = f.store.state_dir.join("test-job.toml");
        let outside = f.manifest.cwd.join("outside");
        fs::write(&outside, "untouched").unwrap();
        fs::remove_file(&record).unwrap();
        symlink(&outside, &record).unwrap();
        assert!(f.store.read(&f.manifest.id).is_err());
        fs::remove_file(f.store.state_dir.join(".lock")).unwrap();
        symlink(&outside, f.store.state_dir.join(".lock")).unwrap();
        assert!(f.store.with_lock(|| Ok(())).is_err());
        assert_eq!(fs::read_to_string(&outside).unwrap(), "untouched");
    }
    #[test]
    fn pending_journal_blocks_reads_and_mutations() {
        let f = Fixture::new();
        f.add();
        fs::write(f.store.state_dir.join(JOURNAL), "interrupted operation").unwrap();
        assert_eq!(f.store.check().unwrap_err().code, "recovery_required");
        assert_eq!(
            f.store.plan_remove(&f.manifest.id).unwrap_err().code,
            "recovery_required"
        );
        assert_eq!(
            f.store.with_lock(|| Ok(())).unwrap_err().code,
            "recovery_required"
        );
    }
    #[test]
    fn stale_plan_and_late_collision_fail_closed() {
        let f = Fixture::new();
        let add = f.store.plan_add(&f.manifest).unwrap();
        f.add();
        assert_eq!(f.store.apply(&add).unwrap_err().code, "stale_plan");
        let remove = f.store.plan_remove(&f.manifest.id).unwrap();
        let native = f.store.definition_path(&f.manifest.id).unwrap();
        fs::write(&native, "changed").unwrap();
        assert!(f.store.apply(&remove).is_err());
        assert_eq!(fs::read_to_string(&native).unwrap(), "changed");
    }
    #[test]
    fn failed_guard_leaves_definitions_unchanged_and_releases_lock() {
        let f = Fixture::new();
        let add = f.store.plan_add(&f.manifest).unwrap();
        let err = f
            .store
            .apply_checked(&add, || Err(Error::new("test_guard", "blocked")))
            .unwrap_err();
        assert_eq!(err.code, "test_guard");
        assert!(!f.store.native_dir.exists());
        assert!(!f.store.state_dir.join(JOURNAL).exists());
        f.store.apply(&add).unwrap();
    }
    #[test]
    fn a_guard_cannot_race_the_commit_preconditions() {
        let f = Fixture::new();
        let add = f.store.plan_add(&f.manifest).unwrap();
        let path = f.store.definition_path(&f.manifest.id).unwrap();
        let err = f
            .store
            .apply_checked(&add, || {
                fs::create_dir(&f.store.native_dir)?;
                fs::write(&path, "foreign")?;
                Ok(())
            })
            .unwrap_err();
        assert_eq!(err.code, "foreign_definition");
        assert_eq!(fs::read_to_string(&path).unwrap(), "foreign");
    }
    #[test]
    fn deleted_executable_does_not_prevent_removal() {
        let f = Fixture::new();
        f.add();
        fs::remove_file(&f.manifest.executable).unwrap();
        let plan = f.store.plan_remove(&f.manifest.id).unwrap();
        f.store.apply(&plan).unwrap();
    }
    #[test]
    fn tampered_public_plan_fields_are_rejected() {
        let f = Fixture::new();
        let mut plan = f.store.plan_add(&f.manifest).unwrap();
        plan.id = "different".into();
        assert_eq!(f.store.apply(&plan).unwrap_err().code, "plan_mismatch");
        assert!(!f.store.native_dir.exists());
    }
    #[test]
    fn lock_is_exclusive() {
        let f = Fixture::new();
        f.store
            .with_lock(|| {
                assert_eq!(f.store.with_lock(|| Ok(())).unwrap_err().code, "store_busy");
                Ok(())
            })
            .unwrap();
        f.store.with_lock(|| Ok(())).unwrap();
    }
    #[test]
    fn manifest_symlinks_are_rejected() {
        let f = Fixture::new();
        let path = f.manifest.cwd.join("manifest.toml");
        let link = f.manifest.cwd.join("linked.toml");
        fs::write(&path, toml::to_string(&f.manifest).unwrap()).unwrap();
        symlink(&path, &link).unwrap();
        assert!(crate::model::read(&link).is_err());
        assert_eq!(crate::model::read(&path).unwrap(), f.manifest);
    }
    #[test]
    fn interrupted_commit_retains_exact_recovery_material() {
        let f = Fixture::new();
        let plan = f.store.plan_add(&f.manifest).unwrap();
        fs::create_dir(&f.store.state_dir).unwrap();
        fs::create_dir(&f.store.native_dir).unwrap();
        let journal = Journal {
            schema_version: 1,
            operation: "add".into(),
            id: f.manifest.id.clone(),
            state_dir: f.store.state_dir.clone(),
            native_dir: f.store.native_dir.clone(),
            before: None,
            after: plan.after.clone(),
        };
        let state = Dir::open(&f.store.state_dir, false).unwrap().unwrap();
        state
            .atomic(
                JOURNAL,
                &serde_json::to_vec_pretty(&journal).unwrap(),
                false,
            )
            .unwrap();
        let after = plan.after.as_ref().unwrap();
        let native = Dir::open(&f.store.native_dir, false).unwrap().unwrap();
        native
            .atomic(&after.native_name, after.native_bytes.as_bytes(), false)
            .unwrap();
        // Simulate interruption before the ownership record is committed.
        assert!(!f.store.state_dir.join("test-job.toml").exists());
        assert_eq!(f.store.apply(&plan).unwrap_err().code, "recovery_required");
        let saved: Journal =
            serde_json::from_slice(&fs::read(f.store.state_dir.join(JOURNAL)).unwrap()).unwrap();
        assert!(saved.before.is_none());
        assert_eq!(saved.after.as_ref().unwrap(), after);
        assert_eq!(saved.state_dir, f.store.state_dir);
        assert_eq!(saved.native_dir, f.store.native_dir);
        // Explicitly complete the recorded pair, then acknowledge recovery.
        state
            .atomic(
                "test-job.toml",
                toml::to_string(after).unwrap().as_bytes(),
                false,
            )
            .unwrap();
        assert_eq!(f.store.check().unwrap_err().code, "recovery_required");
        state.remove(JOURNAL).unwrap();
        f.store.check().unwrap();
        assert!(f.store.plan_add(&f.manifest).unwrap().is_noop());
    }
}
