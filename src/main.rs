use clap::{Parser, Subcommand, ValueEnum};
use loboot::{
    Error, Result,
    backend::{
        self, Action, CommandSpec, Platform, ProcessObserver, ProcessRunner, Runner,
        RuntimeContext, TeardownObserver,
    },
    store::{RecoveryPlan, RecoveryTarget, Store, read_recovery_plan_file},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

#[derive(Parser)]
#[command(version, about = "Native user services, explicit plans, no daemon")]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true)]
    dry_run: bool,
    /// Offline filesystem sandbox. Never connects to a service manager.
    #[arg(long, global = true)]
    root: Option<PathBuf>,
    /// Select rendering platform in offline mode only.
    #[arg(long, global = true, value_enum)]
    platform: Option<Target>,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Clone, Copy, ValueEnum)]
enum Target {
    Linux,
    Macos,
}
#[derive(Clone, Copy, ValueEnum)]
enum ServiceMode {
    Once,
    Service,
}
#[derive(Clone, Copy, ValueEnum)]
enum RestartPolicy {
    Never,
    OnFailure,
    Always,
}
#[derive(Clone, Copy, ValueEnum)]
enum RecoveryDestination {
    Before,
    After,
}
#[derive(Subcommand)]
enum RecoveryCommands {
    /// Inspect a pending transaction without changing files or native state.
    Plan {
        #[arg(long, value_enum, default_value = "before")]
        to: RecoveryDestination,
    },
    /// Apply an exact, saved `recover plan --json` response.
    Apply { plan: PathBuf },
}
#[derive(Subcommand)]
enum Commands {
    Add {
        /// Manifest path, or service ID when followed by -- COMMAND ARGS.
        manifest: PathBuf,
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "service")]
        mode: ServiceMode,
        #[arg(long, value_enum, default_value = "never")]
        restart: RestartPolicy,
        #[arg(last = true)]
        command: Vec<String>,
    },
    Update {
        manifest: PathBuf,
    },
    Remove {
        id: String,
    },
    List,
    Show {
        id: String,
    },
    Discover,
    Status {
        id: String,
    },
    Start {
        id: String,
    },
    Stop {
        id: String,
    },
    Restart {
        id: String,
    },
    Enable {
        id: String,
    },
    Disable {
        id: String,
    },
    Logs {
        id: String,
        /// Maximum text lines per source.
        #[arg(long, default_value_t = loboot::logs::DEFAULT_LINES)]
        lines: usize,
        /// Maximum returned bytes per source (up to 1 MiB).
        #[arg(long, default_value_t = loboot::logs::DEFAULT_BYTES)]
        max_bytes: usize,
    },
    Explain {
        manifest: PathBuf,
    },
    Recover {
        #[command(subcommand)]
        command: RecoveryCommands,
    },
    Doctor,
}
fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().collect();
    let wants_json = args
        .iter()
        .take_while(|a| *a != "--")
        .any(|a| a == "--json");
    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => {
            if e.use_stderr() && wants_json {
                println!(
                    "{}",
                    json!({"schema_version":"loboot.cli/v1","ok":false,"error":{"code":"usage","message":e.to_string()}})
                );
            } else {
                let _ = e.print();
            }
            return ExitCode::from(e.exit_code() as u8);
        }
    };
    match run(&cli) {
        Ok(data) => {
            emit(cli.json, true, data);
            ExitCode::SUCCESS
        }
        Err(e) => {
            emit(cli.json, false, json!({"code":e.code,"message":e.message}));
            ExitCode::FAILURE
        }
    }
}
fn emit(as_json: bool, ok: bool, data: Value) {
    if as_json {
        println!(
            "{}",
            if ok {
                json!({"schema_version":"loboot.cli/v1","ok":true,"data":data})
            } else {
                json!({"schema_version":"loboot.cli/v1","ok":false,"error":data})
            }
        );
    } else if ok {
        println!(
            "{}",
            serde_json::to_string_pretty(&data).expect("JSON value")
        );
    } else {
        eprintln!(
            "{}: {}",
            data["code"].as_str().unwrap_or("error"),
            data["message"].as_str().unwrap_or("unknown error")
        );
    }
}
struct Context {
    store: Store,
    platform: Platform,
    native_dir: PathBuf,
    offline: bool,
}
fn context(cli: &Cli) -> Result<Context> {
    if cli.platform.is_some() && cli.root.is_none() {
        return Err(Error::new(
            "usage",
            "--platform requires --root; cross-platform execution is never allowed",
        ));
    }
    let platform = match cli.platform {
        Some(Target::Linux) => Platform::Linux,
        Some(Target::Macos) => Platform::Macos,
        None => Platform::current()?,
    };
    let (state, native) = if let Some(root) = &cli.root {
        if !root.is_absolute() {
            return Err(Error::new("invalid_path", "--root must be absolute"));
        }
        (root.join("state"), root.join("native"))
    } else {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .ok_or_else(|| Error::new("invalid_home", "HOME must be an absolute path"))?;
        let state = home.join(".local/share/loboot");
        let native = platform.native_dir(&home);
        (state, native)
    };
    Ok(Context {
        store: Store::new(state, native.clone(), platform),
        platform,
        native_dir: native,
        offline: cli.root.is_some(),
    })
}
fn run(cli: &Cli) -> Result<Value> {
    let ctx = context(cli)?;
    match &cli.command {
        Commands::Recover { command } => recover(cli, &ctx, command),
        Commands::Add { manifest, .. } | Commands::Update { manifest } => {
            let m = input_manifest(&cli.command, manifest)?;
            let update = matches!(cli.command, Commands::Update { .. });
            let plan = if update {
                ctx.store.plan_update(&m)?
            } else {
                ctx.store.plan_add(&m)?
            };
            if cli.dry_run {
                return Ok(
                    json!({"dry_run":true,"plan":plan,"preconditions":if update {"native job must be stopped; checked only on apply"}else{"no native collision"},"native_commands":filesystem_commands(&ctx,&m.id,!update,plan.is_noop())?}),
                );
            }
            ctx.store.apply_checked(&plan, || {
                if !ctx.offline && !plan.is_noop() {
                    if update {
                        guard_edit(&ctx, &m.id, false)?;
                    } else if !plan.is_noop() {
                        guard_add(&ctx, &m.id)?;
                    }
                }
                Ok(())
            })?;
            if !ctx.offline && !plan.is_noop() {
                sync_after_write(&ctx)?;
            }
            Ok(
                json!({"applied":plan,"started":false,"enabled_changed":false,"note":"Definition stored. Runtime changes require an explicit start/restart; Linux runtime commands reload definitions first."}),
            )
        }
        Commands::Remove { id } => {
            let plan = ctx.store.plan_remove(id)?;
            let retained_logs = if plan.is_noop() {
                None
            } else {
                ctx.store.log_paths(&ctx.store.read(id)?)?
            };
            if cli.dry_run {
                return Ok(
                    json!({"dry_run":true,"plan":plan,"preconditions":"native job must be stopped and disabled; checked only on apply","native_commands":filesystem_commands(&ctx,id,false,plan.is_noop())?}),
                );
            }
            ctx.store.apply_checked(&plan, || {
                if !ctx.offline && !plan.is_noop() {
                    guard_edit(&ctx, id, true)?;
                }
                Ok(())
            })?;
            if !ctx.offline && !plan.is_noop() {
                sync_after_write(&ctx)?;
            }
            Ok(
                json!({"applied":plan,"retained_logs":retained_logs,"note":"Definition removed. No process was stopped implicitly; captured logs are retained for deliberate private cleanup."}),
            )
        }
        Commands::List => {
            let records = ctx.store.list()?;
            let services = records
                .iter()
                .map(|record| record_display(&ctx, record))
                .collect::<Result<Vec<_>>>()?;
            Ok(
                json!({"services":services,"environment_redacted":records.iter().any(|record| !record.manifest.environment.is_empty())}),
            )
        }
        Commands::Show { id } => {
            let record = ctx.store.read(id)?;
            Ok(
                json!({"log_capture":ctx.store.log_paths(&record)?,"service":record_display(&ctx,&record)?,"environment_redacted":!record.manifest.environment.is_empty()}),
            )
        }
        Commands::Explain { manifest } => {
            let m = loboot::model::read(manifest)?;
            let preview = ctx.store.preview(&m)?;
            let display = record_display(&ctx, &preview)?;
            Ok(
                json!({"manifest":manifest_display(&m),"platform":ctx.platform,"native_definition":display["native_bytes"],"environment_redacted":!m.environment.is_empty(),"log_capture":ctx.store.log_paths(&preview)?,"login_scope":"user manager activation; not every simultaneous login","capabilities":capabilities(ctx.platform)}),
            )
        }
        Commands::Discover => discover(&ctx.native_dir, &ctx.store),
        Commands::Doctor => {
            let check = ctx.store.check();
            let state = match check {
                Ok(()) => json!({"ok":true}),
                Err(e) => json!({"ok":false,"code":e.code,"message":e.message}),
            };
            let session = if ctx.offline || cli.dry_run {
                json!({"checked":false,"reason":"offline or dry-run"})
            } else {
                match ctx.platform.session_check(&mut ProcessRunner) {
                    Ok(s) => json!({"checked":true,"available":true,"uid":s.uid}),
                    Err(e) => {
                        json!({"checked":true,"available":false,"code":e.code,"message":e.message})
                    }
                }
            };
            Ok(
                json!({"state":state,"session":session,"capabilities":capabilities(ctx.platform),"note":"Linux activation follows the user-manager lifetime; lingering and overlapping sessions can outlive logout. loboot never changes linger settings."}),
            )
        }
        Commands::Status { id } => {
            ctx.store.read(id)?;
            if cli.dry_run {
                return Ok(
                    json!({"dry_run":true,"id":id,"queries":["check user session","read native status and enablement"],"executed":false}),
                );
            }
            require_live(&ctx)?;
            let mut runner = ProcessRunner;
            let session = ctx.platform.session_check(&mut runner)?;
            let status = ctx.platform.status(id, &session, &mut runner)?;
            Ok(
                json!({"id":id,"installed":true,"diagnostics":ctx.platform.diagnostics(&status),"native":status}),
            )
        }
        Commands::Logs {
            id,
            lines,
            max_bytes,
        } => {
            let limits = loboot::logs::LogLimits::new(*lines, *max_bytes)?;
            let record = ctx.store.read(id)?;
            if cli.dry_run {
                let source = if ctx.platform == Platform::Linux {
                    json!({"command":ctx.platform.journal_plan(id, limits.lines)?})
                } else {
                    json!({"paths":ctx.store.log_paths(&record)?,"legacy_capture":record.log_generation.is_none()})
                };
                return Ok(
                    json!({"dry_run":true,"id":id,"limits":limits,"source":source,"executed":false}),
                );
            }
            let sources = if ctx.platform == Platform::Linux {
                require_live(&ctx)?;
                vec![loboot::logs::read_journal(id, limits, &mut ProcessRunner)?]
            } else {
                ctx.store.read_logs(&record, limits)?
            };
            Ok(
                json!({"id":id,"limits":limits,"sources":sources,"redaction":"best-effort common token forms only; arbitrary application logs may contain sensitive data","retention":if ctx.platform == Platform::Macos {"append-only capture; no unattended disk cap; rotate only while stopped/unloaded"} else {"native journal retention is managed by the host; loboot does not vacuum or change it"}}),
            )
        }
        Commands::Start { id } => runtime(cli, &ctx, Action::Start, id),
        Commands::Stop { id } => runtime(cli, &ctx, Action::Stop, id),
        Commands::Restart { id } => runtime(cli, &ctx, Action::Restart, id),
        Commands::Enable { id } => runtime(cli, &ctx, Action::Enable, id),
        Commands::Disable { id } => runtime(cli, &ctx, Action::Disable, id),
    }
}

/// Display projections never replace the exact values used by storage or by an
/// explicit private recovery export. Redacted native bytes are not replay data.
fn manifest_display(manifest: &loboot::model::Manifest) -> Value {
    let mut display = json!(manifest);
    if !manifest.environment.is_empty() {
        let environment: serde_json::Map<String, Value> = manifest
            .environment
            .keys()
            .map(|name| (name.clone(), json!("[REDACTED]")))
            .collect();
        display["environment"] = Value::Object(environment);
    }
    display
}

fn record_display(ctx: &Context, record: &loboot::store::Record) -> Result<Value> {
    let mut display = json!(record);
    if !record.manifest.environment.is_empty() {
        display["manifest"] = manifest_display(&record.manifest);
        let definition = if let Some(paths) = ctx.store.log_paths(record)? {
            ctx.platform.render_with_logs_redacted(
                &record.manifest,
                &paths.stdout,
                &paths.stderr,
            )?
        } else {
            ctx.platform.render_redacted(&record.manifest)?
        };
        display["native_bytes"] = json!(definition);
    }
    Ok(display)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedRecoveryResponse {
    schema_version: String,
    ok: bool,
    data: SavedRecoveryData,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedRecoveryData {
    pending: bool,
    plan: Option<RecoveryPlan>,
}

fn parse_recovery_response(bytes: &[u8]) -> Result<RecoveryPlan> {
    let invalid = || {
        Error::new(
            "invalid_recovery_plan",
            "expected an unmodified successful loboot.cli/v1 recover plan JSON response with a pending plan",
        )
    };
    // Never include deserializer diagnostics: unknown keys and invalid values
    // can contain plaintext definitions, command arguments, or other secrets.
    let saved: SavedRecoveryResponse = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if saved.schema_version != "loboot.cli/v1" || !saved.ok || !saved.data.pending {
        return Err(invalid());
    }
    saved.data.plan.ok_or_else(invalid)
}

fn recover(cli: &Cli, ctx: &Context, command: &RecoveryCommands) -> Result<Value> {
    match command {
        RecoveryCommands::Plan { to } => {
            let target = match to {
                RecoveryDestination::Before => RecoveryTarget::Before,
                RecoveryDestination::After => RecoveryTarget::After,
            };
            let plan = ctx.store.plan_recovery(target)?;
            if cli.json {
                // This explicitly requested private export is exact replay data,
                // not an ordinary diagnostic. Never redact its internal values.
                Ok(json!({"pending":plan.is_some(),"plan":plan}))
            } else {
                let summary = plan.as_ref().map(|plan| {
                    json!({"id":plan.id,"target":plan.target,"changes":plan.changes,"note":"Summary only. Use recover plan --json to save an exact private recovery export; that export contains plaintext definitions and environment values."})
                });
                Ok(json!({"pending":plan.is_some(),"plan":summary}))
            }
        }
        RecoveryCommands::Apply { plan } => {
            let plan = parse_recovery_response(&read_recovery_plan_file(plan)?)?;
            if cli.dry_run {
                ctx.store.validate_recovery_plan(&plan)?;
                return Ok(json!({
                    "dry_run":true,
                    "id":plan.id,
                    "target":plan.target,
                    "changes":plan.changes,
                    "validated":true,
                    "native_checked":false,
                    "runtime_restored":false,
                    "note":"Exact filesystem evidence validated without a lock or native queries; live preconditions are checked only on apply."
                }));
            }
            let outcome = ctx.store.apply_recovery_checked(&plan, || {
                guard_recovery(ctx, &plan.id, plan.removes_definition(), &mut ProcessRunner)
            })?;
            Ok(
                json!({"recovered":outcome,"runtime_restored":false,"note":"Only the selected filesystem pair was recovered. Runtime and enablement were not restored; no native mutation or reload was performed."}),
            )
        }
    }
}

/// Recovery is not an ordinary edit: even daemon-reload would change native
/// state before the recovery evidence has been committed. Query only.
fn guard_recovery(
    ctx: &Context,
    id: &str,
    removes_definition: bool,
    runner: &mut dyn Runner,
) -> Result<()> {
    if ctx.offline {
        return Ok(());
    }
    let session = ctx.platform.session_check(runner).map_err(|e| {
        Error::new(
            e.code,
            "recovery could not verify the native user session; no native changes were attempted",
        )
    })?;
    if ctx.platform == Platform::Linux {
        let command = CommandSpec {
            program: "systemctl".into(),
            args: vec![
                "--user".into(),
                "show".into(),
                "--property=NeedDaemonReload".into(),
                "--value".into(),
                "--no-pager".into(),
                "--".into(),
                ctx.platform.native_name(id),
            ],
        };
        let uncertain = || {
            Error::new(
                "status_required",
                "recovery could not prove the native manager has current definitions; inspect native state and deliberately reload outside recovery before saving a fresh plan",
            )
        };
        let output = runner.run(&command).map_err(|_| uncertain())?;
        if !output.success
            || output.code != Some(0)
            || output.signal.is_some()
            || !output.is_complete()
        {
            return Err(uncertain());
        }
        match output.stdout.trim() {
            "no" => (),
            "yes" => {
                return Err(Error::new(
                    "native_sync_needed",
                    "native definitions need a reload; inspect native state and deliberately reload outside recovery before saving a fresh plan; recovery did not reload anything",
                ));
            }
            _ => return Err(uncertain()),
        }
    }
    let status = ctx.platform.status(id, &session, runner).map_err(|e| {
        Error::new(
            e.code,
            "recovery could not read trustworthy native status; no native changes were attempted",
        )
    })?;
    ctx.platform
        .verify_managed_status(id, &ctx.store.definition_path(id)?, &status)
        .map_err(|e| {
            Error::new(e.code, "recovery could not verify exclusive native ownership; no native changes were attempted")
        })?;
    let stopped = status.running == Some(false)
        && match ctx.platform {
            Platform::Linux => status
                .raw
                .get("ActiveState")
                .is_some_and(|state| matches!(state.as_str(), "inactive" | "failed")),
            Platform::Macos => status.loaded == Some(false),
        };
    if !stopped {
        return Err(Error::new(
            "service_not_stopped",
            "recovery requires a stopped Linux service or an unloaded macOS job; runtime state was not changed",
        ));
    }
    if removes_definition && status.enabled != Some(false) {
        return Err(Error::new(
            "service_not_disabled",
            "recovery to an absent definition requires verifiably disabled native state; unknown enablement is not proof of absence",
        ));
    }
    if removes_definition && ctx.platform == Platform::Linux {
        // UnitFileState belongs to the manager's cached status. is-enabled
        // checks current installation state and must independently say disabled.
        let command = CommandSpec {
            program: "systemctl".into(),
            args: vec![
                "--user".into(),
                "is-enabled".into(),
                "--no-pager".into(),
                "--".into(),
                ctx.platform.native_name(id),
            ],
        };
        let uncertain = || {
            Error::new(
                "status_required",
                "recovery could not verify current native enablement; inspect native state before saving a fresh plan",
            )
        };
        let output = runner.run(&command).map_err(|_| uncertain())?;
        if !output.is_complete() || output.signal.is_some() {
            return Err(uncertain());
        }
        if output.success || output.code != Some(1) || output.stdout.trim() != "disabled" {
            return Err(Error::new(
                "service_not_disabled",
                "recovery to absence requires a fresh, unambiguous disabled installation check; runtime-enabled, missing, static and unknown states are refused",
            ));
        }
        reject_activation_collision(&ctx.native_dir, &ctx.platform.native_name(id))
            .map_err(|e| Error::new(e.code, "recovery to absence found native activation links or unsafe activation directories; no links were removed"))?;
    }
    Ok(())
}

fn input_manifest(command: &Commands, path: &Path) -> Result<loboot::model::Manifest> {
    use loboot::model::{Manifest, Mode, Restart};
    if let Commands::Add {
        command,
        cwd,
        mode,
        restart,
        ..
    } = command
    {
        if !command.is_empty() {
            let m = Manifest {
                id: path
                    .to_str()
                    .ok_or_else(|| Error::new("invalid_id", "ID must be UTF-8"))?
                    .into(),
                executable: PathBuf::from(&command[0]),
                args: command[1..].to_vec(),
                cwd: match cwd {
                    Some(p) => p.clone(),
                    None => std::env::current_dir()?.canonicalize()?,
                },
                mode: match mode {
                    ServiceMode::Once => Mode::Once,
                    ServiceMode::Service => Mode::Service,
                },
                restart: match restart {
                    RestartPolicy::Never => Restart::Never,
                    RestartPolicy::OnFailure => Restart::OnFailure,
                    RestartPolicy::Always => Restart::Always,
                },
                environment: Default::default(),
            };
            m.validate()?;
            return Ok(m);
        }
        if cwd.is_some()
            || !matches!(mode, ServiceMode::Service)
            || !matches!(restart, RestartPolicy::Never)
        {
            return Err(Error::new(
                "usage",
                "--cwd/--mode/--restart belong to add ID -- COMMAND ARGS; manifest files carry their own settings",
            ));
        }
    }
    loboot::model::read(path)
}
fn filesystem_commands(ctx: &Context, id: &str, adding: bool, noop: bool) -> Result<Value> {
    if ctx.offline || noop {
        return Ok(json!([]));
    }
    if ctx.platform == Platform::Linux {
        return Ok(json!([
            {"phase":"before filesystem change and native ownership preflight","commands":ctx.platform.reload_plan()},
            {"phase":"after committed filesystem change","commands":ctx.platform.reload_plan()}
        ]));
    }
    if adding {
        let session = backend::Session {
            platform: ctx.platform,
            uid: backend::effective_uid(),
        };
        return Ok(
            json!([{"phase":"after collision preflight, before publishing a new plist","commands":ctx.platform.add_guard_plan(id,&session)?}]),
        );
    }
    Ok(json!([]))
}
fn sync_after_write(ctx: &Context) -> Result<()> {
    ctx.store.with_lock(||execute(&mut ProcessRunner,&ctx.platform.reload_plan()))
        .map_err(|e|Error::new("native_sync_needed",format!("Filesystem change committed, but native reload failed: {e}. Inspect status; the next native action retries reload.")))
}
fn reject_activation_collision(dir: &Path, name: &str) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let n = entry.file_name();
        let n = n.to_string_lossy();
        if n.ends_with(".wants") || n.ends_with(".requires") {
            let path = entry.path();
            if entry.file_type()?.is_symlink() {
                return Err(Error::new(
                    "native_collision",
                    "symlinked activation directory prevents proving a disabled install",
                ));
            }
            match std::fs::symlink_metadata(path.join(name)) {
                Ok(_) => {
                    return Err(Error::new(
                        "native_collision",
                        "existing activation link prevents a disabled add; external links are not removed",
                    ));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}
fn guard_add(ctx: &Context, id: &str) -> Result<()> {
    if ctx.platform == Platform::Linux {
        reject_activation_collision(&ctx.native_dir, &ctx.platform.native_name(id))?;
    }
    let mut runner = ProcessRunner;
    let session = ctx.platform.session_check(&mut runner)?;
    execute(&mut runner, &ctx.platform.reload_plan())?;
    let status = ctx.platform.status(id, &session, &mut runner)?;
    ctx.platform
        .verify_managed_status(id, &ctx.store.definition_path(id)?, &status)?;
    if status.loaded != Some(false) || status.enabled == Some(true) {
        return Err(Error::new(
            "native_collision",
            "an existing loaded or enabled native name prevents add; external jobs are never adopted",
        ));
    }
    execute(&mut runner, &ctx.platform.add_guard_plan(id, &session)?)
}
fn require_live(ctx: &Context) -> Result<()> {
    if ctx.offline {
        Err(Error::new(
            "offline_only",
            "--root is an offline sandbox; native manager execution is disabled",
        ))
    } else {
        Ok(())
    }
}
fn guard_edit(ctx: &Context, id: &str, remove: bool) -> Result<()> {
    let mut runner = ProcessRunner;
    let session = ctx.platform.session_check(&mut runner)?;
    execute(&mut runner, &ctx.platform.reload_plan())?;
    let s = ctx.platform.status(id, &session, &mut runner)?;
    ctx.platform
        .verify_managed_status(id, &ctx.store.definition_path(id)?, &s)?;
    if s.running != Some(false) || (ctx.platform == Platform::Macos && s.loaded != Some(false)) {
        return Err(Error::new(
            "service_not_stopped",
            "stop the service explicitly before updating or removing its definition",
        ));
    }
    if remove && s.enabled != Some(false) {
        return Err(Error::new(
            "service_not_disabled",
            "disable the service explicitly before removing its definition",
        ));
    }
    Ok(())
}
fn runtime(cli: &Cli, ctx: &Context, action: Action, id: &str) -> Result<Value> {
    ctx.store.read(id)?;
    let definition = ctx.store.definition_path(id)?;
    if cli.dry_run {
        let plans = if ctx.platform == Platform::Macos {
            let uid = backend::effective_uid();
            [true,false].into_iter().map(|loaded| {
                match ctx.platform.plan_with_context(action,id,&definition,&RuntimeContext{uid,loaded:Some(loaded)}) {
                    Ok(commands)=>json!({"when":if loaded {"job loaded"}else{"job not loaded"},"commands":commands}),
                    Err(e)=>json!({"when":if loaded {"job loaded"}else{"job not loaded"},"blocked":{"code":e.code,"message":e.message}})
                }
            }).collect::<Vec<_>>()
        } else {
            vec![json!({"commands":ctx.platform.command_plan(action,id,&definition)?})]
        };
        return Ok(
            json!({"dry_run":true,"id":id,"action":action,"preconditions":"session available and exact owned native source with no drop-ins; macOS start/restart requires enabled preference and disable requires unloaded job; checked on apply","plans":plans}),
        );
    }
    require_live(ctx)?;
    ctx.store.with_lock(|| {
        apply_native_action(ctx, action, id, &mut ProcessRunner, &mut ProcessObserver)
    })
}

fn apply_native_action(
    ctx: &Context,
    action: Action,
    id: &str,
    runner: &mut dyn Runner,
    observer: &mut dyn TeardownObserver,
) -> Result<Value> {
    let owned_before = ctx.store.read(id)?;
    if matches!(action, Action::Start | Action::Restart | Action::Enable) {
        ctx.store.verify_capture(&owned_before)?;
    }
    let definition = ctx.store.definition_path(id)?;
    let session = ctx.platform.session_check(runner)?;
    let before = ctx.platform.status(id, &session, runner)?;
    ctx.platform
        .verify_managed_status(id, &definition, &before)?;
    let commands =
        ctx.platform
            .command_plan_with_status(action, id, &definition, &session, &before)?;
    if ctx.platform == Platform::Macos
        && commands
            .iter()
            .any(|c| c.args.first().is_some_and(|a| a == "bootout"))
    {
        ctx.platform.validate_bootout_observation(&before)?;
    }
    for (index, command) in commands.iter().enumerate() {
        execute(runner, std::slice::from_ref(command)).map_err(|e| {
            Error::new(
                "partial_failure",
                format!(
                    "Native plan stopped at step {index}/{}: {e}",
                    commands.len()
                ),
            )
        })?;
        if ctx.platform == Platform::Macos && command.args.first().is_some_and(|a| a == "bootout") {
            let stopped = ctx.platform.wait_for_bootout(id, &definition, &session, &before, runner, observer)
                .map_err(|e| Error::new("partial_failure", format!("bootout was accepted; teardown verification failed: {e}. No bootstrap was attempted; inspect status before retrying.")))?;
            let owned_after = ctx.store.read(id).map_err(|e| Error::new("partial_failure", format!("bootout completed, but owned state changed: {e}. No bootstrap was attempted.")))?;
            if owned_after != owned_before {
                return Err(Error::new(
                    "partial_failure",
                    "bootout completed, but owned record changed during teardown; no bootstrap was attempted",
                ));
            }
            if action == Action::Restart {
                ctx.store.verify_capture(&owned_after).map_err(|e| Error::new("partial_failure", format!("bootout completed, but capture paths changed: {e}. No bootstrap was attempted.")))?;
                let remaining = ctx.platform.command_plan_with_status(action, id, &definition, &session, &stopped)
                    .map_err(|e| Error::new("partial_failure", format!("bootout completed, but restart preconditions changed: {e}. No bootstrap was attempted.")))?;
                if remaining != commands[index + 1..] {
                    return Err(Error::new(
                        "partial_failure",
                        "restart plan changed after bootout; no bootstrap was attempted",
                    ));
                }
            }
        }
        if command.args.iter().any(|a| a == "daemon-reload") {
            let reloaded = ctx.platform.status(id, &session, runner)?;
            ctx.platform
                .verify_managed_status(id, &definition, &reloaded)?;
            ctx.store.read(id)?;
        }
    }
    let after = ctx.platform.status(id, &session, runner).map_err(|e| Error::new(
        "verification_failed", format!("Native commands were accepted, but final state could not be read: {e}. Inspect native status before retrying."),
    ))?;
    ctx.platform
        .verify_managed_status(id, &definition, &after)?;
    Ok(
        json!({"id":id,"action":action,"commands":commands,"before":before,"after":after,"note":"Native commands accepted; see observed state. A short once job may already have exited."}),
    )
}

fn execute(runner: &mut dyn Runner, commands: &[CommandSpec]) -> Result<()> {
    for (index, c) in commands.iter().enumerate() {
        let out=runner.run(c).map_err(|e|Error::new("partial_failure",format!("Command {index}/{} could not run after {index} completed: {e}. Inspect status before retrying.",commands.len())))?;
        if !out.success || !out.is_complete() {
            return Err(Error::new(
                "partial_failure",
                format!(
                    "Command {index}/{} failed after {index} completed: {}. Inspect status before retrying.",
                    commands.len(),
                    out.failure_summary()
                ),
            ));
        }
    }
    Ok(())
}
fn capabilities(p: Platform) -> Value {
    json!({"runtime":true,"login_persistence":true,"platform":p,"system_scope":false,"windows":false,"logs":true,"secrets":false})
}
fn discover(dir: &Path, store: &Store) -> Result<Value> {
    let managed = store.list()?;
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({"directory":dir,"definitions":[]}));
        }
        Err(e) => return Err(e.into()),
    };
    let mut result = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".service") && !name.ends_with(".plist") {
            continue;
        }
        let owned = managed.iter().any(|r| r.native_name == name);
        result.push(json!({"name":name,"managed":owned,"read_only":!owned,"symlink":entry.file_type()?.is_symlink()}));
    }
    result.sort_by_key(|v| v["name"].as_str().unwrap_or("").to_owned());
    Ok(
        json!({"directory":dir,"definitions":result,"scope":"files in configured native directory only; no import/adoption"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use loboot::backend::CommandOutput;
    struct Fake {
        calls: Vec<CommandSpec>,
        fail: usize,
    }
    impl Runner for Fake {
        fn run(&mut self, c: &CommandSpec) -> Result<CommandOutput> {
            self.calls.push(c.clone());
            Ok(CommandOutput {
                success: self.calls.len() != self.fail,
                code: Some(if self.calls.len() == self.fail { 9 } else { 0 }),
                stdout: String::new(),
                stderr: "injected failure".into(),
                ..Default::default()
            })
        }
    }
    #[test]
    fn native_failure_stops_at_first_failed_step() {
        let commands: Vec<_> = (0..3)
            .map(|i| CommandSpec {
                program: "fake".into(),
                args: vec![i.to_string()],
            })
            .collect();
        let mut fake = Fake {
            calls: vec![],
            fail: 2,
        };
        let err = execute(&mut fake, &commands).unwrap_err();
        assert_eq!(fake.calls.len(), 2);
        assert_eq!(err.code, "partial_failure");
        assert!(err.message.contains("after 1 completed"));
    }
    #[test]
    fn dangling_activation_links_are_collisions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wants = tmp.path().join("default.target.wants");
        std::fs::create_dir(&wants).unwrap();
        std::os::unix::fs::symlink("/missing", wants.join("loboot-example.service")).unwrap();
        assert_eq!(
            reject_activation_collision(tmp.path(), "loboot-example.service")
                .unwrap_err()
                .code,
            "native_collision"
        );
    }

    struct ScriptedRunner {
        outputs: std::collections::VecDeque<CommandOutput>,
        calls: Vec<CommandSpec>,
        tamper: Option<(usize, PathBuf)>,
        record_change: Option<(usize, Store, loboot::model::Manifest)>,
        capture_tamper: Option<(usize, PathBuf)>,
    }
    impl Runner for ScriptedRunner {
        fn run(&mut self, command: &CommandSpec) -> Result<CommandOutput> {
            self.calls.push(command.clone());
            if let Some((at, path)) = &self.tamper
                && self.calls.len() == *at
            {
                std::fs::write(path, "foreign replacement").unwrap();
            }
            if let Some((at, path)) = &self.capture_tamper
                && self.calls.len() == *at
            {
                std::fs::remove_file(path).unwrap();
                std::os::unix::fs::symlink("/unrelated-log", path).unwrap();
            }
            if let Some((at, store, manifest)) = &self.record_change
                && self.calls.len() == *at
            {
                store
                    .apply_checked(&store.plan_update(manifest).unwrap(), || Ok(()))
                    .unwrap();
            }
            self.outputs
                .pop_front()
                .ok_or_else(|| Error::new("fake_exhausted", "unexpected native invocation"))
        }
    }
    #[derive(Default)]
    struct FakeObserver {
        alive: std::collections::VecDeque<bool>,
        default_alive: bool,
        fail_probe: bool,
        pids: Vec<u32>,
        pauses: usize,
    }
    impl TeardownObserver for FakeObserver {
        fn process_exists(&mut self, pid: u32) -> Result<bool> {
            self.pids.push(pid);
            if self.fail_probe {
                return Err(Error::new(
                    "status_unavailable",
                    "injected process observation failure",
                ));
            }
            Ok(self.alive.pop_front().unwrap_or(self.default_alive))
        }
        fn pause(&mut self) {
            self.pauses += 1;
        }
    }
    fn native_output(success: bool, stdout: &str, stderr: &str) -> CommandOutput {
        CommandOutput {
            success,
            code: Some(if success { 0 } else { 113 }),
            stdout: stdout.into(),
            stderr: stderr.into(),
            ..Default::default()
        }
    }
    fn native_context() -> (tempfile::TempDir, Context, PathBuf) {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let native_dir = root.join("native");
        let store = Store::new(root.join("state"), native_dir.clone(), Platform::Macos);
        let manifest = loboot::model::Manifest {
            id: "example".into(),
            executable: std::env::current_exe().unwrap().canonicalize().unwrap(),
            args: vec![],
            cwd: root,
            mode: loboot::model::Mode::Once,
            restart: loboot::model::Restart::Never,
            environment: Default::default(),
        };
        store
            .apply_checked(&store.plan_add(&manifest).unwrap(), || Ok(()))
            .unwrap();
        let definition = store.definition_path("example").unwrap();
        (
            temp,
            Context {
                store,
                platform: Platform::Macos,
                native_dir,
                offline: false,
            },
            definition,
        )
    }
    fn launchd_observation(
        definition: &Path,
        state: &str,
        pid: Option<&str>,
        enabled: bool,
    ) -> Vec<CommandOutput> {
        let printed = if state == "absent" {
            native_output(
                false,
                "",
                "Could not find service \"com.loboot.example\" in domain for user gui: 501",
            )
        } else {
            native_output(
                true,
                &format!(
                    "gui/501/com.loboot.example = {{\nstate = {state}\npath = {}\n{}}}\n",
                    definition.display(),
                    pid.map(|pid| format!("pid = {pid}\n")).unwrap_or_default()
                ),
                "",
            )
        };
        vec![
            printed,
            native_output(
                true,
                &format!(
                    "disabled services = {{\n\"com.loboot.example\" => {}\n}}\n",
                    if enabled { "enabled" } else { "disabled" }
                ),
                "",
            ),
        ]
    }
    fn restart_runner(definition: &Path) -> ScriptedRunner {
        let mut outputs = vec![native_output(true, "gui/501 = {\n}\n", "")];
        outputs.extend(launchd_observation(definition, "running", Some("42"), true));
        outputs.push(native_output(true, "", "")); // bootout acknowledgement
        ScriptedRunner {
            outputs: outputs.into(),
            calls: vec![],
            tamper: None,
            record_change: None,
            capture_tamper: None,
        }
    }
    fn mutations(runner: &ScriptedRunner) -> Vec<&str> {
        runner
            .calls
            .iter()
            .filter_map(|command| command.args.first().map(String::as_str))
            .filter(|verb| !matches!(*verb, "print" | "print-disabled"))
            .collect()
    }
    #[test]
    fn mac_restart_waits_for_label_and_prior_pid_then_bootstraps_once() {
        let (_temp, ctx, definition) = native_context();
        let mut runner = restart_runner(&definition);
        runner.outputs.extend(launchd_observation(
            &definition,
            "running",
            Some("42"),
            true,
        ));
        for _ in 0..3 {
            runner
                .outputs
                .extend(launchd_observation(&definition, "absent", None, true));
        }
        runner.outputs.push_back(native_output(true, "", "")); // exactly one bootstrap
        // A short once job already exited. Never infer that it needs a retry.
        runner
            .outputs
            .extend(launchd_observation(&definition, "not running", None, true));
        let mut observer = FakeObserver {
            alive: [true, true, false, false].into(),
            ..Default::default()
        };
        let result =
            apply_native_action(&ctx, Action::Restart, "example", &mut runner, &mut observer)
                .unwrap();
        assert_eq!(result["after"]["running"], false);
        assert_eq!(mutations(&runner), ["bootout", "bootstrap"]);
        assert_eq!(observer.pauses, 3);
        assert_eq!(observer.pids, [42, 42, 42, 42]);
        assert!(runner.outputs.is_empty());
    }
    #[test]
    fn mac_teardown_waits_through_sigtermed_without_calling_it_stopped() {
        let (_temp, ctx, definition) = native_context();
        let mut runner = restart_runner(&definition);
        // Even after its PID exits, a still-loaded SIGTERMed job cannot settle.
        for _ in 0..2 {
            runner.outputs.extend(launchd_observation(
                &definition,
                "SIGTERMed",
                Some("42"),
                true,
            ));
        }
        for _ in 0..2 {
            runner
                .outputs
                .extend(launchd_observation(&definition, "absent", None, true));
        }
        runner.outputs.push_back(native_output(true, "", ""));
        runner
            .outputs
            .extend(launchd_observation(&definition, "not running", None, true));
        let mut observer = FakeObserver {
            alive: [true, false, false, false].into(),
            ..Default::default()
        };
        apply_native_action(&ctx, Action::Restart, "example", &mut runner, &mut observer).unwrap();
        assert_eq!(observer.pids, [42, 42, 42, 42]);
        assert_eq!(observer.pauses, 3);
        assert_eq!(mutations(&runner), ["bootout", "bootstrap"]);
        assert!(runner.outputs.is_empty());
    }

    #[test]
    fn mac_teardown_tracks_new_pid_and_resets_settled_observations() {
        let (_temp, ctx, definition) = native_context();
        let mut runner = restart_runner(&definition);
        runner
            .outputs
            .extend(launchd_observation(&definition, "absent", None, true));
        runner.outputs.extend(launchd_observation(
            &definition,
            "running",
            Some("43"),
            true,
        ));
        for _ in 0..2 {
            runner
                .outputs
                .extend(launchd_observation(&definition, "absent", None, true));
        }
        runner.outputs.push_back(native_output(true, "", ""));
        runner
            .outputs
            .extend(launchd_observation(&definition, "not running", None, true));
        let mut observer = FakeObserver {
            alive: [false, false, true, false, false, false, false].into(),
            ..Default::default()
        };
        apply_native_action(&ctx, Action::Restart, "example", &mut runner, &mut observer).unwrap();
        assert_eq!(observer.pids, [42, 42, 43, 42, 43, 42, 43]);
        assert_eq!(observer.pauses, 3);
        assert_eq!(mutations(&runner), ["bootout", "bootstrap"]);
        assert!(runner.outputs.is_empty());
    }

    #[test]
    fn mac_teardown_observer_error_never_bootstraps() {
        let (_temp, ctx, definition) = native_context();
        let mut runner = restart_runner(&definition);
        runner
            .outputs
            .extend(launchd_observation(&definition, "absent", None, true));
        let mut observer = FakeObserver {
            fail_probe: true,
            ..Default::default()
        };
        let error =
            apply_native_action(&ctx, Action::Restart, "example", &mut runner, &mut observer)
                .unwrap_err();
        assert_eq!(error.code, "partial_failure");
        assert!(
            error
                .message
                .contains("injected process observation failure")
        );
        assert_eq!(mutations(&runner), ["bootout"]);
    }

    #[test]
    fn mac_teardown_native_query_error_never_bootstraps() {
        let (_temp, ctx, definition) = native_context();
        let mut runner = restart_runner(&definition);
        runner.outputs.push_back(native_output(
            false,
            "",
            "Could not find domain for user gui: 501",
        ));
        let error = apply_native_action(
            &ctx,
            Action::Restart,
            "example",
            &mut runner,
            &mut FakeObserver::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, "partial_failure");
        assert!(error.message.contains("status_unavailable"));
        assert_eq!(mutations(&runner), ["bootout"]);
        assert!(runner.outputs.is_empty());
    }

    #[test]
    fn mac_stop_waits_for_delayed_process_exit_without_bootstrap() {
        let (_temp, ctx, definition) = native_context();
        let mut runner = restart_runner(&definition);
        for _ in 0..4 {
            runner
                .outputs
                .extend(launchd_observation(&definition, "absent", None, true));
        }
        let mut observer = FakeObserver {
            alive: [true, false, false].into(),
            ..Default::default()
        };
        apply_native_action(&ctx, Action::Stop, "example", &mut runner, &mut observer).unwrap();
        assert_eq!(mutations(&runner), ["bootout"]);
        assert_eq!(observer.pauses, 2);
    }
    #[test]
    fn mac_teardown_timeout_never_bootstraps_or_retries_bootout() {
        for loaded in [true, false] {
            let (_temp, ctx, definition) = native_context();
            let mut runner = restart_runner(&definition);
            for _ in 0..100 {
                runner.outputs.extend(launchd_observation(
                    &definition,
                    if loaded { "running" } else { "absent" },
                    if loaded { Some("42") } else { None },
                    true,
                ));
            }
            let mut observer = FakeObserver {
                default_alive: !loaded,
                ..Default::default()
            };
            let error =
                apply_native_action(&ctx, Action::Restart, "example", &mut runner, &mut observer)
                    .unwrap_err();
            assert_eq!(error.code, "partial_failure");
            assert!(error.message.contains("teardown_timeout"));
            assert_eq!(mutations(&runner), ["bootout"]);
            assert_eq!(observer.pauses, 99);
            assert!(runner.outputs.is_empty());
        }
    }
    #[test]
    fn mac_teardown_unknown_state_and_changed_ownership_fail_closed() {
        for (state, pid, foreign, code) in [
            ("future-state", None, false, "status_required"),
            ("future-state", Some("42"), false, "status_required"),
            ("sigtermed", Some("42"), false, "status_required"),
            ("SIGTERMed", None, false, "status_required"),
            ("SIGTERMed", Some("0"), false, "status_required"),
            ("running", Some("garbage"), false, "status_unavailable"),
            ("running", Some("43"), true, "native_conflict"),
        ] {
            let (_temp, ctx, definition) = native_context();
            let mut runner = restart_runner(&definition);
            runner.outputs.extend(launchd_observation(
                if foreign {
                    Path::new("/foreign.plist")
                } else {
                    &definition
                },
                state,
                pid,
                true,
            ));
            let error = apply_native_action(
                &ctx,
                Action::Restart,
                "example",
                &mut runner,
                &mut FakeObserver::default(),
            )
            .unwrap_err();
            assert_eq!(error.code, "partial_failure");
            assert!(error.message.contains(code));
            assert_eq!(mutations(&runner), ["bootout"]);
        }
    }
    #[test]
    fn mac_restart_rechecks_capture_after_bootout_before_bootstrap() {
        let (_temp, ctx, definition) = native_context();
        let record = ctx.store.read("example").unwrap();
        let paths = ctx.store.log_paths(&record).unwrap().unwrap();
        let mut runner = restart_runner(&definition);
        runner.capture_tamper = Some((4, paths.stdout));
        for _ in 0..2 {
            runner
                .outputs
                .extend(launchd_observation(&definition, "absent", None, true));
        }
        let error = apply_native_action(
            &ctx,
            Action::Restart,
            "example",
            &mut runner,
            &mut FakeObserver::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, "partial_failure");
        assert!(error.message.contains("capture paths changed"));
        assert_eq!(mutations(&runner), ["bootout"]);
    }
    #[test]
    fn mac_restart_revalidates_disabled_preference_after_bootout() {
        let (_temp, ctx, definition) = native_context();
        let mut runner = restart_runner(&definition);
        for _ in 0..2 {
            runner
                .outputs
                .extend(launchd_observation(&definition, "absent", None, false));
        }
        let error = apply_native_action(
            &ctx,
            Action::Restart,
            "example",
            &mut runner,
            &mut FakeObserver::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, "partial_failure");
        assert!(error.message.contains("disabled_service"));
        assert_eq!(mutations(&runner), ["bootout"]);
    }
    #[test]
    fn mac_restart_revalidates_definition_after_bootout() {
        let (_temp, ctx, definition) = native_context();
        let mut runner = restart_runner(&definition);
        runner.tamper = Some((5, definition.clone()));
        for _ in 0..2 {
            runner
                .outputs
                .extend(launchd_observation(&definition, "absent", None, true));
        }
        let error = apply_native_action(
            &ctx,
            Action::Restart,
            "example",
            &mut runner,
            &mut FakeObserver::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, "partial_failure");
        assert!(error.message.contains("definition_drift"));
        assert_eq!(mutations(&runner), ["bootout"]);
    }
    #[test]
    fn mac_restart_rejects_consistent_record_change_during_teardown() {
        let (_temp, ctx, definition) = native_context();
        let mut runner = restart_runner(&definition);
        let mut changed = ctx.store.read("example").unwrap().manifest;
        changed.args.push("unexpected new argv".into());
        runner.record_change = Some((5, ctx.store.clone(), changed));
        for _ in 0..2 {
            runner
                .outputs
                .extend(launchd_observation(&definition, "absent", None, true));
        }
        let error = apply_native_action(
            &ctx,
            Action::Restart,
            "example",
            &mut runner,
            &mut FakeObserver::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, "partial_failure");
        assert!(error.message.contains("owned record changed"));
        assert_eq!(mutations(&runner), ["bootout"]);
        assert!(ctx.store.read("example").is_ok());
    }

    #[test]
    fn mac_failed_bootstrap_is_never_retried() {
        let (_temp, ctx, definition) = native_context();
        let mut runner = restart_runner(&definition);
        for _ in 0..2 {
            runner
                .outputs
                .extend(launchd_observation(&definition, "absent", None, true));
        }
        runner.outputs.push_back(native_output(
            false,
            "",
            "Bootstrap failed: 5: Input/output error",
        ));
        let error = apply_native_action(
            &ctx,
            Action::Restart,
            "example",
            &mut runner,
            &mut FakeObserver::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, "partial_failure");
        assert_eq!(mutations(&runner), ["bootout", "bootstrap"]);
        assert!(runner.outputs.is_empty());
    }
    #[test]
    fn mac_initial_unknown_process_fails_before_bootout() {
        for (state, pid) in [
            ("running", None),
            ("future-state", None),
            ("future-state", Some("42")),
            ("SIGTERMed", Some("42")),
            ("SIGTERMed", None),
            ("running", Some("0")),
        ] {
            let (_temp, ctx, definition) = native_context();
            let mut runner = ScriptedRunner {
                outputs: [native_output(true, "gui/501 = {}", "")].into(),
                calls: vec![],
                tamper: None,
                record_change: None,
                capture_tamper: None,
            };
            runner
                .outputs
                .extend(launchd_observation(&definition, state, pid, true));
            let error = apply_native_action(
                &ctx,
                Action::Restart,
                "example",
                &mut runner,
                &mut FakeObserver::default(),
            )
            .unwrap_err();
            assert_eq!(error.code, "status_required");
            assert!(mutations(&runner).is_empty());
        }
    }
    #[test]
    fn mac_fresh_once_start_bootstraps_once_without_teardown_wait() {
        let (_temp, ctx, definition) = native_context();
        let mut runner = ScriptedRunner {
            outputs: [native_output(true, "gui/501 = {}", "")].into(),
            calls: vec![],
            tamper: None,
            record_change: None,
            capture_tamper: None,
        };
        runner
            .outputs
            .extend(launchd_observation(&definition, "absent", None, true));
        runner.outputs.push_back(native_output(true, "", ""));
        runner
            .outputs
            .extend(launchd_observation(&definition, "not running", None, true));
        let mut observer = FakeObserver::default();
        apply_native_action(&ctx, Action::Start, "example", &mut runner, &mut observer).unwrap();
        assert_eq!(mutations(&runner), ["bootstrap"]);
        assert_eq!(observer.pauses, 0);
        assert!(observer.pids.is_empty());
    }

    fn recovery_runner(outputs: Vec<CommandOutput>) -> ScriptedRunner {
        ScriptedRunner {
            outputs: outputs.into(),
            calls: vec![],
            tamper: None,
            record_change: None,
            capture_tamper: None,
        }
    }

    fn linux_recovery_context() -> (tempfile::TempDir, Context, PathBuf) {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let native_dir = root.join("native");
        let store = Store::new(root.join("state"), native_dir.clone(), Platform::Linux);
        let definition = store.definition_path("example").unwrap();
        (
            temp,
            Context {
                store,
                platform: Platform::Linux,
                native_dir,
                offline: false,
            },
            definition,
        )
    }

    fn linux_recovery_observation(
        definition: &Path,
        load: &str,
        active: &str,
        enabled: &str,
        dropins: &str,
    ) -> Vec<CommandOutput> {
        vec![
            native_output(true, "Version=256\n", ""),
            native_output(true, "no\n", ""),
            native_output(
                true,
                &format!(
                    "LoadState={load}\nActiveState={active}\nSubState=dead\nUnitFileState={enabled}\nMainPID=0\nFragmentPath={}\nDropInPaths={dropins}\n",
                    definition.display(),
                ),
                "",
            ),
            CommandOutput {
                success: false,
                code: Some(1),
                stdout: "disabled\n".into(),
                ..Default::default()
            },
        ]
    }

    fn assert_recovery_queries_only(runner: &ScriptedRunner, platform: Platform) {
        for command in &runner.calls {
            match platform {
                Platform::Linux => {
                    assert_eq!(command.args.first().map(String::as_str), Some("--user"));
                    assert!(matches!(
                        command.args.get(1).map(String::as_str),
                        Some("show" | "is-enabled")
                    ));
                }
                Platform::Macos => assert!(matches!(
                    command.args.first().map(String::as_str),
                    Some("print" | "print-disabled")
                )),
            }
        }
    }

    #[test]
    fn recovery_linux_preflight_is_read_only_and_requires_stopped_owned_state() {
        let (_temp, ctx, definition) = linux_recovery_context();
        for (load, active, enabled, dropins, foreign, removing, expected) in [
            ("loaded", "inactive", "disabled", "", false, true, None),
            ("loaded", "failed", "disabled", "", false, true, None),
            ("loaded", "inactive", "enabled", "", false, false, None),
            (
                "loaded",
                "inactive",
                "enabled",
                "",
                false,
                true,
                Some("service_not_disabled"),
            ),
            (
                "loaded",
                "inactive",
                "enabled-runtime",
                "",
                false,
                true,
                Some("service_not_disabled"),
            ),
            (
                "not-found",
                "inactive",
                "",
                "",
                false,
                true,
                Some("service_not_disabled"),
            ),
            ("not-found", "inactive", "disabled", "", false, true, None),
            (
                "stub",
                "inactive",
                "disabled",
                "",
                false,
                true,
                Some("status_required"),
            ),
            (
                "future",
                "inactive",
                "disabled",
                "",
                false,
                true,
                Some("status_required"),
            ),
            (
                "loaded",
                "activating",
                "disabled",
                "",
                false,
                true,
                Some("service_not_stopped"),
            ),
            (
                "loaded",
                "active",
                "disabled",
                "",
                false,
                true,
                Some("service_not_stopped"),
            ),
            (
                "loaded",
                "inactive",
                "disabled",
                "/foreign.conf",
                false,
                true,
                Some("native_conflict"),
            ),
            (
                "loaded",
                "inactive",
                "disabled",
                "",
                true,
                true,
                Some("native_conflict"),
            ),
        ] {
            let mut runner = recovery_runner(linux_recovery_observation(
                if foreign {
                    Path::new("/foreign.service")
                } else {
                    &definition
                },
                load,
                active,
                enabled,
                dropins,
            ));
            let result = guard_recovery(&ctx, "example", removing, &mut runner);
            assert_eq!(
                result.err().map(|e| e.code),
                expected,
                "{load}/{active}/{enabled}"
            );
            assert_eq!(
                runner.calls.len(),
                if expected.is_none() && removing { 4 } else { 3 }
            );
            assert_recovery_queries_only(&runner, Platform::Linux);
            assert!(!ctx.store.state_dir().exists());
            assert!(!ctx.native_dir.exists());
        }
    }

    #[test]
    fn recovery_linux_refuses_stale_or_ambiguous_manager_cache_without_reload() {
        let (_temp, ctx, definition) = linux_recovery_context();
        for (reply, success, incomplete, expected) in [
            ("yes\n", true, false, Some("native_sync_needed")),
            ("", true, false, Some("status_required")),
            ("NO", true, false, Some("status_required")),
            ("no\nyes", true, false, Some("status_required")),
            (
                "private-native-secret",
                true,
                false,
                Some("status_required"),
            ),
            ("no\n", false, false, Some("status_required")),
            ("no\n", true, true, Some("status_required")),
            (" \tno\n", true, false, None),
        ] {
            let mut outputs =
                linux_recovery_observation(&definition, "loaded", "inactive", "disabled", "");
            outputs[1] = native_output(success, reply, "private-native-secret");
            outputs[1].stdout_truncated = incomplete;
            let mut runner = recovery_runner(outputs);
            let result = guard_recovery(&ctx, "example", true, &mut runner);
            if let Err(error) = &result {
                assert!(!error.message.contains("private-native-secret"));
                assert!(error.message.contains("outside recovery"));
                assert!(error.message.contains("fresh plan"));
            }
            assert_eq!(result.err().map(|e| e.code), expected);
            assert_eq!(runner.calls.len(), if expected.is_some() { 2 } else { 4 });
            assert_eq!(
                runner.calls[1].args,
                [
                    "--user",
                    "show",
                    "--property=NeedDaemonReload",
                    "--value",
                    "--no-pager",
                    "--",
                    "loboot-example.service",
                ]
            );
            assert_recovery_queries_only(&runner, Platform::Linux);
            assert!(!ctx.store.state_dir().exists());
        }
        let mut runner = recovery_runner(vec![native_output(true, "Version=256\n", "")]);
        assert_eq!(
            guard_recovery(&ctx, "example", true, &mut runner)
                .unwrap_err()
                .code,
            "status_required"
        );
        assert_recovery_queries_only(&runner, Platform::Linux);
    }

    #[test]
    fn recovery_linux_requires_fresh_disabled_installation_not_cached_status() {
        let (_temp, ctx, definition) = linux_recovery_context();
        for (reply, code, success, incomplete, expected) in [
            ("disabled\n", 1, false, false, None),
            (" \tdisabled\n", 1, false, false, None),
            ("disabled\n", 0, true, false, Some("service_not_disabled")),
            (
                "enabled-runtime\n",
                0,
                true,
                false,
                Some("service_not_disabled"),
            ),
            ("enabled\n", 0, true, false, Some("service_not_disabled")),
            ("not-found\n", 4, false, false, Some("service_not_disabled")),
            ("static\n", 0, true, false, Some("service_not_disabled")),
            ("static\n", 1, false, false, Some("service_not_disabled")),
            ("", 1, false, false, Some("service_not_disabled")),
            (
                "private-native-secret",
                1,
                false,
                false,
                Some("service_not_disabled"),
            ),
            (
                "disabled\nenabled-runtime",
                1,
                false,
                false,
                Some("service_not_disabled"),
            ),
            ("disabled\n", 1, false, true, Some("status_required")),
        ] {
            let mut outputs =
                linux_recovery_observation(&definition, "loaded", "inactive", "disabled", "");
            outputs[3] = CommandOutput {
                success,
                code: Some(code),
                stdout: reply.into(),
                stderr: "private-native-secret".into(),
                stdout_truncated: incomplete,
                ..Default::default()
            };
            let mut runner = recovery_runner(outputs);
            let result = guard_recovery(&ctx, "example", true, &mut runner);
            if let Err(error) = &result {
                assert!(!error.message.contains("private-native-secret"));
            }
            assert_eq!(result.err().map(|e| e.code), expected, "{reply}/{code}");
            assert_eq!(runner.calls.len(), 4);
            assert_eq!(
                runner.calls[3].args,
                [
                    "--user",
                    "is-enabled",
                    "--no-pager",
                    "--",
                    "loboot-example.service"
                ]
            );
            assert_recovery_queries_only(&runner, Platform::Linux);
            assert!(!ctx.store.state_dir().exists());
        }
        let mut outputs =
            linux_recovery_observation(&definition, "loaded", "inactive", "disabled", "");
        outputs.pop();
        let mut runner = recovery_runner(outputs);
        assert_eq!(
            guard_recovery(&ctx, "example", true, &mut runner)
                .unwrap_err()
                .code,
            "status_required"
        );
        assert_recovery_queries_only(&runner, Platform::Linux);
    }

    #[test]
    fn recovery_linux_refuses_existing_activation_links_without_removing_them() {
        let (_temp, ctx, definition) = linux_recovery_context();
        for suffix in ["wants", "requires"] {
            let activation = ctx.native_dir.join(format!("default.target.{suffix}"));
            std::fs::create_dir_all(&activation).unwrap();
            let link = activation.join("loboot-example.service");
            std::os::unix::fs::symlink("/missing-external-definition", &link).unwrap();
            let mut runner = recovery_runner(linux_recovery_observation(
                &definition,
                "loaded",
                "inactive",
                "disabled",
                "",
            ));
            assert_eq!(
                guard_recovery(&ctx, "example", true, &mut runner)
                    .unwrap_err()
                    .code,
                "native_collision"
            );
            assert_eq!(
                std::fs::read_link(&link).unwrap(),
                Path::new("/missing-external-definition")
            );
            assert_eq!(runner.calls.len(), 4);
            assert_recovery_queries_only(&runner, Platform::Linux);
            assert!(!ctx.store.state_dir().exists());
            std::fs::remove_file(link).unwrap();
        }
    }

    #[test]
    fn recovery_macos_requires_unloaded_and_disabled_when_removing() {
        let (_temp, ctx, definition) = native_context();
        for (state, enabled, removing, foreign, expected) in [
            ("absent", false, true, false, None),
            ("absent", true, false, false, None),
            ("absent", true, true, false, Some("service_not_disabled")),
            (
                "not running",
                false,
                false,
                false,
                Some("service_not_stopped"),
            ),
            (
                "future-state",
                false,
                true,
                false,
                Some("service_not_stopped"),
            ),
            ("not running", false, true, true, Some("native_conflict")),
        ] {
            let mut outputs = vec![native_output(true, "gui/501 = {}", "")];
            outputs.extend(launchd_observation(
                if foreign {
                    Path::new("/foreign.plist")
                } else {
                    &definition
                },
                state,
                None,
                enabled,
            ));
            let mut runner = recovery_runner(outputs);
            let result = guard_recovery(&ctx, "example", removing, &mut runner);
            assert_eq!(result.err().map(|e| e.code), expected, "{state}/{enabled}");
            assert_eq!(runner.calls.len(), 3);
            assert_recovery_queries_only(&runner, Platform::Macos);
        }
    }

    #[test]
    fn recovery_offline_guard_never_calls_a_runner() {
        let (_temp, mut ctx, _definition) = linux_recovery_context();
        ctx.offline = true;
        for platform in [Platform::Linux, Platform::Macos] {
            ctx.platform = platform;
            let mut runner = recovery_runner(vec![]);
            guard_recovery(&ctx, "example", true, &mut runner).unwrap();
            assert!(runner.calls.is_empty());
            assert!(!ctx.store.state_dir().exists());
        }
    }

    #[test]
    fn recovery_native_failures_preserve_journal_and_redact_output() {
        let (_temp, ctx, definition) = native_context();
        let record = ctx.store.read("example").unwrap();
        let journal = ctx.store.state_dir().join(".pending.json");
        let bytes = serde_json::to_vec(&json!({
            "schema_version":1,"operation":"add","id":"example",
            "state_dir":ctx.store.state_dir(),"native_dir":ctx.native_dir,
            "before":null,"after":record,
        }))
        .unwrap();
        std::fs::write(&journal, &bytes).unwrap();
        let plan = ctx
            .store
            .plan_recovery(RecoveryTarget::Before)
            .unwrap()
            .unwrap();
        let native_before = std::fs::read(&definition).unwrap();
        let record_path = ctx.store.state_dir().join("example.toml");
        let record_before = std::fs::read(&record_path).unwrap();
        for outputs in [
            vec![native_output(
                false,
                "token=secret-output",
                "password=secret-output",
            )],
            vec![
                native_output(true, "gui/501 = {}", ""),
                native_output(false, "secret-output", "secret-output"),
            ],
        ] {
            let mut runner = recovery_runner(outputs);
            let error = ctx
                .store
                .apply_recovery_checked(&plan, || {
                    guard_recovery(&ctx, &plan.id, plan.removes_definition(), &mut runner)
                })
                .unwrap_err();
            assert!(!error.message.contains("secret-output"));
            assert!(!error.message.contains("launchctl"));
            assert_eq!(std::fs::read(&journal).unwrap(), bytes);
            assert_eq!(std::fs::read(&definition).unwrap(), native_before);
            assert_eq!(std::fs::read(&record_path).unwrap(), record_before);
            assert_recovery_queries_only(&runner, Platform::Macos);
        }
    }
}
