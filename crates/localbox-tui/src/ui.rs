//! The guided launcher's widgets: model picker, recommended-plan summary, and
//! the 5-item confirm menu — backend-agnostic (rendered through a `Frame`, so
//! `TestBackend` snapshot tests pin the screens) with the fit-aware colors
//! from the shared fit classifier.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use localx_llama_core::{FitClass, ModelDef};

use crate::plan::GuidedPlan;
use crate::vocab;

/// The fit-aware color for a quant row: green fits / yellow tight / red over;
/// unknown stays uncolored (never guessed).
#[must_use]
pub fn fit_color(fit: FitClass) -> Option<Color> {
    match fit {
        FitClass::Fits => Some(Color::Green),
        FitClass::Tight => Some(Color::Yellow),
        FitClass::Over => Some(Color::Red),
        FitClass::Unknown => None,
    }
}

/// One row of the model picker.
#[derive(Debug, Clone)]
pub struct ModelRow {
    pub key: String,
    pub display_name: String,
    pub strict: bool,
}

impl ModelRow {
    /// Build a row from a catalog entry.
    #[must_use]
    pub fn from_def(key: &str, def: &ModelDef) -> Self {
        Self {
            key: key.to_string(),
            display_name: def.display_name.clone().unwrap_or_else(|| key.to_string()),
            strict: def.strict.unwrap_or(false),
        }
    }
}

/// The model picker: `key · DisplayName`, a dim `[strict]` marker, and the
/// `[Show all tiers]` / `[Cancel]` footer rows.
pub struct ModelPicker {
    pub rows: Vec<ModelRow>,
    pub show_all_offered: bool,
}

impl ModelPicker {
    fn items(&self) -> Vec<ListItem<'_>> {
        let mut items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|row| {
                let mut spans = vec![Span::raw(format!("{} · {}", row.key, row.display_name))];
                if row.strict {
                    spans.push(Span::styled(
                        "  [strict]",
                        Style::default().add_modifier(Modifier::DIM),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();
        if self.show_all_offered {
            items.push(ListItem::new("[Show all tiers]"));
        }
        items.push(ListItem::new("[Cancel]"));
        items
    }

    /// Render the picker into `area` with the selected row highlighted.
    pub fn render(&self, frame: &mut Frame, area: Rect, selected: usize) {
        let mut state = ListState::default();
        state.select(Some(selected));
        let list = List::new(self.items())
            .block(Block::default().borders(Borders::ALL).title("Pick a model"))
            .highlight_symbol("> ")
            .highlight_style(Style::default().add_modifier(Modifier::BOLD));
        frame.render_stateful_widget(list, area, &mut state);
    }
}

/// Render the recommended-plan summary panel (the plain-language centrepiece).
pub fn render_summary(frame: &mut Frame, area: Rect, plan: &GuidedPlan, def: &ModelDef) {
    let text = vocab::plan_summary(plan, def);
    let lines: Vec<Line> = text.lines().map(|l| Line::from(l.to_string())).collect();
    let panel = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Recommended plan"),
    );
    frame.render_widget(panel, area);
}

/// The confirm menu's five actions, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmAction {
    LaunchNow,
    Customize,
    AutoTune,
    Help,
    BackToModels,
}

/// The confirm menu's rows, exactly as the guided flow words them.
pub const CONFIRM_ROWS: &[(&str, ConfirmAction)] = &[
    (
        "▶  Launch now (recommended settings)",
        ConfirmAction::LaunchNow,
    ),
    ("⚙  Customize settings", ConfirmAction::Customize),
    (
        "🔧  Auto-tune this model (run a benchmark)",
        ConfirmAction::AutoTune,
    ),
    ("ℹ  What do these mean?", ConfirmAction::Help),
    ("←  Back to models", ConfirmAction::BackToModels),
];

/// Render the 5-item confirm menu.
pub fn render_confirm(frame: &mut Frame, area: Rect, model_key: &str, selected: usize) {
    let mut state = ListState::default();
    state.select(Some(selected));
    let items: Vec<ListItem> = CONFIRM_ROWS
        .iter()
        .map(|(label, _)| ListItem::new(*label))
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Ready to launch {model_key}?")),
        )
        .highlight_symbol("> ")
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));
    frame.render_stateful_widget(list, area, &mut state);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::plan::{resolve_launch_plan, DefaultLaunch, PlanOverrides};
    use localx_llama_core::Mode;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn def() -> ModelDef {
        serde_json::from_str(
            r#"{
            "DisplayName": "Qwen 3.6 APEX",
            "Repo": "mudler/apex",
            "Quants": { "apex-balanced": { "File": "b.gguf", "SizeGB": 18.6 } },
            "Quant": "apex-balanced",
            "Contexts": { "": 32768 }
        }"#,
        )
        .unwrap()
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer().clone();
        let area = buffer.area;
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn the_summary_snapshot_keeps_the_plain_language_contract() {
        let mut plan = resolve_launch_plan(
            "q36apex",
            &def(),
            &DefaultLaunch::default(),
            &PlanOverrides::default(),
        );
        plan.mode = Mode::Turboquant;
        plan.use_auto_best = true;
        let backend = TestBackend::new(64, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_summary(frame, frame.area(), &plan, &def()))
            .unwrap();
        let screen = buffer_text(&terminal);

        // The rendered SCREEN keeps the plain-language contract.
        for required in [
            "Recommended plan",
            "Model:     Qwen 3.6 APEX",
            "Run with:  LocalPilot (recommended)",
            "Quality:   balanced · 18.6 GB · apex-balanced",
            "Memory:    Standard (~24,576 words)",
            "Speed:     Turbo (auto-tuned for your GPU) · auto-tuned",
            "KV cache:  chosen by auto-tune",
            "Images:    off   ·   Strict: off",
        ] {
            assert!(screen.contains(required), "missing '{required}':\n{screen}");
        }
        for banned in ["quant", "AutoBest", "turboquant"] {
            assert!(!screen.contains(banned), "leaked '{banned}':\n{screen}");
        }
    }

    #[test]
    fn the_confirm_menu_offers_exactly_the_five_guided_actions() {
        let backend = TestBackend::new(60, 9);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_confirm(frame, frame.area(), "q36apex", 0))
            .unwrap();
        let screen = buffer_text(&terminal);
        assert!(screen.contains("Ready to launch q36apex?"));
        for row in [
            "Launch now (recommended settings)",
            "Customize settings",
            "Auto-tune this model (run a benchmark)",
            "What do these mean?",
            "Back to models",
        ] {
            assert!(screen.contains(row), "missing row '{row}':\n{screen}");
        }
        assert!(screen.contains("> "), "the selected row is marked");
        assert_eq!(CONFIRM_ROWS.len(), 5);
        assert_eq!(CONFIRM_ROWS[0].1, ConfirmAction::LaunchNow);
    }

    #[test]
    fn the_model_picker_shows_key_name_strict_and_the_footer_rows() {
        let picker = ModelPicker {
            rows: vec![
                ModelRow {
                    key: "q36apex".to_string(),
                    display_name: "Qwen 3.6 APEX".to_string(),
                    strict: true,
                },
                ModelRow::from_def("plain", &def()),
            ],
            show_all_offered: true,
        };
        let backend = TestBackend::new(60, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| picker.render(frame, frame.area(), 0))
            .unwrap();
        let screen = buffer_text(&terminal);
        assert!(screen.contains("q36apex · Qwen 3.6 APEX"));
        assert!(screen.contains("[strict]"));
        assert!(screen.contains("[Show all tiers]"));
        assert!(screen.contains("[Cancel]"));
        assert!(screen.contains("Pick a model"));
    }

    #[test]
    fn fit_classes_map_to_the_traffic_light() {
        assert_eq!(fit_color(FitClass::Fits), Some(Color::Green));
        assert_eq!(fit_color(FitClass::Tight), Some(Color::Yellow));
        assert_eq!(fit_color(FitClass::Over), Some(Color::Red));
        assert_eq!(fit_color(FitClass::Unknown), None, "never guessed");
    }
}
