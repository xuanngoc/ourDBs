//! OurDB user interface (egui/eframe).
//!
//! Debug hooks (env vars):
//! - `OURDB_CONFIG_DIR=<dir>`: read and write connections.json and history.json
//!   in this folder. Passwords then stay in memory (never in the Keychain).
//! - `OURDB_SCREENSHOT=<file.png>`: when nothing loads (or after 5 s), save a
//!   screenshot of the window as PNG and quit.
//! - `OURDB_AUTO=<steps>`: steps separated by `;`, each one runs after the one
//!   before has finished loading:
//!   `connect:<name>`, `open:<namespace>.<object>`, `query:<text>`,
//!   `select:<row>,<col>`, `edit:<row>,<col>,<text>`, `delete:<row>`, `addrow`,
//!   `dialog:new`, `dialog:edit:<name>`, `save` (open the review), `apply`,
//!   `sql` (press the SQL button of the table tab).
//!   Write `;;` for a `;` inside a step.
//! - `OURDB_THEME=dark|light`: force a theme (default: follow the system).

mod app;
mod conn_dialog;
mod debug;
mod grid;
mod icons;
mod query_tab;
mod sidebar;
mod store;
mod style;
mod table_tab;
mod task;
mod value_panel;

use eframe::egui;
use ourdb_core::Connector;

/// Open the main window. Blocks until the window closes.
pub fn run(connector: Connector) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("OurDB")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([760.0, 480.0])
            // macOS: one unified bar. The content goes under a transparent title
            // bar, so the traffic lights sit over the top of the sidebar.
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false),
        ..Default::default()
    };
    eframe::run_native(
        "OurDB",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc, connector)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
