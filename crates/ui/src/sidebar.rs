//! The left panel: connections, then namespaces and objects of open connections.

use std::collections::{HashMap, HashSet};

use eframe::egui::{self, Color32, Rect, Sense, TextEdit, Ui, pos2, vec2};
use ourdb_core::{ConnConfig, Env, Kind, ObjectKind, ObjectRef};

use crate::{
    app::{ConnState, Live},
    icons::{self, Icon},
    style::{self, IconBtn, Palette},
};

/// Height of a connection row.
const CONN_ROW: f32 = 28.0;
/// Height of a namespace or object row.
const ROW: f32 = 24.0;
/// Indent per tree level.
const INDENT: f32 = 14.0;
/// Space between the panel edge and a row's rounded background.
const INSET: f32 = 8.0;

pub enum SideAction {
    Connect(String),
    Disconnect(String),
    NewQuery(String),
    Edit(String),
    Duplicate(String),
    Delete(String),
    NewConnection,
    Open(String, ObjectRef),
    LoadObjects(String, String),
    Refresh(String),
    SwitchDb(String, String),
}

/// What the sidebar marks as selected: (connection id, object).
pub type Selected<'a> = Option<(&'a str, &'a ObjectRef)>;

pub fn show(
    ui: &mut Ui,
    conns: &[ConnConfig],
    states: &mut HashMap<String, ConnState>,
    expanded: &mut HashSet<String>,
    selected: Selected,
) -> Vec<SideAction> {
    let mut actions = Vec::new();
    let pal = style::pal(ui);

    // Section header.
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    style::paint_clipped(
        ui,
        Rect::from_min_max(pos2(rect.left() + 16.0, rect.top()), rect.right_bottom()),
        "Connections",
        style::semibold(11.0),
        pal.text2,
    );
    let plus = Rect::from_center_size(
        pos2(rect.right() - INSET - 11.0, rect.center().y),
        vec2(22.0, 22.0),
    );
    if ui
        .put(plus, IconBtn::new(Icon::Plus, "New connection").size(22.0))
        .clicked()
    {
        actions.push(SideAction::NewConnection);
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            if conns.is_empty() {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(egui::RichText::new("No connections yet.").color(pal.text2));
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    if ui
                        .add(style::Btn::plain("Add a connection").icon(Icon::Plus))
                        .clicked()
                    {
                        actions.push(SideAction::NewConnection);
                    }
                });
            }
            for cfg in conns {
                let state = states.get_mut(&cfg.id);
                let is_open = expanded.contains(&cfg.id);
                let (resp, query_clicked) = conn_row(ui, cfg, state.as_deref(), is_open, &pal);
                if query_clicked {
                    actions.push(SideAction::NewQuery(cfg.id.clone()));
                } else if resp.clicked() {
                    match state.as_deref() {
                        Some(ConnState::Live(_)) => {
                            if !expanded.remove(&cfg.id) {
                                expanded.insert(cfg.id.clone());
                            }
                        }
                        Some(ConnState::Connecting(_)) => {}
                        _ => actions.push(SideAction::Connect(cfg.id.clone())),
                    }
                }
                let live = matches!(state.as_deref(), Some(ConnState::Live(_)));
                resp.context_menu(|ui| {
                    if live {
                        if ui.button("Disconnect").clicked() {
                            actions.push(SideAction::Disconnect(cfg.id.clone()));
                            ui.close();
                        }
                        if ui.button("New query").clicked() {
                            actions.push(SideAction::NewQuery(cfg.id.clone()));
                            ui.close();
                        }
                    } else if ui.button("Connect").clicked() {
                        actions.push(SideAction::Connect(cfg.id.clone()));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Edit…").clicked() {
                        actions.push(SideAction::Edit(cfg.id.clone()));
                        ui.close();
                    }
                    if ui.button("Duplicate").clicked() {
                        actions.push(SideAction::Duplicate(cfg.id.clone()));
                        ui.close();
                    }
                    if ui.button("Delete…").clicked() {
                        actions.push(SideAction::Delete(cfg.id.clone()));
                        ui.close();
                    }
                });
                if let Some(ConnState::Live(live)) = state
                    && is_open
                {
                    tree(ui, cfg, live, selected, &pal, &mut actions);
                }
            }
            ui.add_space(8.0);
        });
    actions
}

/// The rounded background of a row: hover or selection.
fn row_bg(ui: &Ui, rect: Rect, resp: &egui::Response, selected: bool, pal: &Palette) -> Rect {
    let bg = Rect::from_min_max(
        pos2(rect.left() + INSET, rect.top() + 1.0),
        pos2(rect.right() - INSET, rect.bottom() - 1.0),
    );
    let fill = if selected {
        pal.accent_soft
    } else if resp.hovered() {
        pal.hover
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(bg, style::RADIUS, fill);
    }
    bg
}

fn chevron(ui: &Ui, x: f32, cy: f32, open: bool, pal: &Palette) {
    let r = Rect::from_center_size(pos2(x + 5.0, cy), vec2(10.0, 10.0));
    let icon = if open {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    };
    icons::paint(ui.painter(), r, icon, pal.text3);
}

fn conn_row(
    ui: &mut Ui,
    cfg: &ConnConfig,
    state: Option<&ConnState>,
    open: bool,
    pal: &Palette,
) -> (egui::Response, bool) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), CONN_ROW), Sense::click());
    let bg = row_bg(ui, rect, &resp, false, pal);
    let cy = rect.center().y;
    let live = matches!(state, Some(ConnState::Live(_)));
    let x0 = bg.left() + 4.0;
    if live {
        chevron(ui, x0, cy, open, pal);
    }
    let badge = Rect::from_min_size(pos2(x0 + 14.0, cy - 9.0), vec2(18.0, 18.0));
    style::paint_kind_badge(ui, badge, cfg.kind);

    // Right side: a spinner while connecting, a red mark after a failure.
    let mut right = bg.right() - 6.0;
    match state {
        Some(ConnState::Connecting(_)) => {
            let r = Rect::from_center_size(pos2(right - 6.0, cy), vec2(12.0, 12.0));
            style::paint_spinner(ui, r);
            right -= 18.0;
        }
        Some(ConnState::Failed(_)) => {
            let r = Rect::from_center_size(pos2(right - 6.0, cy), vec2(13.0, 13.0));
            icons::paint(ui.painter(), r, Icon::Warning, pal.danger);
            right -= 18.0;
        }
        _ => {}
    }
    // Open connection: a "new query" button at the right, shown on hover.
    let mut query_clicked = false;
    if live {
        let r = Rect::from_center_size(pos2(right - 10.0, cy), vec2(20.0, 20.0));
        right -= 24.0;
        let btn = ui.interact(r, egui::Id::unique(("conn-query", &cfg.id)), Sense::click());
        if ui.rect_contains_pointer(rect) {
            if btn.hovered() {
                ui.painter().rect_filled(r, 4.0, pal.hover);
            }
            let color = if btn.hovered() { pal.accent } else { pal.text2 };
            icons::paint(ui.painter(), r.shrink(3.0), Icon::Terminal, color);
        }
        query_clicked = btn.on_hover_text("New query").clicked();
    }
    let prod = cfg.env == Env::Prod;
    let pill_font = style::semibold(9.0);
    let pill = style::pill_size(ui, "PROD", &pill_font);
    let dot = style::env_color(cfg.env);
    let mut extras = 0.0;
    if dot.is_some() {
        extras += 12.0;
    }
    if prod {
        extras += pill.x + 6.0;
    }
    let name_x = badge.right() + 8.0;
    let name_max = (right - name_x - extras).max(10.0);
    let color = if live { pal.text } else { pal.text2 };
    let name_rect = Rect::from_min_max(
        pos2(name_x, rect.top()),
        pos2(name_x + name_max, rect.bottom()),
    );
    let font = if live {
        style::semibold(13.0)
    } else {
        style::regular(13.0)
    };
    let w = style::paint_clipped(ui, name_rect, &cfg.name, font, color);
    let mut x = name_x + w + 7.0;
    if let Some(c) = dot {
        ui.painter().circle_filled(pos2(x + 3.0, cy), 3.0, c);
        x += 12.0;
    }
    if prod {
        let r = Rect::from_min_size(pos2(x, cy - pill.y / 2.0), pill);
        style::paint_pill(ui, r, "PROD", pill_font, Color32::WHITE, pal.danger);
    }

    let resp = match state {
        Some(ConnState::Failed(e)) => resp.on_hover_text(e),
        _ => resp.on_hover_text(format!("{} · {}", cfg.kind.label(), target(cfg))),
    };
    (resp, query_clicked)
}

fn target(cfg: &ConnConfig) -> String {
    if cfg.kind == Kind::Sqlite {
        cfg.path.clone()
    } else if cfg.has_url() {
        "URL".into()
    } else {
        format!("{}:{}", cfg.host_or_default(), cfg.port_or_default())
    }
}

/// Left edge of the content at tree `depth` (0 = connection rows).
fn level_x(ui: &Ui, depth: f32) -> f32 {
    ui.max_rect().left() + INSET + 4.0 + INDENT * depth
}

/// A compact row for a tree control (database picker, filter).
fn control_row<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui, f32) -> R) -> R {
    let left = level_x(ui, 1.0) - ui.max_rect().left();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.spacing_mut().interact_size.y = 22.0;
        ui.add_space(left);
        let width = ui.available_width() - INSET;
        add(ui, width)
    })
    .inner
}

fn tree(
    ui: &mut Ui,
    cfg: &ConnConfig,
    live: &mut Live,
    selected: Selected,
    pal: &Palette,
    actions: &mut Vec<SideAction>,
) {
    let id = &cfg.id;
    let picker = cfg.kind == Kind::Postgres && !live.databases.is_empty();
    ui.add_space(4.0);
    if picker {
        control_row(ui, |ui, width| {
            let mut db = live.database.clone();
            style::combo(("db", id), db.clone(), width - 26.0).show_ui(ui, |ui| {
                for d in &live.databases {
                    ui.selectable_value(&mut db, d.clone(), d);
                }
            });
            if db != live.database {
                actions.push(SideAction::SwitchDb(id.clone(), db));
            }
            if ui
                .add(IconBtn::new(Icon::Refresh, "Refresh").size(22.0))
                .clicked()
            {
                actions.push(SideAction::Refresh(id.clone()));
            }
        });
        ui.add_space(4.0);
    }
    control_row(ui, |ui, width| {
        let w = if picker { width } else { width - 26.0 };
        style::field_sized(
            ui,
            TextEdit::singleline(&mut live.filter).hint_text("Filter"),
            Some(Icon::Search),
            w,
            22.0,
        );
        if !picker
            && ui
                .add(IconBtn::new(Icon::Refresh, "Refresh").size(22.0))
                .clicked()
        {
            actions.push(SideAction::Refresh(id.clone()));
        }
    });
    ui.add_space(4.0);

    let message = |ui: &mut Ui, depth: f32, text: &str, color: Color32| {
        let left = level_x(ui, depth) - ui.max_rect().left();
        ui.horizontal(|ui| {
            ui.add_space(left);
            ui.add(egui::Label::new(egui::RichText::new(text).color(color).small()).wrap());
        });
        ui.add_space(4.0);
    };
    let spinner_row = |ui: &mut Ui, depth: f32| {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
        let x = level_x(ui, depth);
        style::paint_spinner(
            ui,
            Rect::from_center_size(pos2(x + 6.0, rect.center().y), vec2(12.0, 12.0)),
        );
    };

    let filter = live.filter.trim().to_lowercase();
    match &mut live.namespaces {
        None => spinner_row(ui, 1.0),
        Some(Err(e)) => message(ui, 1.0, e.as_str(), pal.error),
        Some(Ok(nodes)) => {
            for node in nodes.iter_mut() {
                // While filtering, hide namespaces with no match.
                let matches: Vec<_> = match &node.objects {
                    Some(Ok(objs)) => objs
                        .iter()
                        .filter(|o| filter.is_empty() || o.name.to_lowercase().contains(&filter))
                        .collect(),
                    _ => Vec::new(),
                };
                if !filter.is_empty() && matches.is_empty() {
                    continue;
                }
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
                row_bg(ui, rect, &resp, false, pal);
                let x = level_x(ui, 1.0);
                chevron(ui, x, rect.center().y, node.open, pal);
                let ir = Rect::from_center_size(pos2(x + 21.0, rect.center().y), vec2(14.0, 14.0));
                icons::paint(ui.painter(), ir, Icon::Database, pal.text2);
                let name_rect = Rect::from_min_max(
                    pos2(x + 33.0, rect.top()),
                    pos2(rect.right() - INSET - 4.0, rect.bottom()),
                );
                style::paint_clipped(ui, name_rect, &node.name, style::regular(13.0), pal.text);
                if resp.clicked() {
                    node.open = !node.open;
                    if node.open && node.objects.is_none() && !node.loading {
                        actions.push(SideAction::LoadObjects(id.clone(), node.name.clone()));
                    }
                }
                if !node.open && filter.is_empty() {
                    continue;
                }
                if node.loading {
                    spinner_row(ui, 2.0);
                }
                if let Some(Err(e)) = &node.objects {
                    message(ui, 2.0, e.as_str(), pal.error);
                }
                if let Some(Ok(objs)) = &node.objects
                    && objs.is_empty()
                {
                    let left = level_x(ui, 2.0) + 14.0 - ui.max_rect().left();
                    ui.horizontal(|ui| {
                        ui.add_space(left);
                        ui.label(egui::RichText::new("Empty").color(pal.text3).small());
                    });
                    ui.add_space(4.0);
                }
                for obj in matches {
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
                    let is_sel = selected.is_some_and(|(c, o)| {
                        c == id && o.namespace == node.name && o.name == obj.name
                    });
                    row_bg(ui, rect, &resp, is_sel, pal);
                    let x = level_x(ui, 2.0) + 14.0;
                    let icon = match obj.kind {
                        ObjectKind::Table => Icon::Table,
                        ObjectKind::View => Icon::View,
                        ObjectKind::Collection => Icon::Collection,
                    };
                    let ir =
                        Rect::from_center_size(pos2(x + 7.0, rect.center().y), vec2(14.0, 14.0));
                    let icon_color = if is_sel { pal.accent } else { pal.text2 };
                    icons::paint(ui.painter(), ir, icon, icon_color);
                    let name_rect = Rect::from_min_max(
                        pos2(x + 20.0, rect.top()),
                        pos2(rect.right() - INSET - 4.0, rect.bottom()),
                    );
                    style::paint_clipped(ui, name_rect, &obj.name, style::regular(13.0), pal.text);
                    if resp.clicked() {
                        actions.push(SideAction::Open(
                            id.clone(),
                            ObjectRef::new(&node.name, &obj.name),
                        ));
                    }
                }
            }
        }
    }
    ui.add_space(6.0);
}
