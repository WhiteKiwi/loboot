//! Pure native definitions and command plans, with injectable read-only queries.
//!
//! Native references: systemd.service(5), systemd.exec(5), systemd.syntax(7),
//! systemctl(1), launchd.plist(5), and launchctl(1). In particular, launchd
//! bootstrap + RunAtLoad already launches a job; a second kickstart is unsafe.
use crate::model::{Manifest, Mode, Restart};
pub use crate::process::CaptureLimits;
use crate::{Error, Result};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Linux,
    Macos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Start,
    Stop,
    Restart,
    Enable,
    Disable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CommandOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
    pub termination_for_limit: bool,
    pub stdout_invalid_utf8: bool,
    pub stderr_invalid_utf8: bool,
}

impl CommandOutput {
    /// A successful exit alone cannot establish complete native observations.
    pub fn is_complete(&self) -> bool {
        !self.stdout_truncated
            && !self.stderr_truncated
            && !self.timed_out
            && !self.termination_for_limit
            && !self.stdout_invalid_utf8
            && !self.stderr_invalid_utf8
    }

    /// Deliberately contains no arbitrary native output or command arguments.
    pub fn failure_summary(&self) -> String {
        let reason = if self.timed_out {
            "native command capture timed out; outcome is uncertain"
        } else if self.stdout_truncated || self.stderr_truncated || self.termination_for_limit {
            "native command output exceeded its capture limit; outcome is uncertain"
        } else if self.stdout_invalid_utf8 || self.stderr_invalid_utf8 {
            "native command returned invalid UTF-8; outcome is uncertain"
        } else if self.signal.is_some() {
            "native command was terminated by a signal"
        } else {
            "native command failed"
        };
        let hint = if self.stderr.contains("Failed to connect to bus")
            || self.stderr.contains("Could not find domain")
        {
            "Check the intended logged-in user session and native manager availability."
        } else if self.stderr.contains("Permission denied")
            || self.stderr.contains("Operation not permitted")
        {
            "Check ownership and permissions in the intended user session."
        } else if self.stderr.contains("No such file or directory") {
            "Check the configured executable and definition paths."
        } else {
            "Inspect native status and bounded logs before retrying."
        };
        format!(
            "{reason} (exit {:?}, signal {:?}). {hint}",
            self.code, self.signal
        )
    }
}

/// Both reads and mutations are explicit argv invocations. No shell is involved.
pub trait Runner {
    fn run(&mut self, command: &CommandSpec) -> Result<CommandOutput>;

    fn run_with_limits(
        &mut self,
        command: &CommandSpec,
        _limits: CaptureLimits,
    ) -> Result<CommandOutput> {
        self.run(command)
    }
}

#[derive(Debug, Default)]
pub struct ProcessRunner;

impl Runner for ProcessRunner {
    fn run(&mut self, command: &CommandSpec) -> Result<CommandOutput> {
        self.run_with_limits(command, CaptureLimits::default())
    }

    fn run_with_limits(
        &mut self,
        command: &CommandSpec,
        limits: CaptureLimits,
    ) -> Result<CommandOutput> {
        let output = crate::process::capture(
            Command::new(&command.program).args(&command.args).env("LC_ALL", "C"),
            limits,
        ).map_err(|e| Error::new("native_command", format!("native helper could not be captured ({:?}); inspect native status before retrying", e.kind())))?;
        let stdout_invalid_utf8 = std::str::from_utf8(&output.stdout).is_err();
        let stderr_invalid_utf8 = std::str::from_utf8(&output.stderr).is_err();
        Ok(CommandOutput {
            success: output.success,
            code: output.code,
            signal: output.signal,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            stdout_truncated: output.stdout_truncated,
            stderr_truncated: output.stderr_truncated,
            timed_out: output.timed_out,
            termination_for_limit: output.termination_for_limit,
            stdout_invalid_utf8,
            stderr_invalid_utf8,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: &'static str,
}

/// Read-only process observations and pacing, injectable for deterministic tests.
pub trait TeardownObserver {
    fn process_exists(&mut self, pid: u32) -> Result<bool>;
    fn pause(&mut self);
}

#[derive(Debug, Default)]
pub struct ProcessObserver;

impl TeardownObserver for ProcessObserver {
    fn process_exists(&mut self, pid: u32) -> Result<bool> {
        let pid = i32::try_from(pid)
            .ok()
            .filter(|pid| *pid > 0)
            .ok_or_else(|| Error::new("status_required", "invalid observed launchd PID"))?;
        // SAFETY: signal 0 only probes existence/permission; it sends no signal.
        let result = unsafe { libc::kill(pid, 0) };
        if result == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => Ok(false),
            // A live PID we cannot inspect is still not proof of teardown.
            Some(libc::EPERM) => Ok(true),
            _ => Err(Error::new(
                "status_unavailable",
                format!("PID existence probe: {error}"),
            )),
        }
    }

    fn pause(&mut self) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

// At most 100 observation rounds and 99 pacing waits (9.9s plus query time).
// Two consecutive settled observations avoid bootstrapping in the same turn as
// launchd's removal acknowledgement. There is never a bootstrap retry.
const TEARDOWN_OBSERVATIONS: usize = 100;

fn observed_launchd_pid(status: &Status, after_bootout: bool) -> Result<Option<u32>> {
    // Hosted launchd reports this exact transitional spelling while handling
    // SIGTERM. It is not proof of a running or stopped process. Only an already
    // accepted bootout may keep observing it, with a valid PID below.
    let terminating = after_bootout && status.loaded == Some(true) && status.state == "SIGTERMed";
    if status.running.is_none() && !terminating {
        return Err(Error::new(
            "status_required",
            "launchd process state is unknown",
        ));
    }
    match status.raw.get("pid") {
        Some(raw) => raw
            .parse::<u32>()
            .ok()
            .filter(|pid| *pid > 0 && *pid <= i32::MAX as u32)
            .map(Some)
            .ok_or_else(|| Error::new("status_required", "launchd returned an invalid PID")),
        None if status.running == Some(false) => Ok(None),
        None => Err(Error::new(
            "status_required",
            "launchd process state or PID is unknown",
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Session {
    pub platform: Platform,
    pub uid: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct RuntimeContext {
    pub uid: Option<u32>,
    pub loaded: Option<bool>,
}

/// Unknown native facts remain null. Command acceptance is not process success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    pub loaded: Option<bool>,
    pub running: Option<bool>,
    pub enabled: Option<bool>,
    pub state: String,
    pub raw: BTreeMap<String, String>,
}

pub fn effective_uid() -> Option<u32> {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no arguments, memory access, or preconditions.
        Some(unsafe { libc::geteuid() })
    }
    #[cfg(not(unix))]
    {
        None
    }
}

impl Platform {
    pub fn current() -> Result<Self> {
        if cfg!(target_os = "linux") {
            Ok(Self::Linux)
        } else if cfg!(target_os = "macos") {
            Ok(Self::Macos)
        } else {
            Err(Error::new(
                "unsupported_platform",
                "only Linux and macOS user sessions are supported",
            ))
        }
    }

    /// The filename, including extension; launchd's label omits .plist.
    pub fn native_name(&self, id: &str) -> String {
        match self {
            Self::Linux => format!("loboot-{id}.service"),
            Self::Macos => format!("com.loboot.{id}.plist"),
        }
    }

    pub fn label(&self, id: &str) -> String {
        match self {
            Self::Linux => self.native_name(id),
            Self::Macos => format!("com.loboot.{id}"),
        }
    }

    pub fn native_dir(&self, home: &Path) -> PathBuf {
        match self {
            Self::Linux => home.join(".config/systemd/user"),
            // Default Disabled=true plus a native disabled override guards installation.
            Self::Macos => home.join("Library/LaunchAgents"),
        }
    }

    pub fn render(&self, manifest: &Manifest) -> Result<String> {
        self.render_internal(manifest, false)
    }

    /// A display-only definition. Validate the original values, including PATH,
    /// before hiding them; a redaction placeholder is never a manifest value.
    pub fn render_redacted(&self, manifest: &Manifest) -> Result<String> {
        self.render_internal(manifest, true)
    }

    fn render_internal(&self, manifest: &Manifest, redacted: bool) -> Result<String> {
        manifest.validate_syntax()?;
        let executable = utf8_path(&manifest.executable)?;
        let cwd = utf8_path(&manifest.cwd)?;
        let redaction_notice = if redacted && !manifest.environment.is_empty() {
            match self {
                Self::Linux => "# Environment values redacted; display only, not replayable.\n",
                Self::Macos => {
                    "<!-- Environment values redacted; display only, not replayable. -->\n"
                }
            }
        } else {
            ""
        };
        match self {
            Self::Linux => {
                // systemd validates the decoded executable with string_is_safe;
                // quoting cannot make these pathname characters acceptable.
                if executable.contains(['\\', '\'', '"', '*', '?', '[', ']']) {
                    return Err(Error::new(
                        "unsupported_executable_path",
                        "systemd executable paths cannot contain quotes, backslashes, or glob metacharacters; arguments are not subject to this restriction",
                    ));
                }
                let kind = match manifest.mode {
                    Mode::Once => "oneshot",
                    Mode::Service => "exec",
                };
                let restart = match manifest.restart {
                    Restart::Never => "no",
                    Restart::OnFailure => "on-failure",
                    Restart::Always => "always",
                };
                // The colon prefix disables environment expansion in the complete
                // argv, including argv[0]. It is parsed after native unquoting.
                let mut argv = vec![systemd_quote(&format!(":{executable}"))];
                argv.extend(manifest.args.iter().map(|s| systemd_quote(s)));
                let environment = if manifest.environment.is_empty() {
                    String::new()
                } else {
                    let assignments = manifest
                        .environment
                        .iter()
                        .map(|(name, value)| {
                            let value = if redacted { "[REDACTED]" } else { value };
                            systemd_quote(&format!("{name}={value}"))
                        })
                        .collect::<Vec<_>>();
                    // Environment= does not expand '$'; specifiers still require
                    // '%%'. Quote the entire assignment, not only its value.
                    format!("Environment={}\n", assignments.join(" "))
                };
                Ok(format!(
                    "# Managed by loboot; do not edit.\n{redaction_notice}[Unit]\nDescription=loboot {}\n\n[Service]\nType={kind}\nExecStart={}\nWorkingDirectory={}\n{environment}Restart={restart}\nRestartSec=2s\n\n[Install]\nWantedBy=default.target\n",
                    manifest.id,
                    argv.join(" "),
                    systemd_directory(cwd)
                ))
            }
            Self::Macos => {
                let mut args = format!("    <string>{}</string>\n", xml(executable)?);
                for arg in &manifest.args {
                    args.push_str(&format!("    <string>{}</string>\n", xml(arg)?));
                }
                let keepalive = match manifest.restart {
                    Restart::Never => "<false/>",
                    Restart::OnFailure => "<dict><key>SuccessfulExit</key><false/></dict>",
                    Restart::Always => "<true/>",
                };
                let mut environment = String::new();
                if !manifest.environment.is_empty() {
                    environment.push_str("  <key>EnvironmentVariables</key>\n  <dict>\n");
                    for (name, value) in &manifest.environment {
                        let value = if redacted { "[REDACTED]" } else { value };
                        environment.push_str(&format!(
                            "    <key>{}</key><string>{}</string>\n",
                            xml(name)?,
                            xml(value)?,
                        ));
                    }
                    environment.push_str("  </dict>\n");
                }
                Ok(format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<!-- Managed by loboot; do not edit. -->\n{redaction_notice}<plist version=\"1.0\">\n<dict>\n  <key>Label</key><string>{}</string>\n  <key>ProgramArguments</key>\n  <array>\n{args}  </array>\n  <key>WorkingDirectory</key><string>{}</string>\n{environment}  <key>Disabled</key><true/>\n  <key>RunAtLoad</key><true/>\n  <key>KeepAlive</key>{keepalive}\n  <key>ThrottleInterval</key><integer>2</integer>\n</dict>\n</plist>\n",
                    self.label(&manifest.id),
                    xml(cwd)?
                ))
            }
        }
    }

    /// Schema-2 rendering; the legacy renderer remains byte-for-byte stable.
    pub fn render_with_logs(
        &self,
        manifest: &Manifest,
        stdout_path: &Path,
        stderr_path: &Path,
    ) -> Result<String> {
        self.render_with_logs_internal(manifest, stdout_path, stderr_path, false)
    }

    /// Display-only schema-2 rendering with original environment validation.
    pub fn render_with_logs_redacted(
        &self,
        manifest: &Manifest,
        stdout_path: &Path,
        stderr_path: &Path,
    ) -> Result<String> {
        self.render_with_logs_internal(manifest, stdout_path, stderr_path, true)
    }

    fn render_with_logs_internal(
        &self,
        manifest: &Manifest,
        stdout_path: &Path,
        stderr_path: &Path,
        redacted: bool,
    ) -> Result<String> {
        let rendered = self.render_internal(manifest, redacted)?;
        if *self == Self::Linux {
            return Ok(rendered);
        }
        for path in [stdout_path, stderr_path] {
            if !path.is_absolute()
                || path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                return Err(Error::new(
                    "invalid_path",
                    "launchd log paths must be absolute without traversal",
                ));
            }
        }
        let paths = format!(
            "  <key>StandardOutPath</key><string>{}</string>\n  <key>StandardErrorPath</key><string>{}</string>\n",
            xml(utf8_path(stdout_path)?)?,
            xml(utf8_path(stderr_path)?)?,
        );
        Ok(rendered.replacen(
            "</dict>\n</plist>\n",
            &format!("{paths}</dict>\n</plist>\n"),
            1,
        ))
    }

    /// Reverse journal entries are kept in their native order, including multiline entries.
    pub fn journal_plan(&self, id: &str, lines: usize) -> Result<CommandSpec> {
        validate_id(id)?;
        if *self != Self::Linux {
            return Err(Error::new(
                "unsupported_capability",
                "journal queries are Linux-only",
            ));
        }
        let probe_lines = lines
            .checked_add(1)
            .ok_or_else(|| Error::new("invalid_limit", "log line limit is too large"))?;
        Ok(CommandSpec {
            program: "journalctl".into(),
            args: vec![
                "--user".into(),
                format!("--unit={}", self.native_name(id)),
                "--no-pager".into(),
                "--quiet".into(),
                "--output=cat".into(),
                "--reverse".into(),
                format!("--lines={probe_lines}"),
            ],
        })
    }

    /// Fixed hints interpret reported native evidence, never command acceptance.
    pub fn diagnostics(&self, status: &Status) -> Vec<Diagnostic> {
        let mut result = Vec::new();
        let mut add = |code, message| result.push(Diagnostic { code, message });
        match self {
            Self::Linux => {
                match status.raw.get("Result").map(String::as_str) {
                    Some("start-limit-hit") => add(
                        "start_limit",
                        "The native start limit was reached. Inspect logs and fix the repeated failure before explicitly retrying.",
                    ),
                    Some("timeout") => add(
                        "native_timeout",
                        "The native manager reported a timeout. Inspect bounded logs and the service's startup or shutdown behavior.",
                    ),
                    Some("signal" | "core-dump") => add(
                        "native_signal",
                        "The native manager reported signal termination. Inspect the raw native status and bounded logs; this may describe an earlier run.",
                    ),
                    Some("exit-code") => add(
                        "native_exit",
                        "The native manager reported a nonzero exit. Inspect the exact raw exit status and bounded logs; this may describe an earlier run.",
                    ),
                    _ => {}
                }
                if status
                    .raw
                    .get("ExecMainCode")
                    .and_then(|v| v.parse::<i32>().ok())
                    == Some(1)
                {
                    // Upstream systemd.exec documents 200=CHDIR and 203=EXEC.
                    // These hints are conditional: applications may use the same numbers.
                    match status
                        .raw
                        .get("ExecMainStatus")
                        .and_then(|v| v.parse::<i32>().ok())
                    {
                        Some(200) => add(
                            "working_directory",
                            "Native exit status 200 can indicate a working-directory setup failure. Check that the configured directory exists and is accessible.",
                        ),
                        Some(203) => add(
                            "executable",
                            "Native exit status 203 can indicate an execution setup failure. Check the executable, its permissions, and any script interpreter.",
                        ),
                        _ => {}
                    }
                }
            }
            Self::Macos => {
                if status.state == "spawn failed" {
                    add(
                        "spawn_failed",
                        "launchd reported a spawn failure. Check the executable, working directory, permissions, and bounded logs.",
                    );
                }
                if status
                    .raw
                    .get("last exit code")
                    .and_then(|v| launchd_exit_code(v))
                    .is_some_and(|code| code != 0)
                {
                    add(
                        "native_exit",
                        "The most recently reported launchd exit code was nonzero. Inspect its exact raw value and bounded logs; this does not establish the current process outcome.",
                    );
                }
                if status
                    .raw
                    .get("last terminating signal")
                    .and_then(|v| launchd_signal_number(v))
                    .is_some_and(|signal| signal > 0)
                {
                    add(
                        "native_signal",
                        "launchd reported a previous terminating signal. Inspect its exact raw value and bounded logs; this does not establish the current process outcome.",
                    );
                }
            }
        }
        if status.enabled == Some(false) {
            add(
                "activation_disabled",
                "Future login activation is disabled. This preference is separate from whether a process is currently running.",
            );
        }
        result
    }

    pub fn reload_plan(&self) -> Vec<CommandSpec> {
        match self {
            Self::Linux => vec![systemctl(&["daemon-reload"])],
            Self::Macos => vec![],
        }
    }

    /// Disable a previously unused label before publishing its LaunchAgent.
    /// The caller must first establish that the label is not currently loaded.
    pub fn add_guard_plan(&self, id: &str, session: &Session) -> Result<Vec<CommandSpec>> {
        validate_id(id)?;
        self.validate_session(session)?;
        match self {
            Self::Linux => Ok(vec![]),
            Self::Macos => {
                let uid = session.uid.ok_or_else(|| {
                    Error::new("session_unavailable", "a macOS GUI user id is required")
                })?;
                Ok(vec![launchctl(&[
                    "disable",
                    &format!("gui/{uid}/{}", self.label(id)),
                ])])
            }
        }
    }

    /// Pure planning. No files are read and no manager commands are invoked.
    pub fn command_plan(
        &self,
        action: Action,
        id: &str,
        definition: &Path,
    ) -> Result<Vec<CommandSpec>> {
        self.plan_with_context(
            action,
            id,
            definition,
            &RuntimeContext {
                uid: effective_uid(),
                loaded: None,
            },
        )
    }

    pub fn command_plan_with_status(
        &self,
        action: Action,
        id: &str,
        definition: &Path,
        session: &Session,
        status: &Status,
    ) -> Result<Vec<CommandSpec>> {
        self.validate_session(session)?;
        if *self == Self::Macos && matches!(action, Action::Start | Action::Restart) {
            match status.enabled {
                Some(true) => {}
                Some(false) => {
                    return Err(Error::new(
                        "disabled_service",
                        "launchd service is persistently disabled; enable it before starting",
                    ));
                }
                None => {
                    return Err(Error::new(
                        "status_required",
                        "launchd enablement must be observed before starting",
                    ));
                }
            }
        }
        self.plan_with_context(
            action,
            id,
            definition,
            &RuntimeContext {
                uid: session.uid,
                loaded: status.loaded,
            },
        )
    }

    pub fn plan_with_context(
        &self,
        action: Action,
        id: &str,
        definition: &Path,
        context: &RuntimeContext,
    ) -> Result<Vec<CommandSpec>> {
        validate_id(id)?;
        match self {
            Self::Linux => {
                let mut commands = self.reload_plan();
                let verb = match action {
                    Action::Start => "start",
                    Action::Stop => "stop",
                    Action::Restart => "restart",
                    Action::Enable => "enable",
                    Action::Disable => "disable",
                };
                commands.push(systemctl(&[verb, "--", &self.native_name(id)]));
                Ok(commands)
            }
            Self::Macos => {
                let uid = context.uid.ok_or_else(|| {
                    Error::new("session_unavailable", "a macOS GUI user id is required")
                })?;
                let domain = format!("gui/{uid}");
                let target = format!("{domain}/{}", self.label(id));
                if action == Action::Enable {
                    return Ok(vec![launchctl(&["enable", &target])]);
                }
                let loaded = context.loaded.ok_or_else(|| {
                    Error::new(
                        "status_required",
                        "launchd loaded state must be observed before planning this action",
                    )
                })?;
                if action == Action::Disable {
                    if loaded {
                        return Err(Error::new(
                            "stop_first",
                            "stop the launchd job before disabling login persistence; live disable semantics are not guaranteed",
                        ));
                    }
                    return Ok(vec![launchctl(&["disable", &target])]);
                }
                let bootstrap = || -> Result<CommandSpec> {
                    if !definition.is_absolute() {
                        return Err(Error::new(
                            "invalid_path",
                            "launchd definition path must be absolute",
                        ));
                    }
                    Ok(launchctl(&["bootstrap", &domain, utf8_path(definition)?]))
                };
                match action {
                    Action::Start if loaded => Ok(vec![launchctl(&["kickstart", &target])]),
                    Action::Start => Ok(vec![bootstrap()?]),
                    Action::Stop if loaded => Ok(vec![launchctl(&["bootout", &target])]),
                    Action::Stop => Ok(vec![]),
                    Action::Restart if loaded => {
                        Ok(vec![launchctl(&["bootout", &target]), bootstrap()?])
                    }
                    Action::Restart => Ok(vec![bootstrap()?]),
                    Action::Enable | Action::Disable => unreachable!(),
                }
            }
        }
    }

    /// Validate teardown observations before accepting a destructive bootout.
    pub fn validate_bootout_observation(&self, status: &Status) -> Result<()> {
        if *self == Self::Macos {
            observed_launchd_pid(status, false)?;
        }
        Ok(())
    }

    /// A bootout acknowledgement need not mean the old process has exited.
    /// Wait using only exact-label queries and signal-0 PID probes. Unknown
    /// state or changed ownership never authorizes another bootstrap.
    pub fn wait_for_bootout(
        &self,
        id: &str,
        definition: &Path,
        session: &Session,
        before: &Status,
        runner: &mut dyn Runner,
        observer: &mut dyn TeardownObserver,
    ) -> Result<Status> {
        if *self != Self::Macos {
            return Err(Error::new(
                "unsupported_capability",
                "bootout synchronization is macOS-only",
            ));
        }
        let mut pids = BTreeSet::new();
        if let Some(pid) = observed_launchd_pid(before, false)? {
            pids.insert(pid);
        }
        let mut settled = false;
        for attempt in 0..TEARDOWN_OBSERVATIONS {
            let status = self.status(id, session, runner)?;
            self.verify_managed_status(id, definition, &status)?;
            if let Some(pid) = observed_launchd_pid(&status, true)? {
                pids.insert(pid);
            }
            let mut process_alive = false;
            for pid in &pids {
                process_alive |= observer.process_exists(*pid)?;
            }
            let absent =
                status.loaded == Some(false) && status.running == Some(false) && !process_alive;
            if absent && settled {
                return Ok(status);
            }
            settled = absent;
            if attempt + 1 < TEARDOWN_OBSERVATIONS {
                observer.pause();
            }
        }
        Err(Error::new(
            "teardown_timeout",
            "bootout was accepted, but the job and observed prior PIDs did not settle as absent within 100 observations; no bootstrap was attempted",
        ))
    }

    /// Read-only session query; no assumption that a missing bus means absent job.
    pub fn session_check(&self, runner: &mut dyn Runner) -> Result<Session> {
        self.check_session(effective_uid(), runner)
    }

    pub fn session_check_with_context(&self, uid: u32, runner: &mut dyn Runner) -> Result<Session> {
        self.check_session(Some(uid), runner)
    }

    fn check_session(&self, uid: Option<u32>, runner: &mut dyn Runner) -> Result<Session> {
        let command = match self {
            Self::Linux => systemctl(&["show", "--property=Version", "--no-pager"]),
            Self::Macos => launchctl(&[
                "print",
                &format!(
                    "gui/{}",
                    uid.ok_or_else(|| Error::new(
                        "session_unavailable",
                        "a macOS GUI user id is required"
                    ))?
                ),
            ]),
        };
        let output = runner.run(&command)?;
        require_success("session_unavailable", &command, &output)?;
        if output.stdout.trim().is_empty()
            || (*self == Self::Linux
                && !output
                    .stdout
                    .lines()
                    .any(|line| line.starts_with("Version=") && line.len() > 8))
        {
            return Err(Error::new(
                "session_unavailable",
                "native manager returned no recognizable session information",
            ));
        }
        Ok(Session {
            platform: *self,
            uid,
        })
    }

    pub fn status(&self, id: &str, session: &Session, runner: &mut dyn Runner) -> Result<Status> {
        validate_id(id)?;
        self.validate_session(session)?;
        match self {
            Self::Linux => {
                let command = systemctl(&[
                    "show",
                    "--no-pager",
                    "--property=LoadState,ActiveState,SubState,UnitFileState,MainPID,ExecMainCode,ExecMainStatus,Result,FragmentPath,DropInPaths",
                    "--",
                    &self.native_name(id),
                ]);
                let output = runner.run(&command)?;
                parse_systemd_status(&command, &output)
            }
            Self::Macos => {
                let uid = session.uid.ok_or_else(|| {
                    Error::new("session_unavailable", "a macOS GUI user id is required")
                })?;
                let command = launchctl(&["print", &format!("gui/{uid}/{}", self.label(id))]);
                let output = runner.run(&command)?;
                let mut status = parse_launchd_status(&self.label(id), &command, &output)?;
                let disabled_command = launchctl(&["print-disabled", &format!("gui/{uid}")]);
                let disabled_output = runner.run(&disabled_command)?;
                require_success("status_unavailable", &disabled_command, &disabled_output)?;
                let disabled_override =
                    parse_disabled_override(&self.label(id), &disabled_output.stdout)?;
                // Our definitions contain Disabled=true. A native override takes precedence.
                status.enabled = Some(!disabled_override.unwrap_or(true));
                status.raw.insert(
                    "disabled_override".into(),
                    disabled_override
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "absent".into()),
                );
                status
                    .raw
                    .insert("definition_default_disabled".into(), "true".into());
                Ok(status)
            }
        }
    }

    pub fn status_with_context(
        &self,
        id: &str,
        uid: u32,
        runner: &mut dyn Runner,
    ) -> Result<Status> {
        self.status(
            id,
            &Session {
                platform: *self,
                uid: Some(uid),
            },
            runner,
        )
    }

    fn validate_session(&self, session: &Session) -> Result<()> {
        if session.platform != *self {
            return Err(Error::new(
                "invalid_session",
                "session belongs to a different native backend",
            ));
        }
        Ok(())
    }

    /// Refuse native definitions or drop-ins that are not the owned exact file.
    /// Query after daemon-reload on Linux to avoid relying on stale resolution.
    pub fn verify_managed_status(
        &self,
        id: &str,
        definition: &Path,
        status: &Status,
    ) -> Result<()> {
        validate_id(id)?;
        let expected = utf8_path(definition)?;
        if *self == Self::Linux && status.raw.get("DropInPaths").is_some_and(|s| !s.is_empty()) {
            return Err(Error::new(
                "native_conflict",
                "systemd drop-ins are present; loboot will not mutate an overridden service",
            ));
        }
        match status.loaded {
            Some(false) => Ok(()),
            None => Err(Error::new(
                "status_required",
                "native loaded state is unknown",
            )),
            Some(true) => {
                let key = match self {
                    Self::Linux => "FragmentPath",
                    Self::Macos => "path",
                };
                if status.raw.get(key).map(String::as_str) != Some(expected) {
                    return Err(Error::new(
                        "native_conflict",
                        format!(
                            "native definition path is not the owned file {}",
                            definition.display()
                        ),
                    ));
                }
                if *self == Self::Linux && !status.raw.contains_key("DropInPaths") {
                    return Err(Error::new(
                        "status_required",
                        "systemd returned no drop-in information",
                    ));
                }
                Ok(())
            }
        }
    }
}

fn validate_id(id: &str) -> Result<()> {
    crate::model::validate_id(id)
}

fn utf8_path(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        Error::new(
            "invalid_path",
            "native definition paths must be valid UTF-8",
        )
    })
}

fn systemctl(args: &[&str]) -> CommandSpec {
    let mut result = vec!["--user".into()];
    result.extend(args.iter().map(|s| (*s).into()));
    CommandSpec {
        program: "systemctl".into(),
        args: result,
    }
}

fn launchctl(args: &[&str]) -> CommandSpec {
    CommandSpec {
        program: "/bin/launchctl".into(),
        args: args.iter().map(|s| (*s).into()).collect(),
    }
}

/// Native C escapes and specifiers for argv and whole environment assignments.
/// ExecStart's command prefix disables dollar expansion; Environment= is literal.
fn systemd_quote(value: &str) -> String {
    let mut result = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => result.push_str("\\\\"),
            '"' => result.push_str("\\\""),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            '%' => result.push_str("%%"),
            c if c.is_ascii_control() => result.push_str(&format!("\\x{:02x}", c as u32)),
            c => result.push(c),
        }
    }
    result.push('"');
    result
}

fn systemd_directory(value: &str) -> String {
    // WorkingDirectory is parsed as a raw path, unlike ExecStart argv. A final
    // slash protects literal trailing whitespace and backslashes from the INI
    // reader's trimming/line-continuation handling while naming the same dir.
    let mut result = value.replace('%', "%%");
    if !result.ends_with('/') {
        result.push('/');
    }
    result
}

fn xml(value: &str) -> Result<String> {
    let mut result = String::new();
    for ch in value.chars() {
        match ch {
            '&' => result.push_str("&amp;"),
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            '"' => result.push_str("&quot;"),
            '\'' => result.push_str("&apos;"),
            '\r' => result.push_str("&#13;"),
            c if c < ' ' && c != '\n' && c != '\t' => {
                return Err(Error::new(
                    "invalid_manifest",
                    "launchd XML cannot represent this control character",
                ));
            }
            '\u{fffe}' | '\u{ffff}' => {
                return Err(Error::new(
                    "invalid_manifest",
                    "launchd XML cannot represent this Unicode character",
                ));
            }
            c => result.push(c),
        }
    }
    Ok(result)
}

fn require_complete(code: &'static str, output: &CommandOutput) -> Result<()> {
    if output.is_complete() {
        return Ok(());
    }
    Err(Error::new(code, output.failure_summary()))
}

fn require_success(
    code: &'static str,
    _command: &CommandSpec,
    output: &CommandOutput,
) -> Result<()> {
    require_complete(code, output)?;
    if output.success {
        return Ok(());
    }
    Err(Error::new(code, output.failure_summary()))
}

// launchctl prints decorated exit/signal values, not only decimal integers.
// Keep the exact bounded scalar in raw output; extract a number only for hints.
// `(never exited)` is an observation of no previous exit, never exit code zero.
fn launchd_exit_code(value: &str) -> Option<i32> {
    if value.len() > 128 {
        return None;
    }
    let number = if let Some((number, name)) = value.split_once(": ") {
        if !name.starts_with("EX_")
            || name.len() <= 3
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte == b'_')
        {
            return None;
        }
        number
    } else {
        value
    };
    if number.is_empty()
        || !number
            .bytes()
            .enumerate()
            .all(|(index, byte)| byte.is_ascii_digit() || (index == 0 && byte == b'-'))
    {
        return None;
    }
    number.parse().ok()
}

fn launchd_signal_number(value: &str) -> Option<u32> {
    if value.len() > 128 {
        return None;
    }
    let number = if let Some((name, number)) = value.split_once(": ") {
        if name.is_empty()
            || name.trim() != name
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b" /-()".contains(&byte))
        {
            return None;
        }
        number
    } else {
        value
    };
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    number.parse().ok()
}

// Only bounded native scalars are retained. Unrequested environment, argv,
// arbitrary diagnostics, and unknown fields never enter user-visible raw data.
fn insert_scalar(
    raw: &mut BTreeMap<String, String>,
    key: &str,
    value: &str,
    platform: Platform,
) -> Result<()> {
    let numeric = match platform {
        Platform::Linux => matches!(key, "MainPID" | "ExecMainCode" | "ExecMainStatus"),
        Platform::Macos => matches!(key, "pid" | "runs"),
    };
    let path = matches!(key, "FragmentPath" | "DropInPaths" | "path" | "program");
    let valid = if platform == Platform::Macos && key == "last exit code" {
        value == "(never exited)" || launchd_exit_code(value).is_some()
    } else if platform == Platform::Macos && key == "last terminating signal" {
        launchd_signal_number(value).is_some()
    } else if numeric {
        value.len() <= 20
            && !value.is_empty()
            && if key == "ExecMainStatus" {
                value.parse::<i32>().is_ok()
                    && value
                        .bytes()
                        .enumerate()
                        .all(|(index, byte)| byte.is_ascii_digit() || (index == 0 && byte == b'-'))
            } else {
                value.parse::<u32>().is_ok() && value.bytes().all(|byte| byte.is_ascii_digit())
            }
    } else if path {
        value.len() <= 4096 && !value.chars().any(char::is_control)
    } else {
        value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b' '))
    };
    if !valid {
        return Err(Error::new(
            "status_unavailable",
            "native manager returned a malformed or oversized recognized status field",
        ));
    }
    if raw.insert(key.to_string(), value.to_string()).is_some() {
        return Err(Error::new(
            "status_unavailable",
            "native manager returned duplicate status fields",
        ));
    }
    Ok(())
}

fn parse_systemd_status(command: &CommandSpec, output: &CommandOutput) -> Result<Status> {
    require_complete("status_unavailable", output)?;
    let mut raw = BTreeMap::new();
    for line in output.stdout.lines().filter(|line| !line.is_empty()) {
        let (key, value) = line.split_once('=').ok_or_else(|| {
            Error::new(
                "status_unavailable",
                "systemd returned a malformed status line",
            )
        })?;
        if matches!(
            key,
            "LoadState"
                | "ActiveState"
                | "SubState"
                | "UnitFileState"
                | "MainPID"
                | "ExecMainCode"
                | "ExecMainStatus"
                | "Result"
                | "FragmentPath"
                | "DropInPaths"
        ) {
            insert_scalar(&mut raw, key, value, Platform::Linux)?;
        }
    }
    let missing = raw.get("LoadState").is_some_and(|s| s == "not-found");
    if !output.success
        && !(matches!(output.code, Some(1) | Some(4))
            && output.signal.is_none()
            && missing
            && raw.get("ActiveState").is_some_and(|s| s == "inactive"))
    {
        require_success("status_unavailable", command, output)?;
    }
    let load = raw
        .get("LoadState")
        .ok_or_else(|| Error::new("status_unavailable", "systemd returned no LoadState"))?;
    let active = raw
        .get("ActiveState")
        .ok_or_else(|| Error::new("status_unavailable", "systemd returned no ActiveState"))?;
    let loaded = match load.as_str() {
        "loaded" | "error" | "bad-setting" | "masked" | "merged" => Some(true),
        "not-found" => Some(false),
        "stub" => None,
        _ => None,
    };
    let running = match active.as_str() {
        "active" => raw
            .get("MainPID")
            .and_then(|p| p.parse::<u32>().ok())
            .map(|pid| pid != 0),
        "inactive" | "failed" => Some(false),
        _ => None,
    };
    let enabled = match raw.get("UnitFileState").map(String::as_str) {
        Some("enabled") => Some(true),
        Some(
            "disabled" | "static" | "indirect" | "masked" | "linked" | "generated" | "transient",
        ) => Some(false),
        _ => None,
    };
    let state = format!(
        "{active}/{}",
        raw.get("SubState").map(String::as_str).unwrap_or("unknown")
    );
    Ok(Status {
        loaded,
        running,
        enabled,
        state,
        raw,
    })
}

fn parse_launchd_status(
    label: &str,
    command: &CommandSpec,
    output: &CommandOutput,
) -> Result<Status> {
    require_complete("status_unavailable", output)?;
    if !output.success {
        // ENOENT-like print results vary across OS releases. Only a matching
        // service diagnostic is accepted; arbitrary stderr is never returned.
        let missing_label = format!("Could not find service \"{label}\" in domain");
        if matches!(output.code, Some(113) | Some(3))
            && output.signal.is_none()
            && output
                .stderr
                .lines()
                .any(|line| line.trim().starts_with(&missing_label))
        {
            return Ok(Status {
                loaded: Some(false),
                running: Some(false),
                enabled: None,
                state: "unloaded".into(),
                raw: BTreeMap::from([
                    ("diagnostic".into(), "service-not-found".into()),
                    ("query_exit_code".into(), output.code.unwrap().to_string()),
                ]),
            });
        }
        require_success("status_unavailable", command, output)?;
    }
    // launchctl print is diagnostic text, not a stable machine interface.
    // Require a complete root object, and retain only recognized top-level
    // scalar fields. Nested environment values are deliberately discarded.
    let mut raw = BTreeMap::new();
    let mut depth = 0usize;
    let mut started = false;
    let mut closed = false;
    for line in output
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if closed {
            return Err(Error::new(
                "status_unavailable",
                "unexpected data after launchctl status object",
            ));
        }
        if line == "}" || line == "};" {
            if depth == 0 {
                return Err(Error::new(
                    "status_unavailable",
                    "malformed launchctl status object",
                ));
            }
            depth -= 1;
            closed = depth == 0;
            continue;
        }
        if let Some((key, value)) = line.split_once(" = ") {
            if !started {
                if value != "{" || !key.ends_with(&format!("/{label}")) {
                    return Err(Error::new(
                        "status_unavailable",
                        "launchctl returned no matching job status object",
                    ));
                }
                started = true;
            } else if depth == 1
                && matches!(
                    key,
                    "state"
                        | "pid"
                        | "runs"
                        | "last exit code"
                        | "last terminating signal"
                        | "path"
                        | "program"
                )
            {
                insert_scalar(&mut raw, key, value, Platform::Macos)?;
            }
            if value == "{" {
                depth += 1;
            }
        } else if !started {
            return Err(Error::new(
                "status_unavailable",
                "launchctl returned no recognizable job status object",
            ));
        }
    }
    if !started || !closed {
        return Err(Error::new(
            "status_unavailable",
            "truncated launchctl status object",
        ));
    }
    let state = raw.get("state").cloned().ok_or_else(|| {
        Error::new(
            "status_unavailable",
            "launchctl returned no recognizable job state",
        )
    })?;
    let running = match state.as_str() {
        "running" => Some(true),
        "not running" | "exited" | "waiting" | "spawn scheduled" | "spawn failed" => Some(false),
        _ => None,
    };
    Ok(Status {
        loaded: Some(true),
        running,
        enabled: None,
        state,
        raw,
    })
}

fn parse_disabled_override(label: &str, stdout: &str) -> Result<Option<bool>> {
    let mut lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    if lines.next() != Some("disabled services = {") {
        return Err(Error::new(
            "status_unavailable",
            "launchctl returned no recognizable disabled-services map",
        ));
    }
    let expected = format!("\"{label}\"");
    let mut result = None;
    let mut closed = false;
    for line in lines {
        if line == "}" {
            closed = true;
            continue;
        }
        if closed {
            return Err(Error::new(
                "status_unavailable",
                "unexpected data after disabled-services map",
            ));
        }
        let (key, value) = line.split_once(" => ").ok_or_else(|| {
            Error::new(
                "status_unavailable",
                "unrecognized launchctl disabled-services entry",
            )
        })?;
        if key == expected {
            // Monterey and older report booleans; Ventura and newer report
            // named states. Accept only these exact spellings (plus one optional
            // diagnostic separator), never a prefix or an unknown future state.
            let disabled = match value.strip_suffix(';').unwrap_or(value) {
                "true" | "disabled" => true,
                "false" | "enabled" => false,
                _ => {
                    return Err(Error::new(
                        "status_unavailable",
                        "unrecognized launchctl disabled value",
                    ));
                }
            };
            if result.replace(disabled).is_some() {
                return Err(Error::new(
                    "status_unavailable",
                    "duplicate launchctl disabled override",
                ));
            }
        }
    }
    if !closed {
        return Err(Error::new(
            "status_unavailable",
            "truncated launchctl disabled-services map",
        ));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct FakeRunner {
        outputs: VecDeque<CommandOutput>,
        seen: Vec<CommandSpec>,
    }
    impl Runner for FakeRunner {
        fn run(&mut self, command: &CommandSpec) -> Result<CommandOutput> {
            self.seen.push(command.clone());
            self.outputs
                .pop_front()
                .ok_or_else(|| Error::new("fake_exhausted", "unexpected native invocation"))
        }
    }
    fn output(success: bool, code: i32, stdout: &str, stderr: &str) -> CommandOutput {
        CommandOutput {
            success,
            code: Some(code),
            stdout: stdout.into(),
            stderr: stderr.into(),
            ..Default::default()
        }
    }
    #[test]
    fn incomplete_native_capture_and_unknown_fields_do_not_leak() {
        let command = systemctl(&["show"]);
        let mut out = output(
            true,
            0,
            "LoadState=loaded\nActiveState=failed\nUnitFileState=disabled\nEnvironment=secret-token\nExecMainCode=1\nExecMainStatus=203\nResult=exit-code\n",
            "",
        );
        let status = parse_systemd_status(&command, &out).unwrap();
        assert!(!status.raw.contains_key("Environment"));
        let hints = Platform::Linux.diagnostics(&status);
        assert!(hints.iter().any(|hint| hint.code == "executable"));
        out.stdout_truncated = true;
        assert!(parse_systemd_status(&command, &out).is_err());
        out.stdout_truncated = false;
        out.timed_out = true;
        assert!(parse_systemd_status(&command, &out).is_err());
        let failed = output(false, 1, "", "password=must-not-leak Permission denied");
        assert!(
            !require_success("test", &command, &failed)
                .unwrap_err()
                .message
                .contains("must-not-leak")
        );
    }
    #[test]
    fn duplicate_malformed_and_partial_status_objects_are_refused() {
        let command = systemctl(&["show"]);
        for stdout in [
            "LoadState=loaded\nLoadState=not-found\n",
            "MainPID=secret\n",
            "broken line",
        ] {
            assert!(parse_systemd_status(&command, &output(true, 0, stdout, "")).is_err());
        }
        assert!(
            parse_launchd_status(
                "com.loboot.x",
                &command,
                &output(true, 0, "gui/501/com.loboot.x = {\nstate = running\n", "")
            )
            .is_err()
        );
        let status = parse_systemd_status(
            &command,
            &output(
                true,
                0,
                "LoadState=not-found\nActiveState=inactive\nUnitFileState=\n",
                "",
            ),
        )
        .unwrap();
        assert_eq!(status.enabled, None);
    }
    fn manifest() -> Manifest {
        Manifest {
            id: "example".into(),
            executable: "/opt/my $tool%/run".into(),
            args: vec![
                "".into(),
                "a b".into(),
                "$HOME ${HOME} 100% \\ \" ; &".into(),
            ],
            cwd: "/tmp/a $b%".into(),
            mode: Mode::Once,
            restart: Restart::Never,
            environment: BTreeMap::new(),
        }
    }

    #[test]
    fn empty_environment_preserves_historical_native_bytes() {
        let mut m = manifest();
        m.executable = "/usr/bin/true".into();
        m.args.clear();
        m.cwd = "/tmp".into();
        let linux = concat!(
            "# Managed by loboot; do not edit.\n",
            "[Unit]\nDescription=loboot example\n\n",
            "[Service]\nType=oneshot\nExecStart=\":/usr/bin/true\"\n",
            "WorkingDirectory=/tmp/\nRestart=no\nRestartSec=2s\n\n",
            "[Install]\nWantedBy=default.target\n",
        );
        let macos = concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
            "\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
            "<!-- Managed by loboot; do not edit. -->\n",
            "<plist version=\"1.0\">\n<dict>\n",
            "  <key>Label</key><string>com.loboot.example</string>\n",
            "  <key>ProgramArguments</key>\n  <array>\n",
            "    <string>/usr/bin/true</string>\n  </array>\n",
            "  <key>WorkingDirectory</key><string>/tmp</string>\n",
            "  <key>Disabled</key><true/>\n",
            "  <key>RunAtLoad</key><true/>\n",
            "  <key>KeepAlive</key><false/>\n",
            "  <key>ThrottleInterval</key><integer>2</integer>\n",
            "</dict>\n</plist>\n",
        );
        for (platform, expected) in [(Platform::Linux, linux), (Platform::Macos, macos)] {
            assert_eq!(platform.render(&m).unwrap(), expected);
            assert_eq!(platform.render_redacted(&m).unwrap(), expected);
            let stdout = Path::new("/private/logs/stdout");
            let stderr = Path::new("/private/logs/stderr");
            assert_eq!(
                platform.render_with_logs(&m, stdout, stderr).unwrap(),
                platform
                    .render_with_logs_redacted(&m, stdout, stderr)
                    .unwrap()
            );
        }
    }

    #[test]
    fn systemd_environment_is_sorted_quoted_and_literal() {
        let mut m = manifest();
        m.environment.insert("Z".into(), "last".into());
        m.environment.insert(
            "LITERAL".into(),
            r#" $HOME ${HOME} $(touch /tmp/not-executed) `id` 100% \ " ' = ; & < > "#.into(),
        );
        m.environment.insert("A".into(), String::new());
        m.environment
            .insert("PATH".into(), "/opt/loboot-test/bin:/usr/bin".into());
        let rendered = Platform::Linux.render(&m).unwrap();
        let environment = rendered
            .lines()
            .filter(|line| line.starts_with("Environment="))
            .collect::<Vec<_>>();
        assert_eq!(
            environment,
            vec![r#"Environment="A=" "LITERAL= $HOME ${HOME} $(touch /tmp/not-executed) `id` 100%% \\ \" ' = ; & < > " "PATH=/opt/loboot-test/bin:/usr/bin" "Z=last""#]
        );
        assert!(rendered.contains("ExecStart=\":/opt/my $tool%%/run\""));
        assert!(!rendered.contains("EnvironmentFile="));
        assert!(!rendered.contains("PassEnvironment="));
    }

    #[test]
    fn launchd_environment_is_a_sorted_xml_string_dictionary() {
        let mut m = manifest();
        m.environment.insert("Z".into(), "last".into());
        m.environment.insert("A".into(), String::new());
        m.environment.insert(
            "LITERAL".into(),
            r#"<key>Injected</key>&"' $HOME ${HOME} 100% \ "#.into(),
        );
        let rendered = Platform::Macos.render(&m).unwrap();
        assert!(rendered.contains(concat!(
            "  <key>EnvironmentVariables</key>\n  <dict>\n",
            "    <key>A</key><string></string>\n",
            "    <key>LITERAL</key><string>&lt;key&gt;Injected&lt;/key&gt;&amp;&quot;&apos; $HOME ${HOME} 100% \\ </string>\n",
            "    <key>Z</key><string>last</string>\n  </dict>\n",
        )));
        assert_eq!(rendered.matches("<key>EnvironmentVariables</key>").count(), 1);
        assert!(!rendered.contains("<key>Injected</key>"));
        assert!(!rendered.contains("<key>PATH</key>"));
    }

    #[test]
    fn environment_display_redacts_every_value_without_changing_raw_rendering() {
        let mut m = manifest();
        m.environment.insert("EMPTY".into(), String::new());
        m.environment.insert(
            "PATH".into(),
            "/private-original-path/bin:/usr/bin".into(),
        );
        m.environment.insert(
            "TOKEN".into(),
            "private-original-value $HOME %i <tag>".into(),
        );
        let original = m.clone();
        let stdout = Path::new("/private/logs/stdout");
        let stderr = Path::new("/private/logs/stderr");
        for platform in [Platform::Linux, Platform::Macos] {
            let raw = platform.render(&m).unwrap();
            for display in [
                platform.render_redacted(&m).unwrap(),
                platform
                    .render_with_logs_redacted(&m, stdout, stderr)
                    .unwrap(),
            ] {
                assert_eq!(display.matches("[REDACTED]").count(), 3);
                assert!(
                    display.contains("Environment values redacted; display only, not replayable.")
                );
                assert!(!display.contains("private-original-value"));
                assert!(!display.contains("private-original-path"));
            }
            assert!(raw.contains("private-original-value"));
            assert!(raw.contains("private-original-path"));
            assert!(!raw.contains("display only, not replayable"));
            assert_eq!(platform.render(&m).unwrap(), raw);
            let raw_logs = platform.render_with_logs(&m, stdout, stderr).unwrap();
            assert!(raw_logs.contains("private-original-value"));
            if platform == Platform::Macos {
                let display = platform
                    .render_with_logs_redacted(&m, stdout, stderr)
                    .unwrap();
                assert!(display.contains(
                    "  <key>StandardOutPath</key><string>/private/logs/stdout</string>\n  <key>StandardErrorPath</key><string>/private/logs/stderr</string>\n</dict>\n</plist>\n"
                ));
            }
        }
        assert_eq!(m, original);
    }

    #[test]
    fn raw_and_redacted_rendering_refuse_invalid_original_environment() {
        for (name, value) in [
            ("BAD=NAME", "private-render-value"),
            ("VALUE", "private-render-value\nRestart=always"),
            ("VALUE", "private-render-value\u{1ffff}"),
            ("PATH", "private-render-value"),
            ("PATH", "/private-render-value::/usr/bin"),
        ] {
            let mut m = manifest();
            m.environment.insert(name.into(), value.into());
            for platform in [Platform::Linux, Platform::Macos] {
                for result in [
                    platform.render(&m),
                    platform.render_redacted(&m),
                    platform.render_with_logs(&m, Path::new("/logs/out"), Path::new("/logs/err")),
                    platform.render_with_logs_redacted(
                        &m,
                        Path::new("/logs/out"),
                        Path::new("/logs/err"),
                    ),
                ] {
                    let error = result.unwrap_err();
                    assert_eq!(error.code, "invalid_environment");
                    assert!(!error.to_string().contains("private-render-value"));
                }
            }
        }
    }

    #[test]
    fn systemd_preserves_argv_and_literal_specifiers() {
        let rendered = Platform::Linux.render(&manifest()).unwrap();
        assert!(rendered.contains("Type=oneshot\n"));
        assert!(rendered.contains(
            "ExecStart=\":/opt/my $tool%%/run\" \"\" \"a b\" \"$HOME ${HOME} 100%% \\\\ \\\" ; &\""
        ));
        assert!(rendered.contains("WorkingDirectory=/tmp/a $b%%/"));
        assert!(!rendered.contains("RemainAfterExit"));
        assert!(!rendered.contains("/bin/sh"));
    }
    #[test]
    fn launchd_renders_xml_and_once_without_keepalive() {
        let mut m = manifest();
        m.args.push("<tag>&\"'".into());
        let rendered = Platform::Macos.render(&m).unwrap();
        assert!(rendered.contains("<string>&lt;tag&gt;&amp;&quot;&apos;</string>"));
        assert!(rendered.contains("<key>KeepAlive</key><false/>"));
        assert!(rendered.contains("<key>RunAtLoad</key><true/>"));
        assert!(rendered.contains("<string></string>"));
    }
    #[test]
    fn service_restart_policy_rendering() {
        for (restart, linux, mac) in [
            (Restart::Never, "Restart=no", "<key>KeepAlive</key><false/>"),
            (
                Restart::OnFailure,
                "Restart=on-failure",
                "<key>SuccessfulExit</key><false/>",
            ),
            (
                Restart::Always,
                "Restart=always",
                "<key>KeepAlive</key><true/>",
            ),
        ] {
            let mut m = manifest();
            m.mode = Mode::Service;
            m.restart = restart;
            assert!(Platform::Linux.render(&m).unwrap().contains(linux));
            assert!(Platform::Macos.render(&m).unwrap().contains(mac));
        }
    }
    #[test]
    fn render_rejects_once_restart_and_injection_ids() {
        let mut m = manifest();
        m.restart = Restart::Always;
        assert!(Platform::Linux.render(&m).is_err());
        assert!(Platform::Macos.render(&m).is_err());
        assert!(
            Platform::Linux
                .command_plan(Action::Start, "--system", Path::new("/tmp/u"))
                .is_err()
        );
        assert!(
            Platform::Linux
                .command_plan(Action::Start, "../x", Path::new("/tmp/u"))
                .is_err()
        );
    }
    #[test]
    fn linux_runtime_and_persistence_are_independent() {
        let p = Platform::Linux;
        let start = p
            .command_plan(Action::Start, "example", Path::new("/tmp/u"))
            .unwrap();
        assert_eq!(
            start,
            vec![
                systemctl(&["daemon-reload"]),
                systemctl(&["start", "--", "loboot-example.service"])
            ]
        );
        for (action, verb) in [
            (Action::Enable, "enable"),
            (Action::Disable, "disable"),
            (Action::Stop, "stop"),
        ] {
            assert_eq!(
                p.command_plan(action, "example", Path::new("/tmp/u"))
                    .unwrap(),
                vec![
                    systemctl(&["daemon-reload"]),
                    systemctl(&[verb, "--", "loboot-example.service"])
                ]
            );
        }
    }
    #[test]
    fn mac_start_never_double_launches_and_stop_unloads() {
        let p = Platform::Macos;
        let path = Path::new("/tmp/job.plist");
        let unloaded = RuntimeContext {
            uid: Some(501),
            loaded: Some(false),
        };
        let loaded = RuntimeContext {
            loaded: Some(true),
            ..unloaded
        };
        assert_eq!(
            p.plan_with_context(Action::Start, "example", path, &unloaded)
                .unwrap(),
            vec![launchctl(&["bootstrap", "gui/501", "/tmp/job.plist"])]
        );
        assert_eq!(
            p.plan_with_context(Action::Start, "example", path, &loaded)
                .unwrap(),
            vec![launchctl(&["kickstart", "gui/501/com.loboot.example"])]
        );
        assert_eq!(
            p.plan_with_context(Action::Stop, "example", path, &loaded)
                .unwrap(),
            vec![launchctl(&["bootout", "gui/501/com.loboot.example"])]
        );
        assert!(
            p.plan_with_context(Action::Stop, "example", path, &unloaded)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn mac_restart_reloads_changed_configuration() {
        assert_eq!(
            Platform::Macos
                .plan_with_context(
                    Action::Restart,
                    "example",
                    Path::new("/tmp/u.plist"),
                    &RuntimeContext {
                        uid: Some(501),
                        loaded: Some(true)
                    }
                )
                .unwrap(),
            vec![
                launchctl(&["bootout", "gui/501/com.loboot.example"]),
                launchctl(&["bootstrap", "gui/501", "/tmp/u.plist"])
            ]
        );
    }
    #[test]
    fn mac_persistence_and_unobserved_runtime_fail_closed() {
        let p = Platform::Macos;
        let path = Path::new("/tmp/u");
        assert_eq!(
            p.plan_with_context(
                Action::Enable,
                "example",
                path,
                &RuntimeContext {
                    uid: Some(501),
                    loaded: None
                }
            )
            .unwrap(),
            vec![launchctl(&["enable", "gui/501/com.loboot.example"])]
        );
        assert_eq!(
            p.plan_with_context(
                Action::Disable,
                "example",
                path,
                &RuntimeContext {
                    uid: Some(501),
                    loaded: Some(false)
                }
            )
            .unwrap(),
            vec![launchctl(&["disable", "gui/501/com.loboot.example"])]
        );
        assert_eq!(
            p.plan_with_context(
                Action::Disable,
                "example",
                path,
                &RuntimeContext {
                    uid: Some(501),
                    loaded: Some(true)
                }
            )
            .unwrap_err()
            .code,
            "stop_first"
        );
        assert_eq!(
            p.command_plan(Action::Start, "example", path)
                .unwrap_err()
                .code,
            "status_required"
        );
        assert!(
            p.native_dir(Path::new("/home/me"))
                .ends_with("Library/LaunchAgents")
        );
        assert!(
            p.render(&manifest())
                .unwrap()
                .contains("<key>Disabled</key><true/>")
        );
    }

    #[test]
    fn missing_bus_is_not_missing_service() {
        let mut runner = FakeRunner {
            outputs: VecDeque::from([output(false, 1, "", "Failed to connect to bus")]),
            ..Default::default()
        };
        assert_eq!(
            Platform::Linux.session_check(&mut runner).unwrap_err().code,
            "session_unavailable"
        );
        assert_eq!(
            runner.seen,
            vec![systemctl(&["show", "--property=Version", "--no-pager"])]
        );
    }
    #[test]
    fn systemd_status_exposes_native_failure_without_inventing_process_outcome() {
        let mut runner = FakeRunner {
            outputs: VecDeque::from([output(
                true,
                0,
                "LoadState=loaded\nActiveState=failed\nSubState=failed\nUnitFileState=disabled\nMainPID=0\nExecMainCode=1\nExecMainStatus=42\nResult=exit-code\n",
                "",
            )]),
            ..Default::default()
        };
        let status = Platform::Linux
            .status_with_context("example", 1000, &mut runner)
            .unwrap();
        assert_eq!(status.running, Some(false));
        assert_eq!(status.enabled, Some(false));
        assert_eq!(status.raw["ExecMainStatus"], "42");
    }
    #[test]
    fn systemd_unknown_fields_stay_unknown() {
        let mut runner = FakeRunner {
            outputs: VecDeque::from([output(
                true,
                0,
                "LoadState=future-state\nActiveState=activating\nUnitFileState=enabled-runtime\n",
                "",
            )]),
            ..Default::default()
        };
        let status = Platform::Linux
            .status_with_context("example", 1000, &mut runner)
            .unwrap();
        assert_eq!(status.loaded, None);
        assert_eq!(status.running, None);
        assert_eq!(status.enabled, None);
    }
    #[test]
    fn launchd_absence_is_narrowly_recognized() {
        for (stderr, absent) in [
            (
                "Bad request.\nCould not find service \"com.loboot.example\" in domain for user gui: 501",
                true,
            ),
            ("Could not find domain for user gui: 501", false),
            ("Operation not permitted", false),
            (
                "Could not find service \"com.loboot.other\" in domain for user gui: 501",
                false,
            ),
        ] {
            let mut runner = FakeRunner {
                outputs: VecDeque::from([
                    output(false, 113, "", stderr),
                    output(true, 0, "disabled services = {\n}\n", ""),
                ]),
                ..Default::default()
            };
            let result = Platform::Macos.status_with_context("example", 501, &mut runner);
            if absent {
                assert_eq!(result.unwrap().loaded, Some(false));
            } else {
                assert_eq!(result.unwrap_err().code, "status_unavailable");
            }
        }
    }
    #[test]
    fn launchd_loaded_does_not_mean_running() {
        let mut runner = FakeRunner {
            outputs: VecDeque::from([
                output(
                    true,
                    0,
                    "gui/501/com.loboot.example = {\n\tstate = not running\n\truns = 1\n\tlast exit code = 0\n\tenvironment = {\n\t\tstate = running\n\t}\n}\n",
                    "",
                ),
                output(
                    true,
                    0,
                    "disabled services = {\n\t\"com.loboot.example\" => false\n}\n",
                    "",
                ),
            ]),
            ..Default::default()
        };
        let status = Platform::Macos
            .status_with_context("example", 501, &mut runner)
            .unwrap();
        assert_eq!(status.loaded, Some(true));
        assert_eq!(status.running, Some(false));
        assert_eq!(status.raw["last exit code"], "0");
    }
    #[test]
    fn launchd_decorated_exit_and_signal_fields_preserve_native_evidence() {
        // Primary project observations of launchctl's native spellings:
        // https://github.com/akitaonrails/ai-memory/blob/main/docs/macos.md
        // https://github.com/Homebrew/brew/issues/14924
        // https://github.com/openclaw/openclaw/issues/56122
        // Signal descriptions: apple-oss-distributions/Libc gen/FreeBSD/siglist.c.
        for (key, value, state, exit_hint, signal_hint) in [
            ("last exit code", "(never exited)", "running", false, false),
            ("last exit code", "0", "not running", false, false),
            ("last exit code", "23", "not running", true, false),
            (
                "last exit code",
                "78: EX_CONFIG",
                "not running",
                true,
                false,
            ),
            (
                "last exit code",
                "75: EX_TEMPFAIL",
                "not running",
                true,
                false,
            ),
            ("last terminating signal", "15", "not running", false, true),
            (
                "last terminating signal",
                "Terminated: 15",
                "not running",
                false,
                true,
            ),
            (
                "last terminating signal",
                "Killed: 9",
                "not running",
                false,
                true,
            ),
            (
                "last terminating signal",
                "Segmentation fault: 11",
                "not running",
                false,
                true,
            ),
            (
                "last terminating signal",
                "Trace/BPT trap: 5",
                "not running",
                false,
                true,
            ),
        ] {
            let mut runner = FakeRunner {
                outputs: VecDeque::from([
                    output(
                        true,
                        0,
                        &format!(
                            "gui/501/com.loboot.example = {{\nstate = {state}\npid = 42\n{key} = {value}\n}}\n"
                        ),
                        "",
                    ),
                    output(
                        true,
                        0,
                        "disabled services = {\n\"com.loboot.example\" => enabled\n}\n",
                        "",
                    ),
                ]),
                ..Default::default()
            };
            let status = Platform::Macos
                .status_with_context("example", 501, &mut runner)
                .unwrap();
            assert_eq!(status.raw[key], value);
            assert_eq!(status.running, Some(state == "running"));
            let hints = Platform::Macos.diagnostics(&status);
            assert_eq!(
                hints.iter().any(|hint| hint.code == "native_exit"),
                exit_hint,
                "{key} = {value}"
            );
            assert_eq!(
                hints.iter().any(|hint| hint.code == "native_signal"),
                signal_hint,
                "{key} = {value}"
            );
        }
    }
    #[test]
    fn launchd_decorated_status_is_still_bounded_and_validated() {
        for (key, value) in [
            ("last exit code", "password=private"),
            ("last exit code", "78: private"),
            ("last exit code", "78: EX_CONFIG extra"),
            ("last exit code", "+23"),
            ("last exit code", "2147483648"),
            ("last terminating signal", "Terminated: password=private"),
            ("last terminating signal", "Terminated: -15"),
            ("last terminating signal", "Terminated: 15 extra"),
            ("last terminating signal", "Terminated\u{1b}: 15"),
        ] {
            assert!(
                insert_scalar(&mut BTreeMap::new(), key, value, Platform::Macos).is_err(),
                "{key} = {value}"
            );
        }
        for (key, value) in [
            ("last exit code", format!("78: EX_{}", "X".repeat(128))),
            (
                "last terminating signal",
                format!("{}: 15", "X".repeat(128)),
            ),
        ] {
            assert!(insert_scalar(&mut BTreeMap::new(), key, &value, Platform::Macos).is_err());
        }
    }
    #[test]
    fn launchd_sigtermed_preserves_unknown_running_state() {
        let mut runner = FakeRunner {
            outputs: VecDeque::from([
                output(
                    true,
                    0,
                    "gui/501/com.loboot.example = {\nstate = SIGTERMed\npid = 42\n}\n",
                    "",
                ),
                output(
                    true,
                    0,
                    "disabled services = {\n\"com.loboot.example\" => enabled\n}\n",
                    "",
                ),
            ]),
            ..Default::default()
        };
        let status = Platform::Macos
            .status_with_context("example", 501, &mut runner)
            .unwrap();
        assert_eq!(status.loaded, Some(true));
        assert_eq!(status.running, None);
        assert_eq!(status.state, "SIGTERMed");
        assert_eq!(observed_launchd_pid(&status, true).unwrap(), Some(42));
        assert_eq!(
            observed_launchd_pid(&status, false).unwrap_err().code,
            "status_required"
        );
    }

    #[test]
    fn malformed_success_is_not_trusted() {
        for platform in [Platform::Linux, Platform::Macos] {
            let mut runner = FakeRunner {
                outputs: VecDeque::from([output(true, 0, "unexpected output", "")]),
                ..Default::default()
            };
            assert_eq!(
                platform
                    .status_with_context("example", 501, &mut runner)
                    .unwrap_err()
                    .code,
                "status_unavailable"
            );
        }
    }

    #[test]
    fn systemd_directory_is_raw_not_argv_quoted() {
        assert_eq!(systemd_directory("/tmp/a b\"$%\\"), "/tmp/a b\"$%%\\/");
        assert_eq!(systemd_directory("/tmp/trailing "), "/tmp/trailing /");
        assert_eq!(systemd_directory("/"), "/");
        assert_eq!(systemd_quote(";"), "\";\"");
        assert_eq!(systemd_quote("$ ${HOME}"), "\"$ ${HOME}\"");
        let mut m = manifest();
        m.args = vec![";".into(), "한글".into(), "\\".into()];
        let rendered = Platform::Linux.render(&m).unwrap();
        assert!(rendered.contains("\";\" \"한글\" \"\\\\\""));
    }

    #[test]
    fn unsupported_systemd_executable_names_fail_before_installation() {
        for executable in [
            "/tmp/a'b",
            "/tmp/a\"b",
            "/tmp/a\\b",
            "/tmp/a*b",
            "/tmp/a?b",
            "/tmp/a[b]",
        ] {
            let mut m = manifest();
            m.executable = executable.into();
            assert_eq!(
                Platform::Linux.render(&m).unwrap_err().code,
                "unsupported_executable_path"
            );
            assert!(Platform::Macos.render(&m).is_ok());
        }
    }

    #[test]
    fn mac_disabled_override_is_exact_and_fail_closed() {
        assert_eq!(
            parse_disabled_override("com.loboot.example", "disabled services = {\n}\n").unwrap(),
            None
        );
        assert_eq!(parse_disabled_override("com.loboot.example", "disabled services = {\n\"com.loboot.example-other\" => false\n\"com.loboot.example\" => true\n}\n").unwrap(), Some(true));
        assert_eq!(
            parse_disabled_override(
                "com.loboot.example",
                "disabled services = {\n\"com.loboot.example\" => false\n}\n"
            )
            .unwrap(),
            Some(false)
        );
        for text in [
            "",
            "other = {\n}",
            "disabled services = {",
            "disabled services = {\n\"com.loboot.example\" => maybe\n}\n",
            "disabled services = {\n\"com.loboot.example\" => false\n\"com.loboot.example\" => true\n}\n",
        ] {
            assert_eq!(
                parse_disabled_override("com.loboot.example", text)
                    .unwrap_err()
                    .code,
                "status_unavailable"
            );
        }
    }

    #[test]
    fn mac_disabled_override_accepts_legacy_and_modern_exact_values() {
        for (value, disabled) in [
            ("true", true),
            ("false", false),
            ("disabled", true),
            ("enabled", false),
        ] {
            for suffix in ["", ";"] {
                let text = format!(
                    "disabled services = {{\n\t\"com.loboot.example-other\" => disabled\n\t\"com.loboot.example\" => {value}{suffix}\n}}\n"
                );
                assert_eq!(
                    parse_disabled_override("com.loboot.example", &text).unwrap(),
                    Some(disabled),
                );
            }
            let mut runner = FakeRunner {
                outputs: VecDeque::from([
                    output(
                        false,
                        113,
                        "",
                        "Could not find service \"com.loboot.example\" in domain for user gui: 501",
                    ),
                    output(
                        true,
                        0,
                        &format!("disabled services = {{\n\"com.loboot.example\" => {value}\n}}\n"),
                        "",
                    ),
                ]),
                ..Default::default()
            };
            let status = Platform::Macos
                .status_with_context("example", 501, &mut runner)
                .unwrap();
            assert_eq!(status.enabled, Some(!disabled));
            assert_eq!(status.raw["disabled_override"], disabled.to_string());
        }
    }

    #[test]
    fn mac_disabled_override_rejects_ambiguous_modern_values() {
        for value in [
            "Disabled",
            "ENABLED",
            "disabled-later",
            "enabled true",
            "not disabled",
            "1",
            "0",
            "disabled;;",
            "true;;",
            "\"disabled\"",
            "",
        ] {
            let text = format!("disabled services = {{\n\"com.loboot.example\" => {value}\n}}\n");
            assert_eq!(
                parse_disabled_override("com.loboot.example", &text)
                    .unwrap_err()
                    .code,
                "status_unavailable",
            );
        }
        let duplicate = "disabled services = {\n\"com.loboot.example\" => enabled\n\"com.loboot.example\" => disabled\n}\n";
        assert_eq!(
            parse_disabled_override("com.loboot.example", duplicate)
                .unwrap_err()
                .code,
            "status_unavailable",
        );
    }

    #[test]
    fn disabled_job_cannot_be_started_by_silently_enabling_it() {
        let session = Session {
            platform: Platform::Macos,
            uid: Some(501),
        };
        let mut status = Status {
            loaded: Some(false),
            running: Some(false),
            enabled: Some(false),
            state: "unloaded".into(),
            raw: BTreeMap::new(),
        };
        for action in [Action::Start, Action::Restart] {
            assert_eq!(
                Platform::Macos
                    .command_plan_with_status(
                        action,
                        "example",
                        Path::new("/tmp/u.plist"),
                        &session,
                        &status
                    )
                    .unwrap_err()
                    .code,
                "disabled_service"
            );
        }
        status.enabled = Some(true);
        let commands = Platform::Macos
            .command_plan_with_status(
                Action::Start,
                "example",
                Path::new("/tmp/u.plist"),
                &session,
                &status,
            )
            .unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].args[0], "bootstrap");
        status.enabled = None;
        assert_eq!(
            Platform::Macos
                .command_plan_with_status(
                    Action::Start,
                    "example",
                    Path::new("/tmp/u.plist"),
                    &session,
                    &status
                )
                .unwrap_err()
                .code,
            "status_required"
        );
    }

    #[test]
    fn native_resolution_must_match_owned_file_without_dropins() {
        let definition = Path::new("/home/me/unit.service");
        let mut status = Status {
            loaded: Some(true),
            running: Some(false),
            enabled: Some(false),
            state: "inactive/dead".into(),
            raw: BTreeMap::from([
                ("FragmentPath".into(), "/home/me/unit.service".into()),
                ("DropInPaths".into(), "".into()),
            ]),
        };
        assert!(
            Platform::Linux
                .verify_managed_status("example", definition, &status)
                .is_ok()
        );
        status.raw.insert(
            "DropInPaths".into(),
            "/etc/systemd/user/service.d/override.conf".into(),
        );
        assert_eq!(
            Platform::Linux
                .verify_managed_status("example", definition, &status)
                .unwrap_err()
                .code,
            "native_conflict"
        );
        status.loaded = Some(false);
        assert_eq!(
            Platform::Linux
                .verify_managed_status("example", definition, &status)
                .unwrap_err()
                .code,
            "native_conflict"
        );
        status.raw.remove("DropInPaths");
        status.loaded = Some(true);
        assert_eq!(
            Platform::Linux
                .verify_managed_status("example", definition, &status)
                .unwrap_err()
                .code,
            "status_required"
        );
        status.raw.insert(
            "FragmentPath".into(),
            "/etc/systemd/user/unit.service".into(),
        );
        assert_eq!(
            Platform::Linux
                .verify_managed_status("example", definition, &status)
                .unwrap_err()
                .code,
            "native_conflict"
        );
        assert_eq!(
            Platform::Macos
                .verify_managed_status("example", definition, &status)
                .unwrap_err()
                .code,
            "native_conflict"
        );
        status
            .raw
            .insert("path".into(), definition.to_str().unwrap().into());
        assert!(
            Platform::Macos
                .verify_managed_status("example", definition, &status)
                .is_ok()
        );
    }
}
