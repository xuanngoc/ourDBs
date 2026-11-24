//! The app: connections, tabs, dialogs and the reply loop.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, mpsc::Receiver},
};

use eframe::egui::{
    self, Color32, Key, KeyboardShortcut, Modifiers, Rect, RichText, Sense, Stroke, StrokeKind, Ui,
    ViewportCommand, pos2, vec2,
};
use ourdb_core::{Connector, Driver, Env, Kind, ObjectInfo, ObjectRef, tunnel::Tunnel};

use crate::{
    conn_dialog::{ConnDialog, DialogAction, TestState},
    debug::{Debug, Step},
    icons::{self, Icon},
    query_tab::QueryTab,
    sidebar::{self, SideAction},
    store::Store,
    style::{self, Btn, IconBtn},
    table_tab::TableTab,
    task::{Reply, Spawner, err_text, next_req},
};

pub struct NsNode {
    pub name: String,
    pub open: bool,
    pub loading: bool,
    pub objects: Option<Result<Vec<ObjectInfo>, String>>,
}

/// An open connection with its sidebar tree.
pub struct Live {
    /// The request number of the connect: replies for older sessions are dropped.
    pub session: u64,
    pub driver: Arc<dyn Driver>,
    _tunnel: Option<Tunnel>,
    /// PostgreSQL: the current database and the list for the picker.
    pub database: String,
    pub databases: Vec<String>,
    pub namespaces: Option<Result<Vec<NsNode>, String>>,
    pub filter: String,
}

impl Live {
    fn busy(&self) -> bool {
        match &self.namespaces {
            None => true,
            Some(Ok(nodes)) => nodes.iter().any(|n| n.loading),
            Some(Err(_)) => false,
        }
    }
}

pub enum ConnState {
    Connecting(u64),
    Failed(String),
    Live(Live),
}

pub enum Tab {
    Table(Box<TableTab>),
    Query(Box<QueryTab>),
}

impl Tab {
    fn id(&self) -> u64 {
        match self {
            Tab::Table(t) => t.id,
            Tab::Query(t) => t.id,
        }
    }
    fn conn_id(&self) -> &str {
        match self {
            Tab::Table(t) => &t.conn_id,
            Tab::Query(t) => &t.conn_id,
        }
    }
    fn env(&self) -> Env {
        match self {
            Tab::Table(t) => t.env,
            Tab::Query(t) => t.env,
        }
    }
    fn title(&self) -> String {
        match self {
            Tab::Table(t) => t.title(),
            Tab::Query(t) => t.title.clone(),
        }
    }
    fn busy(&self) -> bool {
        match self {
            Tab::Table(t) => t.busy(),
            Tab::Query(t) => t.busy(),
        }
    }
}

pub struct App {
    rt: Option<tokio::runtime::Runtime>,
    spawner: Spawner,
    rx: Receiver<Reply>,
    connector: Connector,
    store: Store,
    states: HashMap<String, ConnState>,
    expanded: HashSet<String>,
    /// PostgreSQL database chosen in the picker (not saved).
    db_override: HashMap<String, String>,
    tabs: Vec<Tab>,
    active: Option<u64>,
    current_conn: Option<String>,
    next_tab: u64,
    query_count: u32,
    dialog: Option<ConnDialog>,
    delete_confirm: Option<String>,
    /// A table tab with staged changes that the user wants to close.
    close_confirm: Option<u64>,
    /// A message at the bottom of the sidebar: (text, is error).
    notice: Option<(String, bool)>,
    debug: Debug,
    /// The app icon for the empty state.
    icon: Option<egui::TextureHandle>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, connector: Connector) -> Self {
        style::setup(&cc.egui_ctx);
        match std::env::var("OURDB_THEME").as_deref() {
            Ok("dark") => cc.egui_ctx.set_theme(egui::Theme::Dark),
            Ok("light") => cc.egui_ctx.set_theme(egui::Theme::Light),
            _ => {}
        }
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("ourdb-worker")
            .build()
            .expect("tokio runtime");
        let (tx, rx) = std::sync::mpsc::channel();
        let spawner = Spawner {
            rt: rt.handle().clone(),
            tx,
            ctx: cc.egui_ctx.clone(),
        };
        Self {
            rt: Some(rt),
            spawner,
            rx,
            connector,
            store: Store::load(),
            states: HashMap::new(),
            expanded: HashSet::new(),
            db_override: HashMap::new(),
            tabs: Vec::new(),
            active: None,
            current_conn: None,
            next_tab: 1,
            query_count: 0,
            dialog: None,
            delete_confirm: None,
            close_confirm: None,
            notice: None,
            debug: Debug::from_env(),
            icon: style::load_app_icon(&cc.egui_ctx),
        }
    }

    fn busy(&self) -> bool {
        self.states.values().any(|s| match s {
            ConnState::Connecting(_) => true,
            ConnState::Live(l) => l.busy(),
            ConnState::Failed(_) => false,
        }) || self.tabs.iter().any(Tab::busy)
            || self
                .dialog
                .as_ref()
                .is_some_and(|d| matches!(d.test, TestState::Running(_)))
    }

    fn conn_name(&self, id: &str) -> String {
        self.store
            .get(id)
            .map(|c| c.name.clone())
            .unwrap_or_default()
    }

    fn live(&self, id: &str) -> Option<&Live> {
        match self.states.get(id) {
            Some(ConnState::Live(l)) => Some(l),
            _ => None,
        }
    }

    fn active_tab(&mut self) -> Option<&mut Tab> {
        let id = self.active?;
        self.tabs.iter_mut().find(|t| t.id() == id)
    }

    // -- replies ------------------------------------------------------------

    fn poll(&mut self) {
        while let Ok(reply) = self.rx.try_recv() {
            self.handle(reply);
        }
    }

    fn handle(&mut self, reply: Reply) {
        match reply {
            Reply::Connected { conn, req, result } => {
                if !matches!(self.states.get(&conn), Some(ConnState::Connecting(r)) if *r == req) {
                    // An old connect: close what it opened.
                    if let Ok(s) = result {
                        self.spawner.detach(async move { s.driver.close().await });
                    }
                    return;
                }
                match result {
                    Ok(session) => self.on_connected(conn, req, session),
                    Err(e) => {
                        self.notice = Some((format!("{}: {e}", self.conn_name(&conn)), true));
                        self.states.insert(conn, ConnState::Failed(e));
                    }
                }
            }
            Reply::Tested { req, result } => {
                if let Some(d) = self.dialog.as_mut()
                    && matches!(d.test, TestState::Running(r) if r == req)
                {
                    d.test = match result {
                        Ok(()) => TestState::Ok,
                        Err(e) => TestState::Failed(e),
                    };
                }
            }
            Reply::Databases {
                conn,
                session,
                result,
            } => {
                if let Some(ConnState::Live(l)) = self.states.get_mut(&conn)
                    && l.session == session
                {
                    l.databases = result.unwrap_or_default();
                }
            }
            Reply::Namespaces {
                conn,
                session,
                result,
            } => self.on_namespaces(conn, session, result),
            Reply::Objects {
                conn,
                session,
                ns,
                result,
            } => {
                if let Some(ConnState::Live(l)) = self.states.get_mut(&conn)
                    && l.session == session
                    && let Some(Ok(nodes)) = &mut l.namespaces
                    && let Some(node) = nodes.iter_mut().find(|n| n.name == ns)
                {
                    node.loading = false;
                    node.objects = Some(result);
                }
            }
            Reply::Tab { tab, req, data } => match self.tabs.iter_mut().find(|t| t.id() == tab) {
                Some(Tab::Table(t)) => t.on_reply(req, data),
                Some(Tab::Query(t)) => t.on_reply(req, data),
                None => {}
            },
        }
    }

    fn on_connected(&mut self, conn: String, req: u64, session: ourdb_core::Session) {
        let driver = session.driver.clone();
        let cfg = self.store.get(&conn).cloned().unwrap_or_default();
        let database = driver
            .current_database()
            .or_else(|| self.db_override.get(&conn).cloned())
            .unwrap_or_else(|| cfg.database.clone());
        self.states.insert(
            conn.clone(),
            ConnState::Live(Live {
                session: req,
                driver: driver.clone(),
                _tunnel: session.tunnel,
                database,
                databases: Vec::new(),
                namespaces: None,
                filter: String::new(),
            }),
        );
        self.expanded.insert(conn.clone());
        self.current_conn = Some(conn.clone());
        for t in &mut self.tabs {
            if let Tab::Query(q) = t
                && q.conn_id == conn
            {
                q.driver = Some(driver.clone());
            }
        }
        let (c, d) = (conn.clone(), driver.clone());
        self.spawner.spawn(async move {
            let result = d.namespaces().await.map_err(err_text);
            Reply::Namespaces {
                conn: c,
                session: req,
                result,
            }
        });
        if driver.kind() == Kind::Postgres {
            self.spawner.spawn(async move {
                let result = driver.databases().await.map_err(err_text);
                Reply::Databases {
                    conn,
                    session: req,
                    result,
                }
            });
        }
    }

    fn on_namespaces(&mut self, conn: String, session: u64, result: Result<Vec<String>, String>) {
        let Some(ConnState::Live(l)) = self.states.get_mut(&conn) else {
            return;
        };
        if l.session != session {
            return;
        }
        let default = l.driver.default_namespace();
        // Keep open/closed state across a refresh.
        let was_open: HashSet<String> = match &l.namespaces {
            Some(Ok(nodes)) => nodes
                .iter()
                .filter(|n| n.open)
                .map(|n| n.name.clone())
                .collect(),
            _ => default.iter().cloned().collect(),
        };
        let names = match result {
            Ok(n) => n,
            Err(e) => {
                l.namespaces = Some(Err(e));
                return;
            }
        };
        // A single namespace always opens.
        let single = names.len() == 1;
        let nodes = names
            .into_iter()
            .map(|name| NsNode {
                open: single || was_open.contains(&name),
                name,
                loading: false,
                objects: None,
            })
            .collect::<Vec<_>>();
        l.namespaces = Some(Ok(nodes));
        let open: Vec<String> = match &l.namespaces {
            Some(Ok(nodes)) => nodes
                .iter()
                .filter(|n| n.open)
                .map(|n| n.name.clone())
                .collect(),
            _ => Vec::new(),
        };
        for ns in open {
            self.load_objects(&conn, &ns);
        }
        // Query tabs show the namespace list in their picker.
        let list = self.namespace_names(&conn);
        for t in &mut self.tabs {
            if let Tab::Query(q) = t
                && q.conn_id == conn
            {
                q.namespaces = list.clone();
            }
        }
    }

    fn namespace_names(&self, conn: &str) -> Vec<String> {
        match self.live(conn).and_then(|l| l.namespaces.as_ref()) {
            Some(Ok(nodes)) => nodes.iter().map(|n| n.name.clone()).collect(),
            _ => Vec::new(),
        }
    }

    fn load_objects(&mut self, conn: &str, ns: &str) {
        let Some(ConnState::Live(l)) = self.states.get_mut(conn) else {
            return;
        };
        let Some(Ok(nodes)) = &mut l.namespaces else {
            return;
        };
        let Some(node) = nodes.iter_mut().find(|n| n.name == ns) else {
            return;
        };
        node.loading = true;
        let (driver, session, conn, ns) = (
            l.driver.clone(),
            l.session,
            conn.to_string(),
            ns.to_string(),
        );
        self.spawner.spawn(async move {
            let result = driver.objects(&ns).await.map_err(err_text);
            Reply::Objects {
                conn,
                session,
                ns,
                result,
            }
        });
    }

    // -- connections ----------------------------------------------------------

    fn connect(&mut self, id: &str) {
        let Some(mut cfg) = self.store.get(id).cloned() else {
            return;
        };
        if let Some(db) = self.db_override.get(id) {
            cfg.database = db.clone();
        }
        match self.store.password(id) {
            Ok(p) => cfg.password = p,
            Err(e) => self.notice = Some((e, true)),
        }
        let req = next_req();
        self.states
            .insert(id.to_string(), ConnState::Connecting(req));
        let (connector, conn) = (self.connector.clone(), id.to_string());
        self.spawner.spawn(async move {
            let result = connector(cfg).await.map_err(err_text);
            Reply::Connected { conn, req, result }
        });
    }

    /// Close the session. Table tabs close; query tabs stay without a driver.
    fn disconnect(&mut self, id: &str) {
        if let Some(ConnState::Live(l)) = self.states.remove(id) {
            let driver = l.driver.clone();
            self.spawner.detach(async move { driver.close().await });
            drop(l); // Drops the tunnel too.
        }
        self.tabs
            .retain(|t| !matches!(t, Tab::Table(tt) if tt.conn_id == id));
        for t in &mut self.tabs {
            if let Tab::Query(q) = t
                && q.conn_id == id
            {
                q.driver = None;
            }
        }
        if self
            .active
            .is_some_and(|a| !self.tabs.iter().any(|t| t.id() == a))
        {
            self.active = self.tabs.last().map(Tab::id);
        }
    }

    fn open_table(&mut self, conn: &str, object: ObjectRef) {
        if let Some(t) = self
            .tabs
            .iter()
            .find(|t| matches!(t, Tab::Table(tt) if tt.conn_id == conn && tt.object == object))
        {
            self.active = Some(t.id());
            return;
        }
        let Some(driver) = self.live(conn).map(|l| l.driver.clone()) else {
            return;
        };
        let Some((kind, env)) = self.store.get(conn).map(|c| (c.kind, c.env)) else {
            return;
        };
        let id = self.next_tab;
        self.next_tab += 1;
        let tab = TableTab::new(
            id,
            conn.to_string(),
            object,
            kind,
            env,
            driver,
            self.spawner.clone(),
        );
        self.tabs.push(Tab::Table(Box::new(tab)));
        self.active = Some(id);
        self.current_conn = Some(conn.to_string());
    }

    fn new_query(&mut self, conn: &str) -> Option<&mut QueryTab> {
        let live = self.live(conn)?;
        let cfg = self.store.get(conn)?;
        let driver = live.driver.clone();
        let namespace = driver.default_namespace();
        let namespaces = self.namespace_names(conn);
        let id = self.next_tab;
        self.next_tab += 1;
        self.query_count += 1;
        let title = format!("Query {}", self.query_count);
        let tab = QueryTab::new(
            id,
            conn.to_string(),
            cfg.name.clone(),
            cfg.kind,
            cfg.env,
            driver,
            self.spawner.clone(),
            title,
            namespace,
            namespaces,
        );
        self.tabs.push(Tab::Query(Box::new(tab)));
        self.active = Some(id);
        self.current_conn = Some(conn.to_string());
        match self.tabs.last_mut() {
            Some(Tab::Query(q)) => Some(q),
            _ => None,
        }
    }

    /// The connection a new query tab should use: the active tab's
    /// connection, else the last one used, else any open connection.
    fn query_target(&self) -> Option<String> {
        self.active
            .and_then(|a| self.tabs.iter().find(|t| t.id() == a))
            .map(|t| t.conn_id().to_string())
            .or_else(|| self.current_conn.clone())
            .filter(|c| self.live(c).is_some())
            .or_else(|| {
                self.store
                    .connections
                    .iter()
                    .find(|c| self.live(&c.id).is_some())
                    .map(|c| c.id.clone())
            })
    }

    /// Open a query tab for the current connection (⌘T and the + button).
    fn new_query_here(&mut self) {
        match self.query_target() {
            Some(c) => {
                self.new_query(&c);
            }
            None => {
                self.notice = Some((
                    "Connect to a database first, then open a query.".into(),
                    false,
                ))
            }
        }
    }

    /// Close a tab, but ask first when it has staged changes.
    fn request_close(&mut self, id: u64) {
        let staged = self
            .tabs
            .iter()
            .any(|t| matches!(t, Tab::Table(tt) if tt.id == id && tt.change_count() > 0));
        if staged {
            self.close_confirm = Some(id);
        } else {
            self.close_tab(id);
        }
    }

    fn close_confirm_ui(&mut self, ctx: &egui::Context) {
        let Some(id) = self.close_confirm else {
            return;
        };
        let mut answer = None;
        let pal = palette_of(ctx);
        style::modal(egui::Id::unique("close-staged-tab"), ctx).show(ctx, |ui| {
            ui.set_width(400.0);
            style::alert_header(ui, Icon::Warning, pal.warn, "Discard changes?");
            ui.add_space(6.0);
            style::note(
                ui,
                "This tab has changes that are not saved. Close it and discard them?",
                pal.text2,
            );
            ui.add_space(18.0);
            style::footer(ui, |ui| {
                if ui.add(Btn::danger("Discard and close")).clicked() {
                    answer = Some(true);
                }
                if ui.add(Btn::secondary("Cancel")).clicked() {
                    answer = Some(false);
                }
            });
        });
        if let Some(discard) = answer {
            self.close_confirm = None;
            if discard {
                self.close_tab(id);
            }
        }
    }

    fn close_tab(&mut self, id: u64) {
        let Some(pos) = self.tabs.iter().position(|t| t.id() == id) else {
            return;
        };
        if let Tab::Query(q) = &mut self.tabs[pos] {
            q.stop();
        }
        self.tabs.remove(pos);
        if self.active == Some(id) {
            self.active = self
                .tabs
                .get(pos.min(self.tabs.len().saturating_sub(1)))
                .map(Tab::id);
        }
    }

    fn on_side_action(&mut self, action: SideAction) {
        match action {
            SideAction::Connect(id) => self.connect(&id),
            SideAction::Disconnect(id) => self.disconnect(&id),
            SideAction::NewQuery(id) => {
                self.new_query(&id);
            }
            SideAction::Edit(id) => self.open_edit_dialog(&id),
            SideAction::Duplicate(id) => {
                if let Some(mut cfg) = self.store.get(&id).cloned() {
                    let password = self.store.password(&id).unwrap_or_default();
                    cfg.id = uuid::Uuid::new_v4().to_string();
                    cfg.name = format!("{} copy", cfg.name);
                    self.dialog = Some(ConnDialog::edit(cfg, password, true));
                }
            }
            SideAction::Delete(id) => self.delete_confirm = Some(id),
            SideAction::NewConnection => self.dialog = Some(ConnDialog::new_connection()),
            SideAction::Open(conn, object) => self.open_table(&conn, object),
            SideAction::LoadObjects(conn, ns) => self.load_objects(&conn, &ns),
            SideAction::Refresh(id) => {
                if let Some(ConnState::Live(l)) = self.states.get_mut(&id) {
                    let (driver, session, conn) = (l.driver.clone(), l.session, id.clone());
                    // Keep the old tree until the new list comes.
                    self.spawner.spawn(async move {
                        let result = driver.namespaces().await.map_err(err_text);
                        Reply::Namespaces {
                            conn,
                            session,
                            result,
                        }
                    });
                }
            }
            SideAction::SwitchDb(id, db) => {
                self.db_override.insert(id.clone(), db);
                self.disconnect(&id);
                self.connect(&id);
            }
        }
    }

    fn open_edit_dialog(&mut self, id: &str) {
        let Some(cfg) = self.store.get(id).cloned() else {
            return;
        };
        let password = match self.store.password(id) {
            Ok(p) => p,
            Err(e) => {
                self.notice = Some((e, true));
                String::new()
            }
        };
        self.dialog = Some(ConnDialog::edit(cfg, password, false));
    }

    // -- dialogs --------------------------------------------------------------

    fn dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        match dialog.show(ctx) {
            DialogAction::None => {}
            DialogAction::Cancel => self.dialog = None,
            DialogAction::Test => match dialog.result() {
                Ok(cfg) => {
                    let req = next_req();
                    dialog.test = TestState::Running(req);
                    let connector = self.connector.clone();
                    self.spawner.spawn(async move {
                        let result = match connector(cfg).await {
                            Ok(s) => {
                                s.driver.close().await;
                                Ok(())
                            }
                            Err(e) => Err(err_text(e)),
                        };
                        Reply::Tested { req, result }
                    });
                }
                Err(e) => dialog.test = TestState::Failed(e),
            },
            DialogAction::Save => match dialog.result() {
                Ok(cfg) => {
                    let password = dialog.password_to_save().map(str::to_string);
                    let id = cfg.id.clone();
                    let mut saved = cfg.clone();
                    saved.password = String::new();
                    self.store.upsert(saved);
                    if let Err(e) = self.store.save_connections() {
                        if let Some(d) = self.dialog.as_mut() {
                            d.error = Some(e);
                        }
                        return;
                    }
                    if let Some(p) = password
                        && let Err(e) = self.store.set_password(&id, &p)
                    {
                        self.notice = Some((e, true));
                    }
                    // Open tabs follow the new name and env tag.
                    for t in &mut self.tabs {
                        match t {
                            Tab::Table(tt) if tt.conn_id == id => tt.env = cfg.env,
                            Tab::Query(q) if q.conn_id == id => {
                                q.env = cfg.env;
                                q.conn_name = cfg.name.clone();
                            }
                            _ => {}
                        }
                    }
                    self.dialog = None;
                }
                Err(e) => dialog.error = Some(e),
            },
        }
    }

    fn delete_ui(&mut self, ctx: &egui::Context) {
        let Some(id) = self.delete_confirm.clone() else {
            return;
        };
        let name = self.conn_name(&id);
        let mut close = false;
        let mut delete = false;
        let pal = palette_of(ctx);
        style::modal(egui::Id::unique("delete-conn"), ctx).show(ctx, |ui| {
            ui.set_width(400.0);
            style::alert_header(ui, Icon::Trash, pal.danger, "Delete connection");
            ui.add_space(6.0);
            style::note(
                ui,
                &format!("Delete \"{name}\" and its saved password and history?"),
                pal.text2,
            );
            ui.add_space(18.0);
            style::footer(ui, |ui| {
                if ui.add(Btn::danger("Delete")).clicked() {
                    delete = true;
                }
                if ui.add(Btn::secondary("Cancel")).clicked() {
                    close = true;
                }
            });
        });
        if delete {
            self.disconnect(&id);
            self.tabs.retain(|t| t.conn_id() != id);
            if self
                .active
                .is_some_and(|a| !self.tabs.iter().any(|t| t.id() == a))
            {
                self.active = self.tabs.last().map(Tab::id);
            }
            self.store.remove(&id);
            if let Err(e) = self.store.save_connections() {
                self.notice = Some((e, true));
            }
            self.delete_confirm = None;
        } else if close {
            self.delete_confirm = None;
        }
    }

    // -- layout ---------------------------------------------------------------

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let new_query = ctx
            .input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::T)));
        let close = ctx
            .input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::W)));
        if new_query {
            self.new_query_here();
        }
        if close && let Some(id) = self.active {
            self.request_close(id);
        }
    }

    /// The top bar of the main area: the window drag area and the tab strip.
    fn top_bar(&mut self, ui: &mut Ui) {
        let pal = style::pal(ui);
        let bar = ui.max_rect();
        drag_area(ui, bar, "main-bar");
        let can_query = self.query_target().is_some();
        if self.tabs.is_empty() {
            if can_query {
                ui.horizontal_centered(|ui| {
                    ui.add_space(8.0);
                    if ui.add(IconBtn::new(Icon::Plus, "New query (⌘T)")).clicked() {
                        self.new_query_here();
                    }
                });
            }
            return;
        }
        let mut new_query = false;
        let mut activate = None;
        let mut close = None;
        let chip_h = 28.0;
        let n = self.tabs.len() as f32;
        // Tabs share the width when there are many; past that, the strip scrolls.
        let fit = (bar.width() - 16.0 - 4.0 * (n - 1.0)) / n;
        let scroll = egui::ScrollArea::horizontal()
            .id_salt("tab-strip")
            .auto_shrink([false, false])
            .scroll_source(
                egui::scroll_area::ScrollSource::MOUSE_WHEEL
                    | egui::scroll_area::ScrollSource::SCROLL_BAR,
            );
        scroll.show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.add_space(8.0);
                for tab in &self.tabs {
                    let is_active = self.active == Some(tab.id());
                    let title = tab.title();
                    let font = if is_active {
                        style::semibold(12.5)
                    } else {
                        style::regular(12.5)
                    };
                    let tw = style::galley(ui, &title, font.clone(), pal.text).size().x;
                    let width = (tw + 58.0).clamp(110.0, 220.0).min(fit.max(96.0));
                    let (rect, resp) = ui.allocate_exact_size(vec2(width, chip_h), Sense::click());
                    let painter = ui.painter();
                    if is_active {
                        if !pal.dark {
                            painter.rect_filled(
                                rect.translate(vec2(0.0, 0.5)),
                                style::RADIUS,
                                Color32::from_black_alpha(14),
                            );
                        }
                        painter.rect_filled(rect, style::RADIUS, pal.content);
                        painter.rect_stroke(
                            rect,
                            style::RADIUS,
                            Stroke::new(1.0, pal.separator),
                            StrokeKind::Inside,
                        );
                    } else if resp.hovered() {
                        painter.rect_filled(rect, style::RADIUS, pal.hover);
                    }
                    if let Some(c) = style::env_color(tab.env()) {
                        let line = Rect::from_min_max(
                            pos2(rect.left() + 8.0, rect.bottom() - 2.0),
                            pos2(rect.right() - 8.0, rect.bottom()),
                        );
                        let c = if is_active { c } else { c.gamma_multiply(0.5) };
                        painter.rect_filled(line, 1.0, c);
                    }
                    let icon = match tab {
                        Tab::Table(t) if t.kind.is_document() => Icon::Collection,
                        Tab::Table(_) => Icon::Table,
                        Tab::Query(_) => Icon::Terminal,
                    };
                    let ir = Rect::from_center_size(
                        pos2(rect.left() + 17.0, rect.center().y),
                        vec2(14.0, 14.0),
                    );
                    let fg = if is_active { pal.text } else { pal.text2 };
                    icons::paint(
                        painter,
                        ir,
                        icon,
                        if is_active { pal.accent } else { pal.text2 },
                    );
                    let text_rect = Rect::from_min_max(
                        pos2(rect.left() + 30.0, rect.top()),
                        pos2(rect.right() - 26.0, rect.bottom()),
                    );
                    style::paint_clipped(ui, text_rect, &title, font, fg);

                    let x_rect = Rect::from_center_size(
                        pos2(rect.right() - 14.0, rect.center().y),
                        vec2(18.0, 18.0),
                    );
                    let x_resp = ui.interact(
                        x_rect,
                        egui::Id::unique(("close-tab", tab.id())),
                        Sense::click(),
                    );
                    if resp.hovered() || x_resp.hovered() || is_active {
                        if x_resp.hovered() {
                            ui.painter().rect_filled(x_rect, 4.0, pal.hover);
                        }
                        icons::paint(
                            ui.painter(),
                            x_rect.shrink(4.0),
                            Icon::Close,
                            if x_resp.hovered() {
                                pal.text
                            } else {
                                pal.text2
                            },
                        );
                    }
                    let hover = match tab {
                        Tab::Table(t) => format!(
                            "{} · {}.{}",
                            self.conn_name(&t.conn_id),
                            t.object.namespace,
                            t.object.name
                        ),
                        Tab::Query(q) => q.conn_name.clone(),
                    };
                    let resp = resp.on_hover_text(hover);
                    if x_resp.clicked() || resp.middle_clicked() {
                        close = Some(tab.id());
                    } else if resp.clicked() {
                        activate = Some(tab.id());
                    }
                }
                if can_query && ui.add(IconBtn::new(Icon::Plus, "New query (⌘T)")).clicked() {
                    new_query = true;
                }
                ui.add_space(8.0);
            });
        });
        if let Some(id) = close {
            self.request_close(id);
        } else if let Some(id) = activate {
            self.active = Some(id);
        }
        if new_query {
            self.new_query_here();
        }
    }

    fn empty_state(&mut self, ui: &mut Ui) {
        let pal = style::pal(ui);
        let area = ui.available_rect_before_wrap();
        let content_h = 360.0;
        let top = area.top() + ((area.height() - content_h) / 2.0 - 24.0).max(12.0);
        let cx = area.center().x;
        let painter = ui.painter().clone();
        let mut y = top;
        let icon_rect = Rect::from_center_size(pos2(cx, y + 48.0), vec2(96.0, 96.0));
        if let Some(tex) = &self.icon {
            // The icon file has a transparent margin of 100 / 1024 on each side.
            let m = 100.0 / 1024.0;
            let uv = Rect::from_min_max(pos2(m, m), pos2(1.0 - m, 1.0 - m));
            painter.image(tex.id(), icon_rect, uv, Color32::WHITE);
        }
        y += 96.0 + 20.0;
        painter.text(
            pos2(cx, y),
            egui::Align2::CENTER_TOP,
            "OurDB",
            style::semibold(22.0),
            pal.text,
        );
        y += 32.0;
        let text = if self.store.connections.is_empty() {
            "Add a connection to get started."
        } else {
            "Pick a connection in the sidebar, then open a table or a query."
        };
        painter.text(
            pos2(cx, y),
            egui::Align2::CENTER_TOP,
            text,
            style::regular(13.0),
            pal.text2,
        );
        y += 34.0;
        let btn = Btn::primary("New connection").icon(Icon::Plus).height(28.0);
        let bw = 168.0;
        let btn_rect = Rect::from_min_size(pos2(cx - bw / 2.0, y), vec2(bw, 28.0));
        if ui.put(btn_rect, btn.min_width(bw)).clicked() {
            self.dialog = Some(ConnDialog::new_connection());
        }
        y += 28.0 + 34.0;
        for (keys, label) in [
            ("⌘T", "New query"),
            ("⌘↵", "Run query"),
            ("⌘W", "Close tab"),
        ] {
            painter.text(
                pos2(cx - 10.0, y + 10.0),
                egui::Align2::RIGHT_CENTER,
                label,
                style::regular(12.0),
                pal.text3,
            );
            let g = style::galley(ui, keys, style::regular(11.5), pal.text3);
            let key = Rect::from_min_size(pos2(cx + 10.0, y + 1.0), vec2(g.size().x + 12.0, 18.0));
            painter.rect_stroke(
                key,
                4.0,
                Stroke::new(1.0, pal.separator),
                StrokeKind::Inside,
            );
            style::paint_galley_v(&painter, key, key.left() + 6.0, g);
            y += 24.0;
        }
    }

    fn run_step(&mut self, step: Step, ctx: &egui::Context) {
        let find = |store: &Store, name: &str| {
            store
                .connections
                .iter()
                .find(|c| c.name == name)
                .map(|c| c.id.clone())
        };
        match step {
            Step::Connect(name) => match find(&self.store, &name) {
                Some(id) => self.connect(&id),
                None => eprintln!("OurDB: no connection named \"{name}\""),
            },
            Step::Open(ns, obj) => {
                if let Some(conn) = self.current_conn.clone() {
                    self.open_table(&conn, ObjectRef::new(ns, obj));
                }
            }
            Step::Query(text) => {
                if let Some(conn) = self.current_conn.clone()
                    && let Some(q) = self.new_query(&conn)
                {
                    q.text = text.clone();
                    if let Some(ran) = q.request_run(text) {
                        self.store.add_history(&conn, &ran);
                    }
                }
            }
            Step::Select(r, c) => match self.active_tab() {
                Some(Tab::Table(t)) => t.grid.select(r, c),
                Some(Tab::Query(q)) => q.select(r, c),
                None => {}
            },
            Step::Edit(r, c, text) => {
                if let Some(Tab::Table(t)) = self.active_tab() {
                    t.stage(r, c, ourdb_core::Edit::Text(text));
                    t.grid.select(r, c);
                }
            }
            Step::Delete(r) => {
                if let Some(Tab::Table(t)) = self.active_tab() {
                    t.toggle_delete(r);
                }
            }
            Step::AddRow => {
                if let Some(Tab::Table(t)) = self.active_tab() {
                    if t.kind.is_document() {
                        t.open_doc_editor(None);
                    } else {
                        t.add_row();
                    }
                }
            }
            Step::DialogNew => self.dialog = Some(ConnDialog::new_connection()),
            Step::DialogEdit(name) => {
                if let Some(id) = find(&self.store, &name) {
                    self.open_edit_dialog(&id);
                }
            }
            Step::Save => {
                if let Some(Tab::Table(t)) = self.active_tab() {
                    t.open_review();
                }
            }
            Step::Sql => {
                if let Some(Tab::Table(t)) = self.active_tab() {
                    t.open_query = Some(t.starter_query());
                }
            }
            Step::Apply => {
                if let Some(Tab::Table(t)) = self.active_tab() {
                    t.apply();
                }
            }
        }
        ctx.request_repaint();
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        let busy = self.busy();
        if let Some(step) = self.debug.next_step(ctx, busy) {
            self.run_step(step, ctx);
        }
        let busy = self.busy();
        self.debug.tick(ctx, busy);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.shortcuts(&ctx);
        let pal = style::pal(ui);

        let selected = self
            .active
            .and_then(|a| self.tabs.iter().find(|t| t.id() == a))
            .and_then(|t| match t {
                Tab::Table(tt) => Some((tt.conn_id.clone(), tt.object.clone())),
                Tab::Query(_) => None,
            });
        let actions = egui::Panel::left("sidebar")
            .default_size(250.0)
            .size_range(200.0..=440.0)
            .frame(egui::Frame::NONE.fill(pal.sidebar))
            .show(ui, |ui| {
                // Room for the traffic lights; this strip also moves the window.
                let (bar, _) = ui.allocate_exact_size(
                    vec2(ui.available_width(), style::BAR_HEIGHT),
                    Sense::hover(),
                );
                drag_area(ui, bar, "side-bar");
                if let Some((text, is_err)) = self.notice.clone() {
                    egui::Panel::bottom("notice")
                        .show_separator_line(false)
                        .frame(egui::Frame::NONE.inner_margin(egui::Margin::same(8)))
                        .show(ui, |ui| {
                            let fill = if is_err {
                                pal.danger.gamma_multiply(0.12)
                            } else {
                                pal.hover
                            };
                            egui::Frame::NONE
                                .fill(fill)
                                .corner_radius(8)
                                .inner_margin(egui::Margin {
                                    left: 10,
                                    right: 4,
                                    top: 6,
                                    bottom: 6,
                                })
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal_top(|ui| {
                                        let color = if is_err { pal.error } else { pal.text2 };
                                        let w = ui.available_width() - 26.0;
                                        ui.allocate_ui(vec2(w, 0.0), |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(&text).color(color).small(),
                                                )
                                                .wrap(),
                                            );
                                        });
                                        if ui
                                            .add(IconBtn::new(Icon::Close, "Close").size(20.0))
                                            .clicked()
                                        {
                                            self.notice = None;
                                        }
                                    });
                                });
                        });
                }
                let sel = selected.as_ref().map(|(c, o)| (c.as_str(), o));
                sidebar::show(
                    ui,
                    &self.store.connections,
                    &mut self.states,
                    &mut self.expanded,
                    sel,
                )
            })
            .inner;
        for a in actions {
            self.on_side_action(a);
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(pal.content))
            .show(ui, |ui| {
                let has_tabs = !self.tabs.is_empty();
                egui::Panel::top("tabs")
                    .exact_size(style::BAR_HEIGHT)
                    .frame(egui::Frame::NONE.fill(if has_tabs { pal.chrome } else { pal.content }))
                    .show_separator_line(has_tabs)
                    .show(ui, |ui| self.top_bar(ui));
                if !has_tabs {
                    self.empty_state(ui);
                    return;
                }
                let Some(active) = self.active else { return };
                let mut ran = None;
                let mut open_query = None;
                match self.tabs.iter_mut().find(|t| t.id() == active) {
                    Some(Tab::Table(t)) => {
                        t.ui(ui);
                        open_query = t
                            .open_query
                            .take()
                            .map(|text| (t.conn_id.clone(), t.object.namespace.clone(), text));
                    }
                    Some(Tab::Query(q)) => {
                        let history = self.store.history_of(&q.conn_id).to_vec();
                        if let Some(text) = q.ui(ui, &history) {
                            ran = Some((q.conn_id.clone(), text));
                        }
                    }
                    None => {}
                }
                if let Some((conn, text)) = ran {
                    self.store.add_history(&conn, &text);
                }
                if let Some((conn, namespace, text)) = open_query
                    && let Some(q) = self.new_query(&conn)
                {
                    q.namespace = Some(namespace);
                    q.text = text;
                }
            });

        self.dialog_ui(&ctx);
        self.delete_ui(&ctx);
        self.close_confirm_ui(&ctx);
    }
}

/// Drag `rect` to move the window; double-click it to zoom.
fn drag_area(ui: &mut Ui, rect: Rect, salt: &str) {
    let resp = ui.interact(rect, egui::Id::unique(salt), Sense::click_and_drag());
    if resp.drag_started_by(egui::PointerButton::Primary) {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if resp.double_clicked() {
        let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx()
            .send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    }
}

fn palette_of(ctx: &egui::Context) -> style::Palette {
    style::palette(ctx.global_style().visuals.dark_mode)
}

impl Drop for App {
    fn drop(&mut self) {
        // Do not wait for driver calls that may hang.
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}
