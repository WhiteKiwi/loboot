use clap::{Parser, Subcommand, ValueEnum};
use loboot::{
    Error, Result,
    backend::{self, Action, CommandSpec, Platform, ProcessRunner, Runner, RuntimeContext},
    store::Store,
};
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
    },
    Explain {
        manifest: PathBuf,
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
                json!({"applied":plan,"note":"Definition removed. No process was stopped implicitly."}),
            )
        }
        Commands::List => Ok(json!({"services":ctx.store.list()?})),
        Commands::Show { id } => Ok(json!({"service":ctx.store.read(id)?})),
        Commands::Explain { manifest } => {
            let m = loboot::model::read(manifest)?;
            Ok(
                json!({"manifest":m,"platform":ctx.platform,"native_definition":ctx.platform.render(&m)?,"login_scope":"user manager activation; not every simultaneous login","capabilities":capabilities(ctx.platform)}),
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
            Ok(
                json!({"id":id,"installed":true,"native":ctx.platform.status(id,&session,&mut runner)?}),
            )
        }
        Commands::Logs { id } => {
            ctx.store.read(id)?;
            Err(Error::new(
                "unsupported_capability",
                format!(
                    "logs is not implemented in v0.1; inspect native logs for {} using journalctl --user on Linux or Console on macOS",
                    ctx.platform.native_name(id)
                ),
            ))
        }
        Commands::Start { id } => runtime(cli, &ctx, Action::Start, id),
        Commands::Stop { id } => runtime(cli, &ctx, Action::Stop, id),
        Commands::Restart { id } => runtime(cli, &ctx, Action::Restart, id),
        Commands::Enable { id } => runtime(cli, &ctx, Action::Enable, id),
        Commands::Disable { id } => runtime(cli, &ctx, Action::Disable, id),
    }
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
    ctx.store.with_lock(||{
        ctx.store.read(id)?;
        let mut runner=ProcessRunner;
        let session=ctx.platform.session_check(&mut runner)?;
        let before=ctx.platform.status(id,&session,&mut runner)?;
        ctx.platform.verify_managed_status(id,&definition,&before)?;
        let commands=ctx.platform.command_plan_with_status(action,id,&definition,&session,&before)?;
        for (index,command) in commands.iter().enumerate() {
            execute(&mut runner,std::slice::from_ref(command)).map_err(|e|Error::new("partial_failure",format!("Native plan stopped at step {index}/{}: {e}",commands.len())))?;
            if command.args.iter().any(|a|a=="daemon-reload") {
                let reloaded=ctx.platform.status(id,&session,&mut runner)?;
                ctx.platform.verify_managed_status(id,&definition,&reloaded)?;
                ctx.store.read(id)?;
            }
        }
        let after=ctx.platform.status(id,&session,&mut runner).map_err(|e|Error::new("verification_failed",format!("Native commands were accepted, but final state could not be read: {e}. Inspect native status before retrying.")))?;
        ctx.platform.verify_managed_status(id,&definition,&after)?;
        Ok(json!({"id":id,"action":action,"commands":commands,"before":before,"after":after,"note":"Native commands accepted; see observed state. A short once job may already have exited."}))
    })
}
fn execute(runner: &mut dyn Runner, commands: &[CommandSpec]) -> Result<()> {
    for (index, c) in commands.iter().enumerate() {
        let out=runner.run(c).map_err(|e|Error::new("partial_failure",format!("Command {index}/{} could not run after {index} completed: {e}. Inspect status before retrying.",commands.len())))?;
        if !out.success {
            return Err(Error::new(
                "partial_failure",
                format!(
                    "Command {index}/{} failed after {index} completed (exit {:?}): {}. Inspect status before retrying.",
                    commands.len(),
                    out.code,
                    out.stderr.trim()
                ),
            ));
        }
    }
    Ok(())
}
fn capabilities(p: Platform) -> Value {
    json!({"runtime":true,"login_persistence":true,"platform":p,"system_scope":false,"windows":false,"logs":false,"secrets":false})
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
}
