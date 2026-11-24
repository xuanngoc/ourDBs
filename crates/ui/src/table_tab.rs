//! A table tab: browse one table or collection, stage edits, save them.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use eframe::egui::{self, RichText, TextEdit, Ui, vec2};
use ourdb_core::{
    BrowseRequest, Change, Driver, Edit, Env, Kind, ObjectRef, Page, ResultSet, Value, sql,
    value::row_document,
};

use crate::{
    grid::{self, GridEvent, GridModel, GridOptions, GridState, Mark, ResultModel, Shown},
    icons::Icon,
    style::{self, Btn, IconBtn, Segment},
    task::{Reply, Spawner, TabData, err_text, next_req},
    value_panel::{ValueAction, ValuePanel},
};

const PAGE_SIZES: [u32; 4] = [50, 100, 500, 1000];

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Data,
    Structure,
}

/// Local changes that wait for Save. Row numbers index the loaded page.
#[derive(Default)]
pub struct Staged {
    pub edits: BTreeMap<(usize, usize), Edit>,
    pub new_rows: Vec<Vec<Option<Edit>>>,
    pub new_docs: Vec<String>,
    pub replaced: BTreeMap<usize, String>,
    pub deleted: BTreeSet<usize>,
}

impl Staged {
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
            && self.new_rows.is_empty()
            && self.new_docs.is_empty()
            && self.replaced.is_empty()
            && self.deleted.is_empty()
    }
}

/// The page rows, then new rows, then new documents, with staged values on top.
struct TableModel<'a> {
    rs: &'a ResultSet,
    staged: &'a Staged,
    offset: u64,
}

impl GridModel for TableModel<'_> {
    fn columns(&self) -> &[String] {
        &self.rs.columns
    }
    fn types(&self) -> &[String] {
        &self.rs.types
    }
    fn len(&self) -> usize {
        self.rs.rows.len() + self.staged.new_rows.len() + self.staged.new_docs.len()
    }
    fn cell(&self, row: usize, col: usize) -> (Shown<'_>, Mark) {
        let n = self.rs.rows.len();
        if row < n {
            let v = self.rs.rows[row].get(col).unwrap_or(&Value::Missing);
            if self.staged.deleted.contains(&row) {
                (Shown::Val(v), Mark::Deleted)
            } else if let Some(e) = self.staged.edits.get(&(row, col)) {
                (Shown::Edit(e), Mark::Edited)
            } else if self.staged.replaced.contains_key(&row) {
                (Shown::Val(v), Mark::Edited)
            } else {
                (Shown::Val(v), Mark::Clean)
            }
        } else if let Some(new) = self.staged.new_rows.get(row - n) {
            match new.get(col) {
                Some(Some(e)) => (Shown::Edit(e), Mark::New),
                _ => (Shown::Default, Mark::New),
            }
        } else {
            let json = self
                .staged
                .new_docs
                .get(row - n - self.staged.new_rows.len());
            match json {
                Some(j) if col == 0 => (Shown::Raw(j), Mark::New),
                _ => (Shown::Raw(""), Mark::New),
            }
        }
    }
    fn row_label(&self, row: usize) -> String {
        if row < self.rs.rows.len() {
            (self.offset + row as u64 + 1).to_string()
        } else {
            "+".into()
        }
    }
}

struct DocEditor {
    /// The page row to replace, or `None` for a new document.
    row: Option<usize>,
    text: String,
    focus: bool,
}

struct Review {
    changes: Vec<Change>,
    statements: Result<Vec<String>, String>,
    error: Option<String>,
}

pub struct TableTab {
    pub id: u64,
    /// Query text to open in a new query tab (set by the SQL button; the app takes it).
    pub open_query: Option<String>,
    pub conn_id: String,
    pub object: ObjectRef,
    pub kind: Kind,
    pub env: Env,
    driver: Arc<dyn Driver>,
    spawner: Spawner,
    view: View,
    filter: String,
    applied_filter: String,
    sort: Option<(String, bool)>,
    page_size: u32,
    offset: u64,
    page: Option<Page>,
    page_req: Option<u64>,
    error: Option<String>,
    count: Option<u64>,
    count_req: Option<u64>,
    structure: Option<Result<ResultSet, String>>,
    structure_req: Option<u64>,
    apply_req: Option<u64>,
    pub staged: Staged,
    pub grid: GridState,
    structure_grid: GridState,
    value: ValuePanel,
    /// Changes when the page or the staged changes change.
    version: u64,
    doc_editor: Option<DocEditor>,
    review: Option<Review>,
}

impl TableTab {
    pub fn new(
        id: u64,
        conn_id: String,
        object: ObjectRef,
        kind: Kind,
        env: Env,
        driver: Arc<dyn Driver>,
        spawner: Spawner,
    ) -> Self {
        let mut tab = Self {
            open_query: None,
            id,
            conn_id,
            object,
            kind,
            env,
            driver,
            spawner,
            view: View::Data,
            filter: String::new(),
            applied_filter: String::new(),
            sort: None,
            page_size: 100,
            offset: 0,
            page: None,
            page_req: None,
            error: None,
            count: None,
            count_req: None,
            structure: None,
            structure_req: None,
            apply_req: None,
            staged: Staged::default(),
            grid: GridState::default(),
            structure_grid: GridState::default(),
            value: ValuePanel::default(),
            version: 0,
            doc_editor: None,
            review: None,
        };
        tab.reload();
        tab
    }

    pub fn title(&self) -> String {
        if self.staged.is_empty() {
            self.object.name.clone()
        } else {
            format!("{} •", self.object.name)
        }
    }

    pub fn busy(&self) -> bool {
        self.page_req.is_some()
            || self.count_req.is_some()
            || self.structure_req.is_some()
            || self.apply_req.is_some()
    }

    fn request(&self) -> BrowseRequest {
        BrowseRequest {
            object: self.object.clone(),
            filter: self.applied_filter.clone(),
            sort: self.sort.clone(),
            limit: self.page_size,
            offset: self.offset,
        }
    }

    /// Load the current page again.
    pub fn reload(&mut self) {
        let (req, id, tab, driver) = (self.request(), next_req(), self.id, self.driver.clone());
        self.page_req = Some(id);
        self.spawner.spawn(async move {
            let result = driver.browse(&req).await.map_err(err_text);
            Reply::Tab {
                tab,
                req: id,
                data: TabData::Page(result),
            }
        });
    }

    fn load_count(&mut self) {
        let (req, id, tab, driver) = (self.request(), next_req(), self.id, self.driver.clone());
        self.count_req = Some(id);
        self.spawner.spawn(async move {
            let result = driver.count(&req).await.map_err(err_text);
            Reply::Tab {
                tab,
                req: id,
                data: TabData::Count(result),
            }
        });
    }

    fn load_structure(&mut self) {
        let (object, id, tab, driver) = (
            self.object.clone(),
            next_req(),
            self.id,
            self.driver.clone(),
        );
        self.structure_req = Some(id);
        self.spawner.spawn(async move {
            let result = driver.columns(&object).await.map_err(err_text);
            Reply::Tab {
                tab,
                req: id,
                data: TabData::Columns(result),
            }
        });
    }

    pub fn on_reply(&mut self, req: u64, data: TabData) {
        match data {
            TabData::Page(result) if self.page_req == Some(req) => {
                self.page_req = None;
                match result {
                    Ok(page) => {
                        self.page = Some(page);
                        self.error = None;
                        self.version += 1;
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            TabData::Count(result) if self.count_req == Some(req) => {
                self.count_req = None;
                match result {
                    Ok(n) => self.count = Some(n),
                    Err(e) => self.error = Some(e),
                }
            }
            TabData::Columns(result) if self.structure_req == Some(req) => {
                self.structure_req = None;
                self.structure = Some(result.map(|cols| {
                    let names = ["name", "type", "nullable", "key", "default"];
                    ResultSet {
                        columns: names.iter().map(|s| s.to_string()).collect(),
                        types: vec![String::new(); names.len()],
                        rows: cols
                            .into_iter()
                            .map(|c| {
                                vec![
                                    Value::Text(c.name),
                                    Value::Text(c.type_name),
                                    Value::Text(if c.nullable { "YES" } else { "NO" }.into()),
                                    Value::Text(if c.key { "PRI" } else { "" }.into()),
                                    c.default.map(Value::Text).unwrap_or(Value::Null),
                                ]
                            })
                            .collect(),
                        ..Default::default()
                    }
                }));
            }
            TabData::Applied(result) if self.apply_req == Some(req) => {
                self.apply_req = None;
                match result {
                    Ok(_) => {
                        self.staged = Staged::default();
                        self.review = None;
                        self.version += 1;
                        self.reload();
                    }
                    Err(e) => {
                        if let Some(r) = self.review.as_mut() {
                            r.error = Some(e);
                        }
                    }
                }
            }
            _ => {} // An old reply: drop it.
        }
    }

    fn editable(&self) -> bool {
        self.page.as_ref().is_some_and(|p| !p.key.is_empty())
    }

    /// Paging, sort and filter would move rows under staged changes, so they wait.
    fn can_move(&self) -> bool {
        self.staged.is_empty() && self.page_req.is_none()
    }

    fn apply_filter(&mut self) {
        if !self.can_move() {
            return;
        }
        self.applied_filter = self.filter.trim().to_string();
        self.offset = 0;
        self.count = None;
        self.reload();
    }

    // -- staging ------------------------------------------------------------

    /// Stage a new value for one cell.
    pub fn stage(&mut self, row: usize, col: usize, edit: Edit) {
        let Some(page) = &self.page else { return };
        let rows = &page.result.rows;
        if col >= page.result.columns.len() {
            return;
        }
        if row < rows.len() {
            if self.staged.deleted.contains(&row) {
                return;
            }
            let orig = rows[row].get(col).unwrap_or(&Value::Missing);
            let same = match (&edit, orig) {
                (Edit::Null, Value::Null) => true,
                (Edit::Text(t), v) if !v.is_null() && !v.is_missing() => *t == v.edit_text(),
                _ => false,
            };
            if same {
                self.staged.edits.remove(&(row, col));
            } else {
                self.staged.edits.insert((row, col), edit);
                self.staged.replaced.remove(&row);
            }
        } else if let Some(new) = self.staged.new_rows.get_mut(row - rows.len()) {
            new[col] = Some(edit);
        }
        self.version += 1;
    }

    /// Mark a row for delete, or undo that. New rows are removed.
    pub fn toggle_delete(&mut self, row: usize) {
        let Some(page) = &self.page else { return };
        let n = page.result.rows.len();
        if row < n {
            if !self.staged.deleted.remove(&row) {
                self.staged.deleted.insert(row);
            }
        } else if row - n < self.staged.new_rows.len() {
            self.staged.new_rows.remove(row - n);
        } else if row - n - self.staged.new_rows.len() < self.staged.new_docs.len() {
            self.staged
                .new_docs
                .remove(row - n - self.staged.new_rows.len());
        }
        self.version += 1;
    }

    pub fn add_row(&mut self) {
        let Some(page) = &self.page else { return };
        let ncols = page.result.columns.len();
        let row = page.result.rows.len() + self.staged.new_rows.len();
        self.staged.new_rows.push(vec![None; ncols]);
        self.grid.select(row, 0);
        self.version += 1;
    }

    /// Build `Change` values from the staged edits.
    fn changes(&self) -> Result<Vec<Change>, String> {
        let page = self.page.as_ref().ok_or("No data loaded.")?;
        let rs = &page.result;
        let mut key_idx = Vec::new();
        for k in &page.key {
            let i = rs
                .columns
                .iter()
                .position(|c| c == k)
                .ok_or(format!("The key column \"{k}\" is not in the result."))?;
            key_idx.push(i);
        }
        let key_of = |r: usize| -> Vec<(String, Value)> {
            key_idx
                .iter()
                .map(|&i| {
                    (
                        rs.columns[i].clone(),
                        rs.rows[r].get(i).cloned().unwrap_or(Value::Missing),
                    )
                })
                .collect()
        };
        let mut out = Vec::new();
        let mut by_row: BTreeMap<usize, Vec<(String, Value, Edit)>> = BTreeMap::new();
        for (&(r, c), e) in &self.staged.edits {
            if !self.staged.deleted.contains(&r) {
                let orig = rs.rows[r].get(c).cloned().unwrap_or(Value::Missing);
                by_row
                    .entry(r)
                    .or_default()
                    .push((rs.columns[c].clone(), orig, e.clone()));
            }
        }
        for (r, set) in by_row {
            out.push(Change::Update {
                key: key_of(r),
                set,
            });
        }
        for (&r, json) in &self.staged.replaced {
            if !self.staged.deleted.contains(&r) {
                out.push(Change::ReplaceDoc {
                    key: key_of(r),
                    json: json.clone(),
                });
            }
        }
        for row in &self.staged.new_rows {
            let values = rs
                .columns
                .iter()
                .zip(row)
                .filter_map(|(c, e)| e.clone().map(|e| (c.clone(), e)))
                .collect();
            out.push(Change::Insert { values });
        }
        for json in &self.staged.new_docs {
            out.push(Change::InsertDoc { json: json.clone() });
        }
        for &r in &self.staged.deleted {
            out.push(Change::Delete { key: key_of(r) });
        }
        Ok(out)
    }

    pub(crate) fn change_count(&self) -> usize {
        let edited_rows: BTreeSet<usize> = self
            .staged
            .edits
            .keys()
            .map(|(r, _)| *r)
            .filter(|r| !self.staged.deleted.contains(r))
            .collect();
        edited_rows.len()
            + self
                .staged
                .replaced
                .keys()
                .filter(|r| !self.staged.deleted.contains(r))
                .count()
            + self.staged.new_rows.len()
            + self.staged.new_docs.len()
            + self.staged.deleted.len()
    }

    /// Open the review dialog with the statements the driver will run.
    pub fn open_review(&mut self) {
        let review = match self.changes() {
            Ok(changes) => {
                let statements = self
                    .driver
                    .preview(&self.object, &changes)
                    .map_err(err_text);
                Review {
                    changes,
                    statements,
                    error: None,
                }
            }
            Err(e) => Review {
                changes: Vec::new(),
                statements: Err(e),
                error: None,
            },
        };
        self.review = Some(review);
    }

    /// Apply the changes of the open review dialog.
    pub fn apply(&mut self) {
        let Some(review) = &self.review else { return };
        let (changes, object, driver, id, tab) = (
            review.changes.clone(),
            self.object.clone(),
            self.driver.clone(),
            next_req(),
            self.id,
        );
        self.apply_req = Some(id);
        self.spawner.spawn(async move {
            let result = driver.apply(&object, &changes).await.map_err(err_text);
            Reply::Tab {
                tab,
                req: id,
                data: TabData::Applied(result),
            }
        });
    }

    /// A filter that matches the value of one cell.
    fn filter_for(&self, row: usize, col: usize) -> Option<String> {
        let page = self.page.as_ref()?;
        let name = page.result.columns.get(col)?;
        let v = page.result.rows.get(row)?.get(col)?;
        if self.kind.is_document() {
            let key = serde_json::to_string(name).ok()?;
            return Some(match v {
                Value::Missing => format!("{{{key}: {{\"$exists\": false}}}}"),
                other => format!(
                    "{{{key}: {}}}",
                    serde_json::to_string(&other.to_json()).ok()?
                ),
            });
        }
        let col_sql = sql::quote_ident(self.kind, name);
        Some(match v {
            Value::Null | Value::Missing => format!("{col_sql} IS NULL"),
            Value::Int(i) => format!("{col_sql} = {i}"),
            Value::Float(f) => format!("{col_sql} = {f}"),
            Value::Bytes(b) if self.kind != Kind::Postgres => {
                format!("{col_sql} = {}", sql::x_literal(b))
            }
            other => {
                let text = other.edit_text();
                let lit = match self.kind {
                    Kind::MySql => sql::mysql_literal(&text, true),
                    Kind::Sqlite => sql::sqlite_literal(&text),
                    _ => sql::pg_literal(&text, true),
                };
                format!("{col_sql} = {lit}")
            }
        })
    }

    pub fn open_doc_editor(&mut self, row: Option<usize>) {
        let text = match row {
            None => "{\n  \n}".to_string(),
            Some(r) => match (self.staged.replaced.get(&r), &self.page) {
                (Some(t), _) => t.clone(),
                (None, Some(p)) => match p.result.rows.get(r) {
                    Some(values) => {
                        serde_json::to_string_pretty(&row_document(&p.result.columns, values))
                            .unwrap_or_default()
                    }
                    None => return,
                },
                _ => return,
            },
        };
        self.doc_editor = Some(DocEditor {
            row,
            text,
            focus: true,
        });
    }

    // -- UI -----------------------------------------------------------------

    pub fn ui(&mut self, ui: &mut Ui) {
        let pal = style::pal(ui);
        egui::Panel::top(egui::Id::unique(("table-toolbar", self.id)))
            .exact_size(style::TOOLBAR_HEIGHT)
            .frame(
                egui::Frame::NONE
                    .fill(pal.content)
                    .inner_margin(egui::Margin::symmetric(10, 0)),
            )
            .show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom(egui::Id::unique(("table-footer", self.id)))
            .exact_size(style::STATUS_HEIGHT)
            .frame(
                egui::Frame::NONE
                    .fill(pal.chrome)
                    .inner_margin(egui::Margin::symmetric(10, 0)),
            )
            .show(ui, |ui| self.footer(ui, &pal));

        if self.view == View::Structure {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| match &self.structure {
                    Some(Ok(rs)) => {
                        let opts = GridOptions {
                            editable: false,
                            sort: None,
                            sortable: false,
                            filterable: false,
                            keys: &[],
                        };
                        grid::show(
                            ui,
                            ("structure", self.id),
                            &ResultModel(rs),
                            &mut self.structure_grid,
                            &opts,
                        );
                    }
                    Some(Err(e)) => style::center_text(ui, e, pal.error),
                    None => {
                        ui.centered_and_justified(|ui| style::spinner(ui, 18.0));
                    }
                });
            return;
        }

        let editable = self.editable();
        let document = self.kind.is_document();
        egui::Panel::right(egui::Id::unique("value-panel"))
            .default_size(280.0)
            .size_range(220.0..=700.0)
            .frame(
                egui::Frame::NONE
                    .inner_margin(egui::Margin::same(12))
                    .fill(pal.sidebar),
            )
            .show(ui, |ui| {
                let Some(page) = &self.page else { return };
                let model = TableModel {
                    rs: &page.result,
                    staged: &self.staged,
                    offset: self.offset,
                };
                let action = self.value.show(
                    ui,
                    &model,
                    self.grid.selected,
                    self.version,
                    editable,
                    document,
                );
                if let (Some(action), Some((r, c))) = (action, self.grid.selected) {
                    match action {
                        ValueAction::Apply(text) => self.stage(r, c, Edit::Text(text)),
                        ValueAction::SetNull => self.stage(r, c, Edit::Null),
                        ValueAction::EditDocument(row) => self.open_doc_editor(Some(row)),
                    }
                }
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let Some(page) = &self.page else {
                    match &self.error {
                        Some(e) => style::center_text(ui, e, pal.error),
                        None => {
                            ui.centered_and_justified(|ui| style::spinner(ui, 18.0));
                        }
                    }
                    return;
                };
                let model = TableModel {
                    rs: &page.result,
                    staged: &self.staged,
                    offset: self.offset,
                };
                let opts = GridOptions {
                    editable,
                    sort: self.sort.as_ref(),
                    sortable: self.staged.is_empty(),
                    filterable: self.staged.is_empty(),
                    keys: &page.key,
                };
                let events = grid::show(ui, ("table", self.id), &model, &mut self.grid, &opts);
                for ev in events {
                    self.on_grid_event(ev);
                }
            });

        self.doc_editor_ui(ui.ctx());
        self.review_ui(ui.ctx(), &pal);
    }

    fn on_grid_event(&mut self, ev: GridEvent) {
        match ev {
            GridEvent::Sort(col) => {
                if !self.can_move() {
                    return;
                }
                // asc → desc → off
                self.sort = match self.sort.take() {
                    Some((c, true)) if c == col => Some((c, false)),
                    Some((c, false)) if c == col => None,
                    _ => Some((col, true)),
                };
                self.offset = 0;
                self.reload();
            }
            GridEvent::Commit { row, col, text } => self.stage(row, col, Edit::Text(text)),
            GridEvent::SetNull { row, col } => self.stage(row, col, Edit::Null),
            GridEvent::DeleteRow(row) => self.toggle_delete(row),
            GridEvent::FilterBy { row, col } => {
                if let Some(f) = self.filter_for(row, col) {
                    self.filter = f;
                    self.apply_filter();
                }
            }
        }
    }

    /// A first query for this table: the current filter and 100 rows.
    pub(crate) fn starter_query(&self) -> String {
        let filter = self.applied_filter.trim();
        if self.kind.is_document() {
            let name = &self.object.name;
            let simple = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit());
            let coll = if simple {
                format!("db.{name}")
            } else {
                format!(
                    "db.getCollection({})",
                    serde_json::Value::String(name.clone())
                )
            };
            let filter = if filter.is_empty() { "{}" } else { filter };
            format!("{coll}.find({filter}).limit(100)")
        } else {
            let table =
                ourdb_core::sql::qualified(self.kind, &self.object.namespace, &self.object.name);
            let filter = if filter.is_empty() {
                String::new()
            } else {
                format!(" WHERE {filter}")
            };
            format!("SELECT * FROM {table}{filter} LIMIT 100;")
        }
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let mut view = self.view;
            let items = [
                (View::Data, Segment::text("Data").icon(Icon::Table)),
                (
                    View::Structure,
                    Segment::text("Structure").icon(Icon::Columns),
                ),
            ];
            if style::segmented(ui, &mut view, &items, 0.0) {
                self.view = view;
                if view == View::Structure && self.structure.is_none() {
                    self.load_structure();
                }
            }
            let label = if self.kind.is_document() {
                "Query"
            } else {
                "SQL"
            };
            if ui
                .add(Btn::secondary(label).icon(Icon::Terminal))
                .on_hover_text("Open this table in a new query tab")
                .clicked()
            {
                self.open_query = Some(self.starter_query());
            }
            ui.add_space(4.0);
            if self.view == View::Structure {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(IconBtn::new(Icon::Refresh, "Refresh")).clicked() {
                        self.load_structure();
                    }
                });
                return;
            }
            let document = self.kind.is_document();
            let editable = self.editable();
            let n = self.change_count();

            // Right side first, so the filter box can take the rest of the row.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let save = format!("Save ({n})");
                if ui
                    .add_enabled(
                        n > 0 && self.apply_req.is_none(),
                        Btn::primary(&save).min_width(74.0),
                    )
                    .clicked()
                {
                    self.open_review();
                }
                if ui.add_enabled(n > 0, Btn::secondary("Discard")).clicked() {
                    self.staged = Staged::default();
                    self.version += 1;
                }
                style::toolbar_divider(ui);
                ui.spacing_mut().item_spacing.x = 2.0;
                let has_row = self.grid.selected.is_some();
                if ui
                    .add_enabled(
                        editable && has_row,
                        IconBtn::new(Icon::Trash, "Delete row (or undo delete)"),
                    )
                    .clicked()
                    && let Some((r, _)) = self.grid.selected
                {
                    self.toggle_delete(r);
                }
                if document {
                    if ui
                        .add_enabled(editable, IconBtn::new(Icon::Plus, "Add document"))
                        .clicked()
                    {
                        self.open_doc_editor(None);
                    }
                } else if ui
                    .add_enabled(editable, IconBtn::new(Icon::Plus, "Add row"))
                    .clicked()
                {
                    self.add_row();
                }
                let refresh =
                    ui.add_enabled(self.can_move(), IconBtn::new(Icon::Refresh, "Refresh"));
                if refresh
                    .on_disabled_hover_text("Save or discard the changes first.")
                    .clicked()
                {
                    self.count = None;
                    self.reload();
                }
                ui.add_space(6.0);
                ui.spacing_mut().item_spacing.x = 6.0;
                let hint = if document {
                    "Filter: { field: value }"
                } else {
                    "Filter: WHERE clause, e.g. id > 10"
                };
                let resp = style::field(
                    ui,
                    TextEdit::singleline(&mut self.filter)
                        .hint_text(hint)
                        .font(egui::TextStyle::Monospace),
                    Some(Icon::Search),
                    ui.available_width(),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.apply_filter();
                }
            });
        });
    }

    fn footer(&mut self, ui: &mut Ui, pal: &style::Palette) {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let small = |text: String, color| RichText::new(text).size(11.5).color(color);
            if self.view == View::Structure {
                if let Some(Ok(rs)) = &self.structure {
                    ui.label(small(format!("{} columns", rs.rows.len()), pal.text2));
                }
                return;
            }
            let rows = self.page.as_ref().map(|p| p.result.rows.len()).unwrap_or(0) as u64;

            // Left: what is shown and why it cannot be edited.
            let mut parts = Vec::new();
            if let Some(page) = &self.page {
                parts.push(row_count(rows));
                parts.push(fmt_ms(page.result.elapsed));
                if let Some(reason) = &page.read_only_reason {
                    parts.push(format!("Read-only: {reason}"));
                }
            }
            if !self.applied_filter.is_empty() {
                parts.push("Filtered".into());
            }
            let n = self.change_count();
            if n > 0 {
                parts.push(format!("{n} unsaved"));
            }
            let can_move = self.can_move();
            let has_prev = self.offset > 0;
            let has_next = match self.count {
                Some(total) => self.offset + rows < total,
                None => rows == u64::from(self.page_size),
            };

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if self.count_req.is_some() {
                    style::spinner(ui, 12.0);
                } else if let Some(total) = self.count {
                    ui.label(small(row_count(total), pal.text2));
                } else if ui
                    .add(Btn::plain("Count").height(20.0))
                    .on_hover_text("Count all rows")
                    .clicked()
                {
                    self.load_count();
                }
                ui.add_space(6.0);
                ui.add_enabled_ui(can_move, |ui| {
                    ui.spacing_mut().interact_size.y = 20.0;
                    ui.spacing_mut().button_padding = vec2(8.0, 0.0);
                    let before = self.page_size;
                    style::combo(
                        ("page-size", self.id),
                        format!("{} / page", self.page_size),
                        92.0,
                    )
                    .show_ui(ui, |ui| {
                        for s in PAGE_SIZES {
                            ui.selectable_value(&mut self.page_size, s, format!("{s} / page"));
                        }
                    });
                    if before != self.page_size {
                        self.offset = 0;
                        self.reload();
                    }
                });
                ui.add_space(6.0);
                if ui
                    .add_enabled(
                        can_move && has_next,
                        IconBtn::new(Icon::ChevronRight, "Next page").size(20.0),
                    )
                    .clicked()
                {
                    self.offset += u64::from(self.page_size);
                    self.reload();
                }
                let range = if rows == 0 {
                    "No rows".to_string()
                } else {
                    format!(
                        "{}–{}",
                        group_digits(self.offset + 1),
                        group_digits(self.offset + rows)
                    )
                };
                ui.label(small(range, pal.text));
                if ui
                    .add_enabled(
                        can_move && has_prev,
                        IconBtn::new(Icon::ChevronLeft, "Previous page").size(20.0),
                    )
                    .clicked()
                {
                    self.offset = self.offset.saturating_sub(u64::from(self.page_size));
                    self.reload();
                }
                ui.add_space(8.0);
                // Left side fills the rest.
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if let Some(e) = &self.error {
                        ui.add(egui::Label::new(small(e.clone(), pal.error)).truncate())
                            .on_hover_text(e);
                    } else {
                        ui.add(egui::Label::new(small(parts.join("  ·  "), pal.text2)).truncate());
                    }
                    if self.busy() {
                        style::spinner(ui, 11.0);
                    }
                });
            });
        });
    }

    fn doc_editor_ui(&mut self, ctx: &egui::Context) {
        let Some(ed) = &mut self.doc_editor else {
            return;
        };
        let mut close = false;
        let mut stage = false;
        let title = if ed.row.is_some() {
            "Edit document"
        } else {
            "New document"
        };
        let pal = style::palette(ctx.global_style().visuals.dark_mode);
        style::modal(egui::Id::unique(("doc-editor", self.id)), ctx).show(ctx, |ui| {
            ui.set_width(580.0);
            style::dialog_title(ui, title);
            ui.add_space(2.0);
            ui.label(
                RichText::new(
                    "Shell JSON: unquoted keys, ObjectId(\"…\"), ISODate(\"…\") and NumberLong(…) work.",
                )
                .small()
                .color(pal.text2),
            );
            ui.add_space(10.0);
            let out = style::editor_frame(&pal, pal.code_bg).show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(420.0)
                    .show(ui, |ui| {
                        let resp = ui.add(
                            TextEdit::multiline(&mut ed.text)
                                .code_editor()
                                .font(style::mono(12.0))
                                .frame(egui::Frame::NONE.inner_margin(egui::Margin::same(10)))
                                .desired_width(f32::INFINITY)
                                .desired_rows(18),
                        );
                        if std::mem::take(&mut ed.focus) {
                            resp.request_focus();
                        }
                        resp.has_focus()
                    })
                    .inner
            });
            if out.inner {
                style::focus_ring(ui.painter(), out.response.rect, &pal);
            }
            ui.add_space(16.0);
            style::footer(ui, |ui| {
                if ui.add(Btn::primary("Stage").min_width(72.0)).clicked() {
                    stage = true;
                }
                if ui.add(Btn::secondary("Cancel")).clicked() {
                    close = true;
                }
            });
        });
        if stage {
            let ed = self.doc_editor.take().expect("editor is open");
            match ed.row {
                Some(r) => {
                    self.staged.edits.retain(|(row, _), _| *row != r);
                    self.staged.replaced.insert(r, ed.text);
                }
                None => self.staged.new_docs.push(ed.text),
            }
            self.version += 1;
        } else if close {
            self.doc_editor = None;
        }
    }

    fn review_ui(&mut self, ctx: &egui::Context, pal: &style::Palette) {
        let Some(review) = &self.review else { return };
        let mut cancel = false;
        let mut apply = false;
        let applying = self.apply_req.is_some();
        let prod = self.env == Env::Prod;
        style::modal(egui::Id::unique(("review", self.id)), ctx).show(ctx, |ui| {
            ui.set_width(640.0);
            ui.horizontal(|ui| {
                style::dialog_title(ui, &format!("Save changes to {}", self.object.name));
                if prod {
                    ui.add_space(2.0);
                    style::pill(ui, "PRODUCTION", egui::Color32::WHITE, pal.danger);
                }
            });
            ui.add_space(4.0);
            match &review.statements {
                Ok(stmts) => {
                    let what = if self.kind.is_document() {
                        "commands"
                    } else {
                        "statements"
                    };
                    let how = if self.kind.is_document() {
                        "in order"
                    } else {
                        "in one transaction"
                    };
                    ui.label(
                        RichText::new(format!("OurDB will run {} {what} {how}:", stmts.len()))
                            .color(pal.text2),
                    );
                    ui.add_space(10.0);
                    let text = stmts
                        .iter()
                        .map(|s| format!("{s};"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    style::editor_frame(pal, pal.code_bg).show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(360.0)
                            .show(ui, |ui| {
                                ui.add(
                                    TextEdit::multiline(&mut text.as_str())
                                        .code_editor()
                                        .font(style::mono(12.0))
                                        .frame(
                                            egui::Frame::NONE
                                                .inner_margin(egui::Margin::symmetric(12, 10)),
                                        )
                                        .desired_width(f32::INFINITY)
                                        .desired_rows(3),
                                );
                            });
                    });
                }
                Err(e) => style::note(ui, e, pal.error),
            }
            if let Some(e) = &review.error {
                ui.add_space(8.0);
                style::note(ui, e, pal.error);
            }
            ui.add_space(16.0);
            style::footer(ui, |ui| {
                let ok = review.statements.is_ok() && !applying;
                let btn = if prod {
                    Btn::danger("Apply")
                } else {
                    Btn::primary("Apply")
                };
                if ui.add_enabled(ok, btn.min_width(72.0)).clicked() {
                    apply = true;
                }
                if ui
                    .add_enabled(!applying, Btn::secondary("Cancel"))
                    .clicked()
                {
                    cancel = true;
                }
                if applying {
                    style::spinner(ui, 14.0);
                }
            });
        });
        if apply {
            if let Some(r) = self.review.as_mut() {
                r.error = None;
            }
            self.apply();
        } else if cancel {
            self.review = None;
        }
    }
}

/// "12 ms", "1.20 s".
pub fn fmt_ms(d: std::time::Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 1000.0 {
        format!("{ms:.0} ms")
    } else {
        format!("{:.2} s", ms / 1000.0)
    }
}

/// "1 row", "1,234 rows".
pub fn row_count(n: u64) -> String {
    if n == 1 {
        "1 row".into()
    } else {
        format!("{} rows", group_digits(n))
    }
}

/// 1234567 → "1,234,567".
pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}
