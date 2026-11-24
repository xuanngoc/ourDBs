//! A query tab: an editor, Run/Stop, history, results and export.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use eframe::egui::{
    self, Align2, Color32, FontId, Key, KeyboardShortcut, Modifiers, Rect, RichText, Sense, Stroke,
    TextEdit, TextFormat, Ui, pos2, text::LayoutJob, vec2,
};
use ourdb_core::{Driver, Env, Kind, MAX_ROWS, ResultSet, export, sql};
use tokio::task::AbortHandle;

use crate::{
    grid::{self, GridOptions, GridState, ResultModel},
    icons::Icon,
    store::HistoryEntry,
    style::{self, Btn, Palette, Segment},
    table_tab::{group_digits, row_count},
    task::{Reply, Spawner, TabData, err_text, next_req},
    value_panel::ValuePanel,
};

struct Running {
    req: u64,
    abort: AbortHandle,
    started: Instant,
}

/// Text that waits for a "Run anyway" click.
struct Confirm {
    text: String,
    reasons: Vec<String>,
}

pub struct QueryTab {
    pub id: u64,
    pub conn_id: String,
    pub conn_name: String,
    pub kind: Kind,
    pub env: Env,
    /// `None` while the connection is closed.
    pub driver: Option<Arc<dyn Driver>>,
    spawner: Spawner,
    pub title: String,
    pub namespace: Option<String>,
    pub namespaces: Vec<String>,
    pub text: String,
    running: Option<Running>,
    results: Vec<ResultSet>,
    active: usize,
    error: Option<String>,
    notice: Option<String>,
    elapsed: Option<Duration>,
    grid: GridState,
    value: ValuePanel,
    version: u64,
    confirm: Option<Confirm>,
}

impl QueryTab {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: u64,
        conn_id: String,
        conn_name: String,
        kind: Kind,
        env: Env,
        driver: Arc<dyn Driver>,
        spawner: Spawner,
        title: String,
        namespace: Option<String>,
        namespaces: Vec<String>,
    ) -> Self {
        Self {
            id,
            conn_id,
            conn_name,
            kind,
            env,
            driver: Some(driver),
            spawner,
            title,
            namespace,
            namespaces,
            text: String::new(),
            running: None,
            results: Vec::new(),
            active: 0,
            error: None,
            notice: None,
            elapsed: None,
            grid: GridState::default(),
            value: ValuePanel::default(),
            version: 0,
            confirm: None,
        }
    }

    /// Select a cell of the result grid.
    pub fn select(&mut self, row: usize, col: usize) {
        self.grid.select(row, col);
    }

    pub fn busy(&self) -> bool {
        self.running.is_some()
    }

    /// Ask before risky text and before writes on Prod. Returns the text when it ran.
    pub fn request_run(&mut self, text: String) -> Option<String> {
        if text.trim().is_empty() || self.running.is_some() {
            return None;
        }
        let doc = self.kind.is_document();
        let mut reasons = Vec::new();
        if let Some(r) = sql::risky(&text, doc) {
            reasons.push(r);
        }
        if self.env == Env::Prod && sql::is_write(&text, doc) {
            reasons.push(format!(
                "\"{}\" is a PRODUCTION connection and this text can change data.",
                self.conn_name
            ));
        }
        if reasons.is_empty() {
            self.run(text.clone());
            Some(text)
        } else {
            self.confirm = Some(Confirm { text, reasons });
            None
        }
    }

    fn run(&mut self, text: String) {
        let Some(driver) = self.driver.clone() else {
            self.error = Some("Not connected. Connect in the sidebar first.".into());
            return;
        };
        let (id, tab, ns) = (next_req(), self.id, self.namespace.clone());
        let abort = self.spawner.spawn(async move {
            let t0 = Instant::now();
            let result = driver
                .execute(ns.as_deref(), &text, MAX_ROWS)
                .await
                .map_err(err_text);
            Reply::Tab {
                tab,
                req: id,
                data: TabData::Executed(result, t0.elapsed()),
            }
        });
        self.running = Some(Running {
            req: id,
            abort,
            started: Instant::now(),
        });
        self.error = None;
        self.notice = None;
    }

    pub fn stop(&mut self) {
        let Some(run) = self.running.take() else {
            return;
        };
        if let Some(d) = self.driver.clone() {
            self.spawner.detach(async move { d.cancel().await });
        }
        run.abort.abort();
        self.error = Some("Stopped.".into());
    }

    pub fn on_reply(&mut self, req: u64, data: TabData) {
        let TabData::Executed(result, elapsed) = data else {
            return;
        };
        if self.running.as_ref().is_none_or(|r| r.req != req) {
            return; // An old run.
        }
        self.running = None;
        self.elapsed = Some(elapsed);
        match result {
            Ok(results) => {
                // Show the last result with rows, else the last result.
                self.active = results
                    .iter()
                    .rposition(|r| !r.columns.is_empty())
                    .unwrap_or(results.len().saturating_sub(1));
                self.results = results;
                self.grid = GridState::default();
                self.version += 1;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// The selected editor text, or all text.
    fn run_text(&self, ctx: &egui::Context) -> String {
        let state = TextEdit::load_state(ctx, self.editor_id());
        match state.and_then(|s| s.cursor.char_range()) {
            Some(range) if !range.is_empty() => range.slice_str(&self.text).to_string(),
            _ => self.text.clone(),
        }
    }

    fn editor_id(&self) -> egui::Id {
        egui::Id::unique(("query-editor", self.id))
    }

    /// Returns query text that ran this frame (for the history).
    pub fn ui(&mut self, ui: &mut Ui, history: &[HistoryEntry]) -> Option<String> {
        let pal = style::pal(ui);
        let mut ran = None;
        // ⌘↵ before the editor sees the Enter key.
        if ui.input_mut(|i| {
            i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter))
        }) {
            ran = self.request_run(self.run_text(ui.ctx()));
        }

        egui::Panel::top(egui::Id::unique(("query-toolbar", self.id)))
            .exact_size(style::TOOLBAR_HEIGHT)
            .frame(
                egui::Frame::NONE
                    .fill(pal.content)
                    .inner_margin(egui::Margin::symmetric(10, 0)),
            )
            .show_separator_line(false)
            .show(ui, |ui| {
                if let Some(t) = self.toolbar(ui, history) {
                    ran = Some(t);
                }
            });
        egui::Panel::top(egui::Id::unique(("query-editor-panel", self.id)))
            .resizable(true)
            .default_size(220.0)
            .size_range(90.0..=640.0)
            .frame(
                egui::Frame::NONE
                    .fill(pal.content)
                    .inner_margin(egui::Margin {
                        left: 10,
                        right: 10,
                        top: 0,
                        bottom: 10,
                    }),
            )
            .show(ui, |ui| self.editor(ui, &pal));
        egui::Panel::bottom(egui::Id::unique(("query-status", self.id)))
            .exact_size(style::STATUS_HEIGHT)
            .frame(
                egui::Frame::NONE
                    .fill(pal.chrome)
                    .inner_margin(egui::Margin::symmetric(10, 0)),
            )
            .show(ui, |ui| self.status(ui, &pal));

        if self.results.len() > 1 {
            egui::Panel::top(egui::Id::unique(("query-result-tabs", self.id)))
                .exact_size(36.0)
                .frame(
                    egui::Frame::NONE
                        .fill(pal.content)
                        .inner_margin(egui::Margin::symmetric(10, 0)),
                )
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        let labels: Vec<String> = (1..=self.results.len())
                            .map(|i| format!("Result {i}"))
                            .collect();
                        let items: Vec<(usize, Segment)> = labels
                            .iter()
                            .enumerate()
                            .map(|(i, l)| (i, Segment::text(l)))
                            .collect();
                        let mut active = self.active;
                        if style::segmented(ui, &mut active, &items, 0.0) {
                            self.active = active;
                            self.grid = GridState::default();
                        }
                    });
                });
        }

        let has_rows = self
            .results
            .get(self.active)
            .is_some_and(|r| !r.columns.is_empty());
        if has_rows {
            egui::Panel::right(egui::Id::unique("value-panel"))
                .default_size(280.0)
                .size_range(220.0..=700.0)
                .frame(
                    egui::Frame::NONE
                        .inner_margin(egui::Margin::same(12))
                        .fill(pal.sidebar),
                )
                .show(ui, |ui| {
                    let model = ResultModel(&self.results[self.active]);
                    let key = self.version * 1000 + self.active as u64;
                    self.value
                        .show(ui, &model, self.grid.selected, key, false, false);
                });
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(pal.content))
            .show(ui, |ui| match self.results.get(self.active) {
                Some(rs) if !rs.columns.is_empty() => {
                    let opts = GridOptions {
                        editable: false,
                        sort: None,
                        sortable: false,
                        filterable: false,
                        keys: &[],
                    };
                    grid::show(
                        ui,
                        ("query", self.id, self.version, self.active),
                        &ResultModel(rs),
                        &mut self.grid,
                        &opts,
                    );
                }
                Some(rs) => {
                    let text = rs.message.clone().unwrap_or_else(|| match rs.affected {
                        Some(n) => format!("{} affected", row_count(n)),
                        None => "Done.".into(),
                    });
                    style::center_text(ui, &text, pal.text2);
                }
                None if self.running.is_some() => {
                    ui.centered_and_justified(|ui| style::spinner(ui, 18.0));
                }
                None => {
                    let hint = if self.kind.is_document() {
                        "Write a command, e.g. db.users.find({age: {$gt: 30}}), then press ⌘↵."
                    } else {
                        "Write SQL, then press ⌘↵ to run it (or only the selected text)."
                    };
                    style::center_text(ui, hint, pal.text3);
                }
            });

        if let Some(t) = self.confirm_ui(ui.ctx(), &pal) {
            ran = Some(t);
        }
        ran
    }

    fn toolbar(&mut self, ui: &mut Ui, history: &[HistoryEntry]) -> Option<String> {
        let mut ran = None;
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if !self.namespaces.is_empty() {
                let selected = self.namespace.clone().unwrap_or_else(|| "(default)".into());
                style::combo(("ns", self.id), selected, 150.0)
                    .show_ui(ui, |ui| {
                        for ns in &self.namespaces {
                            ui.selectable_value(&mut self.namespace, Some(ns.clone()), ns);
                        }
                    })
                    .response
                    .on_hover_text(if self.kind.is_document() {
                        "Database"
                    } else {
                        "Schema"
                    });
                style::toolbar_divider(ui);
            }
            let running = self.running.is_some();
            let run = Btn::primary("Run").icon(Icon::Play).shortcut("⌘↵");
            if ui.add_enabled(!running, run).clicked() {
                ran = self.request_run(self.run_text(ui.ctx()));
            }
            if ui
                .add_enabled(running, Btn::secondary("Stop").icon(Icon::Stop))
                .clicked()
            {
                self.stop();
            }
            let hist = ui.add(Btn::secondary("History").icon(Icon::History));
            egui::Popup::menu(&hist).show(|ui| {
                ui.set_min_width(320.0);
                if history.is_empty() {
                    ui.label(RichText::new("No queries yet.").color(style::pal(ui).text2));
                }
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for h in history {
                            let line: String =
                                h.text.split_whitespace().collect::<Vec<_>>().join(" ");
                            let short: String = line.chars().take(90).collect();
                            if ui
                                .add(
                                    egui::Button::new(RichText::new(short).monospace())
                                        .frame(false),
                                )
                                .on_hover_text(&h.text)
                                .clicked()
                            {
                                self.text = h.text.clone();
                                ui.close();
                            }
                        }
                    });
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let has = self
                    .results
                    .get(self.active)
                    .is_some_and(|r| !r.columns.is_empty());
                let export = ui.add_enabled(has, Btn::secondary("Export").icon(Icon::Export));
                egui::Popup::menu(&export).show(|ui| {
                    let rs = self
                        .results
                        .get(self.active)
                        .filter(|r| !r.columns.is_empty());
                    if ui.button("CSV…").clicked()
                        && let Some(rs) = rs
                    {
                        let text = export::to_csv(rs);
                        self.save_file("result.csv", "CSV", "csv", &text);
                        ui.close();
                    }
                    let rs = self
                        .results
                        .get(self.active)
                        .filter(|r| !r.columns.is_empty());
                    if ui.button("JSON…").clicked()
                        && let Some(rs) = rs
                    {
                        let text = export::to_json(rs);
                        self.save_file("result.json", "JSON", "json", &text);
                        ui.close();
                    }
                });
            });
        });
        ran
    }

    fn save_file(&mut self, name: &str, label: &str, ext: &str, text: &str) {
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(name)
            .add_filter(label, &[ext])
            .save_file()
        else {
            return;
        };
        match std::fs::write(&path, text) {
            Ok(()) => self.notice = Some(format!("Saved {}", path.display())),
            Err(e) => self.error = Some(format!("Cannot write {}: {e}", path.display())),
        }
    }

    fn editor(&mut self, ui: &mut Ui, pal: &Palette) {
        let doc = self.kind.is_document();
        let font = style::mono(12.5);
        let base = pal.text;
        let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
            let mut job = highlight(buf.as_str(), doc, pal, base, &font);
            job.wrap.max_width = wrap_width;
            ui.ctx().fonts_mut(|f| f.layout_job(job))
        };
        let id = self.editor_id();
        let lines = self.text.split('\n').count().max(1);
        let gutter_w = (lines.to_string().len().max(2) as f32) * 7.5 + 20.0;
        let out = style::editor_frame(pal, pal.content)
            .inner_margin(egui::Margin::same(1))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            let view_h = ui.available_height();
                            let (gutter, _) =
                                ui.allocate_exact_size(vec2(gutter_w, view_h), Sense::hover());
                            let size = ui.available_size();
                            let out = TextEdit::multiline(&mut self.text)
                                .id(id)
                                .code_editor()
                                .font(style::mono(12.5))
                                .frame(egui::Frame::NONE.inner_margin(egui::Margin {
                                    left: 10,
                                    right: 8,
                                    top: 8,
                                    bottom: 8,
                                }))
                                .desired_width(f32::INFINITY)
                                .min_size(size)
                                .layouter(&mut layouter)
                                .show(ui);
                            // Line numbers, one per text line (not per wrapped row).
                            let painter = ui.painter();
                            let bottom = out.response.response.rect.bottom().max(gutter.bottom());
                            let g = Rect::from_min_max(gutter.min, pos2(gutter.right(), bottom));
                            painter.rect_filled(g, 0.0, pal.code_bg);
                            painter.vline(
                                g.right() - 0.5,
                                g.y_range(),
                                Stroke::new(1.0, pal.separator),
                            );
                            let mut n = 1;
                            let mut start = true;
                            for row in &out.galley.rows {
                                if start {
                                    let y = out.galley_pos.y + row.rect().center().y;
                                    painter.text(
                                        pos2(g.right() - 8.0, y),
                                        Align2::RIGHT_CENTER,
                                        n.to_string(),
                                        style::mono(11.0),
                                        pal.text3,
                                    );
                                    n += 1;
                                }
                                start = row.ends_with_newline;
                            }
                            out.response.response.has_focus()
                        })
                        .inner
                    })
                    .inner
            });
        if out.inner {
            style::focus_ring(ui.painter(), out.response.rect, pal);
        }
    }

    fn status(&mut self, ui: &mut Ui, pal: &Palette) {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let small = |text: String, color| RichText::new(text).size(11.5).color(color);
            if let Some(run) = &self.running {
                style::spinner(ui, 11.0);
                ui.label(small(
                    format!("Running… {:.1} s", run.started.elapsed().as_secs_f32()),
                    pal.text2,
                ));
                ui.ctx().request_repaint_after(Duration::from_millis(100));
                return;
            }
            if let Some(e) = &self.error {
                ui.add(egui::Label::new(small(e.clone(), pal.error)).truncate())
                    .on_hover_text(e);
                return;
            }
            if let Some(n) = &self.notice {
                ui.label(small(n.clone(), pal.text2));
                return;
            }
            let Some(rs) = self.results.get(self.active) else {
                ui.label(small(
                    format!("{} · {}", self.conn_name, self.kind.label()),
                    pal.text3,
                ));
                return;
            };
            let mut parts = Vec::new();
            if !rs.columns.is_empty() {
                parts.push(row_count(rs.rows.len() as u64));
                if rs.truncated {
                    parts.push(format!(
                        "showing first {} rows",
                        group_digits(rs.rows.len() as u64)
                    ));
                }
            } else if let Some(n) = rs.affected {
                parts.push(format!("{} affected", row_count(n)));
            }
            if let Some(m) = &rs.message {
                parts.push(m.clone());
            }
            parts.push(fmt_duration(rs.elapsed));
            if self.results.len() > 1
                && let Some(total) = self.elapsed
            {
                parts.push(format!(
                    "{} statements, total {}",
                    self.results.len(),
                    fmt_duration(total)
                ));
            }
            ui.add(egui::Label::new(small(parts.join("  ·  "), pal.text2)).truncate());
        });
    }

    fn confirm_ui(&mut self, ctx: &egui::Context, pal: &Palette) -> Option<String> {
        let confirm = self.confirm.as_ref()?;
        let mut run = false;
        let mut cancel = false;
        let prod = self.env == Env::Prod;
        style::modal(egui::Id::unique(("confirm", self.id)), ctx).show(ctx, |ui| {
            ui.set_width(480.0);
            ui.horizontal(|ui| {
                style::alert_header(ui, Icon::Warning, pal.danger, "Run this?");
                if prod {
                    style::pill(ui, "PRODUCTION", Color32::WHITE, pal.danger);
                }
            });
            ui.add_space(8.0);
            for r in &confirm.reasons {
                style::note(ui, r, pal.text);
                ui.add_space(2.0);
            }
            ui.add_space(8.0);
            let short: String = confirm.text.chars().take(600).collect();
            style::editor_frame(pal, pal.code_bg).show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(short).font(style::mono(12.0)).color(pal.text),
                            )
                            .wrap(),
                        );
                    });
            });
            ui.add_space(16.0);
            style::footer(ui, |ui| {
                if ui.add(Btn::danger("Run anyway")).clicked() {
                    run = true;
                }
                if ui.add(Btn::secondary("Cancel")).clicked() {
                    cancel = true;
                }
            });
        });
        if run {
            let text = self.confirm.take()?.text;
            self.run(text.clone());
            return Some(text);
        }
        if cancel {
            self.confirm = None;
        }
        None
    }
}

fn fmt_duration(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 1000.0 {
        format!("{ms:.0} ms")
    } else {
        format!("{:.2} s", ms / 1000.0)
    }
}

const SQL_KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "AND",
    "OR",
    "NOT",
    "NULL",
    "IS",
    "IN",
    "AS",
    "ON",
    "JOIN",
    "LEFT",
    "RIGHT",
    "INNER",
    "OUTER",
    "FULL",
    "CROSS",
    "GROUP",
    "BY",
    "ORDER",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "INSERT",
    "INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE",
    "CREATE",
    "TABLE",
    "VIEW",
    "INDEX",
    "DROP",
    "ALTER",
    "ADD",
    "COLUMN",
    "TRUNCATE",
    "WITH",
    "UNION",
    "ALL",
    "DISTINCT",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "RETURNING",
    "LIKE",
    "ILIKE",
    "BETWEEN",
    "EXISTS",
    "ASC",
    "DESC",
    "PRIMARY",
    "KEY",
    "DEFAULT",
    "TRUE",
    "FALSE",
    "EXPLAIN",
    "SHOW",
    "USE",
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
];

/// Simple colors: keywords, strings, numbers and comments.
fn highlight(text: &str, document: bool, pal: &Palette, base: Color32, font: &FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    let fmt = |color: Color32| TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let b = bytes[i];
        let color = if b == b'-' && bytes.get(i + 1) == Some(&b'-') && !document
            || b == b'/' && bytes.get(i + 1) == Some(&b'/') && document
        {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            pal.comment
        } else if b == b'\'' || b == b'"' || b == b'`' {
            i += 1;
            while i < bytes.len() && bytes[i] != b {
                i += if bytes[i] == b'\\' { 2 } else { 1 };
            }
            i = (i + 1).min(bytes.len());
            pal.string
        } else if b.is_ascii_digit() {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.') {
                i += 1;
            }
            pal.number
        } else if b.is_ascii_alphabetic() || b == b'_' || b == b'$' {
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'$')
            {
                i += 1;
            }
            let word = &text[start..i];
            let is_kw = if document {
                word == "db" || word.starts_with('$')
            } else {
                SQL_KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(word))
            };
            if is_kw { pal.keyword } else { base }
        } else {
            // One character (may be more than one byte).
            i += text[i..].chars().next().map(char::len_utf8).unwrap_or(1);
            base
        };
        job.append(&text[start..i], 0.0, fmt(color));
    }
    job
}
