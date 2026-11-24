//! The value panel on the right: the full value of the selected cell.

use eframe::egui::{self, RichText, Sense, TextEdit, Ui, vec2};
use ourdb_core::{Edit, Value};

use crate::{
    grid::{GridModel, Mark, Shown},
    icons::{self, Icon},
    style::{self, Btn},
};

pub enum ValueAction {
    Apply(String),
    SetNull,
    EditDocument(usize),
}

#[derive(Default)]
pub struct ValuePanel {
    /// (row, column, data version) of the text in the editor.
    key: Option<(usize, usize, u64)>,
    text: String,
    initial: String,
}

impl ValuePanel {
    /// Show the panel. `version` changes when the data changes, so the editor reloads.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        model: &dyn GridModel,
        selected: Option<(usize, usize)>,
        version: u64,
        editable: bool,
        document: bool,
    ) -> Option<ValueAction> {
        let pal = style::pal(ui);
        let Some((r, c)) = selected.filter(|(r, c)| *r < model.len() && *c < model.columns().len())
        else {
            self.key = None;
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() / 2.0 - 40.0).max(20.0));
                let (rect, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
                icons::paint(ui.painter(), rect, Icon::Table, pal.text3);
                ui.add_space(8.0);
                ui.label(RichText::new("No cell selected").color(pal.text2));
                ui.label(
                    RichText::new("Select a cell to see its value.")
                        .color(pal.text3)
                        .small(),
                );
            });
            return None;
        };
        let (shown, mark) = model.cell(r, c);
        let editable = editable && mark != Mark::Deleted && !matches!(shown, Shown::Raw(_));
        if self.key != Some((r, c, version)) {
            self.text = match (&shown, editable) {
                (Shown::Val(v), false) if !v.is_null() => v.display_full(),
                _ => shown.edit_text(),
            };
            self.initial = self.text.clone();
            self.key = Some((r, c, version));
        }

        let mut action = None;
        // Header: column name, then its type and state as pills.
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
            ui.label(
                RichText::new(&model.columns()[c])
                    .font(style::semibold(13.0))
                    .color(pal.text),
            );
            let ty = model.types().get(c).map(String::as_str).unwrap_or("");
            if !ty.is_empty() {
                style::pill(ui, ty, pal.text2, pal.hover);
            }
            let state = match shown {
                Shown::Val(Value::Null) | Shown::Edit(Edit::Null) => Some("NULL"),
                Shown::Val(Value::Missing) => Some("missing field"),
                Shown::Default => Some("DEFAULT"),
                _ => None,
            };
            if let Some(b) = state {
                style::pill(ui, b, pal.text2, pal.hover);
            }
            let edited = match mark {
                Mark::Edited => Some(("edited", pal.edited)),
                Mark::New => Some(("new", pal.new_row)),
                Mark::Deleted => Some(("deleted", pal.deleted)),
                Mark::Clean => None,
            };
            if let Some((t, bg)) = edited {
                style::pill(ui, t, pal.text, bg);
            }
        });
        ui.add_space(8.0);

        // Buttons at the bottom, editor fills the rest.
        egui::Panel::bottom(egui::Id::unique("value-buttons"))
            .show_separator_line(false)
            .frame(egui::Frame::NONE.inner_margin(egui::Margin {
                left: 0,
                right: 0,
                top: 10,
                bottom: 0,
            }))
            .show(ui, |ui| {
                if !editable {
                    return;
                }
                if document && mark != Mark::New {
                    let w = ui.available_width();
                    if ui
                        .add(
                            Btn::secondary("Edit document…")
                                .icon(Icon::Collection)
                                .min_width(w),
                        )
                        .clicked()
                    {
                        action = Some(ValueAction::EditDocument(r));
                    }
                    ui.add_space(2.0);
                }
                style::footer(ui, |ui| {
                    let changed = self.text != self.initial;
                    if ui
                        .add_enabled(changed, Btn::primary("Apply").min_width(72.0))
                        .clicked()
                    {
                        action = Some(ValueAction::Apply(self.text.clone()));
                    }
                    if ui.add(Btn::secondary("Set NULL")).clicked() {
                        action = Some(ValueAction::SetNull);
                    }
                });
            });
        let frame = style::editor_frame(&pal, pal.content).inner_margin(egui::Margin::same(1));
        let out = frame.show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let size = ui.available_size();
                    let mut read_only = self.text.as_str();
                    let edit = if editable {
                        TextEdit::multiline(&mut self.text)
                    } else {
                        TextEdit::multiline(&mut read_only)
                    };
                    ui.add(
                        edit.font(style::mono(12.0))
                            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(8, 6)))
                            .desired_width(size.x)
                            .min_size(size),
                    )
                    .has_focus()
                })
                .inner
        });
        if out.inner && editable {
            style::focus_ring(ui.painter(), out.response.rect, &pal);
        }
        action
    }
}
