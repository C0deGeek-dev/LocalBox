//! The guided launcher's widgets: the composed guided screen (hardware
//! banner + optional plain-language panel + menu), rich model rows, and the
//! 5-item confirm actions — backend-agnostic (rendered through a `Frame`, so
//! `TestBackend` snapshot tests pin the screens) with the fit-aware colors
//! from the shared fit classifier.
//!
//! Every guided state renders as ONE frame through [`render_guided_screen`]:
//! the widgets replace each other inside a single live band instead of
//! stacking new boxes below old ones.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use localx_llama_core::vram::quant_fit_class;
use localx_llama_core::{FitClass, ModelDef};

use crate::plan::GuidedPlan;
use crate::vocab;

/// The fit-aware color for a row: green fits / yellow tight / red over;
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

/// One menu row: its text plus an optional accent color (the fit traffic
/// light on model rows). The plain path prints the text; only the rich
/// path shows the color.
#[derive(Debug, Clone)]
pub struct MenuRow {
    pub text: String,
    pub accent: Option<Color>,
}

impl MenuRow {
    /// An uncolored row.
    #[must_use]
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            accent: None,
        }
    }

    /// A row colored by its fit class.
    #[must_use]
    pub fn fit(text: impl Into<String>, fit: FitClass) -> Self {
        Self {
            text: text.into(),
            accent: fit_color(fit),
        }
    }
}

/// One row of the model picker, with everything a chooser needs to see:
/// name, on-disk size, the largest conversation it can hold, and whether
/// it can look at images.
#[derive(Debug, Clone)]
pub struct ModelRow {
    pub key: String,
    pub display_name: String,
    /// The default quant's on-disk size, when the catalog names one.
    pub size_gb: Option<f64>,
    /// The largest configured context, in tokens.
    pub max_context_tokens: Option<i64>,
    /// Whether a vision module is configured (the model can look at images).
    pub vision: bool,
    pub strict: bool,
    /// How the default quant fits the probed graphics memory.
    pub fit: FitClass,
}

/// Compact memory-as-words for a token count (`tokens × 0.75`, thousands
/// as `k`), for one-line model rows.
fn words_compact(tokens: i64) -> String {
    let words = tokens * 3 / 4;
    if words >= 1000 {
        format!("~{}k words", words / 1000)
    } else {
        format!("~{words} words")
    }
}

impl ModelRow {
    /// Build a row from a catalog entry, judged against the probed VRAM.
    #[must_use]
    pub fn from_def(key: &str, def: &ModelDef, vram_gb: i64) -> Self {
        let size_gb = def
            .quant
            .as_deref()
            .and_then(|q| def.quants.get(q))
            .and_then(|entry| entry.size_gb);
        Self {
            key: key.to_string(),
            display_name: def.display_name.clone().unwrap_or_else(|| key.to_string()),
            size_gb,
            max_context_tokens: def.contexts.values().copied().max().filter(|t| *t > 0),
            vision: def
                .vision_module
                .as_deref()
                .is_some_and(|v| !v.trim().is_empty()),
            strict: def.strict.unwrap_or(false),
            fit: quant_fit_class(size_gb, vram_gb),
        }
    }

    /// The one-line label both paths show: `key · Name · GB · words · images`.
    #[must_use]
    pub fn label(&self) -> String {
        let mut label = format!("{} · {}", self.key, self.display_name);
        if let Some(gb) = self.size_gb {
            label.push_str(&format!(" · {gb:.1} GB"));
        }
        if let Some(tokens) = self.max_context_tokens {
            label.push_str(&format!(" · up to {}", words_compact(tokens)));
        }
        if self.vision {
            label.push_str(" · images");
        }
        if self.strict {
            label.push_str("  [strict]");
        }
        label
    }

    /// The menu row for this model: the label colored by its fit class.
    #[must_use]
    pub fn menu_row(&self) -> MenuRow {
        MenuRow::fit(self.label(), self.fit)
    }
}

/// One guided state, rendered as one frame: an optional one-line hardware
/// banner, an optional bordered plain-language panel, and the menu.
pub struct GuidedScreen<'a> {
    /// The dim one-line hardware banner (empty = no banner row).
    pub banner: &'a str,
    /// An optional `(title, text)` panel — the recommended-plan summary.
    pub panel: Option<(&'a str, &'a str)>,
    pub menu_title: &'a str,
    pub rows: &'a [MenuRow],
    pub selected: usize,
}

/// Render a bordered plain-text panel.
pub fn render_text_panel(frame: &mut Frame, area: Rect, title: &str, text: &str) {
    let lines: Vec<Line> = text.lines().map(|l| Line::from(l.to_string())).collect();
    let panel = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title.to_string()),
    );
    frame.render_widget(panel, area);
}

/// Render the recommended-plan summary panel (the plain-language centrepiece).
pub fn render_summary(frame: &mut Frame, area: Rect, plan: &GuidedPlan, def: &ModelDef) {
    render_text_panel(
        frame,
        area,
        "Recommended plan",
        &vocab::plan_summary(plan, def),
    );
}

/// The minimum width for the panel to sit beside the menu instead of above it.
const SIDE_BY_SIDE_MIN_WIDTH: u16 = 100;

/// Render one guided state into the live band. Boxes are sized to their
/// content (never a tall empty frame), and the whole band is redrawn every
/// frame so states replace each other instead of stacking.
pub fn render_guided_screen(frame: &mut Frame, screen: &GuidedScreen) {
    let area = frame.area();
    let banner_height = u16::from(!screen.banner.is_empty());
    if banner_height == 1 {
        let banner = Paragraph::new(Line::from(Span::styled(
            screen.banner.to_string(),
            Style::default().add_modifier(Modifier::DIM),
        )));
        frame.render_widget(banner, Rect { height: 1, ..area });
    }
    let body = Rect {
        y: area.y + banner_height,
        height: area.height.saturating_sub(banner_height),
        ..area
    };

    let menu_area = match screen.panel {
        Some((title, text)) => {
            let panel_lines = u16::try_from(text.lines().count()).unwrap_or(u16::MAX);
            let panel_height = (panel_lines + 2).min(body.height);
            if body.width >= SIDE_BY_SIDE_MIN_WIDTH {
                let content_width = text
                    .lines()
                    .map(|l| l.chars().count())
                    .max()
                    .unwrap_or(0)
                    .max(title.chars().count());
                let panel_width =
                    (u16::try_from(content_width).unwrap_or(u16::MAX) + 4).min(body.width / 2);
                render_text_panel(
                    frame,
                    Rect {
                        width: panel_width,
                        height: panel_height,
                        ..body
                    },
                    title,
                    text,
                );
                Rect {
                    x: body.x + panel_width + 1,
                    width: body.width - panel_width - 1,
                    ..body
                }
            } else {
                render_text_panel(
                    frame,
                    Rect {
                        height: panel_height,
                        ..body
                    },
                    title,
                    text,
                );
                Rect {
                    y: body.y + panel_height,
                    height: body.height.saturating_sub(panel_height),
                    ..body
                }
            }
        }
        None => body,
    };

    let mut state = ListState::default();
    state.select(Some(screen.selected));
    let items: Vec<ListItem> = screen
        .rows
        .iter()
        .map(|row| {
            let style = row
                .accent
                .map_or_else(Style::default, |color| Style::default().fg(color));
            ListItem::new(Line::from(Span::styled(row.text.clone(), style)))
        })
        .collect();
    let rows_height = u16::try_from(screen.rows.len()).unwrap_or(u16::MAX);
    let list_area = Rect {
        // Content-sized: the border hugs the rows instead of framing air.
        height: (rows_height + 2).min(menu_area.height),
        ..menu_area
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(screen.menu_title.to_string()),
        )
        .highlight_symbol("> ")
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));
    frame.render_stateful_widget(list, list_area, &mut state);
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
            "Contexts": { "": 32768, "256k": 262144 },
            "VisionModule": "mmproj.gguf"
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
    fn model_rows_carry_size_context_images_and_fit() {
        let row = ModelRow::from_def("q36apex", &def(), 24);
        assert_eq!(
            row.label(),
            "q36apex · Qwen 3.6 APEX · 18.6 GB · up to ~196k words · images"
        );
        assert_eq!(row.fit, FitClass::Tight, "18.6 GB on a 24 GB card");
        assert_eq!(row.menu_row().accent, Some(Color::Yellow));
        // No probe → no verdict, and the label simply omits what it lacks.
        let unknown = ModelRow::from_def("bare", &ModelDef::default(), 0);
        assert_eq!(unknown.fit, FitClass::Unknown);
        assert_eq!(unknown.menu_row().accent, None);
        assert_eq!(unknown.label(), "bare · bare");
    }

    #[test]
    fn the_guided_screen_composes_banner_panel_and_menu_in_one_frame() {
        let plan = resolve_launch_plan(
            "q36apex",
            &def(),
            &DefaultLaunch::default(),
            &PlanOverrides::default(),
        );
        let summary = vocab::plan_summary(&plan, &def());
        let rows: Vec<MenuRow> = CONFIRM_ROWS
            .iter()
            .map(|(label, _)| MenuRow::plain(*label))
            .collect();
        let backend = TestBackend::new(120, 18);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_guided_screen(
                    frame,
                    &GuidedScreen {
                        banner: "Computer:  NVIDIA GeForce RTX 4090 · 24 GB graphics memory",
                        panel: Some(("Recommended plan", &summary)),
                        menu_title: "Ready to launch q36apex?",
                        rows: &rows,
                        selected: 0,
                    },
                );
            })
            .unwrap();
        let screen = buffer_text(&terminal);

        // The banner, the panel, and every action share the one frame.
        for required in [
            "Computer:  NVIDIA GeForce RTX 4090 · 24 GB graphics memory",
            "Recommended plan",
            "Model:     Qwen 3.6 APEX",
            "Run with:  LocalPilot (recommended)",
            "Ready to launch q36apex?",
            "Launch now (recommended settings)",
            "Customize settings",
            "Auto-tune this model (run a benchmark)",
            "What do these mean?",
            "Back to models",
        ] {
            assert!(screen.contains(required), "missing '{required}':\n{screen}");
        }
        assert!(screen.contains("> "), "the selected row is marked");
        // The plain-language contract holds on the rendered screen.
        for banned in ["AutoBest", "turboquant"] {
            assert!(!screen.contains(banned), "leaked '{banned}':\n{screen}");
        }
    }

    #[test]
    fn side_by_side_needs_width_and_stacks_below_it() {
        let summary = "Model:     X\nRun with:  Y";
        let rows = [MenuRow::plain("Launch now")];
        for (width, side_by_side) in [(120u16, true), (80u16, false)] {
            let backend = TestBackend::new(width, 18);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    render_guided_screen(
                        frame,
                        &GuidedScreen {
                            banner: "",
                            panel: Some(("Plan", summary)),
                            menu_title: "Menu",
                            rows: &rows,
                            selected: 0,
                        },
                    );
                })
                .unwrap();
            let screen = buffer_text(&terminal);
            let plan_row = screen
                .lines()
                .position(|l| l.contains("Plan"))
                .expect("panel rendered");
            let menu_row = screen
                .lines()
                .position(|l| l.contains("Menu"))
                .expect("menu rendered");
            if side_by_side {
                assert_eq!(plan_row, menu_row, "wide terminals sit side by side");
            } else {
                assert!(menu_row > plan_row, "narrow terminals stack");
            }
        }
    }

    #[test]
    fn menu_boxes_hug_their_rows_instead_of_framing_air() {
        let rows = [MenuRow::plain("only row")];
        let backend = TestBackend::new(40, 18);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_guided_screen(
                    frame,
                    &GuidedScreen {
                        banner: "",
                        panel: None,
                        menu_title: "Menu",
                        rows: &rows,
                        selected: 0,
                    },
                );
            })
            .unwrap();
        let screen = buffer_text(&terminal);
        let bottom_border = screen
            .lines()
            .position(|l| l.starts_with('└'))
            .expect("bottom border drawn");
        assert_eq!(bottom_border, 2, "1 row + borders = 3 lines, not 18");
    }

    #[test]
    fn the_confirm_actions_stay_exactly_five() {
        assert_eq!(CONFIRM_ROWS.len(), 5);
        assert_eq!(CONFIRM_ROWS[0].1, ConfirmAction::LaunchNow);
        assert_eq!(CONFIRM_ROWS[4].1, ConfirmAction::BackToModels);
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
        for required in [
            "Recommended plan",
            "Model:     Qwen 3.6 APEX",
            "Speed:     Turbo (auto-tuned for your GPU) · auto-tuned",
            "KV cache:  chosen by auto-tune",
        ] {
            assert!(screen.contains(required), "missing '{required}':\n{screen}");
        }
        for banned in ["quant", "AutoBest", "turboquant"] {
            assert!(!screen.contains(banned), "leaked '{banned}':\n{screen}");
        }
    }

    #[test]
    fn fit_classes_map_to_the_traffic_light() {
        assert_eq!(fit_color(FitClass::Fits), Some(Color::Green));
        assert_eq!(fit_color(FitClass::Tight), Some(Color::Yellow));
        assert_eq!(fit_color(FitClass::Over), Some(Color::Red));
        assert_eq!(fit_color(FitClass::Unknown), None, "never guessed");
    }
}
