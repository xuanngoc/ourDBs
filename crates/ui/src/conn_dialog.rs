//! The connection dialog: create or edit one saved connection.

use eframe::egui::{self, Rect, RichText, Sense, Stroke, StrokeKind, TextEdit, Ui, pos2, vec2};
use ourdb_core::{ConnConfig, Env, Kind, TlsMode};

use crate::{
    icons::{self, Icon},
    style::{self, Btn, Segment},
};

/// Width of the label column of the form.
const LABEL_W: f32 = 92.0;
/// Width of the field column of the form.
const FIELD_W: f32 = 372.0;

const AWS_CA_URL: &str = "https://truststore.pki.rds.amazonaws.com/global/global-bundle.pem";

pub enum TestState {
    Idle,
    Running(u64),
    Ok,
    Failed(String),
}

pub enum DialogAction {
    None,
    Cancel,
    Save,
    Test,
}

pub struct ConnDialog {
    pub cfg: ConnConfig,
    pub password: String,
    /// The password when the dialog opened.
    initial_password: String,
    pub is_new: bool,
    port_text: String,
    pub test: TestState,
    pub error: Option<String>,
    /// The "Advanced" section is open.
    advanced: bool,
}

impl ConnDialog {
    pub fn new_connection() -> Self {
        let cfg = ConnConfig {
            id: uuid::Uuid::new_v4().to_string(),
            kind: Kind::Postgres,
            tls: Kind::Postgres.default_tls(),
            ..Default::default()
        };
        Self::edit(cfg, String::new(), true)
    }

    pub fn edit(cfg: ConnConfig, password: String, is_new: bool) -> Self {
        let port_text = if cfg.port == 0 {
            String::new()
        } else {
            cfg.port.to_string()
        };
        let advanced = !cfg.url.trim().is_empty()
            || !cfg.ca_file.trim().is_empty()
            || !cfg.ssh.trim().is_empty()
            || !cfg.ssh_key.trim().is_empty()
            || cfg.tls != cfg.kind.default_tls();
        Self {
            advanced,
            cfg,
            initial_password: password.clone(),
            password,
            is_new,
            port_text,
            test: TestState::Idle,
            error: None,
        }
    }

    /// The password to store, or `None` when there is nothing to change.
    /// An unchanged field never touches the Keychain, so a failed read cannot wipe it.
    pub fn password_to_save(&self) -> Option<&str> {
        let changed = if self.is_new {
            !self.password.is_empty()
        } else {
            self.password != self.initial_password
        };
        changed.then_some(self.password.as_str())
    }

    /// The config with the port parsed and a name filled in.
    pub fn result(&self) -> Result<ConnConfig, String> {
        let mut cfg = self.cfg.clone();
        let port = self.port_text.trim();
        cfg.port = if port.is_empty() {
            0
        } else {
            port.parse()
                .map_err(|_| format!("Port \"{port}\" is not a number."))?
        };
        if cfg.name.trim().is_empty() {
            cfg.name = default_name(&cfg);
        }
        cfg.password = self.password.clone();
        Ok(cfg)
    }

    pub fn show(&mut self, ctx: &egui::Context) -> DialogAction {
        let mut action = DialogAction::None;
        let pal = style::palette(ctx.global_style().visuals.dark_mode);
        style::modal(egui::Id::unique("conn-dialog"), ctx).show(ctx, |ui| {
            ui.set_width(LABEL_W + 12.0 + FIELD_W);
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            style::dialog_title(
                ui,
                if self.is_new {
                    "New connection"
                } else {
                    "Edit connection"
                },
            );
            ui.add_space(6.0);
            self.kind_tiles(ui, &pal);
            ui.add_space(8.0);
            self.fields(ui, &pal);
            self.hints(ui, &pal);
            if let Some(e) = &self.error {
                ui.add_space(2.0);
                style::note(ui, e, pal.error);
            }
            ui.add_space(10.0);
            style::hairline(ui);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let testing = matches!(self.test, TestState::Running(_));
                if ui
                    .add_enabled(!testing, Btn::secondary("Test").icon(Icon::Check))
                    .clicked()
                {
                    action = DialogAction::Test;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(Btn::primary("Save").min_width(72.0)).clicked() {
                        action = DialogAction::Save;
                    }
                    if ui.add(Btn::secondary("Cancel")).clicked() {
                        action = DialogAction::Cancel;
                    }
                    // The test result, inline between the buttons.
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        match &self.test {
                            TestState::Idle => {}
                            TestState::Running(_) => {
                                style::spinner(ui, 12.0);
                                ui.label(RichText::new("Connecting…").color(pal.text2));
                            }
                            TestState::Ok => {
                                let (r, _) =
                                    ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                                icons::paint(ui.painter(), r, Icon::Check, pal.ok);
                                ui.label(RichText::new("Connected").color(pal.ok));
                            }
                            TestState::Failed(e) => {
                                let (r, _) =
                                    ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                                icons::paint(ui.painter(), r, Icon::Warning, pal.danger);
                                ui.add(
                                    egui::Label::new(RichText::new(e).color(pal.error)).truncate(),
                                )
                                .on_hover_text(e);
                            }
                        }
                    });
                });
            });
        });
        action
    }

    /// One tile per database type.
    fn kind_tiles(&mut self, ui: &mut Ui, pal: &style::Palette) {
        let gap = 8.0;
        let total = ui.available_width();
        let w = ((total - gap * 4.0) / 5.0).floor();
        let h = 58.0;
        let (row, _) = ui.allocate_exact_size(vec2(total, h), Sense::hover());
        let old_kind = self.cfg.kind;
        for (i, k) in Kind::ALL.into_iter().enumerate() {
            let rect = Rect::from_min_size(
                pos2(row.left() + i as f32 * (w + gap), row.top()),
                vec2(w, h),
            );
            let resp = ui.interact(rect, ui.scope_id().with(("kind-tile", i)), Sense::click());
            let selected = self.cfg.kind == k;
            let painter = ui.painter();
            let (fill, stroke) = if selected {
                (pal.accent_soft, Stroke::new(1.5, pal.accent))
            } else if resp.hovered() {
                (
                    pal.control_hover,
                    Stroke::new(1.0, pal.control_border_hover),
                )
            } else {
                (pal.control, Stroke::new(1.0, pal.control_border))
            };
            painter.rect_filled(rect, 8.0, fill);
            painter.rect_stroke(rect, 8.0, stroke, StrokeKind::Inside);
            let badge =
                Rect::from_center_size(pos2(rect.center().x, rect.top() + 20.0), vec2(22.0, 22.0));
            style::paint_kind_badge(ui, badge, k);
            let label = match k {
                Kind::DocumentDb => "DocumentDB",
                other => other.label(),
            };
            let color = if selected { pal.text } else { pal.text2 };
            let font = if selected {
                style::semibold(11.0)
            } else {
                style::regular(11.0)
            };
            let g = style::galley(ui, label, font, color);
            ui.painter().galley(
                pos2(rect.center().x - g.size().x / 2.0, rect.bottom() - 19.0),
                g,
                egui::Color32::PLACEHOLDER,
            );
            if resp.clicked() {
                self.cfg.kind = k;
            }
        }
        let cfg = &mut self.cfg;
        if cfg.kind != old_kind {
            // New type: reset the defaults that came from the old type.
            if self.port_text.trim() == old_kind.default_port().to_string() {
                self.port_text.clear();
            }
            if cfg.tls == old_kind.default_tls() {
                cfg.tls = cfg.kind.default_tls();
            }
        }
    }

    fn fields(&mut self, ui: &mut Ui, pal: &style::Palette) {
        let cfg = &mut self.cfg;
        let row = |ui: &mut Ui, label: &str, add: &mut dyn FnMut(&mut Ui)| {
            style::form_row(ui, label, LABEL_W, |ui| add(ui));
        };

        let name_hint = default_name(cfg);
        row(ui, "Name", &mut |ui| {
            style::field(
                ui,
                TextEdit::singleline(&mut cfg.name).hint_text(name_hint.as_str()),
                None,
                FIELD_W,
            );
        });

        if cfg.kind == Kind::Sqlite {
            row(ui, "File", &mut |ui| {
                style::field(
                    ui,
                    TextEdit::singleline(&mut cfg.path).hint_text("/path/to/database.sqlite"),
                    None,
                    FIELD_W - 92.0,
                );
                if ui.add(Btn::secondary("Choose…").min_width(84.0)).clicked()
                    && let Some(p) = rfd::FileDialog::new().pick_file()
                {
                    cfg.path = p.display().to_string();
                }
            });
        } else {
            let use_fields = !cfg.has_url();
            let port_text = &mut self.port_text;
            row(ui, "Host", &mut |ui| {
                ui.add_enabled_ui(use_fields, |ui| {
                    let port_label = style::galley(ui, "Port", style::regular(13.0), pal.text2)
                        .size()
                        .x;
                    let gap = ui.spacing().item_spacing.x;
                    style::field(
                        ui,
                        TextEdit::singleline(&mut cfg.host).hint_text("127.0.0.1"),
                        None,
                        FIELD_W - 80.0 - port_label - 2.0 * gap,
                    );
                    ui.label(RichText::new("Port").color(pal.text2));
                    let hint = cfg.kind.default_port().to_string();
                    style::field(
                        ui,
                        TextEdit::singleline(port_text).hint_text(hint),
                        None,
                        80.0,
                    );
                });
            });
            row(ui, "User", &mut |ui| {
                ui.add_enabled_ui(use_fields, |ui| {
                    style::field(ui, TextEdit::singleline(&mut cfg.user), None, FIELD_W);
                });
            });
            let password = &mut self.password;
            row(ui, "Password", &mut |ui| {
                style::field(
                    ui,
                    TextEdit::singleline(password)
                        .password(true)
                        .hint_text("Saved in the Keychain"),
                    None,
                    FIELD_W,
                );
            });
            let db_hint = if cfg.kind == Kind::Postgres {
                "postgres"
            } else {
                "optional"
            };
            row(ui, "Database", &mut |ui| {
                ui.add_enabled_ui(use_fields, |ui| {
                    style::field(
                        ui,
                        TextEdit::singleline(&mut cfg.database).hint_text(db_hint),
                        None,
                        FIELD_W,
                    );
                });
            });
        }

        row(ui, "Environment", &mut |ui| {
            let items: Vec<(Env, Segment)> = Env::ALL
                .iter()
                .map(|&e| (e, Segment::text(e.label()).dot(style::env_color(e))))
                .collect();
            style::segmented(ui, &mut cfg.env, &items, 0.0);
        });
        row(ui, "Read-only", &mut |ui| {
            let resp = style::toggle(ui, &mut cfg.read_only);
            let text = if cfg.read_only {
                "Writes are refused"
            } else {
                "Writes are allowed"
            };
            ui.label(RichText::new(text).color(pal.text2));
            resp.on_hover_text("Refuse statements that change data.");
        });

        if cfg.kind == Kind::Sqlite {
            return;
        }
        // Advanced: URL, TLS, CA file and SSH tunnel.
        ui.add_space(2.0);
        let advanced = &mut self.advanced;
        ui.horizontal(|ui| {
            ui.add_space(LABEL_W - 64.0);
            let (rect, resp) = ui.allocate_exact_size(vec2(84.0, 22.0), Sense::click());
            let painter = ui.painter();
            if resp.hovered() {
                painter.rect_filled(rect, style::RADIUS, pal.hover);
            }
            let icon = if *advanced {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            };
            icons::paint(
                painter,
                Rect::from_center_size(pos2(rect.left() + 10.0, rect.center().y), vec2(10.0, 10.0)),
                icon,
                pal.text2,
            );
            let g = style::galley(ui, "Advanced", style::semibold(12.0), pal.text2);
            style::paint_galley_v(ui.painter(), rect, rect.left() + 20.0, g);
            if resp.clicked() {
                *advanced = !*advanced;
            }
        });
        if !*advanced {
            return;
        }
        let url_hint = match cfg.kind {
            Kind::Postgres => "postgres://user@host:5432/db",
            Kind::MySql => "mysql://user@host:3306/db",
            _ => "mongodb://host:27017 or mongodb+srv://…",
        };
        row(ui, "URL", &mut |ui| {
            style::field(
                ui,
                TextEdit::singleline(&mut cfg.url)
                    .hint_text(url_hint)
                    .font(egui::TextStyle::Monospace),
                None,
                FIELD_W,
            );
        });
        row(ui, "TLS", &mut |ui| {
            style::combo("conn-tls", cfg.tls.label(), 160.0).show_ui(ui, |ui| {
                for t in TlsMode::ALL {
                    ui.selectable_value(&mut cfg.tls, t, t.label());
                }
            });
        });
        row(ui, "CA file", &mut |ui| {
            style::field(
                ui,
                TextEdit::singleline(&mut cfg.ca_file).hint_text("PEM file (optional)"),
                None,
                FIELD_W - 92.0,
            );
            if ui.add(Btn::secondary("Choose…").min_width(84.0)).clicked()
                && let Some(p) = rfd::FileDialog::new()
                    .add_filter("PEM", &["pem", "crt", "cer"])
                    .pick_file()
            {
                cfg.ca_file = p.display().to_string();
            }
        });
        row(ui, "SSH tunnel", &mut |ui| {
            style::field(
                ui,
                TextEdit::singleline(&mut cfg.ssh).hint_text("alias or user@host:22 (optional)"),
                None,
                FIELD_W,
            );
        });
        row(ui, "SSH key", &mut |ui| {
            style::field(
                ui,
                TextEdit::singleline(&mut cfg.ssh_key).hint_text("~/.ssh/id_ed25519 (optional)"),
                None,
                FIELD_W - 92.0,
            );
            if ui.add(Btn::secondary("Choose…").min_width(84.0)).clicked() {
                let mut picker = rfd::FileDialog::new();
                if let Some(home) = dirs::home_dir() {
                    picker = picker.set_directory(home.join(".ssh"));
                }
                if let Some(p) = picker.pick_file() {
                    cfg.ssh_key = p.display().to_string();
                }
            }
        });
    }

    fn hints(&self, ui: &mut Ui, pal: &style::Palette) {
        let cfg = &self.cfg;
        let tunnel = !cfg.ssh.trim().is_empty();
        let small = |ui: &mut Ui, text: &str| {
            ui.horizontal(|ui| {
                ui.add_space(LABEL_W + 8.0);
                ui.add(egui::Label::new(RichText::new(text).small().color(pal.text2)).wrap());
            });
        };
        if cfg.kind == Kind::DocumentDb {
            small(
                ui,
                "Amazon DocumentDB needs TLS with the AWS CA bundle. Download it and pick it as the CA file:",
            );
            ui.horizontal(|ui| {
                ui.add_space(LABEL_W + 8.0);
                ui.hyperlink_to(RichText::new(AWS_CA_URL).small(), AWS_CA_URL);
            });
        }
        if cfg.kind.is_document() && tunnel && cfg.tls != TlsMode::Off && cfg.tls != TlsMode::Prefer
        {
            small(
                ui,
                "Through an SSH tunnel, OurDB turns off the certificate checks for TLS.",
            );
        }
        if tunnel && cfg.has_url() {
            small(
                ui,
                "An SSH tunnel needs the host and port fields, not a URL.",
            );
        }
        if cfg.has_url() && cfg.kind != Kind::Sqlite {
            small(
                ui,
                "The URL is set, so the host, user and database fields are off.",
            );
        }
    }
}

fn default_name(cfg: &ConnConfig) -> String {
    if cfg.kind == Kind::Sqlite {
        return std::path::Path::new(&cfg.path)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| "SQLite".into());
    }
    let host = cfg.host_or_default();
    if cfg.database.is_empty() {
        host.to_string()
    } else {
        format!("{}@{host}", cfg.database)
    }
}
