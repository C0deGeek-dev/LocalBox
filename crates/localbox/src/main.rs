//! The `localbox` command-line entry point.
//!
//! Hand-rolled argument handling: the surface is small and stable, and the
//! binary must start instantly on every OS. Command work runs on a worker
//! thread with an explicit stack size — Windows main threads are smaller
//! than the Linux/macOS default and deep CLI/TUI stacks overflow there.

use std::process::ExitCode;

use localbox::exec::{build_launcher, home_dir};
use localbox::guided::{catalog_dir, run_guided};
use localbox::live::{execute_launch, status_report, stop_all, AgentKind, LiveError};
use localbox::product_envelope;
use localbox_launcher::catalog::Catalog;
use localbox_launcher::launcher::LlamaLauncher;
use localbox_launcher::orchestrate::{
    plan_launch, smoke_fallback, LaunchPlan, LaunchRequest, SmokeFallback,
};
use localx_llama_core::{Launcher, Mode, TunerBestConfig, TunerEntry};
use localx_llama_runtime::proxy::{serve_proxy_on, ProxyConfig};

const DEFAULT_PROXY_PORT: u16 = 11_435;
const DEFAULT_SERVER_PORT: u16 = 8080;

const USAGE: &str = "\
localbox — run a local model and the coding agent of your choice

Usage:
  localbox                            open the guided launcher (pick a model)
  localbox --plain                    guided launcher with plain-text menus
  localbox launch <model> [options]   resolve, start, and hand off to an agent
  localbox serve <model> [options]    start the model (and proxy) headless
  localbox stop                       stop every model server and the proxy
  localbox status                     report serve health and the remedy
  localbox info [model]               list the configured models, or one in detail
  localbox purge                      stop servers and delete downloaded model files
  localbox log [--lines <n>]          tail the most recent server log
  localbox embed-serve [--port <p>]   start the CPU-only embedding server
  localbox embed-stop                 stop the embedding server
  localbox update [--mode <m>] [--check] [--refresh-pins] [--merge-models]
                                      install or update the llama.cpp binaries;
                                      --check also reports pin freshness, and
                                      --refresh-pins (explicit --mode) advances
                                      the pin to the latest release, verified
                                      against the published release digest;
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
  --auto-best           apply the saved localbench profile (best-<model>.json):
                        tuned quant/context/mode/KV-cache/n-cpu-moe. Explicit
                        --quant/--context/--mode still filter and override.
  --vision              load the vision projector when the model has one
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

/// Fold the saved localbench profile (`best-<model>.json`) into the request:
/// adopt its quant/context/mode for the fields the user did not pin, and its
/// tuned launch params (KV types, `n-cpu-moe`, flash-attn, …). Fails loudly when
/// no profile exists so `--auto-best` never silently launches raw defaults.
fn apply_saved_auto_best(
    request: &mut LaunchRequest,
    home: &std::path::Path,
    explicit_mode: bool,
    explicit_quant: bool,
    explicit_context: bool,
) -> Result<(), String> {
    let path = home
        .join(".local-llm")
        .join("tuner")
        .join(format!("best-{}.json", request.key));
    let store = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<TunerBestConfig>(&raw).ok())
        .ok_or_else(|| {
            format!(
                "no saved AutoBest profile at {} — run `localbench findbest {}` first, \
                 or launch through the guided launcher",
                path.display(),
                request.key
            )
        })?;
    if !store.schema_supported() {
        return Err("the saved AutoBest profile uses an unsupported schema version".to_string());
    }
    // Honor any pinned quant/context/mode as filters; among the rest, the
    // highest-scoring entry wins.
    let mut candidates: Vec<&TunerEntry> = store
        .entries
        .iter()
        .filter(|e| !explicit_quant || request.quant.as_deref() == Some(e.quant.as_str()))
        .filter(|e| !explicit_context || e.context_key == request.context_key)
        .filter(|e| !explicit_mode || e.mode == request.mode)
        .collect();
    if candidates.is_empty() {
        return Err("no saved AutoBest entry matches the requested quant/context/mode".to_string());
    }
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    let entry = candidates[0];
    if !explicit_mode {
        request.mode = entry.mode;
    }
    if !explicit_quant {
        request.quant = Some(entry.quant.clone());
    }
    if !explicit_context {
        request.context_key = entry.context_key.clone();
    }
    request.params = entry.overrides.to_launch_params();
    eprintln!(
        "AutoBest: {} · {} · {} (score {:.0} {})",
        entry.quant,
        entry.context_key,
        entry.mode.as_str(),
        entry.score,
        entry.score_unit,
    );
    Ok(())
}

/// Build the launch request a `launch`/`serve` invocation asks for. Called for
/// the primary plan and — with `auto_best` forced off — for the native retry
/// after a failed smoke, so the retry re-derives clean defaults instead of
/// carrying fork-tuned AutoBest params/quant/context into the native build.
fn build_request(
    args: &[String],
    model: &str,
    home: &std::path::Path,
    launcher: &LlamaLauncher,
    auto_best: bool,
) -> Result<LaunchRequest, String> {
    launcher.model_def(model).map_err(|e| e.to_string())?;
    let required_mode = launcher.required_mode(model);
    let explicit_mode = flag_value(args, "--mode").is_some();
    let mut mode = parse_mode(flag_value(args, "--mode"))?;
    if let Some(required) = required_mode {
        if explicit_mode && mode != required {
            return Err(format!(
                "{model} requires --mode {}; '{}' is incompatible",
                cli_mode_name(required),
                cli_mode_name(mode)
            ));
        }
        mode = required;
    }
    let mut request = LaunchRequest::new(
        model.to_string(),
        flag_value(args, "--context").unwrap_or("").to_string(),
        mode,
    );
    request.quant = flag_value(args, "--quant").map(str::to_string);
    request.use_vision = has_flag(args, "--vision");
    request.keep_thinking = has_flag(args, "--keep-thinking");

    // Opt-in: fold in the saved localbench profile so a headless serve/launch uses
    // the tuned quant/context/mode + launch params (KV types, n-cpu-moe, …) that
    // the interactive guided launcher applies — instead of raw defaults that OOM a
    // large model. Explicit --quant/--context/--mode filter and override.
    if auto_best {
        apply_saved_auto_best(
            &mut request,
            home,
            explicit_mode || required_mode.is_some(),
            flag_value(args, "--quant").is_some(),
            flag_value(args, "--context").is_some(),
        )?;
    }

    // Settings are the lowest precedence (an AutoBest profile or a flag already
    // arrived as `Some(_)`), then the single-session one-slot default — for
    // every launch, headless `serve` included: llama-server's own `--parallel`
    // default is now multi-slot auto, which allocates the full context per slot
    // and OOMs a model sized for one slot. See `apply_session_defaults`.
    request.apply_session_defaults(&launcher.settings_launch_params());
    Ok(request)
}

fn cmd_launch(args: &[String], default_agent: AgentKind) -> Result<(), String> {
    let model = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .ok_or("a model key is required (run `localbox launch <model>`)")?;
    let home = home_dir().ok_or("could not determine the user home directory")?;

    let agent = parse_agent(flag_value(args, "--agent"), default_agent)?;
    let launcher = build_launcher(&home)?;
    let request = build_request(args, model, &home, &launcher, has_flag(args, "--auto-best"))?;

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
            let mut native = build_request(args, model, &home, &launcher, false)?;
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

fn cmd_stop() -> Result<(), String> {
    let home = home_dir().ok_or("could not determine the user home directory")?;
    let stopped = stop_all(&home, &[DEFAULT_PROXY_PORT]);
    if stopped == 0 {
        println!("Nothing was running.");
    } else {
        println!("Stopped {stopped} process(es).");
    }
    Ok(())
}

fn cmd_status(args: &[String]) -> Result<(), String> {
    let proxy_port = flag_value(args, "--proxy-port")
        .map(|v| v.parse().map_err(|_| format!("bad port '{v}'")))
        .transpose()?
        .unwrap_or(DEFAULT_PROXY_PORT);
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

fn cmd_purge() -> Result<(), String> {
    use localbox::manage::purge_targets;
    use localbox_launcher::launcher::expand_path_with_home;
    let home = home_dir().ok_or("could not determine the user home directory")?;
    let catalog = Catalog::load(&catalog_dir(&home)).map_err(|e| e.to_string())?;
    let root = catalog
        .gguf_root()
        .ok_or("LlamaCppGgufRoot is not configured; set it in settings.json")?;
    let root = expand_path_with_home(&root.to_string_lossy(), &home);

    let stopped = stop_all(&home, &[DEFAULT_PROXY_PORT]);
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
    use localbox::update::{plan_binary_update, write_stamp, UpdatePlan};
    let home = home_dir().ok_or("could not determine the user home directory")?;
    let catalog = localbox_launcher::catalog::Catalog::load(&catalog_dir(&home))
        .map_err(|e| e.to_string())?;
    let launcher = build_launcher(&home)?;
    let check_only = has_flag(args, "--check");
    let refresh = has_flag(args, "--refresh-pins");
    let explicit_mode = flag_value(args, "--mode");
    if refresh && explicit_mode.is_none() {
        return Err(
            "--refresh-pins re-pins to the latest upstream release, so it needs an \
             explicit --mode (native, turboquant, or prism)."
                .to_string(),
        );
    }
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
        if refresh {
            refresh_mode_pins(
                &runtime,
                &catalog,
                mode,
                &root,
                &home,
                driver_major,
                amd_gpu,
                check_only,
            )?;
            continue;
        }
        match runtime.block_on(plan_binary_update(
            &catalog,
            mode,
            &root,
            driver_major,
            amd_gpu,
        )) {
            Ok(UpdatePlan::UpToDate { tag }) => println!("Up to date ({tag})."),
            Ok(UpdatePlan::MtpStatus { message }) => println!("{message}"),
            Ok(UpdatePlan::Install { release, assets }) => {
                if check_only {
                    println!(
                        "Update available: {} (assets {}).",
                        release.tag,
                        assets
                            .iter()
                            .map(|asset| asset.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    report_pin_freshness(&runtime, &catalog, mode);
                    continue;
                }
                let require = catalog
                    .setting("LlamaCppRequireDownloadPins")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                for asset in &assets {
                    let pin = localbox::update::pin_for(&catalog, &asset.name);
                    runtime.block_on(localbox::update::install_asset(
                        asset,
                        &root,
                        pin.as_deref(),
                        require,
                    ))?;
                }
                let names: Vec<&str> = assets.iter().map(|a| a.name.as_str()).collect();
                let variant = localbox::update::stamp_variant(mode, &names, driver_major, amd_gpu);
                write_stamp(&root, &release.tag, &variant).map_err(|e| e.to_string())?;
                println!("Installed {} into {}.", release.tag, root.display());
            }
            Err(message) => println!("Skipped: {message}"),
        }
        if check_only {
            report_pin_freshness(&runtime, &catalog, mode);
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

/// On `--check`, report whether a mode's configured pin lags the latest
/// upstream release. Informational only — nothing auto-installs; a stale pin
/// advances via `--refresh-pins` or the settings ceremony.
fn report_pin_freshness(
    runtime: &tokio::runtime::Runtime,
    catalog: &localbox_launcher::catalog::Catalog,
    mode: Mode,
) {
    use localbox::update::{fetch_release, mode_release_source, pin_freshness, PinFreshness};
    let Some((repo, Some(pinned))) = mode_release_source(catalog, mode) else {
        return; // unpinned modes already track latest; mtpturbo reports itself
    };
    match runtime.block_on(fetch_release(&repo, None)) {
        Ok(latest) => match pin_freshness(&pinned, &latest.tag) {
            PinFreshness::Current => println!("Pin {pinned} is the latest upstream release."),
            PinFreshness::Behind { pinned, latest } => println!(
                "Pin is behind upstream: pinned {pinned}, latest {latest}. Advance \
                 deliberately with `localbox update --mode {} --refresh-pins`.",
                cli_mode_name(mode)
            ),
        },
        Err(e) => println!("Pin freshness unknown (release lookup failed: {e})."),
    }
}

/// The `--refresh-pins` path for one mode: resolve the latest release, show
/// the preview under `--check`, otherwise install with the upstream digest as
/// the integrity check and record the resulting tag + hashes in
/// `settings.json` (which outlives upgrades and wins layer precedence).
#[allow(clippy::too_many_arguments)]
fn refresh_mode_pins(
    runtime: &tokio::runtime::Runtime,
    catalog: &localbox_launcher::catalog::Catalog,
    mode: Mode,
    root: &std::path::Path,
    home: &std::path::Path,
    driver_major: Option<u32>,
    amd_gpu: bool,
    check_only: bool,
) -> Result<(), String> {
    use localbox::update::{
        install_asset, pinned_tag_setting_key, plan_refresh, refreshed_settings, write_stamp,
    };
    let Some(tag_key) = pinned_tag_setting_key(mode) else {
        println!("mtpturbo is source-built; nothing to refresh.");
        return Ok(());
    };
    let (release, assets) = runtime.block_on(plan_refresh(catalog, mode, driver_major, amd_gpu))?;
    if check_only {
        println!(
            "Would refresh {tag_key} to {} and record pins for: {}.",
            release.tag,
            assets
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        return Ok(());
    }
    let mut pins: Vec<(String, String)> = Vec::with_capacity(assets.len());
    for asset in &assets {
        // No local pin yet by definition; the upstream release digest inside
        // install_asset is the integrity check for these bytes.
        let sha = runtime.block_on(install_asset(asset, root, None, false))?;
        pins.push((asset.name.clone(), sha));
    }
    let settings_path = catalog_dir(home).join("settings.json");
    let existing: serde_json::Map<String, serde_json::Value> =
        match std::fs::read_to_string(&settings_path) {
            Ok(raw) => serde_json::from_str(raw.trim_start_matches('\u{feff}')).map_err(|e| {
                format!("settings.json is not valid JSON ({e}); fix it before refreshing")
            })?,
            Err(_) => serde_json::Map::new(),
        };
    let merged = refreshed_settings(&existing, tag_key, &release.tag, &pins);
    let pretty = serde_json::to_string_pretty(&serde_json::Value::Object(merged))
        .map_err(|e| e.to_string())?;
    std::fs::write(&settings_path, pretty + "\n").map_err(|e| e.to_string())?;
    let names: Vec<&str> = assets.iter().map(|a| a.name.as_str()).collect();
    let variant = localbox::update::stamp_variant(mode, &names, driver_major, amd_gpu);
    write_stamp(root, &release.tag, &variant).map_err(|e| e.to_string())?;
    println!(
        "Refreshed {tag_key} to {} and recorded {} pin(s) in {}.",
        release.tag,
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
                    "tuner_version": 1
                }]
            }"#,
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

        let tuned = build_request(&args, "m", home.path(), &launcher, true).unwrap();
        assert_eq!(tuned.quant.as_deref(), Some("tuned-quant"));
        assert_eq!(tuned.params.n_cpu_moe, Some(12));
        // Every request — `launch` and `serve` alike — carries the
        // single-session defaults unless a setting/profile chose otherwise.
        assert_eq!(tuned.params.parallel, Some(1));
        assert_eq!(tuned.params.cache_reuse, Some(256));

        let mut retry = build_request(&args, "m", home.path(), &launcher, false).unwrap();
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

        let request = build_request(&[], "bonsai", home.path(), &launcher, false).unwrap();
        assert_eq!(request.mode, Mode::PrismMl);

        let err = build_request(
            &args(&["--mode", "native"]),
            "bonsai",
            home.path(),
            &launcher,
            false,
        )
        .unwrap_err();
        assert!(err.contains("requires --mode prism"));
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
