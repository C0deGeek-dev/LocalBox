//! The `localbox` command-line entry point.
//!
//! Hand-rolled argument handling: the surface is small and stable, and the
//! binary must start instantly on every OS. Command work runs on a worker
//! thread with an explicit stack size — Windows main threads are smaller
//! than the Linux/macOS default and deep CLI/TUI stacks overflow there.

use std::process::ExitCode;

use localbox::exec::{home_dir, probe_vram_gb};
use localbox::guided::{catalog_dir, run_guided};
use localbox::live::{execute_launch, status_report, stop_all, AgentKind};
use localbox::{product_envelope, product_version};
use localbox_launcher::catalog::Catalog;
use localbox_launcher::launcher::LlamaLauncher;
use localbox_launcher::orchestrate::{plan_launch, LaunchRequest};
use localx_llama_core::Mode;
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
  localbox update [--mode <m>] [--check]
                                      install or update the llama.cpp binaries
  localbox version                    print the launcher version envelope
  localbox nothink-proxy --listen <port> --target-port <port>
                                      host the no-think proxy (plumbing)

Options for launch/serve:
  --context <key>       context window key from the model catalog (e.g. 64k)
  --mode <m>            native | turboquant | mtpturbo   (default native)
  --quant <key>         quant variant from the catalog (default per model)
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
        other => Err(format!(
            "unknown mode '{other}' (expected native, turboquant, or mtpturbo)"
        )),
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

fn build_launcher(home: &std::path::Path) -> Result<LlamaLauncher, String> {
    let dir = catalog_dir(home);
    let catalog = Catalog::load(&dir).map_err(|e| e.to_string())?;
    Ok(LlamaLauncher::new(
        catalog,
        product_version(),
        home,
        probe_vram_gb(),
    ))
}

fn cmd_launch(args: &[String], default_agent: AgentKind) -> Result<(), String> {
    let model = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .ok_or("a model key is required (run `localbox launch <model>`)")?;
    let home = home_dir().ok_or("could not determine the user home directory")?;

    let mut request = LaunchRequest::new(
        model.clone(),
        flag_value(args, "--context").unwrap_or("").to_string(),
        parse_mode(flag_value(args, "--mode"))?,
    );
    request.quant = flag_value(args, "--quant").map(str::to_string);
    request.use_vision = has_flag(args, "--vision");
    request.keep_thinking = has_flag(args, "--keep-thinking");

    let agent = parse_agent(flag_value(args, "--agent"), default_agent)?;

    // Single-session defaults for an interactive agent launch: one slot and a
    // prompt-cache-reuse window keep the session predictable (llama-server's
    // default multi-slot competition destabilizes a single agent's cache). A
    // headless `serve` keeps the server defaults; AutoBest overrides win because
    // they arrive as `Some(_)`.
    if agent != AgentKind::ServeOnly {
        if request.params.parallel.is_none() {
            request.params.parallel = Some(1);
        }
        if request.params.cache_reuse.is_none() {
            request.params.cache_reuse = Some(256);
        }
    }

    let launcher = build_launcher(&home)?;
    let mut plan = plan_launch(&launcher, &request).map_err(|e| e.to_string())?;

    // Apply the LAN posture BEFORE the dry-run print so `--dry-run --lan` shows
    // the gateway plan that a live `--lan` launch would actually use.
    if has_flag(args, "--lan") {
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

    if has_flag(args, "--dry-run") {
        print_plan(&plan);
        return Ok(());
    }

    let outcome =
        execute_launch(&launcher, &plan, &request, agent, &home).map_err(|e| e.to_string())?;
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
        println!("Vision:    {}", vision.display());
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
    let modes: Vec<Mode> = match flag_value(args, "--mode") {
        Some(m) => vec![parse_mode(Some(m))?],
        None => vec![Mode::Native, Mode::Turboquant, Mode::Mtpturbo],
    };
    let driver_major = localbox::update::parse_cuda_driver_major(&nvidia_smi_banner());
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;

    for mode in modes {
        let root = localx_llama_core::Launcher::install_root(&launcher, mode);
        println!("== {} ==", mode.as_str());
        match runtime.block_on(plan_binary_update(&catalog, mode, &root, driver_major)) {
            Ok(UpdatePlan::UpToDate { tag }) => println!("Up to date ({tag})."),
            Ok(UpdatePlan::MtpStatus { message }) => println!("{message}"),
            Ok(UpdatePlan::Install { release, asset }) => {
                if check_only {
                    println!("Update available: {} (asset {}).", release.tag, asset.name);
                    continue;
                }
                let pin = localbox::update::pin_for(&catalog, &asset.name);
                let require = catalog
                    .setting("LlamaCppRequireDownloadPins")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                runtime.block_on(localbox::update::install_asset(
                    &asset,
                    &root,
                    pin.as_deref(),
                    require,
                ))?;
                let variant = if driver_major.is_some() {
                    "cuda"
                } else {
                    "cpu"
                };
                write_stamp(&root, &release.tag, variant).map_err(|e| e.to_string())?;
                println!("Installed {} into {}.", release.tag, root.display());
            }
            Err(message) => println!("Skipped: {message}"),
        }
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
