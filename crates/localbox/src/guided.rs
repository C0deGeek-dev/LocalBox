//! The guided launcher: pick a model → plain-language summary → confirm →
//! launch, with the power knobs one level down in Customize. A persistent
//! loop — after the agent exits, the picker returns.
//!
//! All flow decisions are pure functions over the tested `localbox-tui`
//! vocabulary/plan/customize layers; the interactive frontends (a ratatui
//! inline list and a numbered plain-text fallback) only pick indexes.

use std::io::{IsTerminal, Write as _};
use std::path::{Path, PathBuf};

use localbox_launcher::catalog::Catalog;
use localbox_launcher::fetch::DownloadKind;
use localbox_launcher::orchestrate::{plan_launch, LaunchRequest};
use localbox_launcher::permissions::JsonSettingsStore;
use localbox_launcher::profile::{
    adopt_superseded_profile, resolve_run_profile, RunProfileQuery, RunProfileResolution,
};
use localbox_tui::customize::{
    customize_menu_with_required_mode, locked_explanation, save_gate, set_auto_tune_off,
    set_auto_tune_on, CustomizeAction,
};
use localbox_tui::driver::{ensure_utf8_output, plain_menu, plain_warning, should_degrade};
use localbox_tui::plan::{
    find_workspace_default, resolve_launch_plan_with_required_mode, DefaultLaunch, GuidedPlan,
    PlanOverrides,
};
use localbox_tui::ui::{
    render_guided_screen, render_notice_screen, ConfirmAction, GuidedScreen, MenuRow, ModelRow,
    CONFIRM_ROWS,
};
use localbox_tui::vocab::{glossary, gpu_banner, plan_summary, target_label};
use localx_llama_core::tuner::Profile;
use localx_llama_core::{FitClass, Mode, ModelDef, TunerEntry};
use ratatui::style::Color;

use crate::exec::{home_dir, probe_gpu};
use crate::fetch::ProgressPrinter;
use crate::hf_install::{candidate_size_gb, discover_hf_repo, recommend_quant, HfDiscovery};
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
    // The same unit the catalog's `SizeGB` is written in; anything else makes
    // the comparison below a unit conversion masquerading as a drift check.
    // Unrounded: a measurement keeps its precision, and the 0.15GB drift
    // threshold is far wider than the catalog's one-decimal rounding.
    Some(localbox_launcher::catalog_entry::gib(bytes))
}

fn validated_quant_size_gb(
    key: &str,
    quant: &str,
    catalog_size_gb: Option<f64>,
    disk_size_gb: Option<f64>,
) -> Option<f64> {
    if let (Some(catalog), Some(disk)) = (catalog_size_gb, disk_size_gb) {
        if (catalog - disk).abs() > 0.15 {
            eprintln!(
                "Warning: catalog size for {key}/{quant} is {catalog:.1}GB but the downloaded GGUF is {disk:.1}GB; using the on-disk size."
            );
        }
    }
    disk_size_gb.or(catalog_size_gb)
}

/// A quant's size in GB. A downloaded file is authoritative; catalog metadata
/// is the fallback when the file is not present yet.
fn quant_size_gb(gguf_root: Option<&Path>, key: &str, def: &ModelDef, quant: &str) -> Option<f64> {
    let catalog_size = def.quants.get(quant).and_then(|entry| entry.size_gb);
    let disk_size = quant_disk_size_gb(gguf_root, key, def, Some(quant));
    validated_quant_size_gb(key, quant, catalog_size, disk_size)
}

fn quant_note_for_display(note: Option<&str>, quant: &str) -> Option<String> {
    fn canonical(value: &str) -> String {
        value
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    }
    fn only_size(value: &str) -> bool {
        let trimmed = value.trim().trim_start_matches('~').trim();
        let Some(number) = trimmed
            .strip_suffix("GB")
            .or_else(|| trimmed.strip_suffix("gb"))
        else {
            return false;
        };
        number.trim().parse::<f64>().is_ok()
    }

    let quant = canonical(quant);
    let useful = note?
        .split('·')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .filter(|part| canonical(part) != quant && !only_size(part))
        .collect::<Vec<_>>()
        .join(" · ");
    (!useful.is_empty()).then_some(useful)
}

fn truncate_row_note(note: &str, available: usize) -> Option<String> {
    if available < 4 {
        return None;
    }
    let count = note.chars().count();
    if count <= available {
        return Some(note.to_string());
    }
    let mut shortened = note
        .chars()
        .take(available.saturating_sub(1))
        .collect::<String>();
    shortened.push('…');
    Some(shortened)
}

/// One quality option row: `hint · quant-key · GB · fit · note`. The size is
/// colored in the TUI, while the explicit fit word keeps the plain/screen-
/// reader path truthful. At narrow widths the redundant hint is dropped and
/// only the note is ellipsized, so the quant, size, and verdict remain visible.
fn quant_menu_row(
    gguf_root: Option<&Path>,
    key: &str,
    def: &ModelDef,
    quant: &str,
    vram: i64,
) -> MenuRow {
    let width = crossterm::terminal::size()
        .ok()
        .map_or(usize::MAX, |(width, _)| {
            // Reserve the list border/selection marker and the longest
            // `Quality:  ` prefix used by the customize summary.
            usize::from(width.saturating_sub(15))
        });
    quant_menu_row_at_width(gguf_root, key, def, quant, vram, width)
}

fn quant_menu_row_at_width(
    gguf_root: Option<&Path>,
    key: &str,
    def: &ModelDef,
    quant: &str,
    vram: i64,
    max_width: usize,
) -> MenuRow {
    let full_base = format!("{} · {quant}", localbox_tui::vocab::quality_hint(quant));
    let size = quant_size_gb(gguf_root, key, def, quant);
    let fit = localx_llama_core::vram::quant_fit_class(size, vram);
    let size_text = size.map(|gb| format!(" · {gb:.1} GB"));
    let fit_text = size.map(|_| format!(" · {}", fit.as_str()));
    let note = def
        .quants
        .get(quant)
        .and_then(|entry| quant_note_for_display(entry.note.as_deref(), quant));
    let fixed_suffix_len =
        size_text.as_ref().map_or(0, String::len) + fit_text.as_ref().map_or(0, String::len);
    let full_note_len = note.as_ref().map_or(0, |note| note.chars().count() + 3);
    let base = if full_base.chars().count() + fixed_suffix_len + full_note_len <= max_width {
        full_base
    } else {
        quant.to_string()
    };
    let used = base.chars().count() + fixed_suffix_len;
    let note = note.and_then(|note| {
        let available = max_width.saturating_sub(used + 3);
        truncate_row_note(&note, available)
    });

    let mut row = MenuRow::plain(base);
    if let Some(size_text) = size_text {
        row = row.with_fit(size_text, fit);
    }
    if let Some(fit_text) = fit_text {
        row = row.with(fit_text);
    }
    if let Some(note) = note {
        row = row.with(format!(" · {note}"));
    }
    row
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
        vision: Some(plan.vision),
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
    /// Temporarily leave menu rendering and read one trimmed line. Empty input
    /// cancels. The rich chooser re-acquires its inline band on the next menu.
    fn input(&mut self, prompt: &str) -> Option<String> {
        self.release();
        println!("{prompt}");
        print!("> ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return None;
        }
        let value = line.trim();
        (!value.is_empty()).then(|| value.to_string())
    }
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
    let probed_vram = i64::from(gpu.as_ref().map_or(0, |info| info.vram_gb));
    // Recommendations and all fit rows use the same shared ladder as launch:
    // explicit VRAMGB setting > hardware probe > conservative fallback.
    let vram = crate::exec::build_launcher(&home)
        .map(|launcher| i64::from(localx_llama_core::Launcher::vram_gb(&launcher)))
        .unwrap_or(probed_vram);
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
                        let disk_size = quant_disk_size_gb(gguf_root.as_deref(), key, def, None);
                        if disk_size.is_some() {
                            let quant = def.quant.as_deref().unwrap_or("default");
                            row.size_gb =
                                validated_quant_size_gb(key, quant, row.size_gb, disk_size);
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
        let add_model_index = rows.len();
        rows.push(MenuRow::plain("[Add a Hugging Face model]"));
        let cancel_index = rows.len();
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
        if show_all_offered && index == add_model_index - 1 {
            show_all = true;
            continue;
        }
        if index == add_model_index {
            chooser.set_panel(None);
            if add_hf_model_flow(chooser.as_mut(), &home, vram) {
                // Synthesized entries default to the experimental tier. Show
                // all on return so the model the user just added is visible.
                show_all = true;
            }
            if chooser.quit_requested() {
                chooser.release();
                return Ok(());
            }
            continue;
        }
        if index == cancel_index {
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

/// One discovered quant row: friendly quality hint, key, combined size, and an
/// explicit fit label that remains meaningful in the plain/screen-reader path.
fn discovered_quant_row(
    candidate: &localx_llama_core::quant::QuantCandidate,
    vram: i64,
    recommended: bool,
) -> MenuRow {
    let mut row = MenuRow::plain(format!(
        "{} · {}",
        localbox_tui::vocab::quality_hint(&candidate.key),
        candidate.key
    ));
    if let Some(gb) = candidate_size_gb(candidate) {
        let fit = localx_llama_core::vram::quant_fit_class(Some(gb), vram);
        row = row
            .with_fit(format!(" · {gb:.1} GB"), fit)
            .with(format!(" · {}", fit.as_str()));
    } else {
        row = row.with(" · size unknown");
    }
    if recommended {
        row = row.with("  [recommended]");
    }
    row
}

fn recommendation_panel(
    discovery: &HfDiscovery,
    recommended: &localx_llama_core::quant::QuantCandidate,
    vram: i64,
) -> String {
    let estimate = match candidate_size_gb(recommended) {
        Some(gb) => {
            let fit = localx_llama_core::vram::quant_fit_class(Some(gb), vram);
            let reason = match fit {
                FitClass::Fits => "leaves at least 7 GB for context and overhead",
                FitClass::Tight => "leaves at least 2 GB; shorter contexts are safer",
                FitClass::Over => "exceeds the detected graphics-memory budget",
                FitClass::Unknown => "has no reliable size estimate",
            };
            format!(
                "{} ({gb:.1} GB, {} — {reason})",
                recommended.key,
                fit.as_str()
            )
        }
        None => format!("{} (repository default; size unavailable)", recommended.key),
    };
    format!(
        "Found {} quant variants in {}.\n\
         Suggested: {estimate}. This is a weight-size/headroom estimate, not a benchmark.\n\
         Nothing downloads until you explicitly choose a quant below.",
        discovery.candidates.len(),
        discovery.hf.repo_id()
    )
}

/// Register a Hugging Face repository from inside the guided launcher. Returns
/// `true` only when the catalog was intentionally registered/enriched.
fn add_hf_model_flow(chooser: &mut dyn Chooser, home: &Path, vram: i64) -> bool {
    let Some(reference) = chooser.input(
        "Enter a Hugging Face GGUF repo id or URL (for example owner/model-GGUF).\n\
         Leave blank to go back.",
    ) else {
        return false;
    };
    let hf = match localbox_launcher::hf_meta::parse_hf_ref(&reference) {
        Ok(hf) => hf,
        Err(_) => {
            chooser.announce_error(
                "That is not a Hugging Face repo id or URL. Expected owner/repo or \
                 https://huggingface.co/owner/repo.",
            );
            return false;
        }
    };
    chooser.notice("Reading the repository's GGUF variants — nothing is downloading …");
    let discovery = match discover_hf_repo(hf) {
        Ok(discovery) => discovery,
        Err(error) => {
            chooser.announce_error(&plain_warning("add model", &error));
            return false;
        }
    };
    let Some(recommended) = recommend_quant(&discovery.candidates, vram) else {
        chooser.announce_error("No selectable GGUF quant variants were found.");
        return false;
    };
    let recommended_key = recommended.key.clone();
    let recommended_index = discovery
        .candidates
        .iter()
        .position(|candidate| candidate.key == recommended_key)
        .unwrap_or(0);
    let mut rows: Vec<MenuRow> = discovery
        .candidates
        .iter()
        .map(|candidate| discovered_quant_row(candidate, vram, candidate.key == recommended_key))
        .collect();
    let register_only_index = rows.len();
    rows.push(MenuRow::plain("[Register only — download later]"));
    let cancel_index = rows.len();
    rows.push(MenuRow::plain("[Cancel — make no changes]"));
    chooser.set_panel(Some((
        "Quant recommendation".to_string(),
        recommendation_panel(&discovery, recommended, vram),
    )));
    let Some(choice) = chooser.choose(
        "Choose a quant to download, or register without downloading",
        &rows,
        recommended_index,
    ) else {
        return false;
    };
    if choice == cancel_index {
        return false;
    }

    let selected_key =
        (choice < discovery.candidates.len()).then(|| discovery.candidates[choice].key.clone());
    let default_key = selected_key.as_deref().unwrap_or(&recommended_key);
    let outcome = match discovery.register(&catalog_dir(home), default_key) {
        Ok(outcome) => outcome,
        Err(error) => {
            chooser.announce_error(&plain_warning("add model", &error));
            return false;
        }
    };
    let key = outcome.key().to_string();
    let catalog_message = match &outcome {
        CatalogInsert::Inserted(_) => format!(
            "Registered '{key}' with all {} quant variants.",
            discovery.candidates.len()
        ),
        CatalogInsert::Updated { added_quants, .. } => format!(
            "Updated '{key}' with {} missing quant variants.",
            added_quants.len()
        ),
        CatalogInsert::AlreadyPresent(_) => {
            format!("'{key}' was already complete in your catalog.")
        }
    };
    if choice == register_only_index {
        let default_message = if matches!(outcome, CatalogInsert::Inserted(_)) {
            format!("Suggested default: {recommended_key}.")
        } else {
            "Your existing default quant was preserved.".to_string()
        };
        chooser.announce(&format!(
            "{catalog_message}\nNo model files were downloaded. {default_message}"
        ));
        return true;
    }

    let Some(selected_key) = selected_key else {
        return true;
    };
    match download_guided_quant(chooser, home, &key, &selected_key) {
        Ok(paths) => chooser.announce(&format!(
            "{catalog_message}\nDownloaded '{}' ({} file(s)).",
            selected_key,
            paths.len()
        )),
        Err(error) => chooser.announce_error(&format!(
            "{catalog_message}\nThe catalog registration succeeded, but the download failed:\n{}",
            plain_warning("download", &error)
        )),
    }
    true
}

fn download_guided_quant(
    chooser: &mut dyn Chooser,
    home: &Path,
    key: &str,
    quant: &str,
) -> Result<Vec<PathBuf>, String> {
    let launcher = crate::exec::build_launcher(home)?;
    let targets = launcher
        .model_download_targets(key, Some(quant))
        .map_err(|e| e.to_string())?;
    let count = targets
        .iter()
        .filter(|target| target.kind == DownloadKind::Gguf)
        .count();
    if count == 0 {
        return Err(format!("'{key}' resolves to no GGUF files"));
    }
    chooser.release();
    chooser.notice(&format!("Downloading quant '{quant}' ({count} file(s)) …"));
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let mut printer = ProgressPrinter::default();
    let paths = runtime
        .block_on(launcher.fetch_model_files(
            key,
            Some(quant),
            &[DownloadKind::Gguf],
            &mut |progress| printer.report(progress),
        ))
        .map_err(|e| e.to_string())?;
    for path in &paths {
        chooser.notice(&format!("ready: {}", path.display()));
    }
    Ok(paths)
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
    let required_mode = catalog.required_mode(key);
    let mut overrides = PlanOverrides::default();
    let gguf_root = catalog.gguf_root().map(|root| {
        localbox_launcher::launcher::expand_path_with_home(&root.to_string_lossy(), home)
    });

    loop {
        let plan =
            resolve_launch_plan_with_required_mode(key, def, &defaults, &overrides, required_mode);
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
                if launch_guided(chooser, home, &plan) {
                    return;
                }
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
                    required_mode,
                );
                if chooser.quit_requested() {
                    return;
                }
            }
            ConfirmAction::AutoTune => {
                auto_tune_flow(
                    chooser,
                    key,
                    def,
                    &plan,
                    vram,
                    gguf_root.as_deref(),
                    required_mode,
                );
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
    required_mode: Option<Mode>,
) {
    // The cursor survives the loop: a toggle re-renders with the selection
    // still on the row that was toggled, not jumped back to the top.
    let mut cursor = 0;
    loop {
        if chooser.quit_requested() {
            return;
        }
        let plan =
            resolve_launch_plan_with_required_mode(key, def, defaults, overrides, required_mode);
        chooser.set_panel(Some(("Current plan".to_string(), plan_summary(&plan, def))));
        let menu = customize_menu_with_required_mode(&plan, def, required_mode);
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
                let fork = matches!(plan.mode, Mode::Turboquant | Mode::Mtpturbo);
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
            CustomizeAction::ModeLocked
            | CustomizeAction::ModeRequired
            | CustomizeAction::KvLocked => {}
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
    (
        "Prism",
        Mode::PrismMl,
        "Prism (prism) — PrismML's low-bit Bonsai engine",
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
\x20                tuned forks (Turbo+ adds draft speed-ups); Prism runs\n\
\x20                models that explicitly require the PrismML fork.\n\
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
    required_mode: Option<Mode>,
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
            MenuRow::plain(format!(
                "Engine:        {}{}",
                TUNE_ENGINES[choices.engine].0,
                if required_mode.is_some() {
                    " (required)"
                } else {
                    ""
                }
            )),
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
                if required_mode.is_some() {
                    chooser.notice("This model requires this engine; it cannot be changed.");
                    continue;
                }
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

fn launch_guided(chooser: &mut dyn Chooser, home: &Path, plan: &GuidedPlan) -> bool {
    // The same launcher construction as the CLI path (shared VRAM ladder:
    // config > probe > fallback) — a raw probe here once ignored the VRAMGB
    // setting and painted every quant `over` on a no-probe host.
    let launcher = match crate::exec::build_launcher(home) {
        Ok(l) => l,
        Err(e) => {
            chooser.announce_error(&plain_warning("launch", &e));
            return true;
        }
    };
    let vram = i64::from(localx_llama_core::Launcher::vram_gb(&launcher));
    let mut profile = plan.use_auto_best.then(|| {
        resolve_run_profile(
            home,
            &plan.model_key,
            RunProfileQuery {
                quant: Some(&plan.quant),
                context_key: Some(&plan.context_key),
                mode: Some(plan.mode),
                preferred_profile: Some(if plan.auto_best_profile.eq_ignore_ascii_case("pure") {
                    Profile::Pure
                } else {
                    Profile::Balanced
                }),
                vram_gb: Some(vram),
            },
        )
    });
    if let Some(unavailable) = profile.as_mut().filter(|profile| !profile.is_tuned()) {
        if !choose_unavailable_profile(chooser, &plan.model_key, unavailable) {
            return false;
        }
    }

    // Hand the screen back to normal printing only after the fallback choice:
    // downloads, server spawn, and the agent all write plain lines from here.
    chooser.release();
    chooser.notice(&format!("Launching {} …", plan.model_key));
    let entry = profile.as_ref().and_then(|profile| profile.entry.as_ref());
    let (mut request, agent) = request_from_guided(plan, entry);
    // The same finalization as the CLI path: settings-file params under any
    // AutoBest values, then the single-session one-slot default — without it a
    // guided launch (serve included) fell to llama-server's multi-slot auto
    // default, allocating the full context per slot and OOMing the GPU.
    request.apply_session_defaults(&launcher.settings_launch_params());
    let resolved = match plan_launch(&launcher, &request) {
        Ok(p) => p,
        Err(e) => {
            chooser.announce_error(&plain_warning("launch", &e.to_string()));
            return true;
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
    true
}

fn unavailable_profile_rows(profile: &RunProfileResolution) -> Vec<MenuRow> {
    if let Some(entry) = &profile.superseded {
        vec![
            MenuRow::plain("Re-tune now (recommended)"),
            MenuRow::plain(format!(
                "Use the version-{} tune anyway (adopt as v{})",
                entry.tuner_version,
                localx_llama_core::CURRENT_TUNER_VERSION
            )),
            MenuRow::plain("Continue once with LocalBox defaults"),
        ]
    } else {
        vec![
            MenuRow::plain("Configure first (return to Auto-tune)"),
            MenuRow::plain("Continue once with LocalBox defaults"),
        ]
    }
}

fn choose_unavailable_profile(
    chooser: &mut dyn Chooser,
    key: &str,
    profile: &mut RunProfileResolution,
) -> bool {
    let warning = profile
        .warning(key)
        .unwrap_or_else(|| "No tuned profile is available.".to_string());
    chooser.set_panel(Some(("Tuned settings unavailable".to_string(), warning)));
    let rows = unavailable_profile_rows(profile);
    let choice = chooser.choose("Choose how to continue", &rows, 0);
    if profile.superseded.is_some() {
        match choice {
            Some(1) => match adopt_superseded_profile(profile) {
                Ok(()) => true,
                Err(error) => {
                    chooser.announce_error(&format!("The older tune was not adopted: {error}"));
                    false
                }
            },
            Some(2) => true,
            _ => false,
        }
    } else {
        choice == Some(1)
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

/// The shipped defaults layer embedded in this binary (pins, ports, repos).
pub const SHIPPED_DEFAULTS: &str = include_str!("../../../local-llm/defaults.json");

/// The shipped example catalog embedded in this binary — the source of truth
/// for which models a fresh install knows about.
pub const SHIPPED_CATALOG: &str = include_str!("../../../local-llm/llm-models.example.json");

/// Seeding of `~/.local-llm`. The two **shipped** layers (`defaults.json`,
/// `llm-models.example.json`) are refreshed to match this binary whenever
/// they differ — they carry release pins and the shipped model set, and a
/// once-seeded copy silently pinned old installs to release-day state
/// (user overrides belong in `settings.json`, which wins layer precedence,
/// so refreshing shipped defaults never loses a user choice). The **user**
/// layer (`llm-models.json`) is seeded when absent and never touched after —
/// new shipped models reach it only through the explicit additive merge
/// (`localbox update --merge-models`).
/// Whether a path is a symbolic link (never following it).
fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// Warn about one path at most once per process. `catalog_dir` runs on every
/// command and more than once within some of them, so an unconditional warning
/// would repeat several times for a single invocation.
fn warn_once(path: &Path) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let first = seen
        .lock()
        .map_or(true, |mut set| set.insert(path.to_path_buf()));
    if !first {
        return;
    }
    eprintln!(
        concat!(
            "Warning: {} is a symlink and differs from this binary's ",
            "shipped copy; leaving it untouched. Machine overrides ",
            "belong in settings.json, which wins layer precedence ",
            "and is never seeded."
        ),
        path.display()
    );
}

/// What seeding should do with one shipped layer on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShippedLayer {
    /// On disk already matches this binary; nothing to do.
    Matches,
    /// Differs and is a plain file this seeding owns: refresh it.
    Refresh,
    /// Differs but is a symlink, so writing would follow it into whatever it
    /// points at — typically a developer's checkout. Leave it and warn.
    SkipSymlink,
}

/// The seeding decision for one shipped layer, split out from the file I/O so
/// the symlink rule is testable on every platform rather than only where a
/// test can create a link.
#[must_use]
pub fn shipped_layer_action(current: Option<&str>, shipped: &str, is_link: bool) -> ShippedLayer {
    if current == Some(shipped) {
        ShippedLayer::Matches
    } else if is_link {
        ShippedLayer::SkipSymlink
    } else {
        ShippedLayer::Refresh
    }
}

pub fn seed_installed_tree(installed: &Path) {
    let _ = std::fs::create_dir_all(installed);
    for (name, content) in [
        ("defaults.json", SHIPPED_DEFAULTS),
        ("llm-models.example.json", SHIPPED_CATALOG),
    ] {
        let path = installed.join(name);
        let current = std::fs::read_to_string(&path).ok();
        match shipped_layer_action(current.as_deref(), content, is_symlink(&path)) {
            ShippedLayer::Matches => {}
            ShippedLayer::Refresh => {
                let _ = std::fs::write(path, content);
            }
            // Refreshing is for the copies this function owns. A developer may
            // link a shipped layer at their checkout so the live tree and the
            // source agree; `fs::write` follows that link and rewrites their
            // working tree, reverting edits and reinstating the pins this
            // binary happens to carry.
            ShippedLayer::SkipSymlink => warn_once(&path),
        }
    }
    let user_catalog = installed.join("llm-models.json");
    if !user_catalog.exists() {
        let _ = std::fs::write(user_catalog, SHIPPED_CATALOG);
    }
}

/// Model keys the shipped catalog has that the user's catalog lacks. Both
/// arguments are full catalog documents (`{"Models": {...}, ...}`).
#[must_use]
pub fn missing_model_keys(
    shipped: &serde_json::Map<String, serde_json::Value>,
    user: &serde_json::Map<String, serde_json::Value>,
) -> Vec<String> {
    let models = |doc: &serde_json::Map<String, serde_json::Value>| {
        doc.get("Models")
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default()
    };
    let user_models = models(user);
    models(shipped)
        .into_iter()
        .filter(|(key, _)| !user_models.contains_key(key))
        .map(|(key, _)| key)
        .collect()
}

/// Add the named shipped models to the user's catalog document, changing
/// nothing else: existing model entries, `CommandAliases`, and every other
/// top-level key stay exactly as they were. Pure — the caller owns the write.
#[must_use]
pub fn merge_missing_models(
    user: &serde_json::Map<String, serde_json::Value>,
    shipped: &serde_json::Map<String, serde_json::Value>,
    keys: &[String],
) -> serde_json::Map<String, serde_json::Value> {
    let mut merged = user.clone();
    let mut models = merged
        .get("Models")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let shipped_models = shipped
        .get("Models")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    for key in keys {
        if let Some(def) = shipped_models.get(key) {
            models.entry(key.clone()).or_insert_with(|| def.clone());
        }
    }
    merged.insert("Models".to_string(), serde_json::Value::Object(models));
    merged
}

/// Whether installing a synthesised model entry added it or found it already
/// present, plus the catalog key it lives under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogInsert {
    /// A new entry was written under this key.
    Inserted(String),
    /// An existing same-repository entry gained these previously missing
    /// quant keys; every pre-existing value was preserved.
    Updated {
        key: String,
        added_quants: Vec<String>,
    },
    /// The repo was already in the catalog under this key; nothing was written.
    AlreadyPresent(String),
}

impl CatalogInsert {
    /// The catalog key the model lives under, whichever outcome occurred.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            CatalogInsert::Inserted(key)
            | CatalogInsert::Updated { key, .. }
            | CatalogInsert::AlreadyPresent(key) => key,
        }
    }
}

/// The first `hint`, `hint-2`, `hint-3`, … that no model in `models` already
/// uses — so a slug collision with an unrelated model never clobbers it.
fn unique_model_key(models: &serde_json::Map<String, serde_json::Value>, hint: &str) -> String {
    if !models.contains_key(hint) {
        return hint.to_string();
    }
    (2..)
        .map(|n| format!("{hint}-{n}"))
        .find(|candidate| !models.contains_key(candidate))
        .unwrap_or_else(|| hint.to_string())
}

/// Install a synthesised catalog entry into the user's `llm-models.json` under
/// `catalog_dir`, additively. An entry whose `Repo` is already present is left
/// enriched with quant keys it does not yet contain; otherwise the entry is
/// written under a collision-free key, with its `Root` rewritten to match.
/// Same-repository enrichment preserves every existing field and quant value
/// verbatim and writes only when a key is missing. A valid legacy `File`-only
/// entry remains untouched rather than silently changing its resolution
/// semantics. Every other part of the catalog — existing models,
/// `CommandAliases`, scalar settings — is preserved. The caller passes
/// [`catalog_dir`]`(home)` (which seeds the tree); taking the directory keeps
/// this write unit-testable against a temp path.
///
/// # Errors
/// A string message when the catalog file cannot be read, is not valid JSON, or
/// cannot be written.
pub fn install_catalog_model(
    catalog_dir: &Path,
    key_hint: &str,
    entry: &serde_json::Value,
) -> Result<CatalogInsert, String> {
    let user_path = catalog_dir.join("llm-models.json");
    let raw = std::fs::read_to_string(&user_path)
        .map_err(|e| format!("could not read {}: {e}", user_path.display()))?;
    let mut user: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(raw.trim_start_matches('\u{feff}')).map_err(|e| {
            format!(
                "{} is not valid JSON ({e}); fix it before installing",
                user_path.display()
            )
        })?;
    let mut models = user
        .get("Models")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();

    // Idempotency plus safe backfill: the same repo is reused, and only missing
    // quant keys are copied into a real Quants object. Existing values and all
    // non-quant fields remain the user's. A valid File-only entry is left alone
    // because adding Quants would change which field model resolution prefers.
    let repo = entry.get("Repo").and_then(serde_json::Value::as_str);
    if let Some(repo) = repo {
        if let Some(existing_key) = models
            .iter()
            .find(|(_, def)| def.get("Repo").and_then(serde_json::Value::as_str) == Some(repo))
            .map(|(key, _)| key.clone())
        {
            let incoming_quants = entry
                .get("Quants")
                .and_then(serde_json::Value::as_object)
                .cloned()
                .unwrap_or_default();
            let existing_entry = models.get_mut(&existing_key).ok_or_else(|| {
                format!("catalog entry '{existing_key}' disappeared during merge")
            })?;
            let existing_object = existing_entry.as_object_mut().ok_or_else(|| {
                format!("catalog entry '{existing_key}' for repo '{repo}' is not an object")
            })?;
            let Some(existing_quants_value) = existing_object.get_mut("Quants") else {
                return Ok(CatalogInsert::AlreadyPresent(existing_key));
            };
            let existing_quants = existing_quants_value.as_object_mut().ok_or_else(|| {
                format!(
                    "catalog entry '{existing_key}' for repo '{repo}' has a malformed Quants value"
                )
            })?;
            let mut added_quants = Vec::new();
            for (quant, value) in incoming_quants {
                if !existing_quants.contains_key(&quant) {
                    existing_quants.insert(quant.clone(), value);
                    added_quants.push(quant);
                }
            }
            if added_quants.is_empty() {
                return Ok(CatalogInsert::AlreadyPresent(existing_key));
            }

            user.insert("Models".to_string(), serde_json::Value::Object(models));
            let pretty = serde_json::to_string_pretty(&serde_json::Value::Object(user))
                .map_err(|e| e.to_string())?;
            std::fs::write(&user_path, pretty + "\n")
                .map_err(|e| format!("could not write {}: {e}", user_path.display()))?;
            return Ok(CatalogInsert::Updated {
                key: existing_key,
                added_quants,
            });
        }
    }

    let key = unique_model_key(&models, key_hint);
    let mut entry = entry.clone();
    if let Some(obj) = entry.as_object_mut() {
        obj.insert("Root".to_string(), serde_json::Value::String(key.clone()));
    }
    models.insert(key.clone(), entry);
    user.insert("Models".to_string(), serde_json::Value::Object(models));

    let pretty = serde_json::to_string_pretty(&serde_json::Value::Object(user))
        .map_err(|e| e.to_string())?;
    std::fs::write(&user_path, pretty + "\n")
        .map_err(|e| format!("could not write {}: {e}", user_path.display()))?;
    Ok(CatalogInsert::Inserted(key))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use localbox_tui::ui::{render_guided_screen, GuidedScreen};
    use localx_llama_core::tuner::{Overrides, Profile, PromptLength};
    use localx_llama_core::TunerBestConfig;
    use ratatui::{backend::TestBackend, Terminal};

    struct ScriptedChooser {
        choice: Option<usize>,
        panel: Option<(String, String)>,
        rows: Vec<String>,
        start: usize,
        errors: Vec<String>,
    }

    impl ScriptedChooser {
        fn choosing(choice: usize) -> Self {
            Self {
                choice: Some(choice),
                panel: None,
                rows: Vec::new(),
                start: usize::MAX,
                errors: Vec::new(),
            }
        }
    }

    impl Chooser for ScriptedChooser {
        fn set_banner(&mut self, _banner: String) {}

        fn set_panel(&mut self, panel: Option<(String, String)>) {
            self.panel = panel;
        }

        fn choose(&mut self, _title: &str, rows: &[MenuRow], start: usize) -> Option<usize> {
            self.rows = rows.iter().map(MenuRow::text).collect();
            self.start = start;
            self.choice
        }

        fn notice(&mut self, _text: &str) {}

        fn announce_error(&mut self, text: &str) {
            self.errors.push(text.to_string());
        }
    }

    fn def_with_tier(tier: Option<&str>) -> ModelDef {
        let mut def: ModelDef = serde_json::from_str(r#"{"Repo":"o/m"}"#).unwrap();
        def.tier = tier.map(str::to_string);
        def
    }

    fn rendered_row(width: u16, row: &MenuRow) -> String {
        let backend = TestBackend::new(width, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_guided_screen(
                    frame,
                    &GuidedScreen {
                        banner: "",
                        panel: None,
                        menu_title: "Quality",
                        rows: std::slice::from_ref(row),
                        selected: 0,
                    },
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn quant_notes_are_readable_without_repeating_name_or_size_at_wide_and_narrow_widths() {
        let def: ModelDef = serde_json::from_str(
            r#"{
                "Repo":"o/m",
                "Quants":{
                    "q4km":{
                        "File":"m.gguf",
                        "SizeGB":6.0,
                        "Note":"Q4_K_M · ~6.0 GB · balanced headroom for daily coding"
                    }
                },
                "Quant":"q4km"
            }"#,
        )
        .unwrap();

        let wide = quant_menu_row_at_width(None, "m", &def, "q4km", 16, 114);
        assert!(wide.text().contains("balanced headroom for daily coding"));
        assert!(wide.text().contains(" · 6.0 GB · fits · "));
        assert_eq!(wide.text().matches("6.0 GB").count(), 1);
        assert_eq!(wide.text().to_ascii_lowercase().matches("q4km").count(), 1);
        assert!(rendered_row(120, &wide).contains("balanced headroom for daily coding"));

        let narrow = quant_menu_row_at_width(None, "m", &def, "q4km", 16, 42);
        assert!(narrow.text().chars().count() <= 42, "{}", narrow.text());
        assert!(narrow.text().starts_with("q4km · 6.0 GB · fits · "));
        assert!(narrow.text().ends_with('…'));
        assert!(rendered_row(48, &narrow).contains(&narrow.text()));
    }

    #[test]
    fn quant_rows_tolerate_absent_note_and_size() {
        let def: ModelDef = serde_json::from_str(
            r#"{"Repo":"o/m","Quants":{"q4":{"File":"m.gguf"}},"Quant":"q4"}"#,
        )
        .unwrap();
        let row = quant_menu_row_at_width(None, "m", &def, "q4", 8, 54);
        assert!(row.text().contains("q4"));
        assert!(!row.text().contains("GB"));
        for fit in ["fits", "tight", "over", "unknown"] {
            assert!(!row.text().contains(fit), "{}", row.text());
        }
        assert!(rendered_row(60, &row).contains(&row.text()));
    }

    fn seed_catalog(dir: &Path, json: &str) {
        std::fs::write(dir.join("llm-models.json"), json).unwrap();
    }

    #[test]
    fn installing_a_new_entry_adds_it_and_preserves_existing_models() {
        let dir = tempfile::tempdir().unwrap();
        seed_catalog(
            dir.path(),
            r#"{"Models":{"existing":{"Repo":"other/model"}},"CommandAliases":{"x":"existing"}}"#,
        );
        let entry = serde_json::json!({
            "Repo": "owner/new-GGUF", "Root": "new-gguf", "SourceType": "gguf",
            "Quants": {"q4km": {"File": "m.Q4_K_M.gguf"}}, "Quant": "q4km",
            "Contexts": {"": 32768}
        });

        let outcome = install_catalog_model(dir.path(), "new-gguf", &entry).unwrap();
        assert_eq!(outcome, CatalogInsert::Inserted("new-gguf".to_string()));

        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("llm-models.json")).unwrap(),
        )
        .unwrap();
        // The new model is present; the existing model and aliases are untouched.
        assert_eq!(written["Models"]["new-gguf"]["Repo"], "owner/new-GGUF");
        assert_eq!(written["Models"]["existing"]["Repo"], "other/model");
        assert_eq!(written["CommandAliases"]["x"], "existing");
    }

    #[test]
    fn reinstalling_the_same_repo_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        seed_catalog(
            dir.path(),
            r#"{"Models":{"mine":{"Repo":"owner/new-GGUF"}}}"#,
        );
        let entry = serde_json::json!({"Repo": "owner/new-GGUF", "Root": "new-gguf"});

        let outcome = install_catalog_model(dir.path(), "new-gguf", &entry).unwrap();
        // Same repo already present under a different key → reused, not duplicated.
        assert_eq!(outcome, CatalogInsert::AlreadyPresent("mine".to_string()));
        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("llm-models.json")).unwrap(),
        )
        .unwrap();
        assert!(written["Models"].get("new-gguf").is_none());
    }

    #[test]
    fn reinstalling_the_same_repo_adds_only_missing_quants() {
        let dir = tempfile::tempdir().unwrap();
        seed_catalog(
            dir.path(),
            r#"{
                "Models": {
                    "mine": {
                        "Repo": "owner/new-GGUF",
                        "Root": "custom-root",
                        "Quants": {
                            "q4km": {"File":"custom-q4.gguf","Note":"keep me"}
                        },
                        "Quant": "q4km",
                        "Contexts": {"":65536},
                        "CustomField": {"owned":true}
                    }
                },
                "CommandAliases": {"m":"mine"}
            }"#,
        );
        let entry = serde_json::json!({
            "Repo": "owner/new-GGUF",
            "Root": "new-gguf",
            "Quants": {
                "q4km": {"File":"upstream-q4.gguf","SizeGB":4.0},
                "q6k": {"File":"upstream-q6.gguf","SizeGB":6.0}
            },
            "Quant": "q6k",
            "Contexts": {"":32768}
        });

        let outcome = install_catalog_model(dir.path(), "new-gguf", &entry).unwrap();
        assert_eq!(
            outcome,
            CatalogInsert::Updated {
                key: "mine".to_string(),
                added_quants: vec!["q6k".to_string()]
            }
        );

        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("llm-models.json")).unwrap(),
        )
        .unwrap();
        let mine = &written["Models"]["mine"];
        assert_eq!(mine["Root"], "custom-root");
        assert_eq!(mine["Quant"], "q4km", "the user's default stays selected");
        assert_eq!(mine["Contexts"][""], 65536);
        assert_eq!(mine["CustomField"]["owned"], true);
        assert_eq!(mine["Quants"]["q4km"]["File"], "custom-q4.gguf");
        assert_eq!(mine["Quants"]["q4km"]["Note"], "keep me");
        assert_eq!(mine["Quants"]["q6k"]["File"], "upstream-q6.gguf");
        assert_eq!(written["CommandAliases"]["m"], "mine");
    }

    #[test]
    fn a_file_only_same_repo_entry_is_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let original = r#"{"Models":{"mine":{"Repo":"owner/new-GGUF","File":"custom.gguf"}}}"#;
        seed_catalog(dir.path(), original);
        let entry = serde_json::json!({
            "Repo": "owner/new-GGUF",
            "Quants": {"q4km": {"File":"upstream-q4.gguf"}},
            "Quant": "q4km"
        });

        let outcome = install_catalog_model(dir.path(), "new-gguf", &entry).unwrap();
        assert_eq!(outcome, CatalogInsert::AlreadyPresent("mine".to_string()));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("llm-models.json")).unwrap(),
            original
        );
    }

    #[test]
    fn malformed_quants_on_a_same_repo_entry_fail_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let original = r#"{"Models":{"mine":{"Repo":"owner/new-GGUF","Quants":"bad"}}}"#;
        seed_catalog(dir.path(), original);
        let entry = serde_json::json!({
            "Repo": "owner/new-GGUF",
            "Quants": {"q4km": {"File":"upstream-q4.gguf"}},
            "Quant": "q4km"
        });

        let err = install_catalog_model(dir.path(), "new-gguf", &entry).unwrap_err();
        assert!(err.contains("malformed Quants"), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("llm-models.json")).unwrap(),
            original
        );
    }

    #[test]
    fn a_key_collision_with_another_repo_never_clobbers() {
        let dir = tempfile::tempdir().unwrap();
        seed_catalog(dir.path(), r#"{"Models":{"slug":{"Repo":"someone/else"}}}"#);
        let entry = serde_json::json!({"Repo": "owner/thing", "Root": "slug"});

        let outcome = install_catalog_model(dir.path(), "slug", &entry).unwrap();
        assert_eq!(outcome, CatalogInsert::Inserted("slug-2".to_string()));
        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("llm-models.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(written["Models"]["slug"]["Repo"], "someone/else");
        assert_eq!(written["Models"]["slug-2"]["Repo"], "owner/thing");
        // Root is rewritten to the collision-free key.
        assert_eq!(written["Models"]["slug-2"]["Root"], "slug-2");
    }

    #[test]
    fn an_invalid_catalog_is_a_clear_error_not_a_clobber() {
        let dir = tempfile::tempdir().unwrap();
        seed_catalog(dir.path(), "{ not json");
        let entry = serde_json::json!({"Repo": "owner/thing"});
        let err = install_catalog_model(dir.path(), "thing", &entry).unwrap_err();
        assert!(err.contains("is not valid JSON"), "{err}");
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
        // Binary gigabytes, the unit the catalog's `SizeGB` is written in.
        assert!((gb - localbox_launcher::catalog_entry::gib(2_000_000)).abs() < 1e-9);
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

    /// The on-disk measurement and the catalog value are the same unit, so a
    /// correct entry never trips the drift warning. Reading the file in
    /// decimal GB against a catalog written in binary GB made every model over
    /// ~2GB look like it had drifted.
    #[test]
    fn the_disk_measurement_uses_the_catalog_size_unit() {
        let root = tempfile::tempdir().unwrap();
        let bytes: u64 = 13_850_000_000;
        let folder = root.path().join("m");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("m.gguf"), vec![0_u8; 4096]).unwrap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(folder.join("m.gguf"))
            .unwrap();
        file.set_len(bytes).unwrap();
        drop(file);

        let def: ModelDef = serde_json::from_str(
            r#"{"Repo":"o/m","Root":"m","Quants":{"q4":{"File":"m.gguf"}},"Quant":"q4"}"#,
        )
        .unwrap();
        let disk = quant_disk_size_gb(Some(root.path()), "m", &def, Some("q4")).unwrap();
        // Same unit, so the rounding the catalog applies is the only gap left.
        assert!((disk - localbox_launcher::catalog_entry::size_gb(bytes)).abs() < 0.05);
        // And a catalog entry written from the same byte count agrees, so the
        // 0.15GB drift warning stays silent.
        assert_eq!(
            validated_quant_size_gb(
                "m",
                "q4",
                Some(localbox_launcher::catalog_entry::size_gb(bytes)),
                Some(disk)
            ),
            Some(disk)
        );
    }

    #[test]
    fn downloaded_size_is_authoritative_over_legacy_catalog_metadata() {
        assert_eq!(
            validated_quant_size_gb("model", "q4km", Some(8.0), Some(6.5)),
            Some(6.5)
        );
        assert_eq!(
            validated_quant_size_gb("model", "q4km", Some(8.0), None),
            Some(8.0)
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
            tuner_version: localx_llama_core::CURRENT_TUNER_VERSION,
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

    fn write_profile_store(home: &Path, store: &TunerBestConfig) {
        let tuner = home.join(".local-llm").join("tuner");
        std::fs::create_dir_all(&tuner).unwrap();
        std::fs::write(
            tuner.join("best-m.json"),
            serde_json::to_string_pretty(store).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn a_superseded_tune_gets_the_three_row_adopt_menu_and_continues_tuned() {
        let home = tempfile::tempdir().unwrap();
        let mut older = entry("q4", "64k", Mode::Native, 24, Profile::Balanced, 300.0);
        older.tuner_version = localx_llama_core::CURRENT_TUNER_VERSION - 1;
        let store = TunerBestConfig {
            schema: 1,
            key: "m".to_string(),
            vram_gb: Some(24),
            entries: vec![older],
        };
        write_profile_store(home.path(), &store);
        let mut profile = resolve_run_profile(
            home.path(),
            "m",
            RunProfileQuery {
                quant: Some("q4"),
                context_key: Some("64k"),
                mode: Some(Mode::Native),
                preferred_profile: Some(Profile::Balanced),
                vram_gb: Some(24),
            },
        );
        let mut chooser = ScriptedChooser::choosing(1);

        assert!(choose_unavailable_profile(&mut chooser, "m", &mut profile));
        assert_eq!(chooser.start, 0, "re-tune remains the default selection");
        assert_eq!(
            chooser.rows,
            vec![
                "Re-tune now (recommended)".to_string(),
                format!(
                    "Use the version-{} tune anyway (adopt as v{})",
                    localx_llama_core::CURRENT_TUNER_VERSION - 1,
                    localx_llama_core::CURRENT_TUNER_VERSION
                ),
                "Continue once with LocalBox defaults".to_string(),
            ]
        );
        let panel = chooser.panel.as_ref().unwrap();
        assert!(panel.1.contains("templated /v1/chat/completions"));
        assert!(panel.1.contains("NGpuLayers=99"));
        assert!(panel.1.contains("measured 2026-01-01"));
        assert!(chooser.errors.is_empty());
        assert!(profile.is_tuned());

        let (request, _) =
            request_from_guided(&guided("q4", "64k", Mode::Native), profile.entry.as_ref());
        assert_eq!(request.params.n_gpu_layers, Some(99));
        assert!(home
            .path()
            .join(".local-llm/tuner/best-m.json.bak")
            .is_file());
    }

    #[test]
    fn non_adoptable_reasons_keep_the_existing_two_row_guided_menu() {
        let missing_home = tempfile::tempdir().unwrap();
        let missing = resolve_run_profile(missing_home.path(), "m", RunProfileQuery::default());

        let no_match_home = tempfile::tempdir().unwrap();
        write_profile_store(
            no_match_home.path(),
            &TunerBestConfig {
                schema: 1,
                key: "m".to_string(),
                vram_gb: Some(24),
                entries: vec![entry(
                    "q8",
                    "64k",
                    Mode::Native,
                    24,
                    Profile::Balanced,
                    300.0,
                )],
            },
        );
        let no_match = resolve_run_profile(
            no_match_home.path(),
            "m",
            RunProfileQuery {
                quant: Some("q4"),
                ..RunProfileQuery::default()
            },
        );

        let schema_home = tempfile::tempdir().unwrap();
        let schema_path = schema_home.path().join(".local-llm/tuner");
        std::fs::create_dir_all(&schema_path).unwrap();
        std::fs::write(
            schema_path.join("best-m.json"),
            r#"{"schema":2,"key":"m","entries":[]}"#,
        )
        .unwrap();
        let unsupported_schema =
            resolve_run_profile(schema_home.path(), "m", RunProfileQuery::default());

        for profile in [&missing, &no_match, &unsupported_schema] {
            assert_eq!(
                unavailable_profile_rows(profile)
                    .iter()
                    .map(MenuRow::text)
                    .collect::<Vec<_>>(),
                vec![
                    "Configure first (return to Auto-tune)",
                    "Continue once with LocalBox defaults",
                ]
            );
        }
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
        plan.vision = true;
        let saved = default_launch_from_plan(&plan);
        assert_eq!(saved.model_key.as_deref(), Some("m"));
        assert_eq!(saved.action.as_deref(), Some("localpilot"));
        assert_eq!(saved.quant.as_deref(), Some("q4"));
        // The on-disk spelling stays PascalCase/Action (the shipped shape).
        let value = serde_json::to_value(&saved).unwrap();
        assert!(value.get("Action").is_some());
        assert!(value.get("ModelKey").is_some());
        assert_eq!(value["Vision"], true);
    }

    #[test]
    fn a_symlinked_shipped_layer_is_never_refreshed_through() {
        // Matching content is left alone whatever it is.
        assert_eq!(
            shipped_layer_action(Some("same"), "same", false),
            ShippedLayer::Matches
        );
        assert_eq!(
            shipped_layer_action(Some("same"), "same", true),
            ShippedLayer::Matches
        );
        // A plain file that has drifted is refreshed: shipped pins must not
        // stay frozen at whatever release-day copy was seeded first.
        assert_eq!(
            shipped_layer_action(Some("old pins"), "new pins", false),
            ShippedLayer::Refresh
        );
        assert_eq!(
            shipped_layer_action(None, "new pins", false),
            ShippedLayer::Refresh
        );
        // A symlink that has drifted is not: writing follows it into whatever
        // it points at, which is how a developer's working tree gets reverted.
        assert_eq!(
            shipped_layer_action(Some("edited pin"), "shipped pin", true),
            ShippedLayer::SkipSymlink
        );
    }

    #[test]
    #[cfg(unix)]
    fn seeding_leaves_a_symlinked_shipped_layer_alone() {
        // A developer may link ~/.local-llm/defaults.json at their checkout.
        // Refreshing through that link rewrites their working tree with the
        // pins this binary was built with, which silently reverts edits.
        let home = tempfile::tempdir().unwrap();
        let checkout = home.path().join("checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        let source = checkout.join("defaults.json");
        let edited = "{
  \"LlamaCppTurboquantPinnedTag\": \"tqp-v0.3.1\"
}
";
        std::fs::write(&source, edited).unwrap();

        let installed = home.path().join(".local-llm");
        std::fs::create_dir_all(&installed).unwrap();
        std::os::unix::fs::symlink(&source, installed.join("defaults.json")).unwrap();

        seed_installed_tree(&installed);

        assert_eq!(
            std::fs::read_to_string(&source).unwrap(),
            edited,
            "seeding must not write through a symlink into a working tree"
        );
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
        let seeded = Catalog::load(&installed).unwrap();
        for key in seeded.model_keys() {
            let model = seeded.model(key).unwrap();
            assert!(
                model
                    .quants
                    .values()
                    .all(|quant| quant.size_gb.is_some() && quant.note.is_some()),
                "fresh-profile model {key} has incomplete quant metadata"
            );
            if let Some(description) = model.description.as_deref() {
                assert!(
                    !description.contains("<div")
                        && !description.contains("<script")
                        && !description.contains("<style"),
                    "fresh-profile model {key} leaked markup: {description}"
                );
            }
        }
        // A user edit is never clobbered on a later run.
        std::fs::write(installed.join("llm-models.json"), "{\"Models\":{}}").unwrap();
        let _ = catalog_dir(home.path());
        assert_eq!(
            std::fs::read_to_string(installed.join("llm-models.json")).unwrap(),
            "{\"Models\":{}}",
            "seeding never overwrites an existing catalog"
        );
        // The shipped layers, by contrast, are refreshed when they drift —
        // a stale defaults.json would silently pin old installs to
        // release-day engine pins (user overrides live in settings.json).
        std::fs::write(installed.join("defaults.json"), "{\"stale\":true}").unwrap();
        let _ = catalog_dir(home.path());
        assert_eq!(
            std::fs::read_to_string(installed.join("defaults.json")).unwrap(),
            SHIPPED_DEFAULTS,
            "shipped defaults refresh to match the binary"
        );
    }

    #[test]
    fn shipped_model_merge_is_additive_only() {
        let shipped: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(SHIPPED_CATALOG).unwrap();
        // A user catalog that predates the Bonsai entries, with one model the
        // user edited (a custom context) and one alias.
        let user: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            r#"{
                "Models": {
                    "tbonsai27b": { "Repo": "prism-ml/Ternary-Bonsai-27B-gguf", "Contexts": { "": 12345 } }
                },
                "CommandAliases": { "b": "tbonsai27b" }
            }"#,
        )
        .unwrap();
        let missing = missing_model_keys(&shipped, &user);
        // Every shipped model except the one the user already has.
        assert!(missing.contains(&"bonsai27b".to_string()));
        assert!(!missing.contains(&"tbonsai27b".to_string()));

        let merged = merge_missing_models(&user, &shipped, &missing);
        // The user's edited entry is byte-identical at the value level…
        assert_eq!(
            merged["Models"]["tbonsai27b"]["Contexts"][""], 12345,
            "an existing user entry is never rewritten"
        );
        // …the missing shipped models arrived…
        assert_eq!(
            merged["Models"]["bonsai27b"]["Repo"],
            "prism-ml/Bonsai-27B-gguf"
        );
        // …and unrelated top-level keys survive.
        assert_eq!(merged["CommandAliases"]["b"], "tbonsai27b");

        // A catalog that already has everything merges to itself.
        let full: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(SHIPPED_CATALOG).unwrap();
        assert!(missing_model_keys(&shipped, &full).is_empty());
    }
}
