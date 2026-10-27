//! Minute-resolution telemetry history.
//!
//! The live socket is 1 Hz, which is right for "what is happening now" and wrong
//! for "what happened this morning". This keeps a rolling ring buffer of
//! one-minute averages (a full day) so the dashboard can draw a 1h/6h/24h curve
//! without the browser having to stay open.
//!
//! It is deliberately in memory: history is a view of this process's uptime, and
//! a restart legitimately starts an empty graph.

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

use serde::Serialize;

/// One minute between retained points.
pub const INTERVAL_MS: u64 = 60 * 1000;
/// A full day at one point per minute.
pub const CAPACITY: usize = 1440;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct HistoryPoint {
    pub ts_ms: u64,
    pub cpu_pct: f32,
    pub mem_pct: f32,
    /// Aggregate bytes/second across every interface at that minute.
    pub rx_bps: u64,
    pub tx_bps: u64,
}

/// A shared, bounded ring buffer. Cheap to clone (one `Arc`).
#[derive(Clone, Default)]
pub struct History {
    points: Arc<RwLock<VecDeque<HistoryPoint>>>,
}

impl History {
    pub fn new() -> Self {
        Self {
            points: Arc::new(RwLock::new(VecDeque::with_capacity(CAPACITY))),
        }
    }

    pub fn push(&self, point: HistoryPoint) {
        let mut points = self.points.write().expect("history lock");
        points.push_back(point);
        // The buffer is a ring: the oldest minute falls off the front.
        while points.len() > CAPACITY {
            points.pop_front();
        }
    }

    /// Points at or after `from_ms`, oldest first.
    pub fn since(&self, from_ms: u64) -> Vec<HistoryPoint> {
        let points = self.points.read().expect("history lock");
        points
            .iter()
            .filter(|point| point.ts_ms >= from_ms)
            .copied()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.points.read().expect("history lock").len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(ts_ms: u64) -> HistoryPoint {
        HistoryPoint {
            ts_ms,
            cpu_pct: 1.0,
            mem_pct: 2.0,
            rx_bps: 3,
            tx_bps: 4,
        }
    }

    #[test]
    fn keeps_only_the_last_day() {
        let history = History::new();
        for index in 0..(CAPACITY + 10) {
            history.push(point(index as u64));
        }
        assert_eq!(history.len(), CAPACITY);
        // The oldest retained point is the one after the first ten.
        assert_eq!(history.since(0).first().map(|p| p.ts_ms), Some(10));
    }

    #[test]
    fn since_filters_by_time() {
        let history = History::new();
        for index in 1..=5 {
            history.push(point(index * 10));
        }
        let recent = history.since(30);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].ts_ms, 30);
        assert_eq!(recent[2].ts_ms, 50);
    }
}
