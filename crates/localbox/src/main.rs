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
use localbox::live::{execute_launch, status_report, stop_all, AgentKind, LiveError};
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
  localbox stop                       stop every model server and the proxy
  localbox status                     report serve health and the remedy
  localbox info [model]               list the configured models, or one in detail
  localbox models [--json]            list launchable models and tuned-profile state
  localbox download <model|hf-repo> [--quant <key>] [--vision] [--draft]
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
  localbox log [--lines <n>]          tail the most recent server log
  localbox embed-serve [--port <p>]   start the CPU-only embedding server
  localbox embed-stop                 stop the embedding server
  localbox update [--mode <m>] [--check] [--allow-downgrade] [--merge-models]
                                      install or update the llama.cpp binaries
                                      to the newest upstream release, verified
                                      against the published release digest and
                                      recorded in settings.json; --check
                                      previews without writing;
                                      --allow-downgrade permits installing a
                                      release older than the installed build;
                                      --merge-models adds newly shipped catalog
                                      models to llm-models.json (additive only,
                                      existing entries untouched)
  localbox version                    print the launcher version envelope
  localbox nothink-proxy --listen <port> --target-port <port>
                                      host the no-think proxy (plumbing)

Options for launch/serve:
  --context <key>       context window key from the model catalog (e.g. 64k)
  --mode <m>            native | turboquant | mtpturbo | prism   (default native)
  --quant <key>         quant variant from the catalog (default per model)
  --auto-best           compatibility spelling: require the saved tuned profile
  --no-auto-best        explicitly use catalog/settings defaults instead
  --allow-untuned       allow non-interactive fallback when no tuned profile exists
  --adopt-tune          carry a matching older tune into the current version
  --vision              load the vision projector when the model has one
  --draft               load the catalog's draft model for speculative
                        decoding (faster generation; needs a DraftModule)
  --keep-thinking       route the agent straight at the server so thinking
                        reaches it unfiltered (bypasses the no-think proxy, so
                        its system-message merge does not apply)
  --agent <a>           claude | localpilot | codex | none  (default claude)
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
        "serve" => cmd_launch(&args[1..], AgentKind::ServeOnly),
        "stop" => cmd_stop(),
        "status" => cmd_status(&args[1..]),
        "info" => cmd_info(&args[1..]),
        "models" => cmd_models(&args[1..]),
        "download" => cmd_download(&args[1..]),
        "purge" => cmd_purge(),
        "log" => cmd_log(&args[1..]),
        "embed-serve" => cmd_embed_serve(&args[1..]),
        "embed-stop" => cmd_embed_stop(),
        "update" => cmd_update(&args[1..]),
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

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn parse_mode(value: Option<&str>) -> Result<Mode, String> {
    match value.unwrap_or("native") {
        "native" => Ok(Mode::Native),
        "turboquant" => Ok(Mode::Turboquant),
        "mtpturbo" => Ok(Mode::Mtpturbo),
        "prism" | "prismml" => Ok(Mode::PrismMl),
        other => Err(format!(
            "unknown mode '{other}' (expected native, turboquant, mtpturbo, or prism)"
        )),
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
    let mut profile = (!disable_profile && !has_flag(args, "--no-auto-best")).then(|| {
        resolve_run_profile(
            home,
            &request.key,
            RunProfileQuery {
                quant: explicit_quant.then_some(request.quant.as_deref()).flatten(),
                context_key: explicit_context.then_some(request.context_key.as_str()),
                mode: (explicit_mode || required_mode.is_some()).then_some(request.mode),
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
    Ok((request, profile))
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

fn cmd_launch(args: &[String], default_agent: AgentKind) -> Result<(), String> {
    let model = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .ok_or("a model key is required (run `localbox launch <model>`)")?;
    let home = home_dir().ok_or("could not determine the user home directory")?;

    let agent = parse_agent(flag_value(args, "--agent"), default_agent)?;
    let launcher = build_launcher(&home)?;
    let (request, profile) = build_request(args, model, &home, &launcher, false)?;
    confirm_profile_fallback(args, &request.key, profile.as_ref())?;
    if let Some(entry) = profile.as_ref().and_then(|profile| profile.entry.as_ref()) {
        eprintln!(
            "Run profile: tuned · {} · {} · {} (score {:.0} {}) · {}",
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

    let mut plan = plan_launch(&launcher, &request).map_err(|e| e.to_string())?;
    apply_launch_posture(&mut plan, args, agent)?;

    if has_flag(args, "--dry-run") {
        print_plan(&plan);
        return Ok(());
    }

    // A fork build (turboquant/mtpturbo) that fails its reply check falls back to
    // native llama.cpp once, rather than hard-stopping. The retry re-derives its
    // request with AutoBest off — the fork's tuned params/quant/context don't
    // apply to the native build (fork-only KV types would even fail its plan) —
    // and the failed launch already tore its own server/proxy down.
    let outcome = match execute_launch(&launcher, &plan, &request, agent, &home) {
        Ok(outcome) => outcome,
        Err(LiveError::Smoke(detail))
            if smoke_fallback(request.mode) == SmokeFallback::RetryNative =>
        {
            eprintln!(
                "The {} build failed the reply check ({detail}).\n\
                 Retrying on native llama.cpp …",
                request.mode.as_str()
            );
            let (mut native, _) = build_request(args, model, &home, &launcher, true)?;
            native.mode = Mode::Native;
            let mut native_plan = plan_launch(&launcher, &native).map_err(|e| e.to_string())?;
            apply_launch_posture(&mut native_plan, args, agent)?;
            let outcome = execute_launch(&launcher, &native_plan, &native, agent, &home)
                .map_err(|e| e.to_string())?;
            plan = native_plan;
            outcome
        }
        Err(e) => return Err(e.to_string()),
    };
    if agent == AgentKind::ServeOnly {
        println!(
            "{}",
            status_report(plan.proxy.listen_port, plan.server_port)
        );
        println!("Serving {} at {}", plan.key, plan.base_url);
    }
    let _ = outcome;
    Ok(())
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
    let stopped = stop_all(&home, &[resolved_proxy_port(&home)]);
    if stopped == 0 {
        println!("Nothing was running.");
    } else {
        println!("Stopped {stopped} process(es).");
    }
    Ok(())
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
        Some(name) => print!("{}", render_model_detail(&catalog, name)?),
        None => print!("{}", render_model_overview(&catalog)),
    }
    Ok(())
}

fn cmd_models(args: &[String]) -> Result<(), String> {
    use localbox::manage::{models_catalog, render_models_catalog};
    let home = home_dir().ok_or("could not determine the user home directory")?;
    let catalog = Catalog::load(&catalog_dir(&home)).map_err(|e| e.to_string())?;
    let contract = models_catalog(&catalog, &home);
    if has_flag(args, "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&contract)
                .map_err(|e| format!("model catalog render: {e}"))?
        );
    } else {
        print!("{}", render_models_catalog(&contract));
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
        return download_from_hf(model, quant, &home);
    };
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
) -> Result<(), String> {
    use localbox::fetch::ProgressPrinter;
    use localbox::guided::CatalogInsert;
    use localbox::hf_install::discover_hf_repo;
    use localbox_launcher::fetch::DownloadKind;

    let hf = localbox_launcher::hf_meta::parse_hf_ref(model).map_err(|_| {
        format!(
            "unknown model '{model}' — not a catalog name, and not a Hugging Face repo id \
             (expected owner/repo or a https://huggingface.co/owner/repo URL). \
             Run `localbox info` for the catalog names."
        )
    })?;
    let discovery = discover_hf_repo(hf)?;
    let chosen = discovery.select(quant)?;
    let outcome = discovery.register(&catalog_dir(home), &chosen.key)?;
    match &outcome {
        CatalogInsert::Inserted(key) => {
            println!(
                "Added '{key}' to your catalog for {} ({} quant(s)).",
                discovery.hf.repo_id(),
                discovery.candidates.len()
            );
        }
        CatalogInsert::Updated { key, added_quants } => {
            println!(
                "Updated '{key}' for {} with {} missing quant(s): {}.",
                discovery.hf.repo_id(),
                added_quants.len(),
                added_quants.join(", ")
            );
        }
        CatalogInsert::AlreadyPresent(key) => {
            println!(
                "{} is already complete in your catalog as '{key}'.",
                discovery.hf.repo_id()
            );
        }
    }
    let key = outcome.key();
    // Resolve and fetch through the now-updated catalog. This keeps a repeat
    // repo install, an ordinary `download <key>`, and launch-on-miss on exactly
    // the same paths and URLs, including existing user-owned quant mappings.
    let launcher = build_launcher(home)?;
    let targets = launcher
        .model_download_targets(key, Some(&chosen.key))
        .map_err(|e| e.to_string())?;
    let gguf_count = targets
        .iter()
        .filter(|target| target.kind == DownloadKind::Gguf)
        .count();
    if gguf_count == 0 {
        return Err(format!("'{key}' resolves to no GGUF files"));
    }
    println!(
        "Downloading quant '{}' ({gguf_count} file(s)) …",
        chosen.key
    );

    let mut printer = ProgressPrinter::default();
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let fetched = runtime
        .block_on(launcher.fetch_model_files(
            key,
            Some(&chosen.key),
            &[DownloadKind::Gguf],
            &mut |progress| printer.report(progress),
        ))
        .map_err(|e| e.to_string())?;
    for path in fetched {
        println!("ready: {}", path.display());
    }
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
    let stopped = stop_all(&home, &[proxy_port]);
    if stopped > 0 {
        println!("Stopped {stopped} running process(es).");
    }
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
    let launcher = build_launcher(&home)?;
    let check_only = has_flag(args, "--check");
    // `--refresh-pins` used to be the opt-in for "resolve the latest
    // release". That is now what every update does, so the flag is accepted
    // and ignored rather than breaking scripts that still pass it.
    let _refresh = has_flag(args, "--refresh-pins");
    let allow_downgrade = has_flag(args, "--allow-downgrade");
    let explicit_mode = flag_value(args, "--mode");
    if has_flag(args, "--merge-models") {
        return merge_shipped_models(&home, check_only);
    }
    let modes: Vec<Mode> = match explicit_mode {
        Some(m) => vec![parse_mode(Some(m))?],
        None => vec![
            Mode::Native,
            Mode::Turboquant,
            Mode::Mtpturbo,
            Mode::PrismMl,
        ],
    };
    let driver_major = localbox::update::parse_cuda_driver_major(&nvidia_smi_banner());
    // No NVIDIA driver but an AMD card present → the Vulkan build uses the GPU
    // instead of silently falling back to CPU.
    let amd_gpu = driver_major.is_none() && host_has_amd_gpu();
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;

    for mode in modes {
        let root = localx_llama_core::Launcher::install_root(&launcher, mode);
        println!("== {} ==", mode.as_str());
        match runtime.block_on(plan_binary_update(
            &catalog,
            mode,
            &root,
            driver_major,
            amd_gpu,
            allow_downgrade,
        )) {
            Ok(UpdatePlan::UpToDate { tag }) => println!("Up to date ({tag})."),
            Ok(UpdatePlan::MtpStatus { message }) => println!("{message}"),
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
                installed,
                resolved
            ),
            Ok(UpdatePlan::Install { release, assets }) => {
                let summary = asset_set_summary(&assets);
                if check_only {
                    println!("Would update to {} (assets: {summary}).", release.tag);
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
                    .map(|asset| localbox::update::pin_for(&catalog, &asset.name))
                    .collect::<Vec<_>>();
                let names: Vec<&str> = assets.iter().map(|a| a.name.as_str()).collect();
                let variant = localbox::update::stamp_variant(mode, &names, driver_major, amd_gpu);
                let recorded = runtime.block_on(install_asset_set(
                    &assets,
                    &pins,
                    &root,
                    false,
                    &release.tag,
                    &variant,
                ))?;
                println!("Installed {} into {}.", release.tag, root.display());
                record_installed_pins(&home, mode, &release.tag, &recorded)?;
            }
            Err(message) => println!("Skipped: {message}"),
        }
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
    let Some(tag_key) = pinned_tag_setting_key(mode) else {
        return Ok(()); // mtpturbo is source-built and has no release tag
    };
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
            &[advertised.clone()],
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
    use super::*;
    use localbox_launcher::proxy::EnsureProxyConfig;

    fn plan(base_url: &str) -> LaunchPlan {
        LaunchPlan {
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
