//! Background work: every driver call runs on the tokio runtime and sends a
//! [`Reply`] back to the UI thread over a std channel.

use std::{
    future::Future,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::Sender,
    },
    time::Duration,
};

use eframe::egui;
use ourdb_core::{ColumnInfo, ObjectInfo, Page, ResultSet, Session};
use tokio::task::AbortHandle;

/// A new request number. Replies with an old number are dropped.
pub fn next_req() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Error text for the user: the message chain, no Rust debug output.
pub fn err_text(e: anyhow::Error) -> String {
    format!("{e:#}")
}

pub enum Reply {
    Connected {
        conn: String,
        req: u64,
        result: Result<Session, String>,
    },
    Tested {
        req: u64,
        result: Result<(), String>,
    },
    Databases {
        conn: String,
        session: u64,
        result: Result<Vec<String>, String>,
    },
    Namespaces {
        conn: String,
        session: u64,
        result: Result<Vec<String>, String>,
    },
    Objects {
        conn: String,
        session: u64,
        ns: String,
        result: Result<Vec<ObjectInfo>, String>,
    },
    Tab {
        tab: u64,
        req: u64,
        data: TabData,
    },
}

pub enum TabData {
    Page(Result<Page, String>),
    Count(Result<u64, String>),
    Columns(Result<Vec<ColumnInfo>, String>),
    Applied(Result<u64, String>),
    Executed(Result<Vec<ResultSet>, String>, Duration),
}

/// Cheap handle to start background work from anywhere in the UI.
#[derive(Clone)]
pub struct Spawner {
    pub rt: tokio::runtime::Handle,
    pub tx: Sender<Reply>,
    pub ctx: egui::Context,
}

impl Spawner {
    /// Run `fut` and send its reply to the UI thread.
    pub fn spawn<F>(&self, fut: F) -> AbortHandle
    where
        F: Future<Output = Reply> + Send + 'static,
    {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        self.rt
            .spawn(async move {
                let reply = fut.await;
                let _ = tx.send(reply);
                ctx.request_repaint();
            })
            .abort_handle()
    }

    /// Run `fut` without a reply (close, cancel).
    pub fn detach<F>(&self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.rt.spawn(fut);
    }
}
