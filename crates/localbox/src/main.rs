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
use localx_llama_runtime::proxy::{serve_proxy, ProxyConfig};

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
  localbox version                    print the launcher version envelope
  localbox nothink-proxy --listen <port> --target-port <port>
                                      host the no-think proxy (plumbing)

Options for launch/serve:
  --context <key>       context window key from the model catalog (e.g. 64k)
  --mode <m>            native | turboquant | mtpturbo   (default native)
  --quant <key>         quant variant from the catalog (default per model)
  --vision              load the vision projector when the model has one
  --keep-thinking       let the model's thinking reach the agent unfiltered
  --agent <a>           claude | localpilot | codex | none  (default claude)
  --dry-run             print what would happen; change nothing
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
    let launcher = build_launcher(&home)?;
    let plan = plan_launch(&launcher, &request).map_err(|e| e.to_string())?;

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
    Ok(())
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
    };
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime
        .block_on(serve_proxy(listen, config))
        .map_err(|e| e.to_string())
}
