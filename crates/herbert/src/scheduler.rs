//! One fair request budget shared by collection and authentication.
//! A slot covers the response lifetime, rather than only the initial dispatch.
use herbert_contract::Diagnostic;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{collections::VecDeque, sync::Mutex};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

struct State {
    ceiling: u8,
    active: usize,
    next_ticket: u64,
    waiting: VecDeque<u64>,
    interval: Duration,
    last_dispatch: Option<Instant>,
    cooldown: Option<(Instant, Duration)>,
}
pub struct Budget {
    state: Mutex<State>,
    changed: Notify,
}
pub struct Permit {
    budget: Arc<Budget>,
}
// A request future can be dropped by a transport or task abort without its
// cancellation token firing. Its queue position must not strand later work.
struct Waiting {
    budget: Arc<Budget>,
    ticket: u64,
}
impl Drop for Waiting {
    fn drop(&mut self) {
        if let Ok(mut state) = self.budget.state.lock() {
            state.waiting.retain(|ticket| *ticket != self.ticket);
        }
        self.budget.changed.notify_waiters();
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        if let Ok(mut state) = self.budget.state.lock() {
            state.active = state.active.saturating_sub(1);
        }
        self.budget.changed.notify_waiters();
    }
}
impl Budget {
    pub fn new(ceiling: u8) -> Result<Arc<Self>, Diagnostic> {
        if ceiling == 0 {
            return Err(Diagnostic::new(
                "INVALID_CONCURRENCY",
                "Simultaneous plugins must be a whole number from 1 to 255.",
            ));
        }
        Ok(Self::with_ceiling(ceiling))
    }
    /// Serialize lease control traffic independently of the download setting.
    pub(crate) fn lease_control() -> Arc<Self> {
        Self::with_ceiling(1)
    }
    fn with_ceiling(ceiling: u8) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                ceiling,
                active: 0,
                next_ticket: 0,
                waiting: VecDeque::new(),
                interval: Duration::ZERO,
                last_dispatch: None,
                cooldown: None,
            }),
            changed: Notify::new(),
        })
    }
    pub fn ceiling(&self) -> u8 {
        self.state.lock().expect("Budget state poisoned").ceiling
    }
    pub fn configure(&self, ceiling: u8) -> Result<(), Diagnostic> {
        if ceiling == 0 {
            return Err(Diagnostic::new(
                "INVALID_CONCURRENCY",
                "Simultaneous plugins must be a whole number from 1 to 255.",
            ));
        }
        self.state
            .lock()
            .map_err(|_| {
                Diagnostic::new(
                    "SCHEDULER_CLOSED",
                    "The host request scheduler is unavailable.",
                )
            })?
            .ceiling = ceiling;
        self.changed.notify_waiters();
        Ok(())
    }
    pub fn pace(&self, milliseconds: Option<u64>) -> Result<(), Diagnostic> {
        self.state
            .lock()
            .map_err(|_| {
                Diagnostic::new(
                    "SCHEDULER_CLOSED",
                    "The host request scheduler is unavailable.",
                )
            })?
            .interval = milliseconds
            .map(Duration::from_millis)
            .unwrap_or(Duration::ZERO);
        self.changed.notify_waiters();
        Ok(())
    }
    pub fn configure_operating(
        &self,
        ceiling: u8,
        milliseconds: Option<u64>,
    ) -> Result<(), Diagnostic> {
        if ceiling == 0 {
            return Err(Diagnostic::new(
                "INVALID_CONCURRENCY",
                "Simultaneous plugins must be a whole number from 1 to 255.",
            ));
        }
        let mut state = self.state.lock().map_err(|_| {
            Diagnostic::new(
                "SCHEDULER_CLOSED",
                "The host request scheduler is unavailable.",
            )
        })?;
        state.ceiling = ceiling;
        state.interval = milliseconds
            .map(Duration::from_millis)
            .unwrap_or(Duration::ZERO);
        drop(state);
        self.changed.notify_waiters();
        Ok(())
    }
    pub fn throttle(&self, duration: Duration) -> Result<(), Diagnostic> {
        let mut state = self.state.lock().map_err(|_| {
            Diagnostic::new(
                "SCHEDULER_CLOSED",
                "The host request scheduler is unavailable.",
            )
        })?;
        let remaining = state
            .cooldown
            .map(|(start, total)| total.saturating_sub(start.elapsed()))
            .unwrap_or(Duration::ZERO);
        state.cooldown = Some((Instant::now(), remaining.max(duration)));
        drop(state);
        self.changed.notify_waiters();
        Ok(())
    }
    pub async fn acquire(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> Result<Permit, Diagnostic> {
        let ticket = {
            let mut state = self.state.lock().map_err(|_| {
                Diagnostic::new(
                    "SCHEDULER_CLOSED",
                    "The host request scheduler is unavailable.",
                )
            })?;
            let ticket = state.next_ticket;
            state.next_ticket = state.next_ticket.wrapping_add(1);
            state.waiting.push_back(ticket);
            ticket
        };
        let _waiting = Waiting {
            budget: self.clone(),
            ticket,
        };
        loop {
            let notified = self.changed.notified();
            let delay = {
                let mut state = self.state.lock().map_err(|_| {
                    Diagnostic::new(
                        "SCHEDULER_CLOSED",
                        "The host request scheduler is unavailable.",
                    )
                })?;
                if cancel.is_cancelled() {
                    state.waiting.retain(|t| *t != ticket);
                    drop(state);
                    self.changed.notify_waiters();
                    return Err(Diagnostic::new("CANCELED", "Queued request was canceled."));
                }
                let pacing = state
                    .last_dispatch
                    .map(|last| state.interval.saturating_sub(last.elapsed()))
                    .unwrap_or(Duration::ZERO);
                let cooldown = state
                    .cooldown
                    .map(|(start, total)| total.saturating_sub(start.elapsed()))
                    .unwrap_or(Duration::ZERO);
                let delay = pacing.max(cooldown);
                if state.waiting.front() == Some(&ticket)
                    && state.active < state.ceiling as usize
                    && delay.is_zero()
                {
                    state.waiting.pop_front();
                    state.active += 1;
                    state.last_dispatch = Some(Instant::now());
                    drop(state);
                    self.changed.notify_waiters();
                    return Ok(Permit {
                        budget: self.clone(),
                    });
                }
                delay
            };
            if delay.is_zero() {
                tokio::select! {_ = cancel.cancelled()=>{},_ = notified=>{}}
            } else {
                let deadline=tokio::time::Instant::now().checked_add(delay).ok_or_else(||Diagnostic::new("TIMING_NOT_REPRESENTABLE","The configured or server-directed delay exceeds this platform's scheduler clock representation."))?;
                tokio::select! {_ = cancel.cancelled()=>{},_ = notified=>{},_ = tokio::time::sleep_until(deadline)=>{}}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[tokio::test]
    async fn every_selected_ceiling_includes_full_response_lifetime() {
        for ceiling in [1, 2, 3, 4, 6, 8] {
            let b = Budget::new(ceiling).unwrap();
            let active = Arc::new(AtomicUsize::new(0));
            let peak = Arc::new(AtomicUsize::new(0));
            let mut tasks = vec![];
            for _ in 0..ceiling * 3 {
                let b = b.clone();
                let a = active.clone();
                let p = peak.clone();
                tasks.push(tokio::spawn(async move {
                    let _slot = b.acquire(&CancellationToken::new()).await.unwrap();
                    let n = a.fetch_add(1, Ordering::SeqCst) + 1;
                    p.fetch_max(n, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    a.fetch_sub(1, Ordering::SeqCst);
                }));
            }
            for task in tasks {
                task.await.unwrap();
            }
            assert!(peak.load(Ordering::SeqCst) <= ceiling as usize);
        }
    }
    #[tokio::test]
    async fn cancellation_removes_queued_work() {
        let b = Budget::new(2).unwrap();
        let cancel = CancellationToken::new();
        let _one = b.acquire(&cancel).await.unwrap();
        let _two = b.acquire(&cancel).await.unwrap();
        let queued = CancellationToken::new();
        queued.cancel();
        assert_eq!(b.acquire(&queued).await.err().unwrap().code, "CANCELED");
    }
    #[tokio::test]
    async fn dropped_waiter_does_not_strand_later_requests() {
        let b = Budget::new(2).unwrap();
        let one = b.acquire(&CancellationToken::new()).await.unwrap();
        let two = b.acquire(&CancellationToken::new()).await.unwrap();
        let waiting = b.clone();
        let task = tokio::spawn(async move { waiting.acquire(&CancellationToken::new()).await });
        while b.state.lock().unwrap().waiting.is_empty() {
            tokio::task::yield_now().await;
        }
        task.abort();
        let _ = task.await;
        drop(one);
        drop(two);
        assert!(b.state.lock().unwrap().waiting.is_empty());
        let _next = b.acquire(&CancellationToken::new()).await.unwrap();
    }
}
