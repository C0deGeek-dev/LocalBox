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
use localbox_tui::ui::{ConfirmAction, CONFIRM_ROWS};
use localbox_tui::vocab::{engine_label, glossary, plan_summary, target_label};
use localx_llama_core::{Mode, ModelDef, TunerBestConfig, TunerEntry};

use crate::exec::{home_dir, probe_vram_gb};
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
trait Chooser {
    fn choose(&mut self, title: &str, rows: &[String], start: usize) -> Option<usize>;
    fn notice(&mut self, text: &str);
}

/// Numbered plain-text menus over stdin — the non-TTY / screen-reader path.
struct PlainChooser;

impl Chooser for PlainChooser {
    fn choose(&mut self, title: &str, rows: &[String], _start: usize) -> Option<usize> {
        print!("{}", plain_menu(title, rows));
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

/// A ratatui inline-viewport list (scrollback-safe per the pinned terminal
/// options); raw mode lives only inside a single choice.
struct TuiChooser;

impl TuiChooser {
    fn run_list(title: &str, rows: &[String], start: usize) -> std::io::Result<Option<usize>> {
        use crossterm::event::{self, Event, KeyCode, KeyEventKind};
        use ratatui::prelude::*;
        use ratatui::widgets::{Block, Borders, List, ListItem, ListState};

        crossterm::terminal::enable_raw_mode()?;
        let backend = CrosstermBackend::new(std::io::stdout());
        let mut terminal =
            Terminal::with_options(backend, localbox_tui::driver::terminal_options())?;
        let mut selected = start.min(rows.len().saturating_sub(1));
        let result = loop {
            terminal.draw(|frame| {
                let mut state = ListState::default();
                state.select(Some(selected));
                let items: Vec<ListItem> =
                    rows.iter().map(|row| ListItem::new(row.as_str())).collect();
                let list = List::new(items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(title.to_string()),
                    )
                    .highlight_symbol("> ")
                    .highlight_style(Style::default().add_modifier(Modifier::BOLD));
                frame.render_stateful_widget(list, frame.area(), &mut state);
            })?;
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
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
        let mut out = std::io::stdout();
        use std::io::Write;
        let _ = writeln!(out);
        Ok(result)
    }
}

impl Chooser for TuiChooser {
    fn choose(&mut self, title: &str, rows: &[String], start: usize) -> Option<usize> {
        match Self::run_list(title, rows, start) {
            Ok(choice) => choice,
            Err(e) => {
                let _ = crossterm::terminal::disable_raw_mode();
                eprintln!("{}", plain_warning("menu", &e.to_string()));
                None
            }
        }
    }

    fn notice(&mut self, text: &str) {
        println!("{text}");
    }
}

/// Run the guided launcher until the user cancels out of the model picker.
///
/// # Errors
/// A plain-language message when the catalog or home cannot be resolved.
pub fn run_guided(plain_requested: bool) -> Result<(), String> {
    ensure_utf8_output();
    let home = home_dir().ok_or("could not determine the user home directory")?;
    let degraded = should_degrade(std::io::stdout().is_terminal(), plain_requested);
    let mut chooser: Box<dyn Chooser> = if degraded {
        Box::new(PlainChooser)
    } else {
        Box::new(TuiChooser)
    };
    let mut show_all = false;

    loop {
        let catalog_dir = catalog_dir(&home);
        let catalog = Catalog::load(&catalog_dir).map_err(|e| e.to_string())?;
        let (keys, show_all_offered) = picker_keys(&catalog, show_all);

        let mut rows: Vec<String> = keys
            .iter()
            .map(|key| {
                let def = catalog.model(key);
                let name = def.and_then(|d| d.display_name.clone()).unwrap_or_default();
                let strict = def.and_then(|d| d.strict).unwrap_or(false);
                let mut row = format!("{key} · {name}");
                if strict {
                    row.push_str("  [strict]");
                }
                row
            })
            .collect();
        if show_all_offered {
            rows.push("[Show all tiers]".to_string());
        }
        rows.push("[Cancel]".to_string());

        let Some(index) = chooser.choose("Pick a model", &rows, 0) else {
            return Ok(());
        };
        if show_all_offered && index == rows.len() - 2 {
            show_all = true;
            continue;
        }
        if index == rows.len() - 1 {
            return Ok(());
        }
        let key = keys[index].clone();
        let Some(def) = catalog.model(&key).cloned() else {
            continue;
        };

        confirm_flow(chooser.as_mut(), &home, &catalog, &key, &def);
        show_all = false;
    }
}

fn confirm_flow(
    chooser: &mut dyn Chooser,
    home: &Path,
    catalog: &Catalog,
    key: &str,
    def: &ModelDef,
) {
    let defaults = load_default_launch(catalog);
    let mut overrides = PlanOverrides::default();

    loop {
        let plan = resolve_launch_plan(key, def, &defaults, &overrides);
        chooser.notice(&plan_summary(&plan, def));
        let rows: Vec<String> = CONFIRM_ROWS
            .iter()
            .map(|(label, _)| (*label).to_string())
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
                customize_flow(chooser, home, key, def, &defaults, &mut overrides);
            }
            ConfirmAction::AutoTune => {
                chooser.notice(
                    "Auto-tune runs a benchmark to find the best settings for your GPU.\n\
                     Run it with:  localbench findbest --model ",
                );
                chooser.notice(&format!("  localbench findbest --model {key}"));
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
) {
    loop {
        let plan = resolve_launch_plan(key, def, defaults, overrides);
        let menu = customize_menu(&plan, def);
        let rows: Vec<String> = menu.iter().map(|row| row.label.clone()).collect();
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
                let labels: Vec<String> = targets.iter().map(|t| target_label(t)).collect();
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
                let labels: Vec<String> = quants
                    .iter()
                    .map(|q| localbox_tui::vocab::quality_label(def, q))
                    .collect();
                if let Some(i) = chooser.choose("Quality", &labels, 0) {
                    overrides.quant = Some(quants[i].clone());
                }
            }
            CustomizeAction::PickContext => {
                let contexts: Vec<String> = def.contexts.keys().cloned().collect();
                let labels: Vec<String> = contexts
                    .iter()
                    .map(|c| localbox_tui::vocab::memory_label(def, c))
                    .collect();
                if let Some(i) = chooser.choose("Memory (conversation size)", &labels, 0) {
                    overrides.context_key = Some(contexts[i].clone());
                }
            }
            CustomizeAction::PickMode => {
                let modes = [Mode::Native, Mode::Turboquant, Mode::Mtpturbo];
                let labels: Vec<String> =
                    modes.iter().map(|m| engine_label(*m).to_string()).collect();
                if let Some(i) = chooser.choose("Engine", &labels, 0) {
                    overrides.mode = Some(modes[i]);
                }
            }
            CustomizeAction::PickAutoTune => {
                let labels = vec![
                    "On — use my auto-tuned settings".to_string(),
                    "Off — use the recommended defaults".to_string(),
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
                let labels: Vec<String> = kinds.iter().map(|k| (*k).to_string()).collect();
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
/// checkout's `local-llm/` when running from source.
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
    installed
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
