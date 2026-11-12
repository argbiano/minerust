// Generated chunks, cached in memory. Requests for the same chunk while it is being
// generated share one job; idle chunks are dropped and regenerated if asked for again.

use super::{Chunk, ChunkSource, Generator, WorldInfo};
use anyhow::{Result, anyhow};
use crossbeam_channel::{Receiver, Sender};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use tracing::{debug, error};

type Waiter = oneshot::Sender<Result<Arc<Chunk>, String>>;

struct Loaded {
    chunk: Arc<Chunk>,
    last_used: Instant,
}

#[derive(Default)]
struct State {
    loaded: HashMap<(i32, i32), Loaded>,
    pending: HashMap<(i32, i32), Vec<Waiter>>,
}

pub struct ChunkStore {
    generator: Arc<dyn Generator>,
    info: WorldInfo,
    state: Arc<Mutex<State>>,
    jobs: Sender<(i32, i32)>,
}

impl ChunkStore {
    pub fn new(generator: Arc<dyn Generator>, threads: usize) -> Self {
        let (jobs, queue) = crossbeam_channel::unbounded();
        let state = Arc::new(Mutex::new(State::default()));
        for i in 0..threads.max(1) {
            let worker = generator.worker();
            let queue: Receiver<(i32, i32)> = queue.clone();
            let state = state.clone();
            thread::Builder::new()
                .name(format!("worldgen-{i}"))
                .spawn(move || run_worker(worker, queue, state))
                .expect("spawning a worldgen thread");
        }
        let info = generator.info();
        Self { generator, info, state, jobs }
    }

    async fn load(&self, x: i32, z: i32) -> Result<Arc<Chunk>> {
        let rx = {
            let mut state = self.state.lock().unwrap();
            if let Some(loaded) = state.loaded.get_mut(&(x, z)) {
                loaded.last_used = Instant::now();
                return Ok(loaded.chunk.clone());
            }
            let (tx, rx) = oneshot::channel();
            let waiters = state.pending.entry((x, z)).or_default();
            waiters.push(tx);
            if waiters.len() == 1 {
                self.jobs.send((x, z)).map_err(|_| anyhow!("worldgen threads are gone"))?;
            }
            rx
        };
        rx.await.map_err(|_| anyhow!("worldgen dropped chunk {x} {z}"))?.map_err(|e| anyhow!(e))
    }

    pub fn loaded_count(&self) -> usize {
        self.state.lock().unwrap().loaded.len()
    }

    /// Drops chunks nobody asked for within `idle`.
    fn evict(&self, idle: Duration) -> usize {
        let mut state = self.state.lock().unwrap();
        let before = state.loaded.len();
        state.loaded.retain(|_, l| l.last_used.elapsed() < idle);
        before - state.loaded.len()
    }
}

#[async_trait::async_trait]
impl ChunkSource for ChunkStore {
    async fn get(&self, x: i32, z: i32) -> Result<Arc<Chunk>> {
        self.load(x, z).await
    }

    fn info(&self) -> &WorldInfo {
        &self.info
    }

    async fn spawn_point(&self) -> Result<[f64; 3]> {
        self.generator.spawn_point()
    }

    fn evict_idle(&self, idle: Duration) -> usize {
        self.evict(idle)
    }
}

fn run_worker(mut worker: Box<dyn super::Worker>, queue: Receiver<(i32, i32)>, state: Arc<Mutex<State>>) {
    while let Ok((x, z)) = queue.recv() {
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| worker.generate(x, z))) {
            Ok(Ok(chunk)) => Ok(Arc::new(chunk)),
            Ok(Err(e)) => Err(format!("{e:#}")),
            Err(_) => Err("generator panicked".to_owned()),
        };
        if let Err(e) = &result {
            error!("generating chunk {x} {z}: {e}");
        }

        let waiters = {
            let mut state = state.lock().unwrap();
            if let Ok(chunk) = &result {
                state.loaded.insert((x, z), Loaded { chunk: chunk.clone(), last_used: Instant::now() });
            }
            state.pending.remove(&(x, z)).unwrap_or_default()
        };
        for waiter in waiters {
            let _ = waiter.send(result.clone());
        }
    }
    debug!("worldgen thread exiting");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::WorldInfo;
    use crate::world::{Worker, flat::FlatGenerator};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting {
        inner: FlatGenerator,
        calls: Arc<AtomicUsize>,
    }

    struct CountingWorker {
        inner: Box<dyn Worker>,
        calls: Arc<AtomicUsize>,
    }

    impl Worker for CountingWorker {
        fn generate(&mut self, x: i32, z: i32) -> Result<Chunk> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(20));
            self.inner.generate(x, z)
        }
    }

    impl Generator for Counting {
        fn worker(&self) -> Box<dyn Worker> {
            Box::new(CountingWorker { inner: self.inner.worker(), calls: self.calls.clone() })
        }
        fn info(&self) -> WorldInfo {
            self.inner.info()
        }
        fn spawn_point(&self) -> Result<[f64; 3]> {
            self.inner.spawn_point()
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_requests_share_one_job() {
        let calls = Arc::new(AtomicUsize::new(0));
        let store =
            Arc::new(ChunkStore::new(Arc::new(Counting { inner: FlatGenerator::classic(), calls: calls.clone() }), 4));

        let mut tasks = Vec::new();
        for i in 0..32 {
            let store = store.clone();
            tasks.push(tokio::spawn(async move { store.get(i % 8, 0).await.unwrap() }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 8);
        assert_eq!(store.loaded_count(), 8);

        store.get(0, 0).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 8, "cached chunk was generated again");
        assert_eq!(store.evict(Duration::ZERO), 8);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn generation_runs_in_parallel() {
        let calls = Arc::new(AtomicUsize::new(0));
        let store = Arc::new(ChunkStore::new(Arc::new(Counting { inner: FlatGenerator::classic(), calls }), 4));
        let started = Instant::now();
        let requests: Vec<_> = (0..8)
            .map(|i| {
                let store = store.clone();
                tokio::spawn(async move { store.get(i, 1).await.unwrap() })
            })
            .collect();
        for r in requests {
            r.await.unwrap();
        }
        // 8 chunks × 20 ms on 4 threads is ~40 ms; one thread would need 160 ms.
        assert!(started.elapsed() < Duration::from_millis(120), "took {:?}", started.elapsed());
    }
}
