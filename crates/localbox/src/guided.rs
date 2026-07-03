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
use localbox_tui::plan::{
    find_workspace_default, resolve_launch_plan, DefaultLaunch, GuidedPlan, PlanOverrides,
};
use localbox_tui::ui::{
    render_guided_screen, render_notice_screen, ConfirmAction, GuidedScreen, MenuRow, ModelRow,
    CONFIRM_ROWS,
};
use localbox_tui::vocab::{glossary, gpu_banner, plan_summary, target_label};
use localx_llama_core::{Mode, ModelDef, TunerBestConfig, TunerEntry};
use ratatui::style::Color;

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

/// The downloaded GGUF's on-disk size for a specific quant (`None` = the
/// model's default quant), for rows whose catalog entry names no `SizeGB`.
/// `None` when nothing is downloaded.
fn quant_disk_size_gb(
    gguf_root: Option<&Path>,
    key: &str,
    def: &ModelDef,
    quant: Option<&str>,
) -> Option<f64> {
    let root = gguf_root?;
    // A named quant resolves ONLY its own file; the single-file fallback
    // belongs to the default-quant path.
    let file = match quant {
        Some(q) => def.quants.get(q).map(|entry| entry.file.clone()),
        None => def
            .quant
            .as_deref()
            .and_then(|q| def.quants.get(q))
            .map(|entry| entry.file.clone())
            .or_else(|| def.file.clone()),
    }
    .filter(|f| !f.trim().is_empty())?;
    let folder = def.root.as_deref().unwrap_or(key);
    let bytes = std::fs::metadata(root.join(folder).join(file)).ok()?.len();
    #[allow(clippy::cast_precision_loss)]
    Some(bytes as f64 / 1e9)
}

/// A quant's size in GB: the catalog's `SizeGB`, else the downloaded file.
fn quant_size_gb(gguf_root: Option<&Path>, key: &str, def: &ModelDef, quant: &str) -> Option<f64> {
    def.quants
        .get(quant)
        .and_then(|entry| entry.size_gb)
        .or_else(|| quant_disk_size_gb(gguf_root, key, def, Some(quant)))
}

/// One quality option row: `hint · quant-key · GB` with the size colored by
/// its fit — enough to tell two "best quality" builds apart.
fn quant_menu_row(
    gguf_root: Option<&Path>,
    key: &str,
    def: &ModelDef,
    quant: &str,
    vram: i64,
) -> MenuRow {
    let base = format!("{} · {quant}", localbox_tui::vocab::quality_hint(quant));
    match quant_size_gb(gguf_root, key, def, quant) {
        Some(gb) => MenuRow::plain(base).with_fit(
            format!(" · {gb:.1} GB"),
            localx_llama_core::vram::quant_fit_class(Some(gb), vram),
        ),
        None => MenuRow::plain(base),
    }
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
    /// Show a terminal outcome (a launch result or an error) and hold it on
    /// screen until the user acknowledges it. The rich path re-acquires the
    /// band and waits for a key; the plain path just prints. Without this, an
    /// outcome shown after [`Chooser::release`] is a bare line the next menu
    /// redraw buries — invisible until the launcher exits.
    fn announce(&mut self, text: &str) {
        self.notice(text);
    }
    /// Show an *error* outcome, held on screen until acknowledged. The rich
    /// path frames it in a red box; the plain path just prints it.
    fn announce_error(&mut self, text: &str) {
        self.notice(text);
    }
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

    /// Hold a terminal outcome on the band until the user acknowledges it.
    /// Re-acquires the band the launch released, so the message renders as a
    /// held screen instead of a bare line the next menu redraw buries.
    fn dwell_notice(&mut self, title: &str, text: &str, border: Option<Color>) {
        use crossterm::event::{self, Event, KeyCode, KeyEventKind};

        let banner = self.banner.clone();
        let Ok(terminal) = self.ensure_terminal() else {
            println!("{text}");
            return;
        };
        if crossterm::terminal::enable_raw_mode().is_err() {
            println!("{text}");
            return;
        }
        let mut quit = false;
        loop {
            let _ = terminal.draw(|frame| {
                render_notice_screen(frame, &banner, title, text, border);
            });
            match event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(event::KeyModifiers::CONTROL)
                    {
                        quit = true;
                        break;
                    }
                    if matches!(
                        key.code,
                        KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char(' ')
                    ) {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let _ = crossterm::terminal::disable_raw_mode();
        self.quit = quit || self.quit;
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

    fn announce(&mut self, text: &str) {
        self.dwell_notice("LocalBox", text, None);
    }

    fn announce_error(&mut self, text: &str) {
        self.dwell_notice("Error", text, Some(Color::Red));
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
                            row.size_gb = quant_disk_size_gb(gguf_root.as_deref(), key, def, None);
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

        // Preselect the workspace's `.llm-default` model when the cwd (or an
        // ancestor) names one and it is in the visible list — the documented
        // per-workspace default that previously did nothing.
        let start = std::env::current_dir()
            .ok()
            .and_then(|cwd| find_workspace_default(&cwd))
            .and_then(|(key, _)| keys.iter().position(|k| *k == key))
            .unwrap_or(0);

        chooser.set_panel(None);
        let Some(index) = chooser.choose("Pick a model", &rows, start) else {
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
    let gguf_root = catalog.gguf_root().map(|root| {
        localbox_launcher::launcher::expand_path_with_home(&root.to_string_lossy(), home)
    });

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
                customize_flow(
                    chooser,
                    home,
                    key,
                    def,
                    &defaults,
                    &mut overrides,
                    vram,
                    gguf_root.as_deref(),
                );
                if chooser.quit_requested() {
                    return;
                }
            }
            ConfirmAction::AutoTune => {
                auto_tune_flow(chooser, key, def, &plan, vram, gguf_root.as_deref());
            }
            ConfirmAction::Help => chooser.notice(glossary()),
            ConfirmAction::BackToModels => return,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn customize_flow(
    chooser: &mut dyn Chooser,
    home: &Path,
    key: &str,
    def: &ModelDef,
    defaults: &DefaultLaunch,
    overrides: &mut PlanOverrides,
    vram: i64,
    gguf_root: Option<&Path>,
) {
    // The cursor survives the loop: a toggle re-renders with the selection
    // still on the row that was toggled, not jumped back to the top.
    let mut cursor = 0;
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
        let Some(choice) = chooser.choose("Customize settings", &rows, cursor) else {
            return;
        };
        cursor = choice;
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
                let current = targets.iter().position(|t| *t == plan.target).unwrap_or(0);
                if let Some(i) = pick_option(chooser, "Run with", labels, current, Some(0)) {
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
                    .map(|q| quant_menu_row(gguf_root, key, def, q, vram))
                    .collect();
                let current = quants.iter().position(|q| *q == plan.quant).unwrap_or(0);
                let default = def
                    .quant
                    .as_deref()
                    .and_then(|dq| quants.iter().position(|q| q == dq));
                if let Some(i) = pick_option(chooser, "Quality", labels, current, default) {
                    overrides.quant = Some(quants[i].clone());
                }
            }
            CustomizeAction::PickContext => {
                let contexts: Vec<String> = def.contexts.keys().cloned().collect();
                let labels: Vec<MenuRow> = contexts
                    .iter()
                    .map(|c| MenuRow::plain(localbox_tui::vocab::memory_label(def, c)))
                    .collect();
                let current = contexts
                    .iter()
                    .position(|c| *c == plan.context_key)
                    .unwrap_or(0);
                let default = contexts.iter().position(String::is_empty);
                if let Some(i) = pick_option(
                    chooser,
                    "Memory (conversation size)",
                    labels,
                    current,
                    default,
                ) {
                    overrides.context_key = Some(contexts[i].clone());
                }
            }
            CustomizeAction::PickMode => {
                let labels: Vec<MenuRow> = TUNE_ENGINES
                    .iter()
                    .map(|(_, _, desc)| MenuRow::plain(*desc))
                    .collect();
                let current = TUNE_ENGINES
                    .iter()
                    .position(|(_, mode, _)| *mode == plan.mode)
                    .unwrap_or(0);
                if let Some(i) = pick_option(chooser, "Engine", labels, current, Some(0)) {
                    overrides.mode = Some(TUNE_ENGINES[i].1);
                }
            }
            CustomizeAction::PickAutoTune => {
                let labels = vec![
                    MenuRow::plain("On — use my auto-tuned settings"),
                    MenuRow::plain("Off — use the recommended defaults"),
                ];
                let current = usize::from(!plan.use_auto_best);
                if let Some(i) = pick_option(chooser, "Auto-tune", labels, current, None) {
                    if i == 0 {
                        set_auto_tune_on(overrides, "balanced");
                    } else {
                        set_auto_tune_off(overrides);
                    }
                }
            }
            CustomizeAction::PickKv => {
                let fork = plan.mode != Mode::Native;
                let mut kinds = vec!["auto", "q8_0", "q4_0", "f16"];
                if fork {
                    kinds.extend(["turbo3", "turbo4"]);
                }
                let labels: Vec<MenuRow> = kinds
                    .iter()
                    .map(|k| {
                        MenuRow::plain(match *k {
                            "auto" => "auto — let the launcher choose",
                            "q8_0" => "q8_0 — compact, nearly lossless",
                            "q4_0" => "q4_0 — smallest, saves the most memory",
                            "f16" => "f16 — full precision, uses the most memory",
                            "turbo3" => "turbo3 — the Turbo engines' 3-bit compact format",
                            "turbo4" => "turbo4 — the Turbo engines' 4-bit compact format",
                            other => other,
                        })
                    })
                    .collect();
                let current = plan
                    .kv_cache_k
                    .as_deref()
                    .and_then(|kv| kinds.iter().position(|k| *k == kv))
                    .unwrap_or(0);
                if let Some(i) = pick_option(chooser, "KV cache", labels, current, Some(0)) {
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
                                // Saving is a finishing move: hand back to
                                // the Ready-to-launch menu.
                                return;
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

/// What the Auto-tune sub-menu is set to; Enter on a value row opens the
/// option list for it (current selection and default marked).
struct TuneChoices {
    /// Index into [`TUNE_PROFILES`].
    profile: usize,
    /// Index into [`TUNE_WORKLOADS`].
    workload: usize,
    /// Index into [`TUNE_ENGINES`]; starts at the launch plan's engine.
    engine: usize,
    /// Index into the model's quant list; starts at the plan's quant.
    quant: usize,
    /// Index into the model's context list; starts at the plan's context.
    context: usize,
    /// Index into [`TUNE_BUDGETS`].
    budget: usize,
    /// Index into [`TUNE_RUNS`].
    runs: usize,
    /// Ignore cached trial measurements and measure fresh.
    fresh: bool,
    /// Save the winner (off = preview only, `--no-save`).
    save: bool,
}

/// `(short row value, findbest flag value, sub-menu description)`.
const TUNE_PROFILES: &[(&str, &str, &str)] = &[
    (
        "Balanced",
        "balanced",
        "Balanced — best mix of speed and stability",
    ),
    ("Pure speed", "pure", "Pure speed — fastest generation wins"),
    (
        "Both profiles",
        "both",
        "Both profiles — save one winner per profile (takes longer)",
    ),
];
const TUNE_WORKLOADS: &[(&str, &str, &str)] = &[
    (
        "Coding agent",
        "coding-agent",
        "Coding agent — the mixed workload a coding assistant produces",
    ),
    (
        "Generation speed",
        "gen",
        "Generation speed — how fast answers are written",
    ),
    (
        "Prompt processing",
        "prompt",
        "Prompt processing — how fast long context is read",
    ),
    (
        "Both",
        "both",
        "Both — generation and prompt processing (takes longer)",
    ),
];
/// `(short row value, engine mode, sub-menu description)`.
const TUNE_ENGINES: &[(&str, Mode, &str)] = &[
    (
        "Standard",
        Mode::Native,
        "Standard (native) — plain llama.cpp, the most compatible",
    ),
    (
        "Turbo",
        Mode::Turboquant,
        "Turbo (turboquant) — a tuned llama.cpp build, faster on supported GPUs",
    ),
    (
        "Turbo+",
        Mode::Mtpturbo,
        "Turbo+ (mtpturbo) — Turbo plus draft speed-ups, fastest when the model supports it",
    ),
];
const TUNE_BUDGETS: &[(&str, &str, &str)] = &[
    (
        "Standard (30 trials)",
        "30",
        "Standard — 30 trials, the sensible middle",
    ),
    (
        "Quick (15 trials)",
        "15",
        "Quick — 15 trials, faster but a rougher answer",
    ),
    (
        "Deep (60 trials)",
        "60",
        "Deep — 60 trials, the best winner, takes longest",
    ),
];
const TUNE_RUNS: &[(&str, &str, &str)] = &[
    (
        "Steady (3 per trial)",
        "3",
        "Steady — each measurement repeated 3 times",
    ),
    (
        "Fast (1 per trial)",
        "1",
        "Fast — one measurement each, quickest but noisier",
    ),
    (
        "Extra steady (5 per trial)",
        "5",
        "Extra steady — 5 repeats, slowest but most reliable numbers",
    ),
];
const TUNE_MEASUREMENTS: &[&str] = &[
    "Reuse cached results — skip measurements already taken in earlier tunes",
    "Fresh — ignore the cache and measure everything again",
];
const TUNE_SAVE: &[&str] = &[
    "Yes — save the winner so Launch now replays it",
    "No — preview only, nothing is saved",
];

/// Offer a setting's options as a sub-menu: the launch default and the
/// current selection are marked, the cursor starts on the current value,
/// and picking an option returns to the previous menu.
fn pick_option(
    chooser: &mut dyn Chooser,
    title: &str,
    options: Vec<MenuRow>,
    current: usize,
    default: Option<usize>,
) -> Option<usize> {
    let rows: Vec<MenuRow> = options
        .into_iter()
        .enumerate()
        .map(|(i, mut row)| {
            if default == Some(i) {
                row = row.with("   (default)");
            }
            if i == current {
                row = row.with("   ← selected");
            }
            row
        })
        .collect();
    chooser.choose(title, &rows, current)
}

/// Plain-language help for every tune setting (the ℹ row).
const TUNE_GLOSSARY: &str = "\
Auto-tune settings:

\x20 Optimize for – Balanced favours stable speed with safe memory use;\n\
\x20                Pure speed chases raw tokens/second; Both saves one\n\
\x20                winner per profile.\n\
\x20 Workload     – what gets measured: Coding agent is the mixed\n\
\x20                agent-style workload; Generation speed times answers;\n\
\x20                Prompt processing times reading long context.\n\
\x20 Engine       – Standard is plain llama.cpp; Turbo and Turbo+ are the\n\
\x20                tuned forks (Turbo+ adds draft speed-ups).\n\
\x20 Quality      – which build of the model to measure. The winner only\n\
\x20                replays on launches with the same Quality.\n\
\x20 Memory       – the conversation size to measure at; also part of\n\
\x20                what the winner replays on.\n\
\x20 Trials       – how many setting combinations to try. More finds a\n\
\x20                better winner but takes longer.\n\
\x20 Runs         – repeats per measurement for steadier numbers.\n\
\x20 Measurements – reuse cached results from earlier tunes, or measure\n\
\x20                everything fresh.\n\
\x20 Save winner  – keep the best settings for Launch now, or run as a\n\
\x20                preview that saves nothing.\n\
\n\
KV-cache variants and GPU offload are explored automatically inside the\n\
tune. Images/vision do not affect it.";

/// The Auto-tune sub-menu: every `findbest` knob with a plain value on the
/// row; Enter opens that setting's option list (default and current
/// selection marked; picking returns here). Quant/context start at the
/// CURRENT launch settings so the winner is one that "Launch now" actually
/// replays.
fn auto_tune_flow(
    chooser: &mut dyn Chooser,
    key: &str,
    def: &ModelDef,
    plan: &GuidedPlan,
    vram: i64,
    gguf_root: Option<&Path>,
) {
    let quants: Vec<String> = def.quants.keys().cloned().collect();
    let contexts: Vec<String> = def.contexts.keys().cloned().collect();
    let engine_default = TUNE_ENGINES
        .iter()
        .position(|(_, mode, _)| *mode == plan.mode)
        .unwrap_or(0);
    let quant_default = quants.iter().position(|q| *q == plan.quant).unwrap_or(0);
    let context_default = contexts
        .iter()
        .position(|c| *c == plan.context_key)
        .unwrap_or(0);
    let mut choices = TuneChoices {
        profile: 0,
        workload: 0,
        engine: engine_default,
        quant: quant_default,
        context: context_default,
        budget: 0,
        runs: 0,
        fresh: false,
        save: true,
    };
    chooser.set_panel(Some((
        "Auto-tune".to_string(),
        "Benchmarks this model on your GPU and saves the winning\n\
         settings, so Launch now (with Auto-tune on) replays them.\n\
         KV-cache variants and GPU offload are explored automatically.\n\
         Pick ℹ for what each setting means."
            .to_string(),
    )));

    let mut cursor = 0;
    loop {
        let quant_value = quants.get(choices.quant).map_or_else(
            || "single build".to_string(),
            |q| quant_menu_row(gguf_root, key, def, q, vram).text(),
        );
        let context_value = contexts.get(choices.context).map_or_else(
            || "model default".to_string(),
            |c| localbox_tui::vocab::memory_label(def, c),
        );
        let rows = vec![
            MenuRow::plain(format!(
                "Optimize for:  {}",
                TUNE_PROFILES[choices.profile].0
            )),
            MenuRow::plain(format!(
                "Workload:      {}",
                TUNE_WORKLOADS[choices.workload].0
            )),
            MenuRow::plain(format!("Engine:        {}", TUNE_ENGINES[choices.engine].0)),
            MenuRow::plain(format!("Quality:       {quant_value}")),
            MenuRow::plain(format!("Memory:        {context_value}")),
            MenuRow::plain(format!("Trials:        {}", TUNE_BUDGETS[choices.budget].0)),
            MenuRow::plain(format!("Runs:          {}", TUNE_RUNS[choices.runs].0)),
            MenuRow::plain(format!(
                "Measurements:  {}",
                if choices.fresh {
                    "fresh (ignore cached results)"
                } else {
                    "reuse cached results"
                }
            )),
            MenuRow::plain(format!(
                "Save winner:   {}",
                if choices.save { "yes" } else { "preview only" }
            )),
            MenuRow::plain("ℹ  What do these mean?"),
            MenuRow::plain("▶  Start auto-tune"),
            MenuRow::plain("←  Back"),
        ];
        let Some(choice) = chooser.choose(
            "Auto-tune this model (Enter opens a setting)",
            &rows,
            cursor,
        ) else {
            return;
        };
        cursor = choice;
        match choice {
            0 => {
                let options = TUNE_PROFILES
                    .iter()
                    .map(|(_, _, desc)| MenuRow::plain(*desc))
                    .collect();
                if let Some(i) =
                    pick_option(chooser, "Optimize for", options, choices.profile, Some(0))
                {
                    choices.profile = i;
                }
            }
            1 => {
                let options = TUNE_WORKLOADS
                    .iter()
                    .map(|(_, _, desc)| MenuRow::plain(*desc))
                    .collect();
                if let Some(i) =
                    pick_option(chooser, "Workload", options, choices.workload, Some(0))
                {
                    choices.workload = i;
                }
            }
            2 => {
                let options = TUNE_ENGINES
                    .iter()
                    .map(|(_, _, desc)| MenuRow::plain(*desc))
                    .collect();
                if let Some(i) = pick_option(
                    chooser,
                    "Engine",
                    options,
                    choices.engine,
                    Some(engine_default),
                ) {
                    choices.engine = i;
                }
            }
            3 => {
                if quants.is_empty() {
                    chooser.notice("This model has a single build; nothing to pick.");
                    continue;
                }
                let options = quants
                    .iter()
                    .map(|q| quant_menu_row(gguf_root, key, def, q, vram))
                    .collect();
                if let Some(i) = pick_option(
                    chooser,
                    "Quality (which build to measure)",
                    options,
                    choices.quant,
                    Some(quant_default),
                ) {
                    choices.quant = i;
                }
            }
            4 => {
                if contexts.is_empty() {
                    chooser.notice("This model has a single conversation size.");
                    continue;
                }
                let options = contexts
                    .iter()
                    .map(|c| MenuRow::plain(localbox_tui::vocab::memory_label(def, c)))
                    .collect();
                if let Some(i) = pick_option(
                    chooser,
                    "Memory (conversation size to measure at)",
                    options,
                    choices.context,
                    Some(context_default),
                ) {
                    choices.context = i;
                }
            }
            5 => {
                let options = TUNE_BUDGETS
                    .iter()
                    .map(|(_, _, desc)| MenuRow::plain(*desc))
                    .collect();
                if let Some(i) = pick_option(chooser, "Trials", options, choices.budget, Some(0)) {
                    choices.budget = i;
                }
            }
            6 => {
                let options = TUNE_RUNS
                    .iter()
                    .map(|(_, _, desc)| MenuRow::plain(*desc))
                    .collect();
                if let Some(i) = pick_option(chooser, "Runs", options, choices.runs, Some(0)) {
                    choices.runs = i;
                }
            }
            7 => {
                let options = TUNE_MEASUREMENTS
                    .iter()
                    .map(|t| MenuRow::plain(*t))
                    .collect();
                if let Some(i) = pick_option(
                    chooser,
                    "Measurements",
                    options,
                    usize::from(choices.fresh),
                    Some(0),
                ) {
                    choices.fresh = i == 1;
                }
            }
            8 => {
                let options = TUNE_SAVE.iter().map(|t| MenuRow::plain(*t)).collect();
                if let Some(i) = pick_option(
                    chooser,
                    "Save winner",
                    options,
                    usize::from(!choices.save),
                    Some(0),
                ) {
                    choices.save = i == 0;
                }
            }
            9 => chooser.notice(TUNE_GLOSSARY),
            10 => break,
            _ => return,
        }
    }

    // Hand the screen over and run the benchmark; the confirm menu
    // returns afterwards.
    chooser.release();
    chooser.notice("Auto-tune is starting — Ctrl+C stops it.");
    let mode = TUNE_ENGINES[choices.engine].1;
    let quant = quants.get(choices.quant).cloned().unwrap_or_default();
    let context = contexts.get(choices.context).cloned().unwrap_or_default();
    let mut args = vec![
        "findbest".to_string(),
        "--model".to_string(),
        key.to_string(),
        "--mode".to_string(),
        mode.as_str().to_string(),
        "--budget".to_string(),
        TUNE_BUDGETS[choices.budget].1.to_string(),
        "--runs".to_string(),
        TUNE_RUNS[choices.runs].1.to_string(),
        "--profile".to_string(),
        TUNE_PROFILES[choices.profile].1.to_string(),
        "--optimize".to_string(),
        TUNE_WORKLOADS[choices.workload].1.to_string(),
    ];
    if !context.trim().is_empty() {
        args.push("--context".to_string());
        args.push(context);
    }
    if !quant.trim().is_empty() {
        args.push("--quant".to_string());
        args.push(quant);
    }
    if choices.fresh {
        args.push("--no-cache".to_string());
    }
    if !choices.save {
        args.push("--no-save".to_string());
    }

    match crate::exec::run_interactive("localbench", &args) {
        Ok(status) if status.success() => chooser.announce(if choices.save {
            "Auto-tune finished. Launch now (with Auto-tune on) uses the saved settings."
        } else {
            "Auto-tune preview finished; nothing was saved."
        }),
        Ok(_) => {
            chooser.announce("Auto-tune did not finish; the recommended defaults still apply.");
        }
        Err(_) => chooser.announce_error(
            "LocalBench is not installed, so Auto-tune cannot run.\n\
             Install it, then either pick this row again or run:\n  localbench findbest --model <model>",
        ),
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
            chooser.announce_error(&plain_warning("launch", &e.to_string()));
            return;
        }
    };
    let launcher = LlamaLauncher::new(catalog, crate::product_version(), home, probe_vram_gb());
    let resolved = match plan_launch(&launcher, &request) {
        Ok(p) => p,
        Err(e) => {
            chooser.announce_error(&plain_warning("launch", &e.to_string()));
            return;
        }
    };
    for note in &resolved.notes {
        chooser.notice(note);
    }
    match execute_launch(&launcher, &resolved, &request, agent, home) {
        Ok(_) => {
            if agent == AgentKind::ServeOnly {
                chooser.announce(&format!(
                    "Serving {} at {}",
                    resolved.key, resolved.base_url
                ));
            }
        }
        Err(e) => chooser.announce_error(&plain_warning("launch", &e.to_string())),
    }
}

/// The catalog directory: the installed `~/.local-llm` tree, or a repo
/// checkout's `local-llm/` when running from source. An empty installed
/// tree is seeded on first use (the defaults plus the example catalog as
/// the user's own `llm-models.json`, never overwriting anything).
#[must_use]
pub fn catalog_dir(home: &Path) -> PathBuf {
    let installed = home.join(".local-llm");
    // First run always seeds the user's own tree, so `llm-models.json` exists
    // before anything reads it — no one is ever told to copy a file by hand.
    // Seeding is idempotent and never overwrites, so it is safe every run and
    // independent of the working directory.
    seed_installed_tree(&installed);
    // A source checkout's `local-llm/` stays the live catalog when developing.
    if PathBuf::from("local-llm").is_dir() {
        return PathBuf::from("local-llm");
    }
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
        assert_eq!(
            quant_disk_size_gb(Some(dir.path()), "mkey", &def, None),
            None
        );
        assert_eq!(quant_disk_size_gb(None, "mkey", &def, None), None);
        // A real file under <root>/<key>/<file> reports its true size.
        std::fs::create_dir_all(dir.path().join("mkey")).unwrap();
        std::fs::write(
            dir.path().join("mkey").join("model.gguf"),
            vec![0u8; 2_000_000],
        )
        .unwrap();
        let gb = quant_disk_size_gb(Some(dir.path()), "mkey", &def, None).unwrap();
        assert!((gb - 0.002).abs() < 1e-9);
        // A named quant resolves its own file (absent here → no number).
        assert_eq!(
            quant_disk_size_gb(Some(dir.path()), "mkey", &def, Some("missing-quant")),
            None
        );
        // An explicit Root folder wins over the key.
        def.root = Some("elsewhere".to_string());
        assert_eq!(
            quant_disk_size_gb(Some(dir.path()), "mkey", &def, None),
            None
        );
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

    #[test]
    fn catalog_dir_seeds_the_user_catalog_so_no_one_copies_by_hand() {
        let home = tempfile::tempdir().unwrap();
        let _ = catalog_dir(home.path());
        // The user's own editable catalog and its defaults exist after the
        // first resolution — the "copy llm-models.example.json" path is gone.
        let installed = home.path().join(".local-llm");
        assert!(
            installed.join("llm-models.json").is_file(),
            "llm-models.json is seeded on first run"
        );
        assert!(
            installed.join("defaults.json").is_file(),
            "defaults.json is seeded alongside it"
        );
        // A user edit is never clobbered on a later run.
        std::fs::write(installed.join("llm-models.json"), "{\"Models\":{}}").unwrap();
        let _ = catalog_dir(home.path());
        assert_eq!(
            std::fs::read_to_string(installed.join("llm-models.json")).unwrap(),
            "{\"Models\":{}}",
            "seeding never overwrites an existing catalog"
        );
    }
}
