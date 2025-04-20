use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time::sleep;

use crate::SHOULD_STOP;

use super::Server;

pub struct Ticker {
    tick_interval: Duration,
    last_tick: Instant,
    /// nanoseconds per tick
    pub nanos: Arc<RwLock<Vec<u64>>>
}

impl Ticker {
    #[must_use]
    pub fn new(tps: f32) -> Self {
        Self {
            tick_interval: Duration::from_millis((1000.0 / tps) as u64),
            last_tick: Instant::now(),
            nanos: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// IMPORTANT: Run this in a new thread/tokio task.
    pub async fn run(&mut self, server: &Server) {
        while !SHOULD_STOP.load(std::sync::atomic::Ordering::Relaxed) {
            let now = Instant::now();
            let elapsed = now - self.last_tick;

            if elapsed >= self.tick_interval {
                let tick_start = Instant::now();
                server.tick().await;
                let tick_duration = tick_start.elapsed();
                let nanos = tick_duration.as_nanos() as u64;
                let mut nano_vec = self.nanos.write().await;
                nano_vec.push(nanos);
                if nano_vec.len() > 1200 {
                    nano_vec.remove(0);
                }
                self.last_tick = now;
            } else {
                // Wait for the remaining time until the next tick.
                let sleep_time = self.tick_interval - elapsed;
                sleep(sleep_time).await;
            }
        }
        log::debug!("Ticker stopped");
    }
}
