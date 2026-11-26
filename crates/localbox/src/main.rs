//! The `localbox` command-line entry point.
//!
//! Hand-rolled argument handling: the surface is small and stable, and the
//! binary must start instantly on every OS. Command work runs on a worker
//! thread with an explicit stack size — Windows main threads are smaller
//! than the Linux/macOS default and deep CLI/TUI stacks overflow there.

use std::io::{IsTerminal, Write as _};
use std::process::ExitCode;

use localbox::exec::{build_launcher, home_dir};
use localbox::guided::{catalog_dir, run_guided};
use localbox::live::{
    describe_stop_remaining, execute_launch, status_report, stop_all_with, AgentKind, LiveError,
    RunningModelPolicy, StopProgress, StopReport, StopTarget,
};
use localbox::product_envelope;
use localbox_launcher::catalog::Catalog;
use localbox_launcher::launcher::LlamaLauncher;
use localbox_launcher::orchestrate::{
    plan_launch, smoke_fallback, LaunchPlan, LaunchRequest, SmokeFallback, DEFAULT_PROXY_PORT,
};
use localbox_launcher::profile::{
    adopt_superseded_profile, resolve_run_profile, RunProfileQuery, RunProfileResolution,
};
use localx_llama_core::{Launcher, Mode};
use localx_llama_runtime::proxy::{serve_proxy_on, ProxyConfig};

const DEFAULT_SERVER_PORT: u16 = 8080;

const USAGE: &str = "\
localbox — run a local model and the coding agent of your choice

Usage:
  localbox                            guided launcher: pick or add a model
  localbox --plain                    guided launcher with plain-text menus
  localbox launch <model> [options]   resolve, start, and hand off to an agent
  localbox serve <model> [options]    start the model (and proxy) headless
                                      launch/serve take --if-running
                                      <ask|continue|stop|cancel> for when a model
                                      is already running (default: ask on a
                                      terminal, cancel without one)
  localbox probe <model> [options] [--fallback] [--keep-serving] [--json]
                                      prove the installed engine can load the
                                      model and answer (a short serve, a
                                      one-word reply, then stop); --fallback
                                      tries the Vulkan, then the CPU build when
                                      the CUDA build fails, keeping the
                                      original until one passes; the verified
                                      backend is recorded and shown by status
  localbox starter [--download [--expect <offer>]] [--json] [--vram-gb <n>] [--vram-free-gb <n>] [--ram-gb <n>]
                   [--ram-available-gb <n>]
                                      recommend a small starter model sized to
                                      this machine's memory and show its
                                      download size; --download fetches it and
                                      checks it against the SHA-256 the Hub
                                      publishes; --json prints one object for
                                      scripts (status on stderr); the memory
                                      flags replace the probed figures
  localbox stop                       stop every model server and the proxy
  localbox status                     report serve health and the remedy
  localbox info [model]               list the configured models, or one in detail
  localbox models [--json] [--proxy-port <p>]
                                      list launchable models, tuned-profile state
                                      and the model the proxy serves now
  localbox download <model|hf-repo> [--quant <key>] [--vision] [--draft]
                    [--mmproj <filename>]
                                      fetch a model's files without starting it
                                      (the GGUF; --vision/--draft add the
                                      catalog's projector/draft model); resumable.
                                      <model> is a catalog name, or a Hugging Face
                                      repo id (owner/repo) or URL — a repo not yet
                                      in the catalog has all quants added; only the
                                      selected variant is fetched (--quant picks it).
                                      To register without downloading, use the
                                      guided launcher's Add Model action.
  localbox purge                      stop servers and delete downloaded model files
  localbox clean --partials [--check] remove partial downloads LocalBox will not
                                      continue (--check previews, removes nothing)
  localbox log [--lines <n>]          tail the most recent server log
  localbox embed-serve [--port <p>]   start the CPU-only embedding server
  localbox embed-stop                 stop the embedding server
  localbox update [--mode <m>] [--check] [--allow-downgrade] [--merge-models]
                  [--skip-load-probe] [--prune-models]
                  [--variant <cuda|vulkan|cpu|auto>]
                                      install or update the llama.cpp binaries
                                      to the newest upstream release, verified
                                      against the published release digest and
                                      recorded in settings.json; --check
                                      previews without writing;
                                      --allow-downgrade permits installing a
                                      release older than the installed build;
                                      a staged build must report its version
                                      before it replaces a working engine,
                                      which --skip-load-probe waives;
                                      --merge-models adds newly shipped catalog
                                      models to llm-models.json (additive only,
                                      existing entries untouched);
                                      --prune-models removes retired fields
                                      from llm-models.json (the ones each load
                                      warns about); --variant installs that
                                      native build and keeps it on later
                                      updates (auto returns the choice to
                                      the driver and GPU checks)
  localbox version                    print the launcher version envelope
  localbox nothink-proxy --listen <port> --target-port <port>
                                      host the no-think proxy (plumbing)

Options for launch/serve:
  --context <key>       context window key from the model catalog (e.g. 64k)
  --mode <m>            native | turboquant | prism              (default native)
  --quant <key>         quant variant from the catalog (default per model)
  --auto-best           compatibility spelling: require the saved tuned profile
  --no-auto-best        explicitly use catalog/settings defaults instead
  --allow-untuned       allow non-interactive fallback when no tuned profile exists
  --json                serve only: print a JSON receipt (profile, arguments)
                        once the server is up; status lines go to stderr
  --prefer-profile <p>  rank compatible saved tunes of kind pure or balanced
                        first (a preference: another compatible tune is used
                        when none of that kind exists)
  --adopt-tune          carry a matching older tune into the current version
  --vision              load the vision projector when the model has one
  --draft               load the catalog's draft model for speculative
                        decoding (faster generation; needs a DraftModule)
  --keep-thinking       route the agent straight at the server so thinking
                        reaches it unfiltered (bypasses the no-think proxy, so
                        its system-message merge does not apply)
  --agent <a>           claude | localpilot | codex | none  (default claude)
  --server-arg <arg>    pass one raw argument to llama-server for this launch
                        (repeatable, appended last, never saved), e.g.
                        --server-arg --spec-type --server-arg ngram-mod
  --dry-run             print what would happen; change nothing
  --lan                 expose the gateway on the network (0.0.0.0)
  --password <p>        the key LAN clients must present (with --lan)
  --allow-public-no-auth  explicit opt-in to an open public gateway
";

fn main() -> ExitCode {
    let worker = std::thread::Builder::new()
        .name("localbox-main".to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn(run);
    match worker.map(std::thread::JoinHandle::join) {
        Ok(Ok(code)) => code,
        _ => {
            eprintln!("Error: LocalBox could not start its worker thread.");
            ExitCode::FAILURE
        }
    }
}

fn run() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("");
    let result = match command {
        "" => run_guided(false),
        "--plain" => run_guided(true),
        "launch" => cmd_launch(&args[1..], AgentKind::Claude),
        "serve" => cmd_serve(&args[1..]),
        "stop" => cmd_stop(),
        "status" => cmd_status(&args[1..]),
        "info" => cmd_info(&args[1..]),
        "models" => cmd_models(&args[1..]),
        "bench-runtime" => cmd_bench_runtime(&args[1..]),
        "download" => cmd_download(&args[1..]),
        "purge" => cmd_purge(),
        "clean" => cmd_clean(&args[1..]),
        "log" => cmd_log(&args[1..]),
        "embed-serve" => cmd_embed_serve(&args[1..]),
        "embed-stop" => cmd_embed_stop(),
        "update" => cmd_update(&args[1..]),
        "probe" => cmd_probe(&args[1..]),
        "starter" => cmd_starter(&args[1..]),
        "version" => cmd_version(),
        "nothink-proxy" => cmd_nothink_proxy(&args[1..]),
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            Ok(())
        }
        "--version" | "-V" => cmd_version(),
        other => Err(format!(
            "unknown command '{other}'. Run `localbox help` for the command list."
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("Error: {message}");
            ExitCode::FAILURE
        }
    }
}

// Versioned, machine-readable handoff to independently installed LocalBench.
fn cmd_bench_runtime(args: &[String]) -> Result<(), String> {
    let key = args.first().ok_or("bench-runtime needs a model key")?;
    let home = home_dir().ok_or("could not determine the user home directory")?;
    let catalog = Catalog::load(&home.join(".local-llm")).map_err(|error| error.to_string())?;
    let launcher = localbox::exec::build_launcher_with(catalog, &home);
    let mut def = launcher.model_def(key).map_err(|error| error.to_string())?;
    let root = launcher
        .inkling2_runtime_root(key)
        .ok_or_else(|| format!("{key} has no dedicated benchmark runtime"))?;
    let server = root.join(localx_llama_runtime::server::server_exe_name());
    localbox::live::ensure_inkling2_runtime(&server).map_err(|error| error.to_string())?;
    let mut defaults = localx_llama_core::args::LaunchParams::default();
    localbox_launcher::orchestrate::apply_inkling2_launch_defaults(&mut def, &mut defaults, false);
    println!(
        "{}",
        serde_json::json!({
            "schema": 1,
            "server": server,
            "model": def,
            "batch_size": defaults.batch_size,
            "ubatch_size": defaults.ubatch_size,
        })
    );
    Ok(())
}

fn cmd_version() -> Result<(), String> {
    let envelope = product_envelope();
    let rendered =
        serde_json::to_string_pretty(&envelope).map_err(|e| format!("envelope render: {e}"))?;
    println!("{rendered}");
    Ok(())
}

/// The value following `--flag`, when present.
fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

/// Every value given for a repeatable flag, in order.
fn flag_values<'a>(args: &'a [String], flag: &str) -> Vec<&'a str> {
    args.iter()
        .enumerate()
        .filter(|(_, a)| *a == flag)
        .filter_map(|(i, _)| args.get(i + 1).map(String::as_str))
        .collect()
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn parse_mode(value: Option<&str>) -> Result<Mode, String> {
    match value.unwrap_or("native") {
        "native" => Ok(Mode::Native),
        "turboquant" => Ok(Mode::Turboquant),
        "prism" | "prismml" => Ok(Mode::PrismMl),
        other => Err(match localbox_launcher::catalog::retired_mode_note(other) {
            Some(note) => note.to_string(),
            None => format!("unknown mode '{other}' (expected native, turboquant, or prism)"),
        }),
    }
}

fn cli_mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::PrismMl => "prism",
        _ => mode.as_str(),
    }
}

fn parse_agent(value: Option<&str>, default: AgentKind) -> Result<AgentKind, String> {
    match value {
        None => Ok(default),
        Some("claude") => Ok(AgentKind::Claude),
        Some("localpilot") => Ok(AgentKind::LocalPilot),
        Some("codex") => Ok(AgentKind::Codex),
        Some("none") | Some("serve") => Ok(AgentKind::ServeOnly),
        Some(other) => Err(format!(
            "unknown agent '{other}' (expected claude, localpilot, codex, or none)"
        )),
    }
}

/// Build the launch request a `launch`/`serve` invocation asks for. Called for
/// the primary plan and — with profiles disabled — for the native retry
/// after a failed smoke, so the retry re-derives clean defaults instead of
/// carrying fork-tuned AutoBest params/quant/context into the native build.
fn build_request(
    args: &[String],
    model: &str,
    home: &std::path::Path,
    launcher: &LlamaLauncher,
    disable_profile: bool,
) -> Result<(LaunchRequest, Option<RunProfileResolution>), String> {
    let key = launcher.resolve_model_key(model).ok_or_else(|| {
        format!("unknown model '{model}' (run `localbox models` for accepted names)")
    })?;
    launcher.model_def(&key).map_err(|e| e.to_string())?;
    let required_mode = launcher.required_mode(&key);
    let explicit_mode = flag_value(args, "--mode").is_some();
    let mut mode = parse_mode(flag_value(args, "--mode"))?;
    if let Some(required) = required_mode {
        if explicit_mode && mode != required {
            return Err(format!(
                "{key} requires --mode {}; '{}' is incompatible",
                cli_mode_name(required),
                cli_mode_name(mode)
            ));
        }
        mode = required;
    }
    let mut request = LaunchRequest::new(
        key,
        flag_value(args, "--context").unwrap_or("").to_string(),
        mode,
    );
    request.quant = flag_value(args, "--quant").map(str::to_string);
    request.use_vision = has_flag(args, "--vision");
    request.use_draft = has_flag(args, "--draft");
    request.keep_thinking = has_flag(args, "--keep-thinking");

    let explicit_quant = flag_value(args, "--quant").is_some();
    let explicit_context = flag_value(args, "--context").is_some();
    if has_flag(args, "--adopt-tune") && has_flag(args, "--dry-run") {
        return Err(
            "--adopt-tune cannot be combined with --dry-run because adoption updates the saved tune"
                .to_string(),
        );
    }
    if has_flag(args, "--adopt-tune") && has_flag(args, "--no-auto-best") {
        return Err("--adopt-tune cannot be combined with --no-auto-best".to_string());
    }
    let preferred = parse_preferred_profile(args)?;
    let mut profile = (!disable_profile && !has_flag(args, "--no-auto-best")).then(|| {
        resolve_run_profile(
            home,
            &request.key,
            RunProfileQuery {
                quant: explicit_quant.then_some(request.quant.as_deref()).flatten(),
                context_key: explicit_context.then_some(request.context_key.as_str()),
                mode: (explicit_mode || required_mode.is_some()).then_some(request.mode),
                preferred_profile: preferred.or_else(|| {
                    launcher
                        .inkling2_runtime_root(&request.key)
                        .map(|_| localx_llama_core::tuner::Profile::Balanced)
                }),
                ..RunProfileQuery::default()
            },
        )
    });
    if !disable_profile && has_flag(args, "--adopt-tune") {
        let profile = profile
            .as_mut()
            .ok_or_else(|| "--adopt-tune requires AutoBest profile resolution".to_string())?;
        if !profile.is_tuned() {
            adopt_superseded_profile(profile).map_err(|error| {
                format!("--adopt-tune could not adopt the saved profile: {error}")
            })?;
        }
    }
    if let Some(profile) = &profile {
        profile.apply_to_request(
            &mut request,
            explicit_mode || required_mode.is_some(),
            explicit_quant,
            explicit_context,
        );
    }

    // Settings are the lowest precedence (an AutoBest profile or a flag already
    // arrived as `Some(_)`), then the single-session one-slot default — for
    // every launch, headless `serve` included: llama-server's own `--parallel`
    // default is now multi-slot auto, which allocates the full context per slot
    // and OOMs a model sized for one slot. See `apply_session_defaults`.
    request.apply_session_defaults(&launcher.settings_launch_params());
    // Raw llama-server arguments for this launch only: appended last, so they
    // win over everything above, and never saved.
    request.params.extra_args.extend(
        flag_values(args, "--server-arg")
            .into_iter()
            .map(str::to_string),
    );
    Ok((request, profile))
}

/// `--prefer-profile <pure|balanced>`: rank compatible saved tunes of that
/// kind first. A preference, not a filter: with none of that kind, the next
/// compatible saved tune is used, and defaults only when there is none.
fn parse_preferred_profile(
    args: &[String],
) -> Result<Option<localx_llama_core::tuner::Profile>, String> {
    match flag_value(args, "--prefer-profile") {
        Some(raw) => serde_json::from_value(serde_json::json!(raw.to_ascii_lowercase()))
            .map(Some)
            .map_err(|_| format!("--prefer-profile takes pure or balanced, not '{raw}'")),
        None if has_flag(args, "--prefer-profile") => {
            Err("--prefer-profile takes pure or balanced".to_string())
        }
        None => Ok(None),
    }
}

/// The run profile a launch resolved, for `--json` callers: the saved tune's
/// identity, or which defaults were used.
fn run_profile_json(args: &[String], profile: Option<&RunProfileResolution>) -> serde_json::Value {
    match profile.and_then(|resolution| resolution.entry.as_ref().map(|entry| (resolution, entry)))
    {
        Some((resolution, entry)) => serde_json::json!({
            "source": "tuned",
            "profile": entry.profile,
            "quant": entry.quant,
            "context": entry.context_key,
            "mode": entry.mode.as_str(),
            "score": entry.score,
            "scoreUnit": entry.score_unit,
            "path": resolution.path.display().to_string(),
        }),
        None => serde_json::json!({
            "source": "defaults",
            "why": if has_flag(args, "--no-auto-best") { "no-auto-best" } else { "no compatible saved tune" },
            "path": profile.map(|resolution| resolution.path.display().to_string()),
        }),
    }
}

fn confirm_profile_fallback(
    args: &[String],
    key: &str,
    profile: Option<&RunProfileResolution>,
) -> Result<(), String> {
    confirm_profile_fallback_with_terminal(args, key, profile, std::io::stdin().is_terminal())
}

fn confirm_profile_fallback_with_terminal(
    args: &[String],
    key: &str,
    profile: Option<&RunProfileResolution>,
    stdin_is_terminal: bool,
) -> Result<(), String> {
    let Some(profile) = profile.filter(|profile| !profile.is_tuned()) else {
        return Ok(());
    };
    let warning = profile
        .warning(key)
        .unwrap_or_else(|| "Warning: tuned settings are unavailable.".to_string());
    eprintln!("{warning}");
    if has_flag(args, "--auto-best") {
        return Err("--auto-best requires a usable tuned profile; launch cancelled".to_string());
    }
    if has_flag(args, "--allow-untuned") || has_flag(args, "--dry-run") {
        return Ok(());
    }
    if !stdin_is_terminal {
        return Err(
            "refusing an untuned non-interactive launch; configure LocalBench or retry with --allow-untuned after reviewing the warning"
                .to_string(),
        );
    }
    eprint!("Continue with LocalBox defaults? [y/N]: ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| format!("could not read the fallback choice: {e}"))?;
    if matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(
            "launch cancelled; run `localbench findbest <model>` to configure tuned settings"
                .to_string(),
        )
    }
}

/// Resolve `--if-running` into a launch policy.
///
/// Absent, the launch asks on a terminal and cancels without one — a scripted
/// launch must never silently stop a model someone else is using.
///
/// # Errors
/// A plain message naming the accepted values.
fn resolve_running_policy(args: &[String]) -> Result<RunningModelPolicy, String> {
    match flag_value(args, "--if-running") {
        None => Ok(RunningModelPolicy::Ask),
        Some(value) => match value.trim().to_ascii_lowercase().as_str() {
            "ask" => Ok(RunningModelPolicy::Ask),
            "continue" => Ok(RunningModelPolicy::Continue),
            "stop" => Ok(RunningModelPolicy::StopFirst),
            "cancel" => Ok(RunningModelPolicy::Cancel),
            other => Err(format!(
                "unknown --if-running value '{other}': expected ask, continue, stop, or cancel"
            )),
        },
    }
}

/// `serve`: a launch with no agent. With `--json`, a failure is one object too.
fn cmd_serve(args: &[String]) -> Result<(), String> {
    let result = cmd_launch(args, AgentKind::ServeOnly);
    if let Err(error) = &result {
        if has_flag(args, "--json") {
            println!(
                "{}",
                serde_json::json!({
                    "ok": false,
                    "model": args.first().filter(|a| !a.starts_with("--")),
                    "error": error,
                })
            );
        }
    }
    result
}

fn cmd_launch(args: &[String], default_agent: AgentKind) -> Result<(), String> {
    // Validate before resolving a model or touching the network: a typo in
    // this flag decides what happens to someone's running model, so it must
    // not surface only after the slow work is done.
    let running_policy = resolve_running_policy(args)?;
    let model = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .ok_or("a model key is required (run `localbox launch <model>`)")?;
    let home = home_dir().ok_or("could not determine the user home directory")?;

    let agent = parse_agent(flag_value(args, "--agent"), default_agent)?;
    let launcher = build_launcher(&home)?;
    let (request, profile) = build_request(args, model, &home, &launcher, false)?;
    confirm_profile_fallback(args, &request.key, profile.as_ref())?;
    print_run_profile(args, profile.as_ref());

    let mut plan = plan_launch(&launcher, &request).map_err(|e| e.to_string())?;
    apply_launch_posture(&mut plan, args, agent)?;
    launch_after_plan(
        args,
        model,
        &home,
        &launcher,
        &request,
        plan,
        agent,
        running_policy,
        profile.as_ref(),
    )
}

/// Say which run profile a launch uses: the saved tune (its identity and
/// score) or the catalog defaults.
fn print_run_profile(args: &[String], profile: Option<&RunProfileResolution>) {
    if let Some(entry) = profile.and_then(|profile| profile.entry.as_ref()) {
        eprintln!(
            "Run profile: tuned · {:?} · {} · {} · {} (score {:.0} {}) · {}",
            entry.profile,
            entry.quant,
            entry.context_key,
            entry.mode.as_str(),
            entry.score,
            entry.score_unit,
            profile
                .as_ref()
                .map_or_else(String::new, |p| p.path.display().to_string())
        );
    } else if has_flag(args, "--no-auto-best") {
        eprintln!("Run profile: LocalBox catalog/settings defaults (--no-auto-best).");
    } else {
        eprintln!("Run profile: LocalBox catalog/settings defaults (approved fallback).");
    }
}

/// Run a resolved launch plan: preview it, or start it and hand off, with the
/// fork-to-native retry when a fork build fails its reply check.
#[allow(clippy::too_many_arguments)]
fn launch_after_plan(
    args: &[String],
    model: &str,
    home: &std::path::Path,
    launcher: &LlamaLauncher,
    request: &LaunchRequest,
    mut plan: LaunchPlan,
    agent: AgentKind,
    running_policy: RunningModelPolicy,
    profile: Option<&RunProfileResolution>,
) -> Result<(), String> {
    if has_flag(args, "--dry-run") {
        print_plan(&plan);
        return Ok(());
    }
    let mut retried_native = false;

    // A fork build (turboquant/prism) that fails its reply check falls back to
    // native llama.cpp once, rather than hard-stopping. The retry re-derives its
    // request with AutoBest off — the fork's tuned params/quant/context don't
    // apply to the native build (fork-only KV types would even fail its plan) —
    // and the failed launch already tore its own server/proxy down.
    let outcome = match execute_launch(launcher, &plan, request, agent, home, running_policy) {
        Ok(outcome) => outcome,
        Err(LiveError::Smoke(detail))
            if smoke_fallback(request.mode) == SmokeFallback::RetryNative =>
        {
            eprintln!(
                "The {} build failed the reply check ({detail}).\n\
                 Retrying on native llama.cpp …",
                request.mode.as_str()
            );
            let (mut native, _) = build_request(args, model, home, launcher, true)?;
            native.mode = Mode::Native;
            let mut native_plan = plan_launch(launcher, &native).map_err(|e| e.to_string())?;
            apply_launch_posture(&mut native_plan, args, agent)?;
            // Same user action, second attempt: the question was already
            // answered (and the failed launch tore its own server down), so
            // asking again would be asking twice for one launch.
            let outcome = execute_launch(
                launcher,
                &native_plan,
                &native,
                agent,
                home,
                RunningModelPolicy::Continue,
            )
            .map_err(|e| e.to_string())?;
            plan = native_plan;
            retried_native = true;
            outcome
        }
        Err(e) => return Err(e.to_string()),
    };
    if agent == AgentKind::ServeOnly {
        let report = status_report(plan.proxy.listen_port, plan.server_port);
        if has_flag(args, "--json") {
            // stdout carries the receipt alone; the human lines move to stderr.
            eprintln!("{report}");
            eprintln!("Serving {} at {}", plan.key, plan.base_url);
            // The native retry re-derived its request with profiles off.
            let used = if retried_native { None } else { profile };
            let mode = if retried_native {
                Mode::Native
            } else {
                request.mode
            };
            let backend = localbox::fallback::Stamp::read(&launcher.install_root(mode));
            println!(
                "{}",
                serve_receipt(args, &plan, used, retried_native, backend.as_ref())
            );
        } else {
            println!("{report}");
            println!("Serving {} at {}", plan.key, plan.base_url);
        }
    }
    let _ = outcome;
    Ok(())
}

/// What `serve --json` started: the run profile the server was launched with,
/// the exact arguments it runs and the engine build it runs on, so a caller
/// can check the live server rather than an earlier probe.
fn serve_receipt(
    args: &[String],
    plan: &LaunchPlan,
    profile: Option<&RunProfileResolution>,
    retried_native: bool,
    backend: Option<&localbox::fallback::Stamp>,
) -> serde_json::Value {
    let mut profile_json = run_profile_json(args, profile);
    if retried_native {
        profile_json["why"] = serde_json::json!("native retry after a failed reply check");
    }
    serde_json::json!({
        "ok": true,
        "model": plan.key,
        "endpoint": plan.base_url,
        "serverPort": plan.server_port,
        "proxyPort": plan.proxy.listen_port,
        "contextTokens": plan.context_tokens,
        "retriedNative": retried_native,
        "backend": backend.map(|stamp| serde_json::json!({"tag": stamp.tag, "variant": stamp.variant})),
        "profile": profile_json,
        "launchArguments": plan.argv,
    })
}

fn print_plan(plan: &localbox_launcher::orchestrate::LaunchPlan) {
    println!("Model:     {} ({} tokens)", plan.key, plan.context_tokens);
    println!(
        "GGUF:      {} ({})",
        plan.gguf_path.display(),
        if plan.gguf_downloaded {
            "downloaded"
        } else {
            "will download"
        }
    );
    if let Some(vision) = &plan.vision_module {
        println!(
            "Vision:    {} ({})",
            vision.display(),
            if plan.vision_module_downloaded {
                "downloaded"
            } else {
                "will download"
            }
        );
    }
    if let Some(drafter) = &plan.draft_module {
        println!(
            "Drafter:   {} ({})",
            drafter.display(),
            if plan.draft_module_downloaded {
                "downloaded"
            } else {
                "will download"
            }
        );
    }
    println!("Server:    127.0.0.1:{}", plan.server_port);
    if let Some(binary) = &plan.inkling2_binary {
        println!("Runtime:   {}", binary.display());
    }
    println!("Endpoint:  {}", plan.base_url);
    println!("Command:   {}", plan.argv.join(" "));
    if !plan.env_plan.is_empty() {
        println!("Env:");
        for (name, value) in &plan.env_plan {
            println!("  {name}={value}");
        }
    }
    for note in &plan.notes {
        println!("Note:      {note}");
    }
}

/// The no-think proxy port this installation actually uses: the configured
/// `NoThinkProxyPort`, else [`DEFAULT_PROXY_PORT`]. Stop, purge and status all
/// resolve it the same way `launch` does, so a configured port cannot leave a
/// proxy that `stop` fails to reap and `status` reports as down.
///
/// Best-effort by design: a catalog that cannot be read must not stop `stop`
/// from working, and the default is the port such an installation opened.
fn resolved_proxy_port(home: &std::path::Path) -> u16 {
    Catalog::load(&catalog_dir(home))
        .ok()
        .and_then(|catalog| catalog.no_think_proxy_port())
        .unwrap_or(DEFAULT_PROXY_PORT)
}

fn cmd_stop() -> Result<(), String> {
    let home = home_dir().ok_or("could not determine the user home directory")?;
    let report = stop_all_with(
        &home,
        &[resolved_proxy_port(&home)],
        &mut print_stop_progress,
    );
    finish_stop(&report)
}

/// Show what a stop found, then a dot a second while it waits for the exits.
fn print_stop_progress(step: StopProgress<'_>) {
    use std::io::Write;
    match step {
        StopProgress::Found([]) => {}
        StopProgress::Found(found) => {
            let list: Vec<String> = found.iter().map(StopTarget::describe).collect();
            println!("Found {} process(es): {}", found.len(), list.join(", "));
            print!("Stopping ");
            let _ = std::io::stdout().flush();
        }
        StopProgress::Waiting => {
            print!(".");
            let _ = std::io::stdout().flush();
        }
    }
}

/// Close the line `print_stop_progress` left open and report the outcome.
fn finish_stop(report: &StopReport) -> Result<(), String> {
    if report.found.is_empty() {
        println!("Nothing was running.");
        return Ok(());
    }
    if report.remaining.is_empty() {
        println!(" done");
        println!("{0}/{0} processes stopped.", report.stopped);
        Ok(())
    } else {
        println!();
        Err(describe_stop_remaining(report))
    }
}

/// What status says about the engine build: the backend a probe last
/// verified, and a fallback build in use.
fn engine_backend_lines(settings_path: &std::path::Path) -> Vec<String> {
    use localbox::fallback::{read_settings, FALLBACK_SETTING, VERIFIED_BACKEND_SETTING};
    let Ok(settings) = read_settings(settings_path) else {
        return Vec::new();
    };
    let text = |v: &serde_json::Value, key: &str| {
        v.get(key)
            .and_then(|x| x.as_str())
            .unwrap_or("?")
            .to_string()
    };
    let mut lines = Vec::new();
    if let Some(v) = settings.get(VERIFIED_BACKEND_SETTING) {
        lines.push(format!(
            "Verified backend: {} build {} answered with {}.",
            text(v, "variant"),
            text(v, "tag"),
            text(v, "model")
        ));
    }
    if let Some(v) = settings.get(FALLBACK_SETTING) {
        lines.push(format!(
            "Backend fallback: using the {} build because the {} build failed on driver {}; \
             a driver change retries it on the next `localbox update`.",
            text(v, "variant"),
            text(v, "from"),
            text(v, "driver")
        ));
    }
    lines
}

fn cmd_status(args: &[String]) -> Result<(), String> {
    let proxy_port = match flag_value(args, "--proxy-port") {
        Some(v) => v.parse().map_err(|_| format!("bad port '{v}'"))?,
        None => home_dir()
            .as_deref()
            .map_or(DEFAULT_PROXY_PORT, resolved_proxy_port),
    };
    let server_port = flag_value(args, "--server-port")
        .map(|v| v.parse().map_err(|_| format!("bad port '{v}'")))
        .transpose()?
        .unwrap_or(DEFAULT_SERVER_PORT);
    println!("{}", status_report(proxy_port, server_port));
    if let Some(home) = home_dir() {
        for line in engine_backend_lines(&catalog_dir(&home).join("settings.json")) {
            println!("{line}");
        }
        let notice =
            localbox::migrate::v1_leftover_notice(&localbox::migrate::find_v1_leftovers(&home));
        if !notice.is_empty() {
            println!("{notice}");
        }
    }
    Ok(())
}

fn cmd_info(args: &[String]) -> Result<(), String> {
    use localbox::manage::{render_model_detail, render_model_overview};
    let home = home_dir().ok_or("could not determine the user home directory")?;
    let catalog = Catalog::load(&catalog_dir(&home)).map_err(|e| e.to_string())?;
    match args.first().filter(|a| !a.starts_with("--")) {
        Some(name) => {
            print!("{}", render_model_detail(&catalog, name)?);
            let key = localbox::manage::resolve_model_key(&catalog, name);
            if let Some((root, entries)) = partial_downloads(&catalog, &home) {
                let own: Vec<_> = entries
                    .into_iter()
                    .filter(|entry| entry.model.is_some() && entry.model == key)
                    .collect();
                if !own.is_empty() {
                    println!("  Partial downloads:");
                    print!("{}", localbox::partials::render(&own, &root));
                }
            }
        }
        None => print!("{}", render_model_overview(&catalog)),
    }
    Ok(())
}

/// What the LocalBox proxy on `proxy_port` serves now: its target port from
/// its own argv, the model server on that port, and the catalog key for the
/// file that server loaded.
fn observe_serving(
    home: &std::path::Path,
    catalog: &Catalog,
    proxy_port: u16,
) -> localbox::manage::Serving {
    let mut serving = localbox::manage::Serving {
        proxy_port,
        target_port: None,
        model_path: None,
        model: None,
        reason: None,
    };
    let target = match localbox::exec::proxy_target_port(proxy_port) {
        Ok(target) => target,
        Err(reason) => {
            serving.reason = Some(reason);
            return serving;
        }
    };
    serving.target_port = Some(target);
    // The model server is the one process that holds the target socket on any
    // address, not one whose argv merely names the port.
    let mut owners = localbox::exec::os_listener_pids(target);
    owners.sort_unstable();
    owners.dedup();
    if owners.len() > 1 {
        serving.reason = Some(format!(
            "more than one process listens on port {target}, so which one serves it is not certain"
        ));
        return serving;
    }
    let servers: Vec<_> = localbox::live::running_models(home)
        .into_iter()
        .filter(|model| owners.contains(&model.pid))
        .collect();
    let path = match servers.as_slice() {
        [] => {
            serving.reason = Some(format!("no model server is listening on port {target}"));
            return serving;
        }
        [server] => server.path.clone(),
        _ => {
            serving.reason = Some(format!("more than one model server holds port {target}"));
            return serving;
        }
    };
    let Some(path) = path else {
        serving.reason = Some("the model server's command line names no model file".to_string());
        return serving;
    };
    serving.model_path = Some(path.clone());
    let launcher = match localbox::exec::build_launcher(home) {
        Ok(launcher) => launcher,
        Err(error) => {
            serving.reason = Some(error);
            return serving;
        }
    };
    let candidates: Vec<(String, Vec<std::path::PathBuf>)> = catalog
        .model_keys()
        .into_iter()
        .filter_map(|key| {
            let def = catalog.model(key)?;
            let paths: Vec<std::path::PathBuf> = if def.quants.is_empty() {
                launcher
                    .expected_gguf_path(def, None)
                    .ok()
                    .into_iter()
                    .collect()
            } else {
                def.quants
                    .keys()
                    .filter_map(|quant| launcher.expected_gguf_path(def, Some(quant)).ok())
                    .collect()
            };
            Some((key.to_string(), paths))
        })
        .collect();
    match localbox::manage::serving_key(&path, &candidates) {
        Ok(key) => serving.model = Some(key.to_string()),
        Err(reason) => serving.reason = Some(reason.to_string()),
    }
    serving
}

fn cmd_models(args: &[String]) -> Result<(), String> {
    use localbox::manage::{models_catalog, render_models_catalog};
    let home = home_dir().ok_or("could not determine the user home directory")?;
    let catalog = Catalog::load(&catalog_dir(&home)).map_err(|e| e.to_string())?;
    let mut contract = models_catalog(&catalog, &home);
    let proxy_port = match flag_value(args, "--proxy-port") {
        Some(v) => v.parse().map_err(|_| format!("bad port '{v}'"))?,
        None => resolved_proxy_port(&home),
    };
    localbox::manage::mark_serving(&mut contract, observe_serving(&home, &catalog, proxy_port));
    if has_flag(args, "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&contract)
                .map_err(|e| format!("model catalog render: {e}"))?
        );
    } else {
        print!("{}", render_models_catalog(&contract));
        if let Some((root, entries)) = partial_downloads(&catalog, &home) {
            if !entries.is_empty() {
                println!("\nPartial downloads under {}:", root.display());
                print!("{}", localbox::partials::render(&entries, &root));
                if let Some(hint) = localbox::partials::hint(&entries) {
                    println!("{hint}");
                }
            }
        }
    }
    Ok(())
}

/// Put a model's files on disk without starting anything: the GGUF for the
/// requested (or default) quant, plus the catalog's projector / draft model on
/// request. Already-present files are skipped; an interrupted pull resumes.
fn cmd_download(args: &[String]) -> Result<(), String> {
    use localbox::fetch::ProgressPrinter;
    let model = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .ok_or("a model key is required (run `localbox download <model>`)")?;
    let home = home_dir().ok_or("could not determine the user home directory")?;
    let launcher = build_launcher(&home)?;
    let quant = flag_value(args, "--quant");
    // A name the catalog does not know may still be a Hugging Face repo id: fetch
    // its listing, write a catalog entry, and download — the from-HF install path.
    let Some(key) = launcher.resolve_model_key(model) else {
        return download_from_hf(model, quant, &home, args);
    };
    if has_flag(args, "--mmproj") {
        return Err("--mmproj selects a projector during Hugging Face import; for a catalog key edit VisionModule instead".to_string());
    }
    let kinds = download_kinds_for_flags(has_flag(args, "--vision"), has_flag(args, "--draft"));

    let targets = launcher
        .model_download_targets(&key, quant)
        .map_err(|e| e.to_string())?;
    let mut anything = false;
    for target in targets.iter().filter(|t| kinds.contains(&t.kind)) {
        anything = true;
        if target.present {
            println!("{} already on disk: {}", target.kind, target.path.display());
        }
    }
    if !anything {
        return Err(format!("{key} names no files to download"));
    }
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let mut printer = ProgressPrinter::default();
    let fetched = runtime
        .block_on(
            launcher.fetch_model_files(&key, quant, &kinds, &mut |progress| {
                printer.report(progress);
            }),
        )
        .map_err(|e| e.to_string())?;
    for path in &fetched {
        println!("ready: {}", path.display());
    }
    print_partials_hint(&home);
    Ok(())
}

/// Recommend the starter model for this machine's memory, show its download
/// size, and with `--download` fetch and verify it. `--json` prints one JSON
/// object on stdout (status lines and progress go to stderr); `--vram-gb`,
/// `--ram-gb` and `--ram-available-gb` replace the probed figures.
fn cmd_starter(args: &[String]) -> Result<(), String> {
    let json = has_flag(args, "--json");
    let mut printed = false;
    let result = run_starter(args, json, &mut printed);
    if let Err(error) = &result {
        // A script asked for one JSON object; give it one even when the
        // failure came before any recommendation could be made.
        if json && !printed {
            println!(
                "{}",
                serde_json::json!({
                    "schema": "localbox-starter-v1",
                    "status": "error",
                    "stage": "setup",
                    "error": error,
                })
            );
        }
    }
    result
}

fn run_starter(args: &[String], json: bool, printed: &mut bool) -> Result<(), String> {
    use localbox::starter::{probe_memory, recommend, to_json, Recommendation};
    let number = |flag: &str| -> Result<Option<f64>, String> {
        match flag_value(args, flag) {
            Some(raw) => raw
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite() && *value >= 0.0)
                .map(Some)
                .ok_or_else(|| format!("{flag} takes a number of GB, not '{raw}'")),
            None if has_flag(args, flag) => Err(format!("{flag} takes a number of GB")),
            None => Ok(None),
        }
    };
    let overrides = (
        number("--vram-gb")?,
        number("--ram-gb")?,
        number("--ram-available-gb")?,
    );
    let free_override = number("--vram-free-gb")?;
    let home = home_dir().ok_or("could not determine the user home directory")?;
    // Servers running now hold part of the GPU; the reason line names them.
    let probed = localbox::starter::note_running(
        probe_memory(localbox::starter::configured_vram_gb(&catalog_dir(&home))),
        &localbox::live::running_models(&home),
    );
    let memory = localbox::starter::Memory {
        vram_total_gb: overrides.0.unwrap_or(probed.vram_total_gb),
        // A stated total without a stated free amount is not this GPU's
        // measurement, so it is not paired with it.
        vram_free_gb: free_override.or(if overrides.0.is_some() {
            None
        } else {
            probed.vram_free_gb
        }),
        running_on_gpu: if free_override.is_some() || overrides.0.is_some() {
            0
        } else {
            probed.running_on_gpu
        },
        ram_total_gb: overrides.1.unwrap_or(probed.ram_total_gb),
        ram_available_gb: overrides
            .2
            .or(overrides.1)
            .unwrap_or(probed.ram_available_gb),
    };
    let source = if overrides == (None, None, None) && free_override.is_none() {
        "probe"
    } else {
        "override"
    };
    let recommendation = recommend(memory);
    let (rung, mode, context) = match &recommendation {
        Recommendation::Starter {
            rung,
            mode,
            context,
            ..
        } => (rung, mode, context),
        Recommendation::NoFit { reason } => {
            if json {
                *printed = true;
                println!("{}", to_json(&recommendation, memory, source, None));
            }
            return Err(reason.clone());
        }
    };
    // What was offered is what is fetched: a caller that showed an offer and
    // got a yes passes it back, and a recommendation that has changed since
    // (memory freed or taken in between) stops here, before any download.
    let offer = localbox::starter::offer_id(rung, *mode, context);
    if let Some(expected) = flag_value(args, "--expect") {
        if expected != offer {
            let error = format!(
                "the starter offer changed from {expected} to {offer} since it was shown \
                 (GPU or RAM use changed); look at the new offer before downloading"
            );
            if json {
                let mut value = to_json(&recommendation, memory, source, None);
                value["status"] = serde_json::json!("error");
                value["stage"] = serde_json::json!("offer");
                value["error"] = serde_json::json!(error);
                *printed = true;
                println!("{value}");
            }
            return Err(error);
        }
    }
    // The Hub's own listing gives the exact size; without a network the
    // ladder's figure stands, labelled as approximate.
    let live_size = localbox_launcher::hf_meta::parse_hf_ref(rung.repo)
        .ok()
        .and_then(|hf| localbox::hf_install::discover_hf_repo(hf).ok())
        .and_then(|discovery| discovery.select(Some(rung.quant)).ok())
        .and_then(|candidate| localbox::hf_install::candidate_size_gb(&candidate));
    let size = live_size.map(|gb| (gb, "hub"));
    let mut value = to_json(&recommendation, memory, source, size);
    if !json {
        let shown = match size {
            Some((gb, _)) => format!("{gb:.1} GB download"),
            None => format!(
                "about {:.1} GB download (the Hub could not be reached)",
                rung.size_gb
            ),
        };
        println!(
            "Starter model: {} ({} {}), {shown}",
            rung.label, rung.repo, rung.quant
        );
        println!(
            "Runs on: {} at {context} context",
            mode.as_str().to_uppercase()
        );
        if let Some(reason) = value["reason"].as_str() {
            println!("Why: {reason}");
        }
    }
    if !has_flag(args, "--download") {
        if json {
            *printed = true;
            println!("{value}");
        } else {
            println!("Download it with `localbox starter --download`.");
        }
        return Ok(());
    }
    let kinds = [localbox_launcher::fetch::DownloadKind::Gguf];
    let installed = match localbox::hf_install::install_from_hf(
        rung.repo,
        Some(rung.quant),
        None,
        Some(rung.projector),
        &kinds,
        true,
        &home,
        &mut |line| {
            if json {
                eprintln!("{line}");
            } else {
                println!("{line}");
            }
        },
    ) {
        Ok(installed) => installed,
        Err(error) => {
            if json {
                value["status"] = serde_json::json!("error");
                value["stage"] = serde_json::json!("download");
                value["error"] = serde_json::json!(error);
                *printed = true;
                println!("{value}");
            }
            return Err(error);
        }
    };
    let start = localbox::starter::start_plan(
        &catalog_dir(&home),
        &installed.key,
        &installed.quant,
        *mode,
        context,
    );
    value["catalog_key"] = serde_json::json!(installed.key);
    value["catalog_quant"] = serde_json::json!(installed.quant);
    value["files"] = serde_json::json!(installed
        .paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>());
    value["verified"] = serde_json::json!(installed
        .verified
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>());
    value["context_in_catalog"] = serde_json::json!(start.is_ok());
    match &start {
        Ok(argv) => value["serve_args"] = serde_json::json!(argv),
        Err(limitation) => value["limitation"] = serde_json::json!(limitation),
    }
    if json {
        *printed = true;
        println!("{value}");
    } else {
        for path in &installed.paths {
            let state = if installed.verified.contains(path) {
                "checked against the published SHA-256"
            } else {
                "not checked: no published SHA-256"
            };
            println!("ready: {} ({state})", path.display());
        }
        match &start {
            Ok(argv) => println!(
                "Start it with `localbox {}`. That uses the settings its memory need was \
                 measured with; if you have tuned this model with LocalBench, drop \
                 --no-auto-best to use the tune instead.",
                argv.join(" ")
            ),
            Err(limitation) => println!("{limitation}"),
        }
    }
    Ok(())
}

/// Install a model straight from a Hugging Face repo reference: resolve its GGUF
/// listing, catalog every quant, pick one (`--quant` or a default), and download
/// only that variant through the ordinary catalog-backed fetch path. Multi-part
/// quants fetch every shard; llama.cpp loads the split from the first one.
fn download_from_hf(
    model: &str,
    quant: Option<&str>,
    home: &std::path::Path,
    args: &[String],
) -> Result<(), String> {
    let requested = flag_value(args, "--mmproj").filter(|value| !value.starts_with("--"));
    if has_flag(args, "--mmproj") && requested.is_none() {
        return Err("--mmproj requires a projector filename".to_string());
    }
    let kinds = download_kinds_for_flags(has_flag(args, "--vision"), has_flag(args, "--draft"));
    let installed = localbox::hf_install::install_from_hf(
        model,
        quant,
        requested,
        None,
        &kinds,
        false,
        home,
        &mut |line| {
            println!("{line}");
        },
    )?;
    for path in installed.paths {
        println!("ready: {}", path.display());
    }
    print_partials_hint(home);
    Ok(())
}

/// Which artifacts `localbox download` fetches: always the GGUF, plus the
/// projector and/or draft model when their flags ask for them.
fn download_kinds_for_flags(
    vision: bool,
    draft: bool,
) -> Vec<localbox_launcher::fetch::DownloadKind> {
    use localbox_launcher::fetch::DownloadKind;
    let mut kinds = vec![DownloadKind::Gguf];
    if vision {
        kinds.push(DownloadKind::VisionModule);
    }
    if draft {
        kinds.push(DownloadKind::DraftModule);
    }
    kinds
}

fn cmd_purge() -> Result<(), String> {
    use localbox::manage::purge_targets;
    use localbox_launcher::launcher::expand_path_with_home;
    let home = home_dir().ok_or("could not determine the user home directory")?;
    let catalog = Catalog::load(&catalog_dir(&home)).map_err(|e| e.to_string())?;
    let root = catalog
        .gguf_root()
        .ok_or("LlamaCppGgufRoot is not configured; set it in settings.json")?;
    let root = expand_path_with_home(&root.to_string_lossy(), &home);

    let proxy_port = catalog.no_think_proxy_port().unwrap_or(DEFAULT_PROXY_PORT);
    let report = stop_all_with(&home, &[proxy_port], &mut print_stop_progress);
    // Deleting model files under a server that is still exiting would fail or
    // leave it running on deleted weights, so a stop that did not finish ends
    // the purge before it deletes anything.
    finish_stop(&report)?;
    let mut removed = 0;
    for folder in purge_targets(&catalog, &root) {
        if !folder.is_dir() {
            continue;
        }
        std::fs::remove_dir_all(&folder)
            .map_err(|e| format!("could not delete {}: {e}", folder.display()))?;
        println!("Deleted {}", folder.display());
        removed += 1;
    }
    if removed == 0 {
        println!(
            "No downloaded model files were found under {}.",
            root.display()
        );
    } else {
        println!("Done. Models download again on the next launch.");
    }
    Ok(())
}

/// The partial downloads under the GGUF root, classified; `None` when no
/// root is configured.
fn partial_downloads(
    catalog: &Catalog,
    home: &std::path::Path,
) -> Option<(std::path::PathBuf, Vec<localbox::partials::PartialEntry>)> {
    use localbox::partials::{classify, quants_in_use, scan};
    use localbox_launcher::launcher::expand_path_with_home;
    let root = catalog.gguf_root()?;
    let root = expand_path_with_home(&root.to_string_lossy(), home);
    let entries = classify(
        catalog,
        &root,
        scan(&root),
        &quants_in_use(catalog),
        &|path| {
            std::fs::metadata(path)
                .ok()
                .filter(std::fs::Metadata::is_file)
                .map(|meta| meta.len())
        },
        std::time::SystemTime::now(),
    );
    Some((root, entries))
}

/// After a download: one line pointing at `clean --partials` when partial
/// downloads LocalBox will not continue are taking space.
fn print_partials_hint(home: &std::path::Path) {
    let Ok(catalog) = Catalog::load(&catalog_dir(home)) else {
        return;
    };
    if let Some(hint) = partial_downloads(&catalog, home)
        .and_then(|(_, entries)| localbox::partials::hint(&entries))
    {
        println!("{hint}");
    }
}

/// Remove the partial downloads LocalBox will not continue: superseded,
/// unresumable, other-quant and orphaned ones. `--check` lists what would go
/// and removes nothing. Active partials are never touched.
fn cmd_clean(args: &[String]) -> Result<(), String> {
    use localbox::partials::{render, CleanPlan, IN_FLIGHT};
    use localbox_launcher::fetch::human_bytes;
    if !has_flag(args, "--partials") {
        return Err(
            "nothing selected to clean: use `localbox clean --partials [--check]`".to_string(),
        );
    }
    let home = home_dir().ok_or("could not determine the user home directory")?;
    let catalog = Catalog::load(&catalog_dir(&home)).map_err(|e| e.to_string())?;
    let (root, entries) = partial_downloads(&catalog, &home)
        .ok_or("LlamaCppGgufRoot is not configured; set it in settings.json")?;
    let candidates: Vec<_> = entries
        .into_iter()
        .filter(|entry| entry.state.is_candidate())
        .collect();
    if candidates.is_empty() {
        println!("No partial downloads to clean under {}.", root.display());
        return Ok(());
    }
    print!("{}", render(&candidates, &root));
    let plan = CleanPlan::from_entries(&candidates);
    if has_flag(args, "--check") {
        println!(
            "\nWould remove {} file(s), {}:",
            plan.files.len(),
            human_bytes(plan.bytes())
        );
        for (path, size) in &plan.files {
            println!("  {} ({})", path.display(), human_bytes(*size));
        }
        println!("Nothing was removed (--check). Run `localbox clean --partials` to remove them.");
        return Ok(());
    }
    let mut freed = 0;
    let mut failed = Vec::new();
    for (path, size) in &plan.files {
        // A download may have picked the partial up since the scan: leave it.
        let written_now = path.to_string_lossy().ends_with(".partial")
            && std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|idle| idle < IN_FLIGHT);
        if written_now {
            println!("Skipped {} (being written)", path.display());
            continue;
        }
        match std::fs::remove_file(path) {
            Ok(()) => {
                println!("Removed {}", path.display());
                freed += size;
            }
            Err(e) => failed.push(format!("{}: {e}", path.display())),
        }
    }
    println!("Freed {}.", human_bytes(freed));
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "could not remove {} file(s):\n  {}",
            failed.len(),
            failed.join("\n  ")
        ))
    }
}

fn cmd_log(args: &[String]) -> Result<(), String> {
    use localbox::manage::{newest_log, tail_lines};
    let lines: usize = match flag_value(args, "--lines") {
        Some(v) => v.parse().map_err(|_| format!("bad line count '{v}'"))?,
        None => 80,
    };
    let home = home_dir().ok_or("could not determine the user home directory")?;
    let logs_dir = home.join(".local-llm").join("logs");
    let Some(path) = newest_log(&logs_dir) else {
        println!(
            "No server logs yet (nothing under {}). Launch a model first.",
            logs_dir.display()
        );
        return Ok(());
    };
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    println!("Tail of {} (last {lines} lines):", path.display());
    println!("{}", tail_lines(&content, lines));
    Ok(())
}

fn cmd_embed_serve(args: &[String]) -> Result<(), String> {
    use localbox::embed;
    let home = home_dir().ok_or("could not determine the user home directory")?;
    let catalog = localbox_launcher::catalog::Catalog::load(&catalog_dir(&home))
        .map_err(|e| e.to_string())?;
    let mut config = embed::EmbedConfig::from_catalog(&catalog);
    if let Some(port) = flag_value(args, "--port") {
        config.port = port.parse().map_err(|_| format!("bad port '{port}'"))?;
    }

    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    if localx_llama_runtime::net::is_port_listening(config.port) {
        return match runtime.block_on(embed::probe_embeddings(config.port)) {
            Some(dims) => {
                println!(
                    "Embedding server already running on 127.0.0.1:{} ({dims} dimensions).",
                    config.port
                );
                Ok(())
            }
            None => Err(format!(
                "port {} is in use by something that does not answer embeddings; \
                 stop it or pick another port with --port",
                config.port
            )),
        };
    }

    let model = runtime.block_on(embed::ensure_embed_model(&catalog, &config, &home))?;
    let launcher = build_launcher(&home)?;
    let binary = localx_llama_core::Launcher::server_binary(&launcher, Mode::Native, true)
        .map_err(|e| e.to_string())?;
    let argv =
        localx_llama_runtime::server::embed_server_args(&model.to_string_lossy(), config.port);
    let log = home
        .join(".local-llm")
        .join("logs")
        .join("embed-server.log");
    let child = localbox::exec::spawn_server(&binary, &argv, &log).map_err(|e| e.to_string())?;

    if !localx_llama_runtime::server::wait_for_ready(
        config.port,
        std::time::Duration::from_secs(120),
    ) {
        return Err(format!(
            "the embedding server did not start — the log is at {}",
            log.display()
        ));
    }
    let dims = runtime
        .block_on(embed::probe_embeddings(config.port))
        .ok_or("the embedding server started but did not answer a probe")?;
    // Record the PID from the socket table, not the spawn handle: on Windows
    // the surviving server process is not always the direct child.
    let listener_pid = localbox::exec::os_listener_pids(config.port)
        .first()
        .copied()
        .or(Some(child.id()));
    embed::write_embed_state(
        &home,
        &embed::EmbedState {
            pid: listener_pid,
            port: config.port,
            base_url: format!("http://127.0.0.1:{}", config.port),
            model: model.to_string_lossy().to_string(),
            pooling: config.pooling.clone(),
        },
    );
    println!(
        "Embedding server running on 127.0.0.1:{} ({dims} dimensions, CPU-only).",
        config.port
    );
    Ok(())
}

fn cmd_embed_stop() -> Result<(), String> {
    let home = home_dir().ok_or("could not determine the user home directory")?;
    if localbox::embed::stop_embed(&home) {
        println!("Embedding server stopped.");
    } else {
        println!("No embedding server was running.");
    }
    Ok(())
}

fn cmd_update(args: &[String]) -> Result<(), String> {
    use localbox::update::{asset_set_summary, install_asset_set, plan_binary_update, UpdatePlan};
    let home = home_dir().ok_or("could not determine the user home directory")?;
    let catalog = localbox_launcher::catalog::Catalog::load(&catalog_dir(&home))
        .map_err(|e| e.to_string())?;
    // One load, one set of warnings: `build_launcher` would read the same three
    // layers again and print every unknown-field warning a second time.
    let launcher = localbox::exec::build_launcher_with(catalog.clone(), &home);
    let check_only = has_flag(args, "--check");
    // `--refresh-pins` used to be the opt-in for "resolve the latest
    // release". That is now what every update does, so the flag is accepted
    // and ignored rather than breaking scripts that still pass it.
    let _refresh = has_flag(args, "--refresh-pins");
    let allow_downgrade = has_flag(args, "--allow-downgrade");
    // The load probe is the gate that stops an archive missing a runtime
    // dependency from replacing a working engine. Skipping it is a last
    // resort for a host where the probe itself is wrong, not a routine flag.
    let load_probe = !has_flag(args, "--skip-load-probe");
    let explicit_mode = flag_value(args, "--mode");
    let chosen_variant = parse_variant_flag(flag_value(args, "--variant"))?;
    if chosen_variant.is_some() && explicit_mode.is_some_and(|m| m != "native") {
        return Err("--variant applies to the native engine only (--mode native)".to_string());
    }
    if has_flag(args, "--merge-models") {
        return merge_shipped_models(&home, check_only);
    }
    if has_flag(args, "--prune-models") {
        return prune_retired_model_fields(&home, check_only);
    }
    let modes: Vec<Mode> = match explicit_mode {
        Some(m) => vec![parse_mode(Some(m))?],
        None => vec![Mode::Native, Mode::Turboquant, Mode::PrismMl],
    };
    let driver = localbox::update::parse_cuda_driver_version(&nvidia_smi_banner()).map(|version| {
        localbox::update::CudaDriver {
            version,
            gpus: localbox::update::parse_compute_capabilities(&nvidia_compute_caps()),
        }
    });
    let driver_major = driver.as_ref().map(|d| d.version.major);
    // No NVIDIA driver but an AMD card present → the Vulkan build uses the GPU
    // instead of silently falling back to CPU.
    let amd_gpu = driver_major.is_none() && host_has_amd_gpu();
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let driver_id = localbox::fallback::driver_fingerprint(&nvidia_smi_banner());
    let settings_path = catalog_dir(&home).join("settings.json");
    let native_root = localx_llama_core::Launcher::install_root(&launcher, Mode::Native);
    let engine_dir = native_root
        .parent()
        .ok_or("the engine install root has no parent directory")?
        .to_path_buf();

    // Everything that reads or changes the engine and its settings runs as one
    // unit under the engine lock: settling an interrupted fallback, reading the
    // saved variant, planning, installing, and recording pins and the variant.
    // A check changes nothing, so it takes no lock.
    let update_all = |catalog: &localbox_launcher::catalog::Catalog,
                      launcher: &LlamaLauncher|
     -> Result<(), String> {
        // The variant this run installs: the one asked for now, else the one
        // the user chose before, else the build an automatic fallback moved to
        // while the driver is still the one it was decided on, else the host
        // decides. A driver change retires the fallback.
        use localbox::fallback::FallbackStanding;
        let standing = localbox::fallback::fallback_standing(
            &localbox::fallback::read_settings(&settings_path)?,
            driver_id.as_deref(),
        );
        let user_choice = catalog
            .setting_str(localbox::update::NATIVE_VARIANT_SETTING)
            .and_then(localbox::update::Variant::parse);
        let forced = match (chosen_variant, user_choice, &standing) {
            (Some(choice), _, _) => choice,
            (None, Some(choice), _) => Some(choice),
            (None, None, FallbackStanding::Holds(variant)) => {
                println!(
                    "Keeping the {} build an earlier backend fallback chose for this driver.",
                    variant.as_str()
                );
                Some(*variant)
            }
            (
                None,
                None,
                FallbackStanding::DriverChanged {
                    from,
                    model,
                    recorded,
                    now,
                },
            ) => {
                println!(
                    "The NVIDIA driver changed since the backend fallback (was {}, now {}): \
                     the {from} build is tried again. Check it with `localbox probe {model}`; \
                     if it still fails, `localbox probe {model} --fallback`.",
                    recorded.as_deref().unwrap_or("none"),
                    now.as_deref().unwrap_or("none"),
                );
                None
            }
            (None, None, FallbackStanding::None) => None,
        };
        // A fallback record is retired by a driver change or the user's own
        // choice, once this run has the native build it planned.
        let retire_fallback = !check_only
            && match standing {
                FallbackStanding::None => false,
                FallbackStanding::Holds(_) => chosen_variant.is_some() || user_choice.is_some(),
                FallbackStanding::DriverChanged { .. } => forced.is_none(),
            };
        for mode in modes.iter().copied() {
            let root = localx_llama_core::Launcher::install_root(launcher, mode);
            println!("== {} ==", mode.as_str());
            let mode_forced = forced.filter(|_| mode == Mode::Native);
            let planned = runtime.block_on(plan_binary_update(
                catalog,
                mode,
                &root,
                driver.as_ref(),
                amd_gpu,
                allow_downgrade,
                mode_forced,
            ));
            let persist_choice = mode == Mode::Native && !check_only && chosen_variant.is_some();
            if mode == Mode::Native && matches!(planned, Ok(UpdatePlan::UpToDate { .. })) {
                if persist_choice {
                    record_variant_choice(&settings_path, chosen_variant.flatten())?;
                }
                if retire_fallback {
                    retire_fallback_record(&settings_path)?;
                }
            }
            match planned {
                Ok(UpdatePlan::UpToDate { tag }) => println!("Up to date ({tag})."),
                Ok(UpdatePlan::WouldDowngrade {
                    installed,
                    resolved,
                }) => println!(
                    concat!(
                        "Refusing to downgrade: {} is installed, but the newest release ",
                        "resolves to the older {}. The working engine was left in place; ",
                        "upstream most likely withdrew or retagged a release. Pass ",
                        "--allow-downgrade to install it anyway."
                    ),
                    installed, resolved
                ),
                Ok(UpdatePlan::Install {
                    release,
                    assets,
                    replaces,
                }) => {
                    let summary = asset_set_summary(&assets);
                    if let Some(stamped) = &replaces {
                        let stamped = if stamped.is_empty() {
                            "unrecorded"
                        } else {
                            stamped
                        };
                        println!(
                        "The installed {} build ({stamped}) is not the one this host should run.",
                        release.tag
                    );
                    }
                    if check_only {
                        let verb = if replaces.is_some() {
                            "reinstall"
                        } else {
                            "update to"
                        };
                        println!("Would {verb} {} (assets: {summary}).", release.tag);
                        continue;
                    }
                    println!("Installing {} (assets: {summary}).", release.tag);
                    // A newly resolved release has no local pin by definition, so
                    // `install_asset` falls back to the digest the release itself
                    // publishes and refuses anything that does not match. Known
                    // pins are still passed, so re-installing a tag already in the
                    // table is checked against the hash recorded at that time.
                    let pins = assets
                        .iter()
                        .map(|asset| localbox::update::pin_for(catalog, &asset.name))
                        .collect::<Vec<_>>();
                    let names: Vec<&str> = assets.iter().map(|a| a.name.as_str()).collect();
                    let variant =
                        localbox::update::stamp_variant(mode, &names, driver_major, amd_gpu);
                    let recorded = runtime.block_on(install_asset_set(
                        &assets,
                        &pins,
                        &root,
                        false,
                        &release.tag,
                        &variant,
                        load_probe,
                    ))?;
                    println!("Installed {} into {}.", release.tag, root.display());
                    record_installed_pins(&home, mode, &release.tag, &recorded)?;
                    if persist_choice {
                        record_variant_choice(&settings_path, chosen_variant.flatten())?;
                    }
                    if mode == Mode::Native && retire_fallback {
                        retire_fallback_record(&settings_path)?;
                    }
                    if mode == Mode::Native {
                        retire_stale_receipt(&settings_path, &release.tag, &variant)?;
                    }
                }
                Err(message) if mode == Mode::Native && matches!(chosen_variant, Some(Some(_))) => {
                    // A build asked for by name that cannot be installed is a
                    // failure the caller must see, not a skipped mode.
                    return Err(format!(
                        "the requested native build cannot be installed: {message}"
                    ));
                }
                Err(message) => println!("Skipped: {message}"),
            }
        }
        Ok(())
    };

    if check_only {
        update_all(&catalog, &launcher)?;
    } else {
        localbox::fallback::with_engine_lock(&engine_dir, || {
            report_recovery(localbox::fallback::recover(&native_root, &settings_path)?);
            // Plan from settings.json as it is now, under the lock: recovery,
            // or another command that finished before the lock was taken, may
            // have changed it since the first load.
            let catalog = localbox_launcher::catalog::Catalog::load(&catalog_dir(&home))
                .map_err(|e| e.to_string())?;
            let launcher = localbox::exec::build_launcher_with(catalog.clone(), &home);
            update_all(&catalog, &launcher)
        })?;
    }
    report_missing_shipped_models(&home);
    Ok(())
}

/// After an update pass, say when this binary ships models the user's catalog
/// does not know yet — the catalog itself is never modified here.
fn report_missing_shipped_models(home: &std::path::Path) {
    use localbox::guided::{missing_model_keys, SHIPPED_CATALOG};
    let user_path = catalog_dir(home).join("llm-models.json");
    let Ok(raw) = std::fs::read_to_string(&user_path) else {
        return;
    };
    let (Ok(shipped), Ok(user)) = (
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(SHIPPED_CATALOG),
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
            raw.trim_start_matches('\u{feff}'),
        ),
    ) else {
        return;
    };
    let missing = missing_model_keys(&shipped, &user);
    if !missing.is_empty() {
        println!(
            "New shipped model(s) not in your catalog: {}. Add them with \
             `localbox update --merge-models` (your existing entries stay untouched).",
            missing.join(", ")
        );
    }
}

/// The `--merge-models` action: add shipped models missing from the user's
/// `llm-models.json`, touching nothing else. `--check` previews the keys
/// without writing.
fn merge_shipped_models(home: &std::path::Path, check_only: bool) -> Result<(), String> {
    use localbox::guided::{
        merge_missing_models, missing_model_keys, seed_installed_tree, SHIPPED_CATALOG,
    };
    let dir = catalog_dir(home);
    // Refresh the shipped layers first so the on-disk example matches this
    // binary; a missing user catalog is seeded complete and needs no merge.
    seed_installed_tree(&dir);
    let user_path = dir.join("llm-models.json");
    let raw = std::fs::read_to_string(&user_path)
        .map_err(|e| format!("could not read {}: {e}", user_path.display()))?;
    let shipped: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(SHIPPED_CATALOG).map_err(|e| e.to_string())?;
    let user: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(raw.trim_start_matches('\u{feff}')).map_err(|e| {
            format!(
                "{} is not valid JSON ({e}); fix it before merging",
                user_path.display()
            )
        })?;
    let missing = missing_model_keys(&shipped, &user);
    if missing.is_empty() {
        println!("Your catalog already has every shipped model.");
        return Ok(());
    }
    if check_only {
        println!(
            "Would add {} shipped model(s) to {}: {}. Existing entries stay untouched.",
            missing.len(),
            user_path.display(),
            missing.join(", ")
        );
        return Ok(());
    }
    let merged = merge_missing_models(&user, &shipped, &missing);
    let pretty = serde_json::to_string_pretty(&serde_json::Value::Object(merged))
        .map_err(|e| e.to_string())?;
    std::fs::write(&user_path, pretty + "\n").map_err(|e| e.to_string())?;
    println!(
        "Added {} shipped model(s) to {}: {}.",
        missing.len(),
        user_path.display(),
        missing.join(", ")
    );
    Ok(())
}

/// Remove retired fields from the user's own catalog, so the warning about them
/// has an end.
///
/// The catalog is the user's file and is never rewritten behind their back —
/// that is why the fields have been warned about rather than dropped. This is
/// the explicit migration, and it removes only the fields this LocalBox knows
/// it retired; an unrecognised field with no such history is left alone,
/// because it may be a typo worth seeing or a field a newer build reads.
fn prune_retired_model_fields(home: &std::path::Path, check_only: bool) -> Result<(), String> {
    let user_path = catalog_dir(home).join("llm-models.json");
    let raw = std::fs::read_to_string(&user_path)
        .map_err(|e| format!("could not read {}: {e}", user_path.display()))?;
    let user: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(raw.trim_start_matches('\u{feff}')).map_err(|e| {
            format!(
                "{} is not valid JSON ({e}); fix it before pruning",
                user_path.display()
            )
        })?;
    let (pruned, removed) = localbox_launcher::catalog::prune_retired_fields(&user);
    if removed.is_empty() {
        println!("Your catalog has no retired fields.");
        return Ok(());
    }
    if check_only {
        println!(
            "Would remove {} retired field(s) from {}: {}.",
            removed.len(),
            user_path.display(),
            removed.join(", ")
        );
        return Ok(());
    }
    let pretty = serde_json::to_string_pretty(&serde_json::Value::Object(pruned))
        .map_err(|e| e.to_string())?;
    std::fs::write(&user_path, pretty + "\n").map_err(|e| e.to_string())?;
    println!(
        "Removed {} retired field(s) from {}: {}.",
        removed.len(),
        user_path.display(),
        removed.join(", ")
    );
    Ok(())
}

/// `--variant`: `Some(Some(v))` installs that build, `Some(None)` (`auto`)
/// returns the choice to the host checks, `None` leaves the saved choice.
fn parse_variant_flag(
    value: Option<&str>,
) -> Result<Option<Option<localbox::update::Variant>>, String> {
    use localbox::update::Variant;
    let Some(value) = value else {
        return Ok(None);
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(Some(None)),
        "cuda" => Ok(Some(Some(Variant::Cuda))),
        "vulkan" => Ok(Some(Some(Variant::Vulkan))),
        "cpu" => Ok(Some(Some(Variant::Cpu))),
        other => Err(format!(
            "unknown --variant '{other}': expected cuda, vulkan, cpu, or auto"
        )),
    }
}

/// Keep (or, for `auto`, forget) the user's native variant choice.
fn record_variant_choice(
    settings_path: &std::path::Path,
    choice: Option<localbox::update::Variant>,
) -> Result<(), String> {
    use localbox::fallback::{read_settings, write_settings};
    use localbox::update::NATIVE_VARIANT_SETTING;
    let mut settings = read_settings(settings_path)?;
    match choice {
        Some(variant) => {
            settings.insert(
                NATIVE_VARIANT_SETTING.to_string(),
                serde_json::Value::String(variant.as_str().to_string()),
            );
            println!(
                "Recorded {NATIVE_VARIANT_SETTING} = {}; later updates keep this build \
                 (`localbox update --variant auto` undoes it).",
                variant.as_str()
            );
        }
        None => {
            if settings.remove(NATIVE_VARIANT_SETTING).is_some() {
                println!(
                    "Removed {NATIVE_VARIANT_SETTING}; the host checks choose the build again."
                );
            }
        }
    }
    write_settings(settings_path, &settings)
}

/// Forget the automatic fallback record once it no longer applies.
/// Forget a verified-backend receipt that no longer describes the installed
/// native build (its tag or variant changed).
fn retire_stale_receipt(
    settings_path: &std::path::Path,
    tag: &str,
    variant: &str,
) -> Result<(), String> {
    use localbox::fallback::{read_settings, write_settings, VERIFIED_BACKEND_SETTING};
    let mut settings = read_settings(settings_path)?;
    let stale = settings.get(VERIFIED_BACKEND_SETTING).is_some_and(|v| {
        v.get("tag").and_then(|t| t.as_str()) != Some(tag)
            || v.get("variant").and_then(|t| t.as_str()) != Some(variant)
    });
    if stale {
        settings.remove(VERIFIED_BACKEND_SETTING);
        write_settings(settings_path, &settings)?;
        println!(
            "Removed {VERIFIED_BACKEND_SETTING}: it described the previous build; \
             `localbox probe <model>` verifies this one."
        );
    }
    Ok(())
}

fn retire_fallback_record(settings_path: &std::path::Path) -> Result<(), String> {
    use localbox::fallback::{read_settings, write_settings, FALLBACK_SETTING};
    let mut settings = read_settings(settings_path)?;
    if settings.remove(FALLBACK_SETTING).is_some() {
        write_settings(settings_path, &settings)?;
        println!("Removed {FALLBACK_SETTING}: the earlier backend fallback no longer applies.");
    }
    Ok(())
}

fn report_recovery(recovery: localbox::fallback::Recovery) {
    use localbox::fallback::Recovery;
    match recovery {
        Recovery::Clean => {}
        Recovery::RestoredOriginal => eprintln!(
            "An interrupted backend fallback was undone: the engine build and settings that \
             were there before it are back."
        ),
        Recovery::FinishedCommit => eprintln!(
            "An interrupted backend fallback had already verified its replacement; its \
             cleanup is now finished."
        ),
    }
}

/// Why a probe failed. Only a failure whose evidence implicates the engine
/// build is worth trying another build for: the build is shared by every
/// model, so a model that is simply too big must never move the whole host
/// to a slower build.
enum ProbeFailure {
    /// The build cannot run here (driver, kernel, device or start-up crash).
    Backend(String),
    /// The model does not fit at this quant and context.
    Memory(String),
    /// A failed load or reply with no evidence either way.
    Unclear(String),
    /// Not a load or reply failure (a download, a refusal, an I/O error).
    Other(String),
}

impl ProbeFailure {
    fn message(&self) -> &str {
        match self {
            Self::Backend(m) | Self::Memory(m) | Self::Unclear(m) | Self::Other(m) => m,
        }
    }

    /// The reason class the `--json` failure envelope reports.
    fn reason(&self) -> &'static str {
        match self {
            Self::Backend(_) => "backend",
            Self::Memory(_) => "out-of-memory",
            Self::Unclear(_) => "unclear",
            Self::Other(_) => "other",
        }
    }

    /// Whether the failure came from a server this probe started.
    fn started_server(&self) -> bool {
        !matches!(self, Self::Other(_))
    }

    fn from_launch(message: String, server_log: &std::path::Path) -> Self {
        use localbox::fallback::{classify_failure, FailureCause};
        let log = std::fs::read_to_string(server_log).unwrap_or_default();
        match classify_failure(&log, &message) {
            FailureCause::Backend => Self::Backend(message),
            FailureCause::OutOfMemory => Self::Memory(message),
            FailureCause::Unknown => Self::Unclear(message),
        }
    }
}

/// A test-only switch that makes a probe of a build whose stamp variant starts
/// with this value fail before anything starts, so the fallback path can be
/// exercised on a machine whose GPU works. Documented as test-only.
const TEST_FAIL_BACKEND_ENV: &str = "LOCALBOX_TEST_FAIL_BACKEND";

fn forced_test_failure(root: &std::path::Path) -> Option<String> {
    let wanted = std::env::var(TEST_FAIL_BACKEND_ENV)
        .ok()?
        .to_ascii_lowercase();
    let variant = localbox::update::read_stamp_variant(root)?;
    wanted
        .split(',')
        .map(str::trim)
        .any(|w| !w.is_empty() && variant.starts_with(w))
        .then(|| {
            format!(
                "the {variant} build was failed on purpose ({TEST_FAIL_BACKEND_ENV}, testing only)"
            )
        })
}

/// One probe: resolve the plan against the build now installed, serve it,
/// check a one-word reply, and stop what was started unless asked to keep it.
fn probe_once(
    launcher: &LlamaLauncher,
    request: &LaunchRequest,
    args: &[String],
    home: &std::path::Path,
    running_policy: RunningModelPolicy,
    keep_serving: bool,
) -> Result<LaunchPlan, ProbeFailure> {
    let root = launcher.install_root(request.mode);
    if let Some(reason) = forced_test_failure(&root) {
        return Err(ProbeFailure::Backend(reason));
    }
    let mut plan =
        plan_launch(launcher, request).map_err(|e| ProbeFailure::Other(e.to_string()))?;
    apply_launch_posture(&mut plan, args, AgentKind::ServeOnly).map_err(ProbeFailure::Other)?;
    // A probe checks the same path agents use (its server behind a no-think
    // proxy, with the same reply check), but through a proxy of its own on a
    // free loopback port, so it never starts, repoints or stops the shared
    // proxy another model may be serving through.
    let probe_port = free_loopback_port().map_err(ProbeFailure::Other)?;
    plan.proxy.listen_port = probe_port;
    plan.proxy.listen_host = "127.0.0.1".to_string();
    // If anything claims the port before the proxy starts, refuse rather than
    // reuse or replace it.
    plan.proxy.start_only = true;
    plan.base_url = format!("http://127.0.0.1:{probe_port}");
    eprintln!("Launch arguments: {}", plan.argv.join(" "));
    let log = localbox::live::server_log_path(home, plan.server_port);
    let outcome = match execute_launch(
        launcher,
        &plan,
        request,
        AgentKind::ServeOnly,
        home,
        running_policy,
    ) {
        Ok(outcome) => outcome,
        Err(LiveError::Server(m)) | Err(LiveError::Smoke(m)) => {
            return Err(ProbeFailure::from_launch(m, &log))
        }
        Err(other) => return Err(ProbeFailure::Other(other.to_string())),
    };
    if !keep_serving {
        for pid in [outcome.proxy_pid, outcome.server_pid]
            .into_iter()
            .flatten()
        {
            localbox::live::stop_and_wait(pid);
        }
    }
    Ok(plan)
}

/// A loopback port nothing listens on right now, for a probe's own proxy.
fn free_loopback_port() -> Result<u16, String> {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .map(|addr| addr.port())
        .map_err(|e| format!("could not find a free loopback port for the probe: {e}"))
}

/// The verified-backend record a passed probe leaves in settings.json.
fn verified_record(stamp: &localbox::fallback::Stamp, plan: &LaunchPlan) -> serde_json::Value {
    let verified_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    serde_json::json!({
        "tag": stamp.tag,
        "variant": stamp.variant,
        "model": plan.key,
        "gguf": plan.gguf_path.file_name().map(|n| n.to_string_lossy().into_owned()),
        "verifiedAtUnix": verified_at,
    })
}

/// The real effects behind a fallback run.
struct ProbeHost<'a> {
    catalog: &'a Catalog,
    launcher: &'a LlamaLauncher,
    request: &'a LaunchRequest,
    args: &'a [String],
    home: &'a std::path::Path,
    runtime: &'a tokio::runtime::Runtime,
    driver: Option<localbox::update::CudaDriver>,
    amd_gpu: bool,
    keep_serving: bool,
    last_plan: Option<LaunchPlan>,
    /// The build that failed first (e.g. `cuda-13.5`).
    failed_variant: String,
    /// The NVIDIA driver the fallback is decided on.
    driver_id: Option<String>,
}

impl localbox::fallback::FallbackHost for ProbeHost<'_> {
    fn resolve(
        &mut self,
        variant: localbox::update::Variant,
    ) -> Result<localbox::fallback::Candidate, String> {
        use localbox::update::{plan_binary_update, stamp_variant, UpdatePlan};
        let root = self.launcher.install_root(Mode::Native);
        match self.runtime.block_on(plan_binary_update(
            self.catalog,
            Mode::Native,
            &root,
            self.driver.as_ref(),
            self.amd_gpu,
            false,
            Some(variant),
        ))? {
            UpdatePlan::Install {
                release, assets, ..
            } => {
                let names: Vec<&str> = assets.iter().map(|a| a.name.as_str()).collect();
                let stamped = stamp_variant(
                    Mode::Native,
                    &names,
                    self.driver.as_ref().map(|d| d.version.major),
                    self.amd_gpu,
                );
                Ok(localbox::fallback::Candidate {
                    stamp: localbox::fallback::Stamp {
                        tag: release.tag,
                        variant: stamped,
                    },
                    assets,
                })
            }
            other => Err(format!(
                "unexpected update plan for an empty engine root: {other:?}"
            )),
        }
    }

    fn install(
        &mut self,
        candidate: &localbox::fallback::Candidate,
        root: &std::path::Path,
    ) -> Result<Vec<(String, String)>, String> {
        let pins = candidate
            .assets
            .iter()
            .map(|asset| localbox::update::pin_for(self.catalog, &asset.name))
            .collect::<Vec<_>>();
        self.runtime.block_on(localbox::update::install_asset_set(
            &candidate.assets,
            &pins,
            root,
            false,
            &candidate.stamp.tag,
            &candidate.stamp.variant,
            true,
        ))
    }

    fn probe(&mut self) -> Result<(), localbox::fallback::AttemptFailure> {
        use localbox::fallback::AttemptFailure;
        match probe_once(
            self.launcher,
            self.request,
            self.args,
            self.home,
            RunningModelPolicy::Continue,
            self.keep_serving,
        ) {
            Ok(plan) => {
                self.last_plan = Some(plan);
                Ok(())
            }
            Err(ProbeFailure::Backend(m)) => Err(AttemptFailure::Backend(m)),
            Err(failure) => Err(AttemptFailure::Stop(failure.message().to_string())),
        }
    }

    fn record(
        &mut self,
        current: &localbox::fallback::Settings,
        stamp: &localbox::fallback::Stamp,
        pins: &[(String, String)],
    ) -> localbox::fallback::Settings {
        let mut next = localbox::update::refreshed_settings(
            current,
            localbox::update::pinned_tag_setting_key(Mode::Native),
            &stamp.tag,
            pins,
        );
        // Not the user's variant choice: a fallback record tied to this
        // driver, so a driver change brings the faster build back.
        next.insert(
            localbox::fallback::FALLBACK_SETTING.to_string(),
            localbox::fallback::fallback_record(
                stamp,
                &self.failed_variant,
                self.driver_id.as_deref(),
                &self.request.key,
            ),
        );
        if let Some(plan) = &self.last_plan {
            next.insert(
                localbox::fallback::VERIFIED_BACKEND_SETTING.to_string(),
                verified_record(stamp, plan),
            );
        }
        next
    }

    fn say(&mut self, line: &str) {
        eprintln!("{line}");
    }
}

/// A probe needs the proxy and the GPU to itself: running beside a serving
/// model would repoint that model's endpoint to the probe and then take it
/// down. So it runs only when nothing is serving, or when the user asked for
/// the running model to be stopped first.
fn probe_running_gate(
    policy: RunningModelPolicy,
    running: &[localbox::live::RunningModel],
) -> Result<RunningModelPolicy, String> {
    if running.is_empty() || policy == RunningModelPolicy::StopFirst {
        return Ok(policy);
    }
    Err(format!(
        "a model is already serving, and a probe would replace its endpoint; nothing was \
         started or stopped. Stop it first (`localbox stop`) or pass --if-running stop:\n{}",
        localbox::live::describe_running(running)
    ))
}

/// Where a probe stopped, for the `--json` failure envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeStage {
    /// Arguments, catalog, model or run-profile resolution.
    Setup,
    /// Another model is serving.
    Occupied,
    /// The engine lock or an interrupted fallback.
    Engine,
    /// Loading the model and checking its reply.
    Probe,
    /// Trying other builds.
    Fallback,
    /// Writing the verified record.
    Record,
}

impl ProbeStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Occupied => "occupied",
            Self::Engine => "engine",
            Self::Probe => "probe",
            Self::Fallback => "fallback",
            Self::Record => "record",
        }
    }
}

/// A failed probe: the stage, a reason class, the message, and the server log
/// when a server was started.
struct ProbeError {
    stage: ProbeStage,
    reason: &'static str,
    message: String,
    log: Option<std::path::PathBuf>,
}

impl ProbeError {
    fn at(stage: ProbeStage) -> impl Fn(String) -> Self {
        move |message| Self {
            stage,
            reason: "other",
            message,
            log: None,
        }
    }
}

fn cmd_probe(args: &[String]) -> Result<(), String> {
    let json = has_flag(args, "--json");
    match probe_command(args, json) {
        Ok(()) => Ok(()),
        Err(failure) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "ok": false,
                        "model": args.first().filter(|a| !a.starts_with("--")),
                        "stage": failure.stage.as_str(),
                        "reason": failure.reason,
                        "error": failure.message,
                        "log": failure.log.as_ref().map(|p| p.display().to_string()),
                    })
                );
            }
            Err(match &failure.log {
                Some(log) => format!(
                    "{}. The server log is at {}",
                    failure.message,
                    log.display()
                ),
                None => failure.message,
            })
        }
    }
}

fn probe_command(args: &[String], json: bool) -> Result<(), ProbeError> {
    use localbox::fallback::{
        read_settings, recover, run, rungs_after, with_engine_lock, write_settings, Stamp,
        VERIFIED_BACKEND_SETTING,
    };
    use std::cell::{Cell, RefCell};
    let setup = ProbeError::at(ProbeStage::Setup);
    let running_policy = resolve_running_policy(args).map_err(&setup)?;
    let model = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .ok_or_else(|| setup("a model key is required (run `localbox probe <model>`)".into()))?;
    let fallback = has_flag(args, "--fallback");
    let keep_serving = has_flag(args, "--keep-serving");
    let home =
        home_dir().ok_or_else(|| setup("could not determine the user home directory".into()))?;
    let running_policy = probe_running_gate(running_policy, &localbox::live::running_models(&home))
        .map_err(|message| ProbeError {
            stage: ProbeStage::Occupied,
            reason: "occupied",
            message,
            log: None,
        })?;
    let launcher = build_launcher(&home).map_err(&setup)?;
    let (mut request, profile) =
        build_request(args, model, &home, &launcher, false).map_err(&setup)?;
    // A probe judges the visible reply, so reasoning stays off.
    request.keep_thinking = false;
    confirm_profile_fallback(args, &request.key, profile.as_ref()).map_err(&setup)?;
    print_run_profile(args, profile.as_ref());
    let root = launcher.install_root(request.mode);
    let settings_path = catalog_dir(&home).join("settings.json");
    let engine_dir = root
        .parent()
        .ok_or_else(|| setup("the engine install root has no parent directory".into()))?
        .to_path_buf();
    let runtime = tokio::runtime::Runtime::new().map_err(|e| setup(e.to_string()))?;
    // Where the run is, and what a failure means, for the failure envelope.
    let stage = Cell::new(ProbeStage::Engine);
    let reason = Cell::new("other");
    let started_log: RefCell<Option<std::path::PathBuf>> = RefCell::new(None);

    let result = with_engine_lock(&engine_dir, || {
        if request.mode == Mode::Native {
            report_recovery(recover(&root, &settings_path)?);
        }
        stage.set(ProbeStage::Setup);
        // Release pins and the saved variant are read under the lock, after
        // recovery, so they match the engine on disk.
        let catalog = Catalog::load(&catalog_dir(&home)).map_err(|e| e.to_string())?;
        let original = Stamp::read(&root);
        stage.set(ProbeStage::Probe);
        let first = probe_once(
            &launcher,
            &request,
            args,
            &home,
            running_policy,
            keep_serving,
        );
        if let Err(failure) = &first {
            reason.set(failure.reason());
            if failure.started_server() {
                *started_log.borrow_mut() = plan_launch(&launcher, &request)
                    .ok()
                    .map(|plan| localbox::live::server_log_path(&home, plan.server_port))
                    .filter(|log| log.is_file());
            }
        }
        let (plan, stamp, failures) = match first {
            Ok(plan) => (plan, original.clone(), Vec::new()),
            Err(ProbeFailure::Other(message)) => return Err(message),
            Err(ProbeFailure::Memory(message)) => {
                return Err(format!(
                    "{} does not fit in this machine's memory at this quant and context \
                     ({message}). The engine build was not changed: another build would not \
                     help and would slow every model. Try a smaller quant or context \
                     (localbox probe {} --quant <key> --context <key>; see localbox info {}), \
                     or a smaller model",
                    request.key, request.key, request.key
                ))
            }
            Err(ProbeFailure::Unclear(message)) => {
                return Err(format!(
                    "{} failed to load or answer, and the log does not show whether the engine \
                     build or the model is at fault ({message}). The engine build was not \
                     changed. Read the server log; if it shows the GPU backend failing, install \
                     another build with localbox update --variant vulkan (or cpu) and probe \
                     again",
                    request.key
                ))
            }
            Err(ProbeFailure::Backend(message)) => {
                let variant = original
                    .as_ref()
                    .map(|s| s.variant.clone())
                    .unwrap_or_default();
                let rungs = if request.mode == Mode::Native {
                    rungs_after(&variant)
                } else {
                    Vec::new()
                };
                if !fallback || rungs.is_empty() {
                    let next = if rungs.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "; to try the other builds this host can run: localbox probe {} --fallback",
                            request.key
                        )
                    };
                    return Err(format!(
                        "the {} build could not run {}: {message}{next}",
                        if variant.is_empty() {
                            "installed"
                        } else {
                            &variant
                        },
                        request.key
                    ));
                }
                eprintln!(
                    "The {variant} build could not run {}: {message}",
                    request.key
                );
                let banner = nvidia_smi_banner();
                let driver_id = localbox::fallback::driver_fingerprint(&banner);
                let driver = localbox::update::parse_cuda_driver_version(&banner).map(|version| {
                    localbox::update::CudaDriver {
                        version,
                        gpus: localbox::update::parse_compute_capabilities(&nvidia_compute_caps()),
                    }
                });
                let amd_gpu = driver.is_none() && host_has_amd_gpu();
                let mut host = ProbeHost {
                    catalog: &catalog,
                    launcher: &launcher,
                    request: &request,
                    args,
                    home: &home,
                    runtime: &runtime,
                    driver,
                    amd_gpu,
                    keep_serving,
                    last_plan: None,
                    failed_variant: variant.clone(),
                    driver_id,
                };
                stage.set(ProbeStage::Fallback);
                let success = run(&root, &settings_path, &rungs, &mut host)?;
                let plan = host
                    .last_plan
                    .ok_or("the fallback passed without a launch plan")?;
                let mut failures = vec![(variant, message)];
                failures.extend(success.failures);
                (plan, Some(success.stamp), failures)
            }
        };
        let stamp = stamp.unwrap_or(Stamp {
            tag: String::from("unrecorded"),
            variant: String::from("unrecorded"),
        });
        // A fallback wrote its receipt together with its build; a first-try
        // pass records it here.
        stage.set(ProbeStage::Record);
        if failures.is_empty() {
            let mut settings = read_settings(&settings_path)?;
            settings.insert(
                VERIFIED_BACKEND_SETTING.to_string(),
                verified_record(&stamp, &plan),
            );
            write_settings(&settings_path, &settings)?;
        }
        Ok((plan, stamp, failures))
    });

    let (plan, stamp, failures) = result.map_err(|message| ProbeError {
        stage: stage.get(),
        reason: reason.get(),
        message,
        log: started_log.borrow().clone(),
    })?;
    let reasons: Vec<serde_json::Value> = failures
        .iter()
        .map(|(variant, why)| serde_json::json!({"variant": variant, "reason": why}))
        .collect();
    if json {
        println!(
            "{}",
            serde_json::json!({
                "ok": true,
                "model": plan.key,
                "backend": {"tag": stamp.tag, "variant": stamp.variant},
                "fallbacks": reasons,
                "serving": keep_serving,
                "endpoint": if keep_serving { Some(plan.base_url.clone()) } else { None },
                "profile": run_profile_json(args, profile.as_ref()),
                "launchArguments": plan.argv,
            })
        );
    } else {
        println!(
            "Verified: {} answers on the {} build ({}).",
            plan.key, stamp.variant, stamp.tag
        );
        for (variant, why) in &failures {
            println!("  fell back from {variant}: {why}");
        }
        if keep_serving {
            println!("Serving {} at {}", plan.key, plan.base_url);
        }
    }
    Ok(())
}

/// Record what was just installed into `settings.json`: the mode's tag key and
/// the SHA-256 of every asset activated.
///
/// `settings.json` outlives upgrades and wins layer precedence, so this is the
/// durable record of which engine build a host is running - `defaults.json` is
/// refreshed from the running binary and cannot hold it.
///
/// # Errors
/// A plain message when `settings.json` exists but is not valid JSON, or when
/// it cannot be written.
fn record_installed_pins(
    home: &std::path::Path,
    mode: Mode,
    tag: &str,
    pins: &[(String, String)],
) -> Result<(), String> {
    use localbox::update::{pinned_tag_setting_key, refreshed_settings};
    let tag_key = pinned_tag_setting_key(mode);
    let settings_path = catalog_dir(home).join("settings.json");
    let existing: serde_json::Map<String, serde_json::Value> =
        match std::fs::read_to_string(&settings_path) {
            Ok(raw) => serde_json::from_str(raw.trim_start_matches('\u{feff}')).map_err(|e| {
                format!("settings.json is not valid JSON ({e}); fix it before updating")
            })?,
            Err(_) => serde_json::Map::new(),
        };
    let merged = refreshed_settings(&existing, tag_key, tag, pins);
    let pretty = serde_json::to_string_pretty(&serde_json::Value::Object(merged))
        .map_err(|e| e.to_string())?;
    std::fs::write(&settings_path, pretty + "\n").map_err(|e| e.to_string())?;
    println!(
        "Recorded {tag_key} = {tag} and {} asset pin(s) in {}.",
        pins.len(),
        settings_path.display()
    );
    Ok(())
}

/// Whether an AMD GPU is present, used to prefer a Vulkan build over CPU when no
/// NVIDIA driver is detected. Only consulted in that no-NVIDIA case, so
/// `probe_gpu`'s AMD fallback (rocm-smi / the video-controller table) answers.
fn host_has_amd_gpu() -> bool {
    localbox::exec::probe_gpu()
        .map(|gpu| {
            let name = gpu.name.to_ascii_uppercase();
            name.contains("AMD") || name.contains("RADEON")
        })
        .unwrap_or(false)
}

/// Apply the post-plan launch posture — LAN gateway exposure (with its
/// serve-guard refusal) and, for Codex, the OpenAI-compatible env swap. Applied
/// to both the primary plan and any native-fallback re-plan so they stay
/// consistent, and before the `--dry-run` print so the preview matches.
fn apply_launch_posture(
    plan: &mut LaunchPlan,
    args: &[String],
    agent: AgentKind,
) -> Result<(), String> {
    if has_flag(args, "--lan") {
        // The gateway (no-think proxy) is the only LAN-bindable listener;
        // `--keep-thinking` routes the agent straight at the server, which
        // binds loopback-only — the combination would announce a gateway
        // that never starts. Refuse it instead of lying about the posture.
        if has_flag(args, "--keep-thinking") {
            return Err(
                "--lan needs the gateway, but --keep-thinking bypasses it (the server \
                 itself stays loopback-only). Drop one of the two flags."
                    .to_string(),
            );
        }
        let password = flag_value(args, "--password").unwrap_or("").to_string();
        let host = std::env::var(if cfg!(windows) {
            "COMPUTERNAME"
        } else {
            "HOSTNAME"
        })
        .unwrap_or_else(|_| "this-machine".to_string());
        let advertised = format!("http://{host}:{}", plan.proxy.listen_port);
        let guard = localbox_launcher::posture::evaluate_serve_guard(
            std::slice::from_ref(&advertised),
            &password,
            has_flag(args, "--allow-public-no-auth"),
        );
        if guard.refuse {
            return Err(guard.reason);
        }
        plan.proxy.listen_host = "0.0.0.0".to_string();
        plan.proxy.api_key = (!password.trim().is_empty()).then_some(password);
        if let Some(key) = &plan.proxy.api_key {
            // The agent authenticates with the same key the gateway enforces
            // (Codex gets its own env swap below).
            localbox_launcher::env::set_auth_token(&mut plan.env_plan, key);
        }
        println!(
            "LAN gateway: {advertised} (key {})",
            if plan.proxy.api_key.is_some() {
                "required"
            } else {
                "OPEN — opted in"
            }
        );
    }

    // Codex speaks the OpenAI protocol: swap in its OPENAI_* env plan (pointed at
    // the local endpoint) so both the dry-run preview and the live launch show
    // what actually reaches Codex — the Anthropic plan would leave it on the cloud.
    if agent == AgentKind::Codex {
        let auth = plan
            .proxy
            .api_key
            .clone()
            .unwrap_or_else(|| "local".to_string());
        plan.env_plan = localbox_launcher::env::codex_env_plan(&plan.base_url, &auth);
    }
    Ok(())
}

/// One compute capability per GPU (`8.9`), empty when `nvidia-smi` cannot
/// answer.
fn nvidia_compute_caps() -> String {
    std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=compute_cap", "--format=csv,noheader"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).to_string())
        .unwrap_or_default()
}

fn nvidia_smi_banner() -> String {
    std::process::Command::new("nvidia-smi")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).to_string())
        .unwrap_or_default()
}

fn cmd_nothink_proxy(args: &[String]) -> Result<(), String> {
    let listen: u16 = flag_value(args, "--listen")
        .ok_or("--listen <port> is required")?
        .parse()
        .map_err(|_| "bad --listen port".to_string())?;
    let target_port: u16 = flag_value(args, "--target-port")
        .ok_or("--target-port <port> is required")?
        .parse()
        .map_err(|_| "bad --target-port port".to_string())?;
    let target_host = flag_value(args, "--target-host")
        .unwrap_or("127.0.0.1")
        .to_string();
    let config = ProxyConfig {
        target_host,
        target_port,
        merge_system: !has_flag(args, "--no-merge-system"),
        api_key: flag_value(args, "--api-key").map(str::to_string),
    };
    let listen_host = flag_value(args, "--listen-host").unwrap_or("127.0.0.1");
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime
        .block_on(serve_proxy_on(listen_host, listen, config))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {

    #[test]
    fn a_probe_never_shares_the_proxy_with_a_serving_model() {
        let serving = vec![localbox::live::RunningModel {
            argv: Vec::new(),
            pid: 4242,
            port: Some(8080),
            model: Some("chat.gguf".into()),
            path: Some("chat.gguf".into()),
        }];
        for policy in [
            RunningModelPolicy::Ask,
            RunningModelPolicy::Continue,
            RunningModelPolicy::Cancel,
        ] {
            let err = probe_running_gate(policy, &serving).unwrap_err();
            assert!(err.contains("nothing was started or stopped"), "{err}");
        }
        assert_eq!(
            probe_running_gate(RunningModelPolicy::StopFirst, &serving).unwrap(),
            RunningModelPolicy::StopFirst
        );
        assert_eq!(
            probe_running_gate(RunningModelPolicy::Continue, &[]).unwrap(),
            RunningModelPolicy::Continue
        );
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| (*p).to_string()).collect()
    }

    /// The default has to be `Ask`, which degrades to cancelling without a
    /// terminal — a scripted launch must never silently stop a model an agent
    /// session or a colleague is using.
    #[test]
    fn if_running_defaults_to_asking_and_accepts_each_answer() {
        assert_eq!(
            resolve_running_policy(&argv(&["model"])),
            Ok(RunningModelPolicy::Ask)
        );
        for (value, expected) in [
            ("ask", RunningModelPolicy::Ask),
            ("continue", RunningModelPolicy::Continue),
            ("stop", RunningModelPolicy::StopFirst),
            ("cancel", RunningModelPolicy::Cancel),
            ("STOP", RunningModelPolicy::StopFirst),
        ] {
            assert_eq!(
                resolve_running_policy(&argv(&["model", "--if-running", value])),
                Ok(expected),
                "--if-running {value}"
            );
        }
    }

    /// A typo must not quietly become a decision about someone's running model.
    #[test]
    fn an_unknown_if_running_value_is_refused_with_the_accepted_ones() {
        let refused = resolve_running_policy(&argv(&["model", "--if-running", "kill"]))
            .expect_err("an unknown value cannot be guessed at");
        assert!(refused.contains("kill"), "{refused}");
        assert!(
            refused.contains("ask, continue, stop, or cancel"),
            "{refused}"
        );
    }

    use super::*;
    use localbox_launcher::proxy::EnsureProxyConfig;

    #[test]
    fn a_retired_mode_is_named_as_retired_not_as_a_typo() {
        let refused = parse_mode(Some("mtpturbo"))
            .expect_err("mtpturbo is no longer an engine LocalBox can launch");
        assert!(refused.contains("was retired"), "{refused}");
        assert!(refused.contains("native"), "{refused}");
        assert!(refused.contains("turboquant"), "{refused}");
        assert!(
            !refused.contains("unknown mode"),
            "a retired mode must not read as a spelling mistake: {refused}"
        );

        // A genuine typo still gets the unknown-mode listing.
        let typo = parse_mode(Some("turbowuant")).expect_err("not a mode");
        assert!(typo.contains("unknown mode 'turbowuant'"), "{typo}");
        assert!(
            typo.contains("native, turboquant, or prism"),
            "the listing names exactly the three live modes: {typo}"
        );

        // The live modes still resolve, including both prism spellings.
        assert_eq!(parse_mode(None).unwrap(), Mode::Native);
        assert_eq!(parse_mode(Some("turboquant")).unwrap(), Mode::Turboquant);
        assert_eq!(parse_mode(Some("prism")).unwrap(), Mode::PrismMl);
        assert_eq!(parse_mode(Some("prismml")).unwrap(), Mode::PrismMl);
    }

    fn plan(base_url: &str) -> LaunchPlan {
        LaunchPlan {
            inkling2_binary: None,
            key: "m".into(),
            context_key: String::new(),
            context_tokens: 0,
            gguf_path: std::path::PathBuf::from("m.gguf"),
            gguf_downloaded: true,
            vision_module: None,
            vision_module_downloaded: false,
            draft_module: None,
            draft_module_downloaded: false,
            argv: vec![],
            server_port: 8080,
            proxy: EnsureProxyConfig::new(11_435, 8080),
            base_url: base_url.to_string(),
            provider_toml: String::new(),
            env_plan: localbox_launcher::env::claude_env_plan(
                &localbox_launcher::env::EnvPlanInputs::new(base_url, "m"),
            ),
            notes: vec![],
        }
    }

    fn args(flags: &[&str]) -> Vec<String> {
        flags.iter().map(|s| (*s).to_string()).collect()
    }

    /// `stop`, `purge` and `status` must reap and report the port `launch`
    /// opened. A configured port that only reached the launch plan left a
    /// proxy `stop` could not kill and `status` called down.
    #[test]
    fn stop_and_status_resolve_the_same_proxy_port_launch_opens() {
        let home = tempfile::tempdir().unwrap();
        let installed = home.path().join(".local-llm");
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(
            installed.join("settings.json"),
            r#"{"NoThinkProxyPort": 11500}"#,
        )
        .unwrap();
        assert_eq!(resolved_proxy_port(home.path()), 11_500);

        // No configured value: the port such an installation actually opened.
        let bare = tempfile::tempdir().unwrap();
        assert_eq!(resolved_proxy_port(bare.path()), DEFAULT_PROXY_PORT);
    }

    fn tuner_store(home: &std::path::Path, entries: &[(&str, &str, f64)]) {
        let with_overrides: Vec<(&str, &str, f64, Option<i64>)> = entries
            .iter()
            .map(|(quant, profile, score)| (*quant, *profile, *score, None))
            .collect();
        tuner_store_with(home, &with_overrides);
    }

    fn tuner_store_with(home: &std::path::Path, entries: &[(&str, &str, f64, Option<i64>)]) {
        let tuner = home.join(".local-llm").join("tuner");
        std::fs::create_dir_all(&tuner).unwrap();
        let entries: Vec<serde_json::Value> = entries
            .iter()
            .map(|(quant, profile, score, n_cpu_moe)| {
                let overrides = n_cpu_moe.map_or_else(
                    || serde_json::json!({}),
                    |n| serde_json::json!({ "NCpuMoe": n }),
                );
                serde_json::json!({
                    "quant": quant, "contextKey": "64k", "mode": "native", "vramGB": 24,
                    "prompt_length": "short", "profile": profile, "score": score,
                    "scoreUnit": "tok/s", "args": [], "overrides": overrides,
                    "measured_at": "2026-01-01T00:00:00Z",
                    "tuner_version": localx_llama_core::CURRENT_TUNER_VERSION
                })
            })
            .collect();
        std::fs::write(
            tuner.join("best-m.json"),
            serde_json::json!({"schema": 1, "key": "m", "entries": entries}).to_string(),
        )
        .unwrap();
    }

    fn launcher_for_m(home: &std::path::Path) -> LlamaLauncher {
        let catalog = localbox_launcher::catalog::Catalog::from_layers(
            &serde_json::Map::new(),
            &serde_json::from_str(r#"{"Models":{"m":{"Repo":"o/m"}}}"#).unwrap(),
            &serde_json::Map::new(),
        )
        .unwrap();
        LlamaLauncher::new(catalog, "0.0.0", home.to_path_buf(), 24)
    }

    #[test]
    fn a_preferred_balanced_tune_beats_a_higher_scoring_pure_one() {
        let home = tempfile::tempdir().unwrap();
        tuner_store(
            home.path(),
            &[("q8", "pure", 200.0), ("q4", "balanced", 80.0)],
        );
        let launcher = launcher_for_m(home.path());

        let (request, profile) = build_request(
            &args(&["--prefer-profile", "balanced"]),
            "m",
            home.path(),
            &launcher,
            false,
        )
        .unwrap();
        let entry = profile.as_ref().unwrap().entry.as_ref().unwrap();
        assert_eq!(entry.profile, localx_llama_core::tuner::Profile::Balanced);
        assert_eq!(request.quant.as_deref(), Some("q4"));
        let reported = run_profile_json(&[], profile.as_ref());
        assert_eq!(reported["source"], "tuned");
        assert_eq!(reported["profile"], "balanced");
        assert_eq!(reported["quant"], "q4");

        // Without the preference the score decides, as before.
        let (request, _) = build_request(&[], "m", home.path(), &launcher, false).unwrap();
        assert_eq!(request.quant.as_deref(), Some("q8"));
    }

    fn launcher_for_m_with_quant(home: &std::path::Path) -> LlamaLauncher {
        let settings = serde_json::json!({ "LlamaCppGgufRoot": home.join("gguf") });
        let catalog = localbox_launcher::catalog::Catalog::from_layers(
            settings.as_object().unwrap(),
            &serde_json::from_str(
                r#"{"Models":{"m":{"Root":"m","Repo":"o/m","Quants":{"q4":"m-q4.gguf"},
                    "Quant":"q4","Contexts":{"":32768,"64k":65536}}}}"#,
            )
            .unwrap(),
            &serde_json::Map::new(),
        )
        .unwrap();
        LlamaLauncher::new(catalog, "0.0.0", home.to_path_buf(), 24)
    }

    #[test]
    fn with_one_quant_and_context_the_balanced_tune_reaches_the_server_arguments() {
        let home = tempfile::tempdir().unwrap();
        tuner_store_with(
            home.path(),
            &[
                ("q4", "pure", 200.0, Some(3)),
                ("q4", "balanced", 80.0, Some(9)),
            ],
        );
        let launcher = launcher_for_m_with_quant(home.path());
        // The exact call setup makes: explicit quant and context, then serve.
        let setup_args = args(&[
            "--quant",
            "q4",
            "--context",
            "64k",
            "--prefer-profile",
            "balanced",
            "--allow-untuned",
            "--json",
        ]);

        let (request, profile) =
            build_request(&setup_args, "m", home.path(), &launcher, false).unwrap();
        let plan = plan_launch(&launcher, &request).unwrap();
        let stamp = localbox::fallback::Stamp {
            tag: "b1".to_string(),
            variant: "vulkan".to_string(),
        };
        let receipt = serve_receipt(&setup_args, &plan, profile.as_ref(), false, Some(&stamp));
        assert_eq!(receipt["backend"]["variant"], "vulkan");
        assert_eq!(
            serve_receipt(&setup_args, &plan, profile.as_ref(), false, None)["backend"],
            serde_json::Value::Null
        );

        let argv = plan.argv.join(" ");
        assert!(argv.contains("--n-cpu-moe 9"), "{argv}");
        assert!(!argv.contains("--n-cpu-moe 3"), "{argv}");
        assert_eq!(receipt["profile"]["profile"], "balanced");
        assert_eq!(receipt["profile"]["source"], "tuned");
        assert!(receipt["launchArguments"]
            .as_array()
            .unwrap()
            .windows(2)
            .any(|pair| pair[0] == "--n-cpu-moe" && pair[1] == "9"));

        let (request, _) = build_request(
            &args(&["--quant", "q4", "--context", "64k"]),
            "m",
            home.path(),
            &launcher,
            false,
        )
        .unwrap();
        let argv = plan_launch(&launcher, &request).unwrap().argv.join(" ");
        assert!(
            argv.contains("--n-cpu-moe 3"),
            "the score decides without it: {argv}"
        );
    }

    #[test]
    fn a_profile_preference_is_not_a_filter() {
        let home = tempfile::tempdir().unwrap();
        tuner_store(home.path(), &[("q8", "pure", 200.0)]);
        let launcher = launcher_for_m(home.path());

        let (request, profile) = build_request(
            &args(&["--prefer-profile", "balanced"]),
            "m",
            home.path(),
            &launcher,
            false,
        )
        .unwrap();
        assert!(
            profile.unwrap().is_tuned(),
            "another compatible tune is used"
        );
        assert_eq!(request.quant.as_deref(), Some("q8"));
        assert!(build_request(
            &args(&["--prefer-profile", "fastest"]),
            "m",
            home.path(),
            &launcher,
            false
        )
        .unwrap_err()
        .contains("pure or balanced"));
    }

    #[test]
    fn the_native_retry_request_drops_the_saved_auto_best_profile() {
        // A fork smoke failure retries on native with AutoBest off (the tuned
        // overrides were tuned for the failing fork; fork-only KV types would
        // even fail the native plan). The retry re-derives the request, so the
        // profile's params/quant/context must not survive into it.
        let home = tempfile::tempdir().unwrap();
        let tuner = home.path().join(".local-llm").join("tuner");
        std::fs::create_dir_all(&tuner).unwrap();
        std::fs::write(
            tuner.join("best-m.json"),
            r#"{
                "schema": 1,
                "key": "m",
                "entries": [{
                    "quant": "tuned-quant",
                    "contextKey": "64k",
                    "mode": "turboquant",
                    "vramGB": 24,
                    "prompt_length": "short",
                    "profile": "balanced",
                    "score": 9.0,
                    "scoreUnit": "tok/s",
                    "args": [],
                    "overrides": { "NCpuMoe": 12 },
                    "measured_at": "2026-01-01T00:00:00Z",
                    "tuner_version": $CURRENT_TUNER_VERSION
                }]
            }"#
            .replace(
                "$CURRENT_TUNER_VERSION",
                &localx_llama_core::CURRENT_TUNER_VERSION.to_string(),
            ),
        )
        .unwrap();
        let catalog = localbox_launcher::catalog::Catalog::from_layers(
            &serde_json::Map::new(),
            &serde_json::from_str(r#"{"Models":{"m":{"Repo":"o/m"}}}"#).unwrap(),
            &serde_json::Map::new(),
        )
        .unwrap();
        let launcher = LlamaLauncher::new(catalog, "0.0.0", home.path().to_path_buf(), 24);
        let args = args(&["--auto-best"]);

        let (tuned, profile) = build_request(&args, "m", home.path(), &launcher, false).unwrap();
        assert!(profile.unwrap().is_tuned());
        assert_eq!(tuned.quant.as_deref(), Some("tuned-quant"));
        assert_eq!(tuned.params.n_cpu_moe, Some(12));
        // Every request — `launch` and `serve` alike — carries the
        // single-session defaults unless a setting/profile chose otherwise.
        assert_eq!(tuned.params.parallel, Some(1));
        assert_eq!(tuned.params.cache_reuse, Some(256));

        let (mut retry, profile) = build_request(&args, "m", home.path(), &launcher, true).unwrap();
        assert!(profile.is_none());
        retry.mode = Mode::Native;
        assert_eq!(retry.quant, None, "AutoBest quant must not carry over");
        assert_eq!(
            retry.context_key, "",
            "AutoBest context must not carry over"
        );
        assert_eq!(
            retry.params.n_cpu_moe, None,
            "fork-tuned params must not carry over"
        );
        assert_eq!(retry.mode, Mode::Native);
    }

    #[test]
    fn required_prism_mode_is_automatic_and_conflicting_cli_mode_is_rejected() {
        let home = tempfile::tempdir().unwrap();
        let catalog = localbox_launcher::catalog::Catalog::from_layers(
            &serde_json::Map::new(),
            &serde_json::from_str(
                r#"{"Models":{"bonsai":{"Repo":"prism-ml/bonsai","RequiredMode":"prism"}}}"#,
            )
            .unwrap(),
            &serde_json::Map::new(),
        )
        .unwrap();
        let launcher = LlamaLauncher::new(catalog, "0.0.0", home.path().to_path_buf(), 24);

        let (request, _) = build_request(&[], "bonsai", home.path(), &launcher, true).unwrap();
        assert_eq!(request.mode, Mode::PrismMl);

        let err = build_request(
            &args(&["--mode", "native"]),
            "bonsai",
            home.path(),
            &launcher,
            true,
        )
        .unwrap_err();
        assert!(err.contains("requires --mode prism"));
    }

    #[test]
    fn direct_launch_uses_tuned_profile_by_default_and_aliases_resolve() {
        let home = tempfile::tempdir().unwrap();
        let tuner = home.path().join(".local-llm").join("tuner");
        std::fs::create_dir_all(&tuner).unwrap();
        std::fs::write(
            tuner.join("best-model.json"),
            r#"{
                "schema":1,"key":"model","entries":[{
                    "quant":"q4","contextKey":"64k","mode":"native","vramGB":24,
                    "prompt_length":"short","profile":"balanced","score":42.0,
                    "scoreUnit":"tok/s","args":[],"overrides":{"NCpuMoe":7},
                    "measured_at":"2026-08-10T00:00:00Z","tuner_version":$CURRENT_TUNER_VERSION
                }]
            }"#
            .replace(
                "$CURRENT_TUNER_VERSION",
                &localx_llama_core::CURRENT_TUNER_VERSION.to_string(),
            ),
        )
        .unwrap();
        let catalog = localbox_launcher::catalog::Catalog::from_layers(
            &serde_json::Map::new(),
            &serde_json::from_str(
                r#"{"Models":{"model":{"Repo":"o/m"}},"CommandAliases":{"m":"model"}}"#,
            )
            .unwrap(),
            &serde_json::Map::new(),
        )
        .unwrap();
        let launcher = LlamaLauncher::new(catalog, "0.0.0", home.path(), 24);

        let (request, profile) = build_request(&[], "m", home.path(), &launcher, false).unwrap();
        assert_eq!(request.key, "model");
        assert_eq!(request.quant.as_deref(), Some("q4"));
        assert_eq!(request.context_key, "64k");
        assert_eq!(request.params.n_cpu_moe, Some(7));
        assert!(profile.unwrap().is_tuned());

        let (defaults, profile) = build_request(
            &args(&["--no-auto-best"]),
            "m",
            home.path(),
            &launcher,
            false,
        )
        .unwrap();
        assert_eq!(defaults.quant, None);
        assert!(profile.is_none());
    }

    #[test]
    fn adopt_tune_is_the_cli_write_and_same_run_application_seam() {
        let home = tempfile::tempdir().unwrap();
        let tuner = home.path().join(".local-llm").join("tuner");
        std::fs::create_dir_all(&tuner).unwrap();
        std::fs::write(
            tuner.join("best-model.json"),
            r#"{
                "schema":1,"key":"model","root_unknown":"keep","entries":[{
                    "quant":"q4","contextKey":"64k","mode":"native","vramGB":24,
                    "prompt_length":"short","profile":"balanced","score":42.0,
                    "scoreUnit":"tok/s","args":[],"overrides":{"NCpuMoe":45},
                    "measured_at":"2026-07-02T00:00:00Z","tuner_version":$OLDER,
                    "third_party_note":"keep"
                }]
            }"#
            .replace(
                "$OLDER",
                &(localx_llama_core::CURRENT_TUNER_VERSION - 1).to_string(),
            ),
        )
        .unwrap();
        let catalog = localbox_launcher::catalog::Catalog::from_layers(
            &serde_json::Map::new(),
            &serde_json::from_str(
                r#"{"Models":{"model":{"Repo":"o/m"}},"CommandAliases":{"m":"model"}}"#,
            )
            .unwrap(),
            &serde_json::Map::new(),
        )
        .unwrap();
        let launcher = LlamaLauncher::new(catalog, "0.0.0", home.path(), 24);

        let (_, unavailable) = build_request(&[], "m", home.path(), &launcher, false).unwrap();
        let error =
            confirm_profile_fallback_with_terminal(&[], "model", unavailable.as_ref(), false)
                .unwrap_err();
        assert!(error.contains("refusing an untuned non-interactive launch"));

        let (request, profile) =
            build_request(&args(&["--adopt-tune"]), "m", home.path(), &launcher, false).unwrap();
        let profile = profile.unwrap();
        assert!(profile.is_tuned());
        assert_eq!(
            profile.entry.as_ref().unwrap().tuner_version,
            localx_llama_core::CURRENT_TUNER_VERSION
        );
        assert_eq!(
            profile
                .adoption
                .as_ref()
                .map(|adoption| adoption.from_tuner_version),
            Some(localx_llama_core::CURRENT_TUNER_VERSION - 1)
        );
        assert_eq!(request.quant.as_deref(), Some("q4"));
        assert_eq!(request.context_key, "64k");
        assert_eq!(request.params.n_cpu_moe, Some(45));
        assert!(tuner.join("best-model.json.bak").is_file());
        assert!(confirm_profile_fallback_with_terminal(
            &args(&["--adopt-tune"]),
            "model",
            Some(&profile),
            false,
        )
        .is_ok());
    }

    #[test]
    fn a_saved_no_op_offload_tune_replays_into_the_launch() {
        let home = tempfile::tempdir().unwrap();
        let tuner = home.path().join(".local-llm").join("tuner");
        std::fs::create_dir_all(&tuner).unwrap();
        std::fs::write(
            tuner.join("best-model.json"),
            r#"{
                "schema":1,"key":"model","entries":[{
                    "quant":"q4","contextKey":"256k","mode":"native","vramGB":24,
                    "prompt_length":"short","profile":"balanced","score":42.0,
                    "scoreUnit":"tok/s","args":[],
                    "overrides":{"NCpuMoe":44,"UbatchSize":128,"BatchSize":128,"NoOpOffload":true},
                    "measured_at":"2026-11-23T00:00:00Z","tuner_version":$CURRENT
                }]
            }"#
            .replace(
                "$CURRENT",
                &localx_llama_core::CURRENT_TUNER_VERSION.to_string(),
            ),
        )
        .unwrap();
        let catalog = localbox_launcher::catalog::Catalog::from_layers(
            &serde_json::Map::new(),
            &serde_json::from_str(
                r#"{"Models":{"model":{"Repo":"o/m"}},"CommandAliases":{"m":"model"}}"#,
            )
            .unwrap(),
            &serde_json::Map::new(),
        )
        .unwrap();
        let launcher = LlamaLauncher::new(catalog, "0.0.0", home.path(), 24);
        let (request, profile) = build_request(&[], "m", home.path(), &launcher, false).unwrap();
        assert!(profile.unwrap().is_tuned());
        assert_eq!(request.params.n_cpu_moe, Some(44));
        assert_eq!(request.params.ubatch_size, Some(128));
        assert_eq!(request.params.no_op_offload, Some(true));
    }

    #[test]
    fn lan_with_keep_thinking_is_refused_not_announced() {
        // The gateway is the only LAN-bindable listener; keep-thinking
        // bypasses it. The old behaviour printed a "key required" banner for
        // a gateway that never started.
        let mut p = plan("http://127.0.0.1:8080");
        let err = apply_launch_posture(
            &mut p,
            &args(&["--lan", "--keep-thinking", "--password", "k"]),
            AgentKind::Claude,
        )
        .unwrap_err();
        assert!(err.contains("--keep-thinking"), "{err}");
    }

    #[test]
    fn a_keyed_lan_posture_reaches_the_proxy_and_the_agent_env() {
        let mut p = plan("http://127.0.0.1:11435");
        apply_launch_posture(
            &mut p,
            &args(&["--lan", "--password", "sesame"]),
            AgentKind::Claude,
        )
        .unwrap();
        assert_eq!(p.proxy.listen_host, "0.0.0.0");
        assert_eq!(p.proxy.api_key.as_deref(), Some("sesame"));
        let token = p
            .env_plan
            .iter()
            .find(|(n, _)| *n == "ANTHROPIC_AUTH_TOKEN")
            .map(|(_, v)| v.as_str());
        assert_eq!(
            token,
            Some("sesame"),
            "agent env must carry the gateway key"
        );
    }
}
