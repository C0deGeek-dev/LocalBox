//! The guided launcher: pick a model → plain-language summary → confirm →
//! launch, with the power knobs one level down in Customize. A persistent
//! loop — after the agent exits, the picker returns.
//!
//! All flow decisions are pure functions over the tested `localbox-tui`
//! vocabulary/plan/customize layers; the interactive frontends (a ratatui
//! inline list and a numbered plain-text fallback) only pick indexes.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use localbox_launcher::catalog::Catalog;
use localbox_launcher::launcher::LlamaLauncher;
use localbox_launcher::orchestrate::{plan_launch, LaunchRequest};
use localbox_launcher::permissions::JsonSettingsStore;
use localbox_tui::customize::{
    customize_menu, locked_explanation, save_gate, set_auto_tune_off, set_auto_tune_on,
    CustomizeAction,
};
use localbox_tui::driver::{ensure_utf8_output, plain_menu, plain_warning, should_degrade};
use localbox_tui::plan::{resolve_launch_plan, DefaultLaunch, GuidedPlan, PlanOverrides};
use localbox_tui::ui::{
    render_guided_screen, ConfirmAction, GuidedScreen, MenuRow, ModelRow, CONFIRM_ROWS,
};
use localbox_tui::vocab::{engine_label, glossary, gpu_banner, plan_summary, target_label};
use localx_llama_core::{Mode, ModelDef, TunerBestConfig, TunerEntry};

use crate::exec::{home_dir, probe_gpu, probe_vram_gb};
use crate::live::{execute_launch, AgentKind};

/// Model rows visible by default: the `recommended` tier only (a definition
/// without a tier reads as `experimental` and stays hidden).
#[must_use]
pub fn model_tier(def: &ModelDef) -> String {
    def.tier
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or("experimental")
        .to_ascii_lowercase()
}

/// The picker's model keys: `recommended` only unless `show_all`; the flag
/// says whether a "[Show all tiers]" row makes sense (something is hidden).
#[must_use]
pub fn picker_keys(catalog: &Catalog, show_all: bool) -> (Vec<String>, bool) {
    let all: Vec<String> = catalog
        .model_keys()
        .iter()
        .map(|k| (*k).to_string())
        .collect();
    if show_all {
        return (all, false);
    }
    let recommended: Vec<String> = all
        .iter()
        .filter(|key| {
            catalog
                .model(key)
                .is_some_and(|def| model_tier(def) == "recommended")
        })
        .cloned()
        .collect();
    let hidden = all.len() > recommended.len();
    if recommended.is_empty() {
        // Nothing is marked recommended: show everything rather than a dead end.
        (all, false)
    } else {
        (recommended, hidden)
    }
}

/// Pick the AutoBest entry for the resolved plan: same quant/context/mode,
/// preferred profile first, then the closest measured VRAM, then score.
#[must_use]
pub fn pick_auto_best<'a>(
    store: &'a TunerBestConfig,
    plan: &GuidedPlan,
    vram_gb: i64,
) -> Option<&'a TunerEntry> {
    if !store.schema_supported() {
        return None;
    }
    let mut candidates: Vec<&TunerEntry> = store
        .entries
        .iter()
        .filter(|e| {
            e.quant == plan.quant && e.context_key == plan.context_key && e.mode == plan.mode
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let wanted_profile = plan.auto_best_profile.to_ascii_lowercase();
    candidates.sort_by(|a, b| {
        let a_profile = profile_rank(a, &wanted_profile);
        let b_profile = profile_rank(b, &wanted_profile);
        a_profile
            .cmp(&b_profile)
            .then_with(|| {
                (a.vram_gb - vram_gb)
                    .abs()
                    .cmp(&(b.vram_gb - vram_gb).abs())
            })
            .then_with(|| b.score.total_cmp(&a.score))
    });
    candidates.first().copied()
}

fn profile_rank(entry: &TunerEntry, wanted: &str) -> u8 {
    let name = match entry.profile {
        localx_llama_core::tuner::Profile::Pure => "pure",
        localx_llama_core::tuner::Profile::Balanced => "balanced",
    };
    u8::from(name != wanted)
}

/// Map the guided plan to the launch request and agent, folding in AutoBest
/// overrides when one was picked.
#[must_use]
pub fn request_from_guided(
    plan: &GuidedPlan,
    auto_best: Option<&TunerEntry>,
) -> (LaunchRequest, AgentKind) {
    let mut request =
        LaunchRequest::new(plan.model_key.clone(), plan.context_key.clone(), plan.mode);
    if !plan.quant.trim().is_empty() {
        request.quant = Some(plan.quant.clone());
    }
    request.use_vision = plan.vision;
    if let Some(entry) = auto_best {
        request.params = entry.overrides.to_launch_params();
    }
    if request.params.kv_k.is_none() {
        request.params.kv_k = plan.kv_cache_k.clone();
    }
    if request.params.kv_v.is_none() {
        request.params.kv_v = plan.kv_cache_v.clone();
    }
    request.params.strict = Some(plan.strict);
    let agent = match plan.target.as_str() {
        "localpilot" => AgentKind::LocalPilot,
        "codex" => AgentKind::Codex,
        "serve" => AgentKind::ServeOnly,
        _ => AgentKind::Claude,
    };
    (request, agent)
}

/// The downloaded default-quant GGUF's on-disk size, for model rows whose
/// catalog entry names no `SizeGB`. `None` when nothing is downloaded.
fn disk_size_gb(gguf_root: Option<&Path>, key: &str, def: &ModelDef) -> Option<f64> {
    let root = gguf_root?;
    let file = def
        .quant
        .as_deref()
        .and_then(|q| def.quants.get(q))
        .map(|entry| entry.file.clone())
        .or_else(|| def.file.clone())
        .filter(|f| !f.trim().is_empty())?;
    let folder = def.root.as_deref().unwrap_or(key);
    let bytes = std::fs::metadata(root.join(folder).join(file)).ok()?.len();
    #[allow(clippy::cast_precision_loss)]
    Some(bytes as f64 / 1e9)
}

/// The saved recipe from settings, when any.
#[must_use]
pub fn load_default_launch(catalog: &Catalog) -> DefaultLaunch {
    catalog
        .setting("DefaultLaunch")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

/// The recipe a "save as default" writes for the current plan.
#[must_use]
pub fn default_launch_from_plan(plan: &GuidedPlan) -> DefaultLaunch {
    DefaultLaunch {
        model_key: Some(plan.model_key.clone()),
        action: Some(plan.target.clone()),
        llama_cpp_mode: Some(plan.mode),
        auto_best_profile: Some(plan.auto_best_profile.clone()),
        use_auto_best: Some(plan.use_auto_best),
        quant: Some(plan.quant.clone()),
        context_key: Some(plan.context_key.clone()),
        kv_cache_k: plan.kv_cache_k.clone(),
        kv_cache_v: plan.kv_cache_v.clone(),
        strict: Some(plan.strict),
    }
}

/// One menu interaction: show rows, get an index back (`None` = cancelled).
/// The banner and panel are screen context the chooser carries between
/// choices — the rich path composes them into one frame with the menu, the
/// plain path prints them as text.
trait Chooser {
    fn set_banner(&mut self, banner: String);
    fn set_panel(&mut self, panel: Option<(String, String)>);
    fn choose(&mut self, title: &str, rows: &[MenuRow], start: usize) -> Option<usize>;
    fn notice(&mut self, text: &str);
    /// Hand the screen back to normal printing (before a launch).
    fn release(&mut self) {}
    /// Whether the user asked to leave the launcher entirely (Ctrl+C).
    fn quit_requested(&self) -> bool {
        false
    }
}

/// Numbered plain-text menus over stdin — the non-TTY / screen-reader path.
#[derive(Default)]
struct PlainChooser {
    banner: Option<String>,
    panel: Option<(String, String)>,
}

impl Chooser for PlainChooser {
    fn set_banner(&mut self, banner: String) {
        // Print once, up front: a repeated banner is noise in a transcript.
        if self.banner.as_deref() != Some(banner.as_str()) {
            println!("{banner}");
            self.banner = Some(banner);
        }
    }

    fn set_panel(&mut self, panel: Option<(String, String)>) {
        self.panel = panel;
    }

    fn choose(&mut self, title: &str, rows: &[MenuRow], _start: usize) -> Option<usize> {
        if let Some((panel_title, text)) = &self.panel {
            println!("{panel_title}:");
            println!("{text}");
        }
        let texts: Vec<String> = rows.iter().map(MenuRow::text).collect();
        print!("{}", plain_menu(title, &texts));
        println!("Enter a number (blank cancels):");
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return None;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        trimmed
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=rows.len()).contains(n))
            .map(|n| n - 1)
    }

    fn notice(&mut self, text: &str) {
        println!("{text}");
    }
}

type TuiTerminal = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

/// The rich path: ONE inline-viewport terminal for the whole guided
/// session (scrollback-safe per the pinned terminal options), every state
/// drawn as one composed frame so screens replace each other instead of
/// stacking. Raw mode lives only inside a single choice; notices are
/// inserted above the band into native scrollback.
#[derive(Default)]
struct TuiChooser {
    terminal: Option<TuiTerminal>,
    banner: String,
    panel: Option<(String, String)>,
    quit: bool,
}

impl TuiChooser {
    fn ensure_terminal(&mut self) -> std::io::Result<&mut TuiTerminal> {
        if self.terminal.is_none() {
            let backend = ratatui::backend::CrosstermBackend::new(std::io::stdout());
            self.terminal = Some(ratatui::Terminal::with_options(
                backend,
                localbox_tui::driver::terminal_options(),
            )?);
        }
        self.terminal
            .as_mut()
            .ok_or_else(|| std::io::Error::other("the terminal was just created"))
    }

    fn run_list(
        &mut self,
        title: &str,
        rows: &[MenuRow],
        start: usize,
    ) -> std::io::Result<Option<usize>> {
        use crossterm::event::{self, Event, KeyCode, KeyEventKind};

        let banner = self.banner.clone();
        let panel = self.panel.clone();
        let mut quit = false;
        let terminal = self.ensure_terminal()?;
        crossterm::terminal::enable_raw_mode()?;
        let mut selected = start.min(rows.len().saturating_sub(1));
        let result = loop {
            terminal.draw(|frame| {
                render_guided_screen(
                    frame,
                    &GuidedScreen {
                        banner: &banner,
                        panel: panel
                            .as_ref()
                            .map(|(panel_title, text)| (panel_title.as_str(), text.as_str())),
                        menu_title: title,
                        rows,
                        selected,
                    },
                );
            })?;
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                // Raw mode swallows the console's Ctrl+C: honor it as
                // "leave the launcher", not as a dead key.
                if key.code == KeyCode::Char('c')
                    && key.modifiers.contains(event::KeyModifiers::CONTROL)
                {
                    quit = true;
                    break None;
                }
                match key.code {
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(rows.len().saturating_sub(1)),
                    KeyCode::Enter => break Some(selected),
                    KeyCode::Esc | KeyCode::Char('q') => break None,
                    _ => {}
                }
            }
        };
        crossterm::terminal::disable_raw_mode()?;
        self.quit = quit;
        Ok(result)
    }
}

impl Chooser for TuiChooser {
    fn set_banner(&mut self, banner: String) {
        self.banner = banner;
    }

    fn set_panel(&mut self, panel: Option<(String, String)>) {
        self.panel = panel;
    }

    fn choose(&mut self, title: &str, rows: &[MenuRow], start: usize) -> Option<usize> {
        match self.run_list(title, rows, start) {
            Ok(choice) => choice,
            Err(e) => {
                let _ = crossterm::terminal::disable_raw_mode();
                eprintln!("{}", plain_warning("menu", &e.to_string()));
                None
            }
        }
    }

    fn notice(&mut self, text: &str) {
        match self.terminal.as_mut() {
            // The band stays live: notices go ABOVE it, into scrollback.
            Some(terminal) => {
                use ratatui::text::Line;
                use ratatui::widgets::{Paragraph, Widget};
                let lines: Vec<Line> = text.lines().map(|l| Line::from(l.to_string())).collect();
                let height = u16::try_from(lines.len().max(1)).unwrap_or(u16::MAX);
                let _ = terminal.insert_before(height, |buf| {
                    Paragraph::new(lines.clone()).render(buf.area, buf);
                });
            }
            None => println!("{text}"),
        }
    }

    fn release(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
        // Drain any queued key events so a child process starting on this
        // console never inherits our leftover keystrokes.
        while crossterm::event::poll(std::time::Duration::ZERO).unwrap_or(false) {
            let _ = crossterm::event::read();
        }
        if let Some(mut terminal) = self.terminal.take() {
            // Clear the band so normal printing continues from a clean line.
            let _ = terminal.clear();
        }
    }

    fn quit_requested(&self) -> bool {
        self.quit
    }
}

/// Run the guided launcher until the user cancels out of the model picker.
///
/// # Errors
/// A plain-language message when the catalog or home cannot be resolved.
pub fn run_guided(plain_requested: bool) -> Result<(), String> {
    ensure_utf8_output();
    let home = home_dir().ok_or("could not determine the user home directory")?;
    // Before the viewport: a 1.x leftover warning belongs in scrollback.
    let leftovers = crate::migrate::v1_leftover_notice(&crate::migrate::find_v1_leftovers(&home));
    if !leftovers.is_empty() {
        println!("{leftovers}");
    }
    let degraded = should_degrade(std::io::stdout().is_terminal(), plain_requested);
    let mut chooser: Box<dyn Chooser> = if degraded {
        Box::new(PlainChooser::default())
    } else {
        Box::new(TuiChooser::default())
    };
    let gpu = probe_gpu();
    let vram = i64::from(gpu.as_ref().map_or(0, |info| info.vram_gb));
    chooser.set_banner(gpu_banner(gpu.as_ref()));
    let mut show_all = false;

    loop {
        let catalog_dir = catalog_dir(&home);
        let catalog = Catalog::load(&catalog_dir).map_err(|e| e.to_string())?;
        let (keys, show_all_offered) = picker_keys(&catalog, show_all);

        let gguf_root = catalog.gguf_root().map(|root| {
            localbox_launcher::launcher::expand_path_with_home(&root.to_string_lossy(), &home)
        });
        let mut rows: Vec<MenuRow> = keys
            .iter()
            .map(|key| {
                catalog.model(key).map_or_else(
                    || MenuRow::plain(key.clone()),
                    |def| {
                        let mut row = ModelRow::from_def(key, def, vram);
                        if row.size_gb.is_none() {
                            // The catalog names no size: the downloaded
                            // file's real size is still honest data.
                            row.size_gb = disk_size_gb(gguf_root.as_deref(), key, def);
                            row.fit = localx_llama_core::vram::quant_fit_class(row.size_gb, vram);
                        }
                        row.menu_row()
                    },
                )
            })
            .collect();
        if show_all_offered {
            rows.push(MenuRow::plain("[Show all tiers]"));
        }
        rows.push(MenuRow::plain("[Cancel]"));

        chooser.set_panel(None);
        let Some(index) = chooser.choose("Pick a model", &rows, 0) else {
            chooser.release();
            return Ok(());
        };
        if show_all_offered && index == rows.len() - 2 {
            show_all = true;
            continue;
        }
        if index == rows.len() - 1 {
            chooser.release();
            return Ok(());
        }
        let key = keys[index].clone();
        let Some(def) = catalog.model(&key).cloned() else {
            continue;
        };

        confirm_flow(chooser.as_mut(), &home, &catalog, &key, &def, vram);
        if chooser.quit_requested() {
            chooser.release();
            return Ok(());
        }
        show_all = false;
    }
}

fn confirm_flow(
    chooser: &mut dyn Chooser,
    home: &Path,
    catalog: &Catalog,
    key: &str,
    def: &ModelDef,
    vram: i64,
) {
    let defaults = load_default_launch(catalog);
    let mut overrides = PlanOverrides::default();

    loop {
        let plan = resolve_launch_plan(key, def, &defaults, &overrides);
        chooser.set_panel(Some((
            "Recommended plan".to_string(),
            plan_summary(&plan, def),
        )));
        let rows: Vec<MenuRow> = CONFIRM_ROWS
            .iter()
            .map(|(label, _)| MenuRow::plain(*label))
            .collect();
        let Some(choice) = chooser.choose(&format!("Ready to launch {key}?"), &rows, 0) else {
            return;
        };
        match CONFIRM_ROWS[choice].1 {
            ConfirmAction::LaunchNow => {
                launch_guided(chooser, home, &plan);
                return;
            }
            ConfirmAction::Customize => {
                customize_flow(chooser, home, key, def, &defaults, &mut overrides, vram);
                if chooser.quit_requested() {
                    return;
                }
            }
            ConfirmAction::AutoTune => {
                // Hand the screen over and actually run the benchmark the
                // row promises; the confirm menu returns afterwards.
                chooser.release();
                chooser.notice(
                    "Auto-tune measures this model on your GPU and saves the fastest safe \
                     settings. The benchmark can take a while — Ctrl+C stops it.",
                );
                let args = vec![
                    "findbest".to_string(),
                    "--model".to_string(),
                    key.to_string(),
                ];
                match crate::exec::run_interactive("localbench", &args) {
                    Ok(status) if status.success() => chooser.notice(
                        "Auto-tune finished. Launch now (with Auto-tune on) uses the saved settings.",
                    ),
                    Ok(_) => chooser.notice(
                        "Auto-tune did not finish; the recommended defaults still apply.",
                    ),
                    Err(_) => chooser.notice(
                        "LocalBench is not installed, so Auto-tune cannot run.\n\
                         Install it, then either pick this row again or run:\n  localbench findbest --model <model>",
                    ),
                }
            }
            ConfirmAction::Help => chooser.notice(glossary()),
            ConfirmAction::BackToModels => return,
        }
    }
}

fn customize_flow(
    chooser: &mut dyn Chooser,
    home: &Path,
    key: &str,
    def: &ModelDef,
    defaults: &DefaultLaunch,
    overrides: &mut PlanOverrides,
    vram: i64,
) {
    loop {
        if chooser.quit_requested() {
            return;
        }
        let plan = resolve_launch_plan(key, def, defaults, overrides);
        chooser.set_panel(Some(("Current plan".to_string(), plan_summary(&plan, def))));
        let menu = customize_menu(&plan, def);
        let rows: Vec<MenuRow> = menu
            .iter()
            .map(|row| MenuRow::plain(row.label.clone()))
            .collect();
        let Some(choice) = chooser.choose("Customize settings", &rows, 0) else {
            return;
        };
        let action = &menu[choice].action;
        if let Some(explanation) = locked_explanation(action) {
            chooser.notice(explanation);
            continue;
        }
        match action {
            CustomizeAction::PickTarget => {
                let targets = ["localpilot", "claude", "codex", "serve"];
                let labels: Vec<MenuRow> = targets
                    .iter()
                    .map(|t| MenuRow::plain(target_label(t)))
                    .collect();
                if let Some(i) = chooser.choose("Run with", &labels, 0) {
                    overrides.target = Some(targets[i].to_string());
                }
            }
            CustomizeAction::PickQuant => {
                let quants: Vec<String> = def.quants.keys().cloned().collect();
                if quants.is_empty() {
                    chooser.notice("This model has a single build; nothing to pick.");
                    continue;
                }
                let labels: Vec<MenuRow> = quants
                    .iter()
                    .map(|q| {
                        let hint = MenuRow::plain(localbox_tui::vocab::quality_hint(q));
                        match def.quants.get(q).and_then(|entry| entry.size_gb) {
                            Some(gb) => hint.with_fit(
                                format!(" · {gb:.1} GB"),
                                localx_llama_core::vram::quant_fit_class(Some(gb), vram),
                            ),
                            None => hint,
                        }
                    })
                    .collect();
                if let Some(i) = chooser.choose("Quality", &labels, 0) {
                    overrides.quant = Some(quants[i].clone());
                }
            }
            CustomizeAction::PickContext => {
                let contexts: Vec<String> = def.contexts.keys().cloned().collect();
                let labels: Vec<MenuRow> = contexts
                    .iter()
                    .map(|c| MenuRow::plain(localbox_tui::vocab::memory_label(def, c)))
                    .collect();
                if let Some(i) = chooser.choose("Memory (conversation size)", &labels, 0) {
                    overrides.context_key = Some(contexts[i].clone());
                }
            }
            CustomizeAction::PickMode => {
                let modes = [Mode::Native, Mode::Turboquant, Mode::Mtpturbo];
                let labels: Vec<MenuRow> = modes
                    .iter()
                    .map(|m| MenuRow::plain(engine_label(*m)))
                    .collect();
                if let Some(i) = chooser.choose("Engine", &labels, 0) {
                    overrides.mode = Some(modes[i]);
                }
            }
            CustomizeAction::PickAutoTune => {
                let labels = vec![
                    MenuRow::plain("On — use my auto-tuned settings"),
                    MenuRow::plain("Off — use the recommended defaults"),
                ];
                if let Some(i) = chooser.choose("Auto-tune", &labels, 0) {
                    if i == 0 {
                        set_auto_tune_on(overrides, "balanced");
                    } else {
                        set_auto_tune_off(overrides);
                    }
                }
            }
            CustomizeAction::PickKv => {
                let fork = plan.mode != Mode::Native;
                let mut kinds = vec!["auto (default)", "q8_0", "q4_0", "f16"];
                if fork {
                    kinds.extend(["turbo3", "turbo4"]);
                }
                let labels: Vec<MenuRow> = kinds.iter().map(|k| MenuRow::plain(*k)).collect();
                if let Some(i) = chooser.choose("KV cache", &labels, 0) {
                    if i == 0 {
                        overrides.kv_cache_k = None;
                        overrides.kv_cache_v = None;
                    } else {
                        overrides.kv_cache_k = Some(kinds[i].to_string());
                        overrides.kv_cache_v = Some(kinds[i].to_string());
                    }
                }
            }
            CustomizeAction::ToggleVision => {
                overrides.vision = Some(!plan.vision);
            }
            CustomizeAction::ToggleStrict => {
                overrides.strict = Some(!plan.strict);
            }
            CustomizeAction::SaveDefault => match save_gate(&plan.target) {
                Ok(()) => {
                    let settings_path = home.join(".local-llm").join("settings.json");
                    match JsonSettingsStore::open(&settings_path) {
                        Ok(mut store) => {
                            if let Ok(value) = serde_json::to_value(default_launch_from_plan(&plan))
                            {
                                store.persist_value("DefaultLaunch", value);
                                chooser.notice("Saved. Launch now replays these settings.");
                            }
                        }
                        Err(e) => chooser.notice(&plain_warning("save", &e.to_string())),
                    }
                }
                Err(reason) => chooser.notice(&reason),
            },
            CustomizeAction::Done => return,
            CustomizeAction::ModeLocked | CustomizeAction::KvLocked => {}
        }
    }
}

fn launch_guided(chooser: &mut dyn Chooser, home: &Path, plan: &GuidedPlan) {
    // Hand the screen back to normal printing: downloads, server spawn,
    // and the agent all write plain lines from here on.
    chooser.release();
    chooser.notice(&format!("Launching {} …", plan.model_key));
    let auto_store = if plan.use_auto_best {
        let path = home
            .join(".local-llm")
            .join("tuner")
            .join(format!("best-{}.json", plan.model_key));
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str::<TunerBestConfig>(&raw).ok())
    } else {
        None
    };
    let vram = i64::from(probe_vram_gb());
    let entry = auto_store
        .as_ref()
        .and_then(|store| pick_auto_best(store, plan, vram));
    if plan.use_auto_best && entry.is_none() {
        chooser.notice(
            "No auto-tuned profile matches these settings; using the recommended defaults.",
        );
    }
    let (request, agent) = request_from_guided(plan, entry);

    let catalog = match Catalog::load(&catalog_dir(home)) {
        Ok(c) => c,
        Err(e) => {
            chooser.notice(&plain_warning("launch", &e.to_string()));
            return;
        }
    };
    let launcher = LlamaLauncher::new(catalog, crate::product_version(), home, probe_vram_gb());
    let resolved = match plan_launch(&launcher, &request) {
        Ok(p) => p,
        Err(e) => {
            chooser.notice(&plain_warning("launch", &e.to_string()));
            return;
        }
    };
    for note in &resolved.notes {
        chooser.notice(note);
    }
    match execute_launch(&launcher, &resolved, &request, agent, home) {
        Ok(_) => {
            if agent == AgentKind::ServeOnly {
                chooser.notice(&format!(
                    "Serving {} at {}",
                    resolved.key, resolved.base_url
                ));
            }
        }
        Err(e) => chooser.notice(&plain_warning("launch", &e.to_string())),
    }
}

/// The catalog directory: the installed `~/.local-llm` tree, or a repo
/// checkout's `local-llm/` when running from source. An empty installed
/// tree is seeded on first use (the defaults plus the example catalog as
/// the user's own `llm-models.json`, never overwriting anything).
#[must_use]
pub fn catalog_dir(home: &Path) -> PathBuf {
    let installed = home.join(".local-llm");
    if installed.join("llm-models.json").is_file() {
        return installed;
    }
    let checkout = PathBuf::from("local-llm");
    if checkout.is_dir() {
        return checkout;
    }
    seed_installed_tree(&installed);
    installed
}

/// First-run seeding of `~/.local-llm`: write the shipped defaults and the
/// example catalog as the user's editable catalog. Existing files are never
/// touched.
pub fn seed_installed_tree(installed: &Path) {
    let _ = std::fs::create_dir_all(installed);
    let seeds: [(&str, &str); 3] = [
        (
            "defaults.json",
            include_str!("../../../local-llm/defaults.json"),
        ),
        (
            "llm-models.example.json",
            include_str!("../../../local-llm/llm-models.example.json"),
        ),
        (
            "llm-models.json",
            include_str!("../../../local-llm/llm-models.example.json"),
        ),
    ];
    for (name, content) in seeds {
        let path = installed.join(name);
        if !path.exists() {
            let _ = std::fs::write(path, content);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use localx_llama_core::tuner::{Overrides, Profile, PromptLength};

    fn def_with_tier(tier: Option<&str>) -> ModelDef {
        let mut def: ModelDef = serde_json::from_str(r#"{"Repo":"o/m"}"#).unwrap();
        def.tier = tier.map(str::to_string);
        def
    }

    #[test]
    fn disk_size_fallback_reads_the_real_file_and_never_guesses() {
        let dir = tempfile::tempdir().unwrap();
        let mut def: ModelDef =
            serde_json::from_str(r#"{"Repo":"o/m","File":"model.gguf"}"#).unwrap();
        // Nothing downloaded → no number.
        assert_eq!(disk_size_gb(Some(dir.path()), "mkey", &def), None);
        assert_eq!(disk_size_gb(None, "mkey", &def), None);
        // A real file under <root>/<key>/<file> reports its true size.
        std::fs::create_dir_all(dir.path().join("mkey")).unwrap();
        std::fs::write(
            dir.path().join("mkey").join("model.gguf"),
            vec![0u8; 2_000_000],
        )
        .unwrap();
        let gb = disk_size_gb(Some(dir.path()), "mkey", &def).unwrap();
        assert!((gb - 0.002).abs() < 1e-9);
        // An explicit Root folder wins over the key.
        def.root = Some("elsewhere".to_string());
        assert_eq!(disk_size_gb(Some(dir.path()), "mkey", &def), None);
    }

    #[test]
    fn tier_defaults_to_experimental_and_hides_from_the_picker() {
        assert_eq!(model_tier(&def_with_tier(None)), "experimental");
        assert_eq!(model_tier(&def_with_tier(Some("  "))), "experimental");
        assert_eq!(
            model_tier(&def_with_tier(Some("Recommended"))),
            "recommended"
        );
    }

    fn entry(
        quant: &str,
        context: &str,
        mode: Mode,
        vram: i64,
        profile: Profile,
        score: f64,
    ) -> TunerEntry {
        TunerEntry {
            quant: quant.to_string(),
            context_key: context.to_string(),
            context_tokens: None,
            mode,
            vram_gb: vram,
            prompt_length: PromptLength::Long,
            profile,
            search_strategy: None,
            beam_width: None,
            score,
            score_unit: "tps".to_string(),
            pure_score: None,
            args: vec![],
            overrides: Overrides {
                n_gpu_layers: Some(99),
                ..Overrides::default()
            },
            measured_at: "2026-01-01".to_string(),
            tuner_version: 1,
            trial_count: None,
            gpu_names: None,
            llamacpp_build: None,
        }
    }

    fn guided(quant: &str, context: &str, mode: Mode) -> GuidedPlan {
        GuidedPlan {
            model_key: "m".to_string(),
            target: "claude".to_string(),
            quant: quant.to_string(),
            context_key: context.to_string(),
            mode,
            auto_best_profile: "balanced".to_string(),
            use_auto_best: true,
            vision: false,
            strict: false,
            kv_cache_k: None,
            kv_cache_v: None,
        }
    }

    #[test]
    fn auto_best_picks_matching_slot_preferring_profile_then_vram() {
        let store = TunerBestConfig {
            schema: 1,
            key: "m".to_string(),
            vram_gb: Some(24),
            entries: vec![
                entry("q4", "64k", Mode::Native, 24, Profile::Pure, 400.0),
                entry("q4", "64k", Mode::Native, 24, Profile::Balanced, 300.0),
                entry("q4", "64k", Mode::Native, 12, Profile::Balanced, 350.0),
                entry("q6", "64k", Mode::Native, 24, Profile::Balanced, 500.0),
            ],
        };
        let plan = guided("q4", "64k", Mode::Native);
        let picked = pick_auto_best(&store, &plan, 24).unwrap();
        // Balanced (the wanted profile) at the matching VRAM wins, even
        // though the pure entry scores higher and q6 scores higher still.
        assert_eq!(picked.profile, Profile::Balanced);
        assert_eq!(picked.vram_gb, 24);

        // A different quant/context/mode never matches.
        let other = guided("q8", "64k", Mode::Native);
        assert!(pick_auto_best(&store, &other, 24).is_none());

        // An unsupported schema yields nothing (fail closed).
        let bad = TunerBestConfig {
            schema: 2,
            ..store.clone()
        };
        assert!(pick_auto_best(&bad, &plan, 24).is_none());
    }

    #[test]
    fn guided_request_maps_target_kv_and_auto_best_overrides() {
        let mut plan = guided("q4", "64k", Mode::Turboquant);
        plan.target = "serve".to_string();
        plan.kv_cache_k = Some("turbo3".to_string());
        plan.kv_cache_v = Some("turbo3".to_string());

        let tuned = entry("q4", "64k", Mode::Turboquant, 24, Profile::Balanced, 300.0);
        let (request, agent) = request_from_guided(&plan, Some(&tuned));
        assert_eq!(agent, AgentKind::ServeOnly);
        assert_eq!(request.quant.as_deref(), Some("q4"));
        // AutoBest overrides land in the launch params...
        assert_eq!(request.params.n_gpu_layers, Some(99));
        // ...and the manual KV only fills gaps the profile left open.
        assert_eq!(request.params.kv_k.as_deref(), Some("turbo3"));

        let (request, agent) = request_from_guided(&guided("", "64k", Mode::Native), None);
        assert_eq!(agent, AgentKind::Claude);
        assert_eq!(request.quant, None);
        assert_eq!(request.params.strict, Some(false));
    }

    #[test]
    fn saved_recipe_round_trips_the_plan() {
        let mut plan = guided("q4", "64k", Mode::Native);
        plan.target = "localpilot".to_string();
        let saved = default_launch_from_plan(&plan);
        assert_eq!(saved.model_key.as_deref(), Some("m"));
        assert_eq!(saved.action.as_deref(), Some("localpilot"));
        assert_eq!(saved.quant.as_deref(), Some("q4"));
        // The on-disk spelling stays PascalCase/Action (the shipped shape).
        let value = serde_json::to_value(&saved).unwrap();
        assert!(value.get("Action").is_some());
        assert!(value.get("ModelKey").is_some());
    }
}
