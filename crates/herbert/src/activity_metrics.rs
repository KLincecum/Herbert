//! Execution timing uses a monotonic clock. Request and write totals can overlap.
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Instant};

pub(crate) struct Metrics {
    started: Instant,
    finished: Option<u64>,
    requests: u64,
    request_ms: u128,
    received_bytes: usize,
    write_ms: u128,
    checkpoint_ms: u128,
    pages: usize,
    plugins: BTreeMap<String, (u64, Option<u64>)>,
    buffers: BTreeMap<String, BufferCounts>,
    peak_buffer_bytes: u64,
    spilled_pages: u64,
}
#[derive(Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BufferCounts { memory_pages: u64, disk_pages: u64, memory_bytes: u64, disk_bytes: u64 }
impl Default for Metrics {
    fn default() -> Self {
        Self { started: Instant::now(), finished: None, requests: 0, request_ms: 0,
            received_bytes: 0, write_ms: 0, checkpoint_ms: 0, pages: 0, plugins: BTreeMap::new(),
            buffers: BTreeMap::new(), peak_buffer_bytes: 0, spilled_pages: 0 }
    }
}
impl Metrics {
    pub fn elapsed(&self) -> u64 { self.finished.unwrap_or_else(|| self.started.elapsed().as_millis() as u64) }
    pub fn finish(&mut self) { self.finished = Some(self.elapsed()); }
    pub fn plugin(&mut self, category: &str, state: &str) {
        let now = self.elapsed();
        if state == "running" { self.plugins.entry(category.into()).or_insert((now, None)); }
        else if matches!(state, "completed" | "incomplete" | "failed" | "canceled") {
            if let Some((_, end)) = self.plugins.get_mut(category) { *end = Some(now); }
        }
    }
    pub fn response(&mut self, ms: u128, bytes: usize) {
        self.requests += 1; self.request_ms += ms; self.received_bytes += bytes;
    }
    pub fn page(&mut self, write_ms: u128, checkpoint_ms: u128) {
        self.pages += 1; self.write_ms += write_ms; self.checkpoint_ms += checkpoint_ms;
    }
    pub fn buffer(&mut self, category: &str, bytes: u64, disk: bool, queued: bool) {
        let counts = self.buffers.entry(category.into()).or_default();
        let (pages, size) = if disk { (&mut counts.disk_pages, &mut counts.disk_bytes) }
            else { (&mut counts.memory_pages, &mut counts.memory_bytes) };
        if queued { *pages += 1; *size += bytes; self.spilled_pages += u64::from(disk); }
        else { *pages = pages.saturating_sub(1); *size = size.saturating_sub(bytes); }
        self.peak_buffer_bytes = self.peak_buffer_bytes.max(self.buffers.values().map(|b| b.memory_bytes).sum());
    }
    pub fn clear_buffer(&mut self, category: &str) { self.buffers.remove(category); }
    pub fn buffer_label(&self, category: &str) -> String {
        match self.buffers.get(category) {
            Some(b) if b.disk_pages > 0 => format!("Downloaded pages waiting: {} in memory, {} on disk", b.memory_pages, b.disk_pages),
            Some(b) => format!("Downloaded pages waiting in memory: {}", b.memory_pages),
            None => "Fetching pages while processing continues".into(),
        }
    }
    pub fn snapshot(&self) -> Value {
        let elapsed = self.elapsed();
        let plugins: BTreeMap<_, _> = self.plugins.iter().map(|(id, (start, end))|
            (id, end.unwrap_or(elapsed).saturating_sub(*start))).collect();
        json!({"elapsedMs":elapsed,"finished":self.finished.is_some(),
            "requests":self.requests,"requestMs":self.request_ms,"receivedBytes":self.received_bytes,
            "pageWriteMs":self.write_ms,"checkpointMs":self.checkpoint_ms,"pages":self.pages,
            "pluginElapsedMs":plugins,"pageBuffers":self.buffers,
            "peakBufferedMemoryBytesEstimate":self.peak_buffer_bytes,"spilledPages":self.spilled_pages})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completed_statistics_are_frozen_and_parallel_totals_are_not_wall_time() {
        let mut m = Metrics::default();
        m.plugin("kinetic-personalizations", "running"); m.response(1200, 2048); m.response(800, 1024);
        m.page(3000, 40); m.plugin("kinetic-personalizations", "completed"); m.finish();
        let before = m.snapshot();
        assert_eq!(before["requests"], 2); assert_eq!(before["requestMs"], 2000);
        assert_eq!(before["receivedBytes"], 3072); assert_eq!(before["pageWriteMs"], 3000);
        assert_eq!(before["pages"], 1); assert_eq!(before["finished"], true);
        m.started = Instant::now() - std::time::Duration::from_secs(100);
        assert_eq!(m.snapshot(), before);
    }
}
