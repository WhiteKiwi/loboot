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

const LIMIT: u64 = 4 * 1024 * 1024;
const JOURNAL: &str = ".pending.json";
static NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema_version: u32,
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema_version: u32,
    operation: String,
    id: String,
    state_dir: PathBuf,
    native_dir: PathBuf,
    before: Option<Record>,
    after: Option<Record>,
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
    fn expected(&self, manifest: &Manifest) -> Result<Record> {
        manifest.validate_syntax()?;
        let native_name = self.platform.native_name(&manifest.id);
        entry_name(&native_name)?;
        Ok(Record {
            schema_version: 1,
            manifest: manifest.clone(),
            native_name,
            native_bytes: self.platform.render(manifest)?,
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
        if record.schema_version != 1
            || record.manifest.id != id
            || self.expected(&record.manifest)? != record
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
        let after = self.expected(manifest)?;
        let before = self.record(&manifest.id, state.as_ref(), native.as_ref())?;
        if let Some(before) = before {
            if before == after {
                return Ok(self.plan("add", Some(before), Some(after), Vec::new()));
            }
            return Err(Error::new(
                "already_managed",
                format!("{} already exists; use update", manifest.id),
            ));
        }
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
        let after = self.expected(manifest)?;
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
            if self.expected(&after.manifest)? != *after {
                return Err(Error::new(
                    "plan_mismatch",
                    "plan does not match this backend",
                ));
            }
        }
        Ok(())
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
                mode,
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
