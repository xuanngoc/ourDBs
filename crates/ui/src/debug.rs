//! Debug hooks from env vars (see the list in lib.rs).

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use eframe::egui;

pub enum Step {
    Connect(String),
    Open(String, String),
    Query(String),
    Select(usize, usize),
    Edit(usize, usize, String),
    Delete(usize),
    AddRow,
    DialogNew,
    DialogEdit(String),
    Save,
    Apply,
    Sql,
}

pub struct Debug {
    pub screenshot: Option<PathBuf>,
    steps: VecDeque<Step>,
    last_step: Instant,
    idle_frames: u32,
    /// When the screenshot was requested.
    shot_sent: Option<Instant>,
    shot_done: Arc<AtomicBool>,
}

impl Debug {
    pub fn from_env() -> Self {
        let screenshot = std::env::var_os("OURDB_SCREENSHOT")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        let steps = std::env::var("OURDB_AUTO")
            .map(|s| parse_steps(&s))
            .unwrap_or_default();
        Self {
            screenshot,
            steps,
            last_step: Instant::now(),
            idle_frames: 0,
            shot_sent: None,
            shot_done: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The next step, once the app is idle for a few frames.
    pub fn next_step(&mut self, ctx: &egui::Context, busy: bool) -> Option<Step> {
        if !self.steps.is_empty() {
            // Keep frames coming while steps wait.
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if busy {
            self.idle_frames = 0;
            return None;
        }
        self.idle_frames += 1;
        if self.idle_frames < 3 {
            return None;
        }
        let step = self.steps.pop_front()?;
        self.idle_frames = 0;
        self.last_step = Instant::now();
        Some(step)
    }

    /// Take the screenshot when all steps ran and nothing loads (or after 5 s), then close.
    pub fn tick(&mut self, ctx: &egui::Context, busy: bool) {
        let Some(path) = self.screenshot.clone() else {
            return;
        };
        if self.shot_done.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(50));
        let settled = self.steps.is_empty() && !busy && self.idle_frames >= 8;
        let timeout = self.last_step.elapsed() > Duration::from_secs(5);
        if let Some(at) = self.shot_sent {
            // macOS does not draw a hidden or covered window, so the
            // screenshot can never come. Quit instead of hanging.
            if at.elapsed() > Duration::from_secs(10) {
                eprintln!("OurDB: no screenshot after 10 s (window hidden?)");
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        if !(settled || timeout) {
            return;
        }
        self.shot_sent = Some(Instant::now());
        let done = self.shot_done.clone();
        let ctx2 = ctx.clone();
        ctx.request_screenshot(move |image| {
            if let Err(e) = save_png(&path, &image) {
                eprintln!("OurDB: cannot save screenshot {}: {e}", path.display());
            }
            done.store(true, Ordering::SeqCst);
            ctx2.request_repaint();
        });
    }
}

fn parse_steps(text: &str) -> VecDeque<Step> {
    let mut out = VecDeque::new();
    // ";;" stands for a literal ";" (for example between SQL statements).
    let text = text.replace(";;", "\u{0}");
    for raw in text.split(';') {
        let raw = raw.replace('\u{0}', ";");
        let s = raw.trim();
        if s.is_empty() {
            continue;
        }
        let (name, arg) = s.split_once(':').unwrap_or((s, ""));
        let nums = |n: usize| -> Option<Vec<usize>> {
            let v: Vec<usize> = arg
                .splitn(n, ',')
                .take(n)
                .filter_map(|x| x.trim().parse().ok())
                .collect();
            (v.len() == n).then_some(v)
        };
        let step = match name {
            "connect" => Some(Step::Connect(arg.to_string())),
            "open" => arg
                .split_once('.')
                .map(|(ns, obj)| Step::Open(ns.to_string(), obj.to_string())),
            "query" => Some(Step::Query(arg.to_string())),
            "select" => nums(2).map(|v| Step::Select(v[0], v[1])),
            "edit" => {
                let mut parts = arg.splitn(3, ',');
                match (
                    parts.next().map(str::parse),
                    parts.next().map(str::parse),
                    parts.next(),
                ) {
                    (Some(Ok(r)), Some(Ok(c)), Some(text)) => {
                        Some(Step::Edit(r, c, text.to_string()))
                    }
                    _ => None,
                }
            }
            "delete" => arg.trim().parse().ok().map(Step::Delete),
            "addrow" => Some(Step::AddRow),
            "dialog" if arg == "new" => Some(Step::DialogNew),
            "dialog" => arg
                .strip_prefix("edit:")
                .map(|n| Step::DialogEdit(n.to_string())),
            "save" => Some(Step::Save),
            "apply" => Some(Step::Apply),
            "sql" => Some(Step::Sql),
            _ => None,
        };
        match step {
            Some(st) => out.push_back(st),
            None => eprintln!("OurDB: unknown OURDB_AUTO step \"{s}\""),
        }
    }
    out
}

fn save_png(path: &PathBuf, image: &egui::ColorImage) -> Result<(), String> {
    let [w, h] = image.size;
    let mut data = Vec::with_capacity(w * h * 4);
    for px in &image.pixels {
        data.extend_from_slice(&px.to_srgba_unmultiplied());
    }
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&data).map_err(|e| e.to_string())
}
