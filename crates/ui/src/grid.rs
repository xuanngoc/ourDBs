//! One data grid for table tabs (editable) and query results (read-only).
//!
//! The grid does not own data. It reads cells through [`GridModel`] and
//! reports what the user did as [`GridEvent`]s. Copy actions run here.

use std::hash::{DefaultHasher, Hash, Hasher};

use eframe::egui::{
    self, Align, Color32, FontId, Key, Rect, Sense, Stroke, StrokeKind, TextEdit, TextFormat, Ui,
    pos2,
    text::{LayoutJob, TextWrapping},
    vec2,
};
use egui_extras::{Column, TableBuilder};
use ourdb_core::{Edit, ResultSet, Value, export};

use crate::{
    icons::{self, Icon},
    style::{self, HEADER_HEIGHT, Palette, ROW_HEIGHT},
};

const PAD: f32 = 8.0;
const CELL_SIZE: f32 = 12.5;

/// What a cell shows: a loaded value, a staged edit, or a default for a new row.
pub enum Shown<'a> {
    Val(&'a Value),
    Edit(&'a Edit),
    Default,
    Raw(&'a str),
}

impl Shown<'_> {
    /// The value the cell stands for (used for copy).
    pub fn to_value(&self) -> Value {
        match self {
            Shown::Val(v) => (*v).clone(),
            Shown::Edit(Edit::Null) => Value::Null,
            Shown::Edit(Edit::Text(s)) => Value::Text(s.clone()),
            Shown::Default => Value::Missing,
            Shown::Raw(s) => Value::Text(s.to_string()),
        }
    }

    /// The text an in-place editor starts with.
    pub fn edit_text(&self) -> String {
        match self {
            Shown::Val(v) => v.edit_text(),
            Shown::Edit(Edit::Text(s)) => s.clone(),
            Shown::Raw(s) => s.to_string(),
            Shown::Edit(Edit::Null) | Shown::Default => String::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    Clean,
    Edited,
    New,
    Deleted,
}

pub trait GridModel {
    fn columns(&self) -> &[String];
    fn types(&self) -> &[String];
    fn len(&self) -> usize;
    fn cell(&self, row: usize, col: usize) -> (Shown<'_>, Mark);
    /// Text of the row-number column.
    fn row_label(&self, row: usize) -> String;
}

/// A read-only model for a query result or the structure view.
pub struct ResultModel<'a>(pub &'a ResultSet);

impl GridModel for ResultModel<'_> {
    fn columns(&self) -> &[String] {
        &self.0.columns
    }
    fn types(&self) -> &[String] {
        &self.0.types
    }
    fn len(&self) -> usize {
        self.0.rows.len()
    }
    fn cell(&self, row: usize, col: usize) -> (Shown<'_>, Mark) {
        let v = self
            .0
            .rows
            .get(row)
            .and_then(|r| r.get(col))
            .unwrap_or(&Value::Missing);
        (Shown::Val(v), Mark::Clean)
    }
    fn row_label(&self, row: usize) -> String {
        (row + 1).to_string()
    }
}

/// The values of one row as shown (staged edits included).
pub fn row_values(model: &dyn GridModel, row: usize) -> Vec<Value> {
    (0..model.columns().len())
        .map(|c| model.cell(row, c).0.to_value())
        .collect()
}

pub enum GridEvent {
    Sort(String),
    Commit {
        row: usize,
        col: usize,
        text: String,
    },
    SetNull {
        row: usize,
        col: usize,
    },
    FilterBy {
        row: usize,
        col: usize,
    },
    DeleteRow(usize),
}

pub struct GridOptions<'a> {
    pub editable: bool,
    pub sort: Option<&'a (String, bool)>,
    pub sortable: bool,
    pub filterable: bool,
    /// Key columns: the header shows a key icon.
    pub keys: &'a [String],
}

struct CellEditor {
    row: usize,
    col: usize,
    text: String,
    /// The text at start: no change means no edit.
    initial: String,
    focus: bool,
}

#[derive(Default)]
pub struct GridState {
    pub selected: Option<(usize, usize)>,
    editing: Option<CellEditor>,
    widths_key: u64,
    widths: Vec<f32>,
    scroll_to_row: Option<usize>,
    /// The grid takes arrow keys after a click on a cell, until something else gets focus.
    has_focus: bool,
}

impl GridState {
    pub fn select(&mut self, row: usize, col: usize) {
        self.selected = Some((row, col));
        self.scroll_to_row = Some(row);
    }

    /// Start the in-place editor on a cell.
    pub fn start_edit(&mut self, row: usize, col: usize, text: String) {
        self.selected = Some((row, col));
        self.editing = Some(CellEditor {
            row,
            col,
            initial: text.clone(),
            text,
            focus: true,
        });
    }
}

/// Show the grid. `salt` must differ between grids that show other data.
pub fn show(
    ui: &mut Ui,
    salt: impl Hash,
    model: &dyn GridModel,
    state: &mut GridState,
    opts: &GridOptions,
) -> Vec<GridEvent> {
    let mut events = Vec::new();
    let columns = model.columns();
    let n_rows = model.len();
    if let Some((r, c)) = state.selected
        && (r >= n_rows || c >= columns.len())
    {
        state.selected = None;
    }

    // New columns: measure widths again and start a fresh table state.
    let mut h = DefaultHasher::new();
    salt.hash(&mut h);
    columns.hash(&mut h);
    let key = h.finish();
    if key != state.widths_key {
        state.widths = measure(ui, model);
        state.widths_key = key;
        state.editing = None;
    }

    let area = ui.available_rect_before_wrap();
    if ui.input(|i| {
        i.pointer.any_click() && i.pointer.interact_pos().is_some_and(|p| !area.contains(p))
    }) {
        state.has_focus = false;
    }
    keyboard(ui, model, state, opts);

    let pal = style::pal(ui);
    let font = style::regular(CELL_SIZE);
    let head_font = style::semibold(12.0);
    let label_font = style::regular(11.0);
    let label_w = row_label_width(ui, model, &label_font);
    let numeric: Vec<bool> = model.types().iter().map(|t| is_numeric_type(t)).collect();
    let digits = digit_widths(ui, &font);

    // The header band runs across the full width, also right of the last column.
    let band = Rect::from_min_size(area.min, vec2(area.width(), HEADER_HEIGHT));
    ui.painter().rect_filled(band, 0.0, pal.header_bg);
    ui.painter().hline(
        band.x_range(),
        band.bottom() - 0.5,
        Stroke::new(1.0, pal.separator),
    );

    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
        // egui_extras draws the column resize lines down the full height. Hide them
        // at rest; the cells draw their own lines, only where rows exist.
        let v = ui.visuals_mut();
        v.widgets.noninteractive.bg_stroke = Stroke::NONE;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, pal.control_border_hover);
        v.widgets.active.bg_stroke = Stroke::new(1.0, pal.accent);
        v.faint_bg_color = pal.zebra;
        egui::ScrollArea::horizontal()
            .id_salt(("grid-h", key))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut tb = TableBuilder::new(ui)
                    .id_salt(("grid", key))
                    .striped(true)
                    .sense(Sense::click())
                    .resizable(true)
                    .auto_shrink([false, false])
                    .min_scrolled_height(0.0)
                    .animate_scrolling(false)
                    .cell_layout(egui::Layout::left_to_right(Align::Center))
                    .column(Column::exact(label_w).resizable(false));
                for w in &state.widths {
                    tb = tb.column(
                        Column::initial(*w)
                            .at_least(40.0)
                            .clip(true)
                            .resizable(true),
                    );
                }
                if let Some(r) = state.scroll_to_row.take() {
                    tb = tb.scroll_to_row(r, None);
                }

                tb.header(HEADER_HEIGHT, |mut header| {
                    header.col(|ui| {
                        let rect = ui.max_rect();
                        header_bg(ui, rect, &pal);
                        ui.painter().vline(
                            rect.right() - 0.5,
                            rect.y_range(),
                            Stroke::new(1.0, pal.separator),
                        );
                    });
                    for (i, name) in columns.iter().enumerate() {
                        let arrow = match opts.sort {
                            Some((c, asc)) if c == name => Some(*asc),
                            _ => None,
                        };
                        let right = numeric.get(i).copied().unwrap_or(false);
                        let key = opts.keys.contains(name);
                        let (_, resp) = header.col(|ui| {
                            let hover = opts.sortable && ui.rect_contains_pointer(ui.max_rect());
                            let look = HeaderLook {
                                arrow,
                                right,
                                hover,
                                key,
                            };
                            header_cell(ui, name, look, &pal, &head_font)
                        });
                        let ty = model.types().get(i).map(String::as_str).unwrap_or("");
                        let resp = if ty.is_empty() {
                            resp
                        } else {
                            resp.on_hover_text(ty)
                        };
                        if opts.sortable && resp.clicked() {
                            events.push(GridEvent::Sort(name.clone()));
                        }
                    }
                })
                .body(|body| {
                    body.rows(ROW_HEIGHT, n_rows, |mut row| {
                        let r = row.index();
                        row.set_hovered(false);
                        let row_selected = state.selected.is_some_and(|(sr, _)| sr == r);
                        let row_mark = model.cell(r, 0).1;
                        row.col(|ui| {
                            let rect = ui.max_rect();
                            if row_selected {
                                ui.painter().rect_filled(rect, 0.0, pal.selected_row);
                            }
                            let color = if row_selected { pal.text2 } else { pal.text3 };
                            let job = style::one_line(
                                &model.row_label(r),
                                label_font.clone(),
                                color,
                                rect.width() - PAD,
                            );
                            paint_job(ui, rect, job, true);
                            ui.painter().vline(
                                rect.right() - 0.5,
                                rect.y_range(),
                                Stroke::new(1.0, pal.separator),
                            );
                        });
                        for c in 0..columns.len() {
                            let (shown, mark) = model.cell(r, c);
                            let selected = state.selected == Some((r, c));
                            let editing_here = state
                                .editing
                                .as_ref()
                                .is_some_and(|e| e.row == r && e.col == c);
                            let (_, resp) = row.col(|ui| {
                                if editing_here {
                                    cell_editor(ui, state, &mut events, &pal);
                                } else {
                                    let right = numeric.get(c).copied().unwrap_or(false)
                                        || is_number(&shown);
                                    paint_cell(
                                        ui,
                                        &shown,
                                        Look {
                                            mark,
                                            selected,
                                            row_selected,
                                            right,
                                        },
                                        &pal,
                                        &font,
                                        &digits,
                                    );
                                }
                            });
                            if resp.clicked() || resp.secondary_clicked() {
                                state.selected = Some((r, c));
                                state.has_focus = true;
                            }
                            if resp.double_clicked()
                                && opts.editable
                                && mark != Mark::Deleted
                                && row_mark != Mark::Deleted
                            {
                                state.start_edit(r, c, shown.edit_text());
                            }
                            resp.context_menu(|ui| cell_menu(ui, model, r, c, opts, &mut events));
                        }
                    });
                });
            });
    });
    events
}

/// Arrow keys move the selection, Enter edits, ⌘C copies. Only when no text field has focus.
fn keyboard(ui: &Ui, model: &dyn GridModel, state: &mut GridState, opts: &GridOptions) {
    if ui.ctx().memory(|m| m.focused().is_some()) {
        state.has_focus = false;
    }
    if !state.has_focus || state.editing.is_some() {
        return;
    }
    let Some((r, c)) = state.selected else { return };
    let (n_rows, n_cols) = (model.len(), model.columns().len());
    let (mut nr, mut nc) = (r, c);
    let mut copy = false;
    let mut edit = false;
    ui.input(|i| {
        if i.key_pressed(Key::ArrowDown) {
            nr = (r + 1).min(n_rows.saturating_sub(1));
        }
        if i.key_pressed(Key::ArrowUp) {
            nr = r.saturating_sub(1);
        }
        if i.key_pressed(Key::ArrowRight) {
            nc = (c + 1).min(n_cols.saturating_sub(1));
        }
        if i.key_pressed(Key::ArrowLeft) {
            nc = c.saturating_sub(1);
        }
        edit = i.key_pressed(Key::Enter);
        copy = i.events.iter().any(|e| matches!(e, egui::Event::Copy));
    });
    if (nr, nc) != (r, c) {
        state.select(nr, nc);
    }
    if copy {
        ui.ctx().copy_text(copy_text(&model.cell(r, c).0));
    }
    if edit && opts.editable && model.cell(r, c).1 != Mark::Deleted {
        state.start_edit(r, c, model.cell(r, c).0.edit_text());
    }
}

fn cell_editor(ui: &mut Ui, state: &mut GridState, events: &mut Vec<GridEvent>, pal: &Palette) {
    let Some(ed) = state.editing.as_mut() else {
        return;
    };
    let rect = ui.max_rect();
    let resp = ui.put(
        rect,
        TextEdit::singleline(&mut ed.text)
            .font(style::regular(CELL_SIZE))
            .frame(
                egui::Frame::NONE
                    .fill(pal.content)
                    .inner_margin(egui::Margin::symmetric(PAD as i8 - 1, 0)),
            )
            .min_size(rect.size())
            .align(egui::Align2::LEFT_CENTER)
            .desired_width(rect.width()),
    );
    ui.painter().rect_stroke(
        rect.shrink(0.5),
        2.0,
        Stroke::new(1.5, pal.accent),
        StrokeKind::Inside,
    );
    if ed.focus {
        resp.request_focus();
        ed.focus = false;
    } else if !resp.has_focus() {
        // Enter or a click elsewhere keeps the text; Esc cancels.
        if !ui.input(|i| i.key_pressed(Key::Escape)) && ed.text != ed.initial {
            events.push(GridEvent::Commit {
                row: ed.row,
                col: ed.col,
                text: std::mem::take(&mut ed.text),
            });
        }
        state.editing = None;
        state.has_focus = true;
    }
}

fn cell_menu(
    ui: &mut Ui,
    model: &dyn GridModel,
    r: usize,
    c: usize,
    opts: &GridOptions,
    events: &mut Vec<GridEvent>,
) {
    let (shown, mark) = model.cell(r, c);
    if ui.button("Copy value").clicked() {
        ui.ctx().copy_text(copy_text(&shown));
        ui.close();
    }
    if ui.button("Copy row (TSV)").clicked() {
        let values = row_values(model, r);
        ui.ctx()
            .copy_text(export::to_tsv(model.columns(), &[values.as_slice()], false));
        ui.close();
    }
    if ui.button("Copy row (JSON)").clicked() {
        let values = row_values(model, r);
        let json = export::row_json(model.columns(), &values);
        ui.ctx()
            .copy_text(serde_json::to_string_pretty(&json).unwrap_or_default());
        ui.close();
    }
    if opts.filterable && mark != Mark::New && ui.button("Filter by this value").clicked() {
        events.push(GridEvent::FilterBy { row: r, col: c });
        ui.close();
    }
    if opts.editable {
        ui.separator();
        if ui
            .add_enabled(mark != Mark::Deleted, egui::Button::new("Set NULL"))
            .clicked()
        {
            events.push(GridEvent::SetNull { row: r, col: c });
            ui.close();
        }
        let label = if model.cell(r, 0).1 == Mark::Deleted {
            "Undo delete"
        } else {
            "Delete row"
        };
        if ui.button(label).clicked() {
            events.push(GridEvent::DeleteRow(r));
            ui.close();
        }
    }
}

fn copy_text(shown: &Shown) -> String {
    match shown {
        Shown::Val(Value::Null) | Shown::Edit(Edit::Null) => "NULL".into(),
        Shown::Val(v @ Value::Json(_)) => v.display_full(),
        other => other.edit_text(),
    }
}

fn is_number(shown: &Shown) -> bool {
    matches!(shown, Shown::Val(v) if v.is_number())
}

/// True for SQL number types (right-aligned).
pub fn is_numeric_type(t: &str) -> bool {
    let t = t.to_ascii_lowercase();
    [
        "int", "serial", "numeric", "decimal", "float", "double", "real", "number", "money",
    ]
    .iter()
    .any(|k| t.contains(k))
        && !t.contains("interval")
        && !t.contains("point")
}

/// How one cell looks: its staged mark and selection.
struct Look {
    mark: Mark,
    selected: bool,
    row_selected: bool,
    right: bool,
}

fn paint_cell(
    ui: &mut Ui,
    shown: &Shown,
    look: Look,
    pal: &Palette,
    font: &FontId,
    digits: &[f32; 10],
) {
    let rect = ui.max_rect();
    let painter = ui.painter();
    let bg = match look.mark {
        Mark::Edited => Some(pal.edited),
        Mark::New => Some(pal.new_row),
        Mark::Deleted => Some(pal.deleted),
        Mark::Clean => None,
    };
    if let Some(bg) = bg {
        painter.rect_filled(rect, 0.0, bg);
    }
    // Staged cells keep their color on the selected row.
    if look.row_selected && bg.is_none() {
        painter.rect_filled(rect, 0.0, pal.selected_row);
    }
    let width = rect.width() - 2.0 * PAD;
    let dim = |text: &str| one_line(text, style::regular(11.5), pal.text3, width, true);
    let mut job = match shown {
        Shown::Val(Value::Null) | Shown::Edit(Edit::Null) => dim("NULL"),
        Shown::Default => dim("DEFAULT"),
        Shown::Val(Value::Missing) => one_line("—", font.clone(), pal.text3, width, false),
        Shown::Val(v) if look.right && v.is_number() => {
            tabular(&v.display(), font.clone(), pal.text, width, digits)
        }
        Shown::Val(v) => one_line(&v.display(), font.clone(), pal.text, width, false),
        Shown::Edit(Edit::Text(s)) => {
            let text = Value::Text(s.clone()).display();
            if look.right && s.trim().parse::<f64>().is_ok() {
                tabular(&text, font.clone(), pal.text, width, digits)
            } else {
                one_line(&text, font.clone(), pal.text, width, false)
            }
        }
        Shown::Raw(s) => one_line(
            &Value::Text(s.to_string()).display(),
            font.clone(),
            pal.text,
            width,
            false,
        ),
    };
    let plain = matches!(
        shown,
        Shown::Val(v) if !v.is_null() && !v.is_missing()
    ) || matches!(shown, Shown::Edit(Edit::Text(_)));
    if look.mark == Mark::Deleted {
        for s in &mut job.sections {
            s.format.color = pal.text2;
            s.format.strikethrough = Stroke::new(1.0, pal.text2);
        }
    }
    paint_job(ui, rect, job, look.right && plain);
    ui.painter().vline(
        rect.right() - 0.5,
        rect.y_range(),
        Stroke::new(1.0, pal.grid_line),
    );
    if look.selected {
        ui.painter().rect_stroke(
            rect.shrink(0.75),
            2.0,
            Stroke::new(1.5, pal.accent),
            StrokeKind::Inside,
        );
    }
}

fn header_bg(ui: &Ui, rect: Rect, pal: &Palette) {
    ui.painter().rect_filled(rect, 0.0, pal.header_bg);
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, pal.separator),
    );
}

/// How one header cell looks.
struct HeaderLook {
    /// Sort direction: `Some(true)` ascending.
    arrow: Option<bool>,
    right: bool,
    hover: bool,
    key: bool,
}

fn header_cell(ui: &mut Ui, name: &str, look: HeaderLook, pal: &Palette, font: &FontId) {
    let rect = ui.max_rect();
    header_bg(ui, rect, pal);
    if look.hover {
        ui.painter().rect_filled(rect, 0.0, pal.hover);
    }
    let arrow_w = if look.arrow.is_some() { 14.0 } else { 0.0 };
    let key_w = if look.key { 15.0 } else { 0.0 };
    let job = style::one_line(
        name,
        font.clone(),
        pal.text2,
        rect.width() - 2.0 * PAD - arrow_w - key_w,
    );
    let galley = ui.painter().layout_job(job);
    let y = rect.center().y - galley.size().y / 2.0;
    let x = if look.right {
        rect.right() - PAD - arrow_w - galley.size().x
    } else {
        rect.left() + PAD + key_w
    };
    if look.key {
        let kx = if look.right {
            x - 15.0
        } else {
            rect.left() + PAD
        };
        let r = Rect::from_min_size(pos2(kx, rect.center().y - 6.0), vec2(12.0, 12.0));
        icons::paint(ui.painter(), r, Icon::Key, pal.warn);
    }
    let text_right = x + galley.size().x;
    ui.painter()
        .galley(pos2(x, y), galley, Color32::PLACEHOLDER);
    if let Some(asc) = look.arrow {
        let icon = if asc {
            Icon::ChevronUp
        } else {
            Icon::ChevronDown
        };
        let r = Rect::from_center_size(pos2(text_right + 9.0, rect.center().y), vec2(10.0, 10.0));
        icons::paint(ui.painter(), r, icon, pal.accent);
    }
    // A short divider, like a Mac table header.
    ui.painter().vline(
        rect.right() - 0.5,
        (rect.top() + 6.0)..=(rect.bottom() - 6.0),
        Stroke::new(1.0, pal.control_border),
    );
}

/// One line of text, cut with "…" when it is wider than `width`.
fn one_line(text: &str, font: FontId, color: Color32, width: f32, italics: bool) -> LayoutJob {
    let mut job = style::one_line(text, font, color, width);
    if let Some(s) = job.sections.first_mut() {
        s.format.italics = italics;
    }
    job
}

/// A number with all digits in slots of the same width, so columns line up.
fn tabular(text: &str, font: FontId, color: Color32, width: f32, digits: &[f32; 10]) -> LayoutJob {
    let max = digits.iter().copied().fold(0.0, f32::max);
    let pad = |c: char| {
        c.to_digit(10)
            .map(|d| (max - digits[d as usize]) / 2.0)
            .unwrap_or(0.0)
    };
    let chars: Vec<char> = text.chars().collect();
    let mut job = LayoutJob::default();
    for (i, &c) in chars.iter().enumerate() {
        let next = chars.get(i + 1).map(|&n| pad(n)).unwrap_or(0.0);
        let mut buf = [0u8; 4];
        job.append(
            c.encode_utf8(&mut buf),
            0.0,
            TextFormat {
                font_id: font.clone(),
                color,
                extra_letter_spacing: pad(c) + next,
                ..Default::default()
            },
        );
    }
    job.wrap = TextWrapping::truncate_at_width(width.max(1.0));
    job.break_on_newline = false;
    job
}

/// Width of each digit 0–9 in `font`.
fn digit_widths(ui: &Ui, font: &FontId) -> [f32; 10] {
    let mut out = [0.0; 10];
    ui.fonts_mut(|f| {
        for (d, w) in out.iter_mut().enumerate() {
            let text = char::from(b'0' + d as u8).to_string();
            *w = f
                .layout_no_wrap(text, font.clone(), Color32::WHITE)
                .size()
                .x;
        }
    });
    out
}

fn paint_job(ui: &Ui, rect: Rect, job: LayoutJob, right: bool) {
    let galley = ui.painter().layout_job(job);
    let y = rect.center().y - galley.size().y / 2.0;
    let x = if right {
        rect.right() - PAD - galley.size().x
    } else {
        rect.left() + PAD
    };
    ui.painter()
        .galley(egui::pos2(x, y), galley, Color32::PLACEHOLDER);
}

fn row_label_width(ui: &Ui, model: &dyn GridModel, font: &FontId) -> f32 {
    let n = model.len();
    let sample = if n == 0 {
        "0".to_string()
    } else {
        model.row_label(n - 1)
    };
    (text_width(ui, &sample, font) + 2.0 * PAD).max(30.0)
}

fn text_width(ui: &Ui, text: &str, font: &FontId) -> f32 {
    ui.ctx().fonts_mut(|f| {
        f.layout_no_wrap(text.to_string(), font.clone(), Color32::WHITE)
            .size()
            .x
    })
}

/// First widths from the header and the first rows, clamped to 64–360 points.
fn measure(ui: &Ui, model: &dyn GridModel) -> Vec<f32> {
    let font = style::regular(CELL_SIZE);
    let head = style::semibold(12.0);
    let sample = model.len().min(50);
    (0..model.columns().len())
        .map(|c| {
            let mut w = text_width(ui, &model.columns()[c], &head) + 2.0 * PAD + 30.0;
            for r in 0..sample {
                let text = match model.cell(r, c).0 {
                    Shown::Val(v) => v.display(),
                    other => other.edit_text(),
                };
                w = w.max(text_width(ui, &text, &font) + 2.0 * PAD + 4.0);
            }
            w.clamp(64.0, 360.0)
        })
        .collect()
}
