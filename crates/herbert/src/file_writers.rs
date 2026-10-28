//! One host-wide writer pool. Pages rotate after each chunk to share disk time.
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;

// Kevin selected these operating defaults on 2026-10-22. They are not file limits.
pub(crate) const WRITERS: usize = 4;
pub(crate) const FILES_PER_CHUNK: usize = 100;
pub(crate) type Job = Box<dyn FnOnce() + Send + 'static>;

#[derive(Default)]
struct Queue {
    pages: VecDeque<VecDeque<Job>>,
    stopping: bool,
}
struct Inner {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    threads: Vec<JoinHandle<()>>,
}
impl Inner {
    fn new(count: usize) -> Self {
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let threads = (0..count)
            .map(|_| {
                let queue = queue.clone();
                std::thread::spawn(move || {
                    loop {
                        let job = {
                            let (lock, ready) = &*queue;
                            let mut state = lock.lock().unwrap();
                            while state.pages.is_empty() && !state.stopping {
                                state = ready.wait(state).unwrap();
                            }
                            let Some(mut page) = state.pages.pop_front() else {
                                return;
                            };
                            let job = page.pop_front().unwrap();
                            if !page.is_empty() {
                                state.pages.push_back(page);
                            }
                            job
                        };
                        // A failed job closes its result sender. Do not lose a pool
                        // worker or leave another plugin waiting after that failure.
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                    }
                })
            })
            .collect();
        Self { queue, threads }
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        let (lock, ready) = &*self.queue;
        lock.lock().unwrap().stopping = true;
        ready.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

#[derive(Default)]
pub(crate) struct WriterPool {
    inner: OnceLock<Inner>,
}
impl WriterPool {
    pub(crate) fn submit_page(&self, jobs: Vec<Job>) {
        if jobs.is_empty() {
            return;
        }
        let inner = self.inner.get_or_init(|| Inner::new(WRITERS));
        let (lock, ready) = &*inner.queue;
        lock.lock().unwrap().pages.push_back(jobs.into());
        ready.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[test]
    fn concurrent_pages_share_four_workers_and_finish_before_pool_drop() {
        let pool = WriterPool::default();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(std::sync::Barrier::new(WRITERS));
        for _ in 0..3 {
            let jobs = (0..4)
                .map(|_| {
                    let (active, peak, completed, barrier) = (
                        active.clone(),
                        peak.clone(),
                        completed.clone(),
                        barrier.clone(),
                    );
                    Box::new(move || {
                        let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(current, Ordering::SeqCst);
                        barrier.wait();
                        active.fetch_sub(1, Ordering::SeqCst);
                        completed.fetch_add(1, Ordering::SeqCst);
                    }) as Job
                })
                .collect();
            pool.submit_page(jobs);
        }
        drop(pool);
        assert_eq!(completed.load(Ordering::SeqCst), 12);
        assert_eq!(peak.load(Ordering::SeqCst), WRITERS);
    }
}
