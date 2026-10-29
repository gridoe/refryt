//! A runtime counter overlay, so a performance regression is visible instead of inferred.
//!
//! This exists because the M3 additions quietly dropped list virtualisation and let the texture
//! cache grow by entry count rather than by bytes, and nothing in the project could see it:
//! `just perf` measured an app that had loaded nothing at all. The number that makes that class
//! of bug obvious is **rows drawn vs. rows in the list** — if it tracks the list length, the list
//! is not virtualised.
//!
//! Off by default and free when off: the counters are a handful of integer stores, and nothing is
//! painted or formatted unless the overlay is up.

use std::time::{Duration, Instant};

use egui::{Align2, Color32, CornerRadius};

use crate::theme;

/// How many frame times to keep for the rolling average.
const WINDOW: usize = 120;

pub struct Stats {
    pub enabled: bool,
    frame_times: Vec<f32>,
    next: usize,
    frames: u32,
    second_started: Instant,
    fps: u32,
    frame_started: Option<Instant>,
    /// Rows actually drawn this frame, summed across every virtualised list.
    pub rows_drawn: usize,
    /// Rows those lists contain. Equal to `rows_drawn` means nothing is being skipped.
    pub rows_total: usize,
}

/// One line of counters, for `--stats-after` and for the overlay.
#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    pub frame_ms: f32,
    pub fps: u32,
    pub textures: usize,
    pub texture_bytes: usize,
    pub inflight: usize,
    pub rows_drawn: usize,
    pub rows_total: usize,
    /// The glyph atlas's width and height in pixels: 4 bytes a pixel in memory, and again on the
    /// GPU, and nothing else here sees it.
    pub atlas: [usize; 2],
}

impl Snapshot {
    /// Machine-readable, one line, stable field order — `scripts/perf.sh` parses this.
    pub fn print(&self) {
        println!(
            "stats frame_ms={:.2} fps={} textures={} texture_bytes={} inflight={} \
             rows_drawn={} rows_total={} atlas={}x{}",
            self.frame_ms,
            self.fps,
            self.textures,
            self.texture_bytes,
            self.inflight,
            self.rows_drawn,
            self.rows_total,
            self.atlas[0],
            self.atlas[1],
        );
    }
}

impl Stats {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            frame_times: Vec::with_capacity(WINDOW),
            next: 0,
            frames: 0,
            second_started: Instant::now(),
            fps: 0,
            frame_started: None,
            rows_drawn: 0,
            rows_total: 0,
        }
    }

    /// Call once at the top of a frame. Resets the row counters, so a list that stops being drawn
    /// stops being counted. Everything that reads them — the overlay and `--stats-after` — runs
    /// after the panels, so they describe the frame in progress.
    ///
    /// Paired with [`Self::end_frame`], which is what actually records the cost.
    pub fn begin_frame(&mut self) {
        self.rows_drawn = 0;
        self.rows_total = 0;
        self.frame_started = Some(Instant::now());

        self.frames += 1;
        if self.second_started.elapsed() >= Duration::from_secs(1) {
            self.fps = self.frames;
            self.frames = 0;
            self.second_started = Instant::now();
        }
    }

    /// Close the frame and record what it cost.
    ///
    /// This times the body of `ui()`, not the gap between frames. For an app that deliberately
    /// repaints a few times a second the interval is meaningless — it would just report our own
    /// repaint schedule back to us — while the cost is the number that says whether the UI is
    /// doing too much work.
    pub fn end_frame(&mut self) {
        let Some(started) = self.frame_started.take() else {
            return;
        };
        let dt = started.elapsed().as_secs_f32();
        if self.frame_times.len() < WINDOW {
            self.frame_times.push(dt);
        } else {
            self.frame_times[self.next] = dt;
            self.next = (self.next + 1) % WINDOW;
        }
    }

    /// A list reports what it drew and what it holds.
    pub fn rows(&mut self, drawn: usize, total: usize) {
        self.rows_drawn += drawn;
        self.rows_total += total;
    }

    fn mean_frame_ms(&self) -> f32 {
        if self.frame_times.is_empty() {
            return 0.0;
        }
        self.frame_times.iter().sum::<f32>() / self.frame_times.len() as f32 * 1000.0
    }

    pub fn snapshot(&self, ctx: &egui::Context, images: &crate::images::Images) -> Snapshot {
        Snapshot {
            frame_ms: self.mean_frame_ms(),
            fps: self.fps,
            textures: images.len(),
            texture_bytes: images.bytes(),
            inflight: images.inflight(),
            rows_drawn: self.rows_drawn,
            rows_total: self.rows_total,
            atlas: ctx.fonts(|f| f.font_image_size()),
        }
    }

    /// Paint the overlay in the top-right corner of the window.
    pub fn overlay(&self, ui: &egui::Ui, palette: &theme::Palette, images: &crate::images::Images) {
        if !self.enabled {
            return;
        }
        let s = self.snapshot(ui.ctx(), images);
        let mb = |b: usize| b as f32 / (1024.0 * 1024.0);
        let lines = [
            format!("{:.2} ms   {} fps", s.frame_ms, s.fps),
            format!("{} textures   {:.1} MB", s.textures, mb(s.texture_bytes)),
            format!("{} fetches in flight", s.inflight),
            format!("rows {} / {}", s.rows_drawn, s.rows_total),
            format!("glyph atlas {}x{}", s.atlas[0], s.atlas[1]),
        ];

        let font = theme::regular(11.0);
        let width = 172.0;
        let height = lines.len() as f32 * 15.0 + 12.0;
        let anchor = ui.ctx().content_rect();
        let rect = egui::Rect::from_min_size(
            egui::pos2(
                anchor.right() - width - 12.0,
                anchor.top() + theme::TITLEBAR_HEIGHT + 12.0,
            ),
            egui::vec2(width, height),
        );

        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("ytm-stats"),
        ));
        painter.rect_filled(
            rect,
            CornerRadius::same(theme::radii().popup),
            Color32::from_black_alpha(210),
        );
        for (i, line) in lines.iter().enumerate() {
            painter.text(
                egui::pos2(rect.left() + 10.0, rect.top() + 12.0 + i as f32 * 15.0),
                Align2::LEFT_CENTER,
                line,
                font.clone(),
                if i == 3 && s.rows_total > 0 && s.rows_drawn == s.rows_total {
                    // Drawing every row means the list is not virtualised. Say so loudly.
                    palette.danger
                } else {
                    palette.text
                },
            );
        }
    }
}
