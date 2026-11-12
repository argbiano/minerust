// /wgen: generates every chunk in a square around a point ahead of time, at a steady
// number of chunks per second, so players don't wait for terrain there later.

use crate::{
    players::Color,
    server::Server,
    world::{ChunkSource, Dimension},
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{sync::mpsc, task::JoinSet, time};
use tracing::{info, warn};
use uuid::Uuid;

pub const MAX_RADIUS: i32 = 128;
pub const MAX_CPS: u32 = 1000;
pub const DEFAULT_CPS: u32 = 50;
const REPORT_EVERY: Duration = Duration::from_secs(5);

pub struct Request {
    pub dimension: Dimension,
    /// Center chunk.
    pub center: (i32, i32),
    /// In chunks: the square is `2 * radius + 1` chunks wide.
    pub radius: i32,
    pub cps: u32,
    /// Player who started it, who gets the progress messages too.
    pub requester: Option<Uuid>,
}

#[derive(Clone)]
pub struct Progress {
    pub dimension: Dimension,
    pub center: (i32, i32),
    pub radius: i32,
    pub cps: u32,
    pub done: usize,
    pub failed: usize,
    pub total: usize,
    pub started: Instant,
}

impl Progress {
    pub fn describe(&self) -> String {
        let elapsed = self.started.elapsed().as_secs_f64().max(0.001);
        let rate = self.done as f64 / elapsed;
        let left = self.total - self.done;
        let eta = if rate > 0.0 { format_duration(left as f64 / rate) } else { "?".into() };
        format!(
            "{}/{} chunks ({:.1}%), {:.1} chunks/s, about {eta} left",
            self.done,
            self.total,
            self.done as f64 * 100.0 / self.total as f64,
            rate
        )
    }
}

enum Command {
    Start(Request),
    Stop,
}

pub struct Handle {
    tx: mpsc::UnboundedSender<Command>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<Command>>>,
    progress: Mutex<Option<Progress>>,
}

impl Default for Handle {
    fn default() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self { tx, rx: Mutex::new(Some(rx)), progress: Mutex::new(None) }
    }
}

impl Handle {
    /// Queues a run unless one is going; returns the number of chunks it covers.
    pub fn start(&self, request: Request) -> Result<usize, String> {
        let mut progress = self.progress.lock().unwrap();
        if progress.is_some() {
            return Err("A world generation is already running. Use /wgen stop first.".into());
        }
        let side = (2 * request.radius + 1) as usize;
        let total = side * side;
        *progress = Some(Progress {
            dimension: request.dimension,
            center: request.center,
            radius: request.radius,
            cps: request.cps,
            done: 0,
            failed: 0,
            total,
            started: Instant::now(),
        });
        self.tx.send(Command::Start(request)).map_err(|_| "The world generation task isn't running.".to_owned())?;
        Ok(total)
    }

    /// Returns false when nothing was running.
    pub fn stop(&self) -> bool {
        let running = self.progress.lock().unwrap().is_some();
        if running {
            let _ = self.tx.send(Command::Stop);
        }
        running
    }

    pub fn status(&self) -> Option<Progress> {
        self.progress.lock().unwrap().clone()
    }

    fn update(&self, f: impl FnOnce(&mut Progress)) {
        if let Some(p) = self.progress.lock().unwrap().as_mut() {
            f(p);
        }
    }
}

enum Outcome {
    Done,
    Stopped,
}

pub async fn run(server: Arc<Server>) {
    let Some(mut rx) = server.wgen.rx.lock().unwrap().take() else {
        warn!("world generation task already running");
        return;
    };
    while let Some(command) = rx.recv().await {
        // A stop with nothing running is a no-op.
        let Command::Start(request) = command else { continue };
        let outcome = generate(&server, &request, &mut rx).await;
        let progress = server.wgen.progress.lock().unwrap().take();
        let Some(progress) = progress else { continue };
        let elapsed = format_duration(progress.started.elapsed().as_secs_f64());
        let failed =
            if progress.failed > 0 { format!(", {} failed (see the log)", progress.failed) } else { String::new() };
        let text = match outcome {
            Outcome::Done => format!("[wgen] Finished: {} chunks in {elapsed}{failed}", progress.done),
            Outcome::Stopped => format!("[wgen] Stopped after {}/{} chunks{failed}", progress.done, progress.total),
        };
        report(&server, request.requester, &text, Color::Green);
    }
}

async fn generate(server: &Arc<Server>, request: &Request, rx: &mut mpsc::UnboundedReceiver<Command>) -> Outcome {
    let chunks = spiral(request.center, request.radius);
    let source: Arc<dyn ChunkSource> = server.level(request.dimension).source.clone();
    // Enough requests in flight to keep every worldgen thread busy, but not an unbounded queue.
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get());
    let max_in_flight = threads * 4;

    let mut pace = time::interval(Duration::from_secs_f64(1.0 / request.cps.max(1) as f64));
    pace.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
    let mut reports = time::interval_at(time::Instant::now() + REPORT_EVERY, REPORT_EVERY);
    let mut in_flight: JoinSet<anyhow::Result<()>> = JoinSet::new();
    let mut next = 0;

    loop {
        if next >= chunks.len() && in_flight.is_empty() {
            return Outcome::Done;
        }
        tokio::select! {
            _ = pace.tick(), if next < chunks.len() && in_flight.len() < max_in_flight => {
                let (x, z) = chunks[next];
                next += 1;
                let source = source.clone();
                in_flight.spawn(async move { source.get(x, z).await.map(|_| ()) });
            }
            Some(result) = in_flight.join_next() => {
                let failed = !matches!(result, Ok(Ok(())));
                if let Ok(Err(e)) = &result {
                    warn!("[wgen] {e:#}");
                }
                server.wgen.update(|p| {
                    p.done += 1;
                    p.failed += failed as usize;
                });
            }
            command = rx.recv() => {
                if matches!(command, Some(Command::Stop) | None) {
                    in_flight.abort_all();
                    return Outcome::Stopped;
                }
            }
            _ = reports.tick() => {
                if let Some(p) = server.wgen.status() {
                    report(server, request.requester, &format!("[wgen] {}", p.describe()), Color::Gray);
                }
            }
        }
    }
}

/// Every chunk of the square, nearest the center first.
fn spiral((cx, cz): (i32, i32), radius: i32) -> Vec<(i32, i32)> {
    let mut chunks: Vec<(i32, i32)> =
        (-radius..=radius).flat_map(|dz| (-radius..=radius).map(move |dx| (dx, dz))).collect();
    chunks.sort_by_key(|&(dx, dz)| (dx.abs().max(dz.abs()), dx * dx + dz * dz));
    chunks.into_iter().map(|(dx, dz)| (cx + dx, cz + dz)).collect()
}

fn report(server: &Server, requester: Option<Uuid>, text: &str, color: Color) {
    info!("{text}");
    if let Some(player) = requester.and_then(|uuid| server.players.get(uuid)) {
        player.send(crate::players::Event::Message(text.to_owned(), color));
    }
}

/// Rough memory a finished chunk of this height takes: 2 bytes per block plus biomes.
pub fn chunk_bytes(height: u32) -> usize {
    (height as usize / 16) * (4096 * 2 + 64 * 2)
}

pub fn format_bytes(bytes: usize) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
    } else {
        format!("{} MB", bytes.div_ceil(1 << 20))
    }
}

pub fn format_duration(seconds: f64) -> String {
    let s = seconds.round() as u64;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spiral_covers_the_square_nearest_first() {
        let chunks = spiral((10, -4), 2);
        assert_eq!(chunks.len(), 25);
        assert_eq!(chunks[0], (10, -4));
        assert!(chunks[1..9].iter().all(|&(x, z)| (x - 10).abs() <= 1 && (z + 4).abs() <= 1));
        let mut unique = chunks.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 25);
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(42.4), "42s");
        assert_eq!(format_duration(185.0), "3m 05s");
        assert_eq!(format_duration(7300.0), "2h 01m");
        assert_eq!(format_bytes(169 * chunk_bytes(384)), "33 MB");
        assert_eq!(format_bytes(66049 * chunk_bytes(384)), "12.3 GB");
    }

    #[test]
    fn only_one_run_at_a_time() {
        let handle = Handle::default();
        let request =
            || Request { dimension: Dimension::Overworld, center: (0, 0), radius: 3, cps: 10, requester: None };
        assert_eq!(handle.start(request()), Ok(49));
        assert!(handle.start(request()).is_err());
        assert!(handle.stop());
    }
}
