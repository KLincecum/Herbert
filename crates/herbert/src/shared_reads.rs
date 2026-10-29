//! Share a read result inside one execution, including its final failure.
//! Connection retries happen before this result. Explicit run retry/recovery
//! creates a new cache, so a refused shared export is not repeated for each item.
use crate::connection::NativeResponse;
use herbert_contract::Diagnostic;
use std::{collections::BTreeMap, future::Future, sync::Arc};
use tokio::sync::{Mutex, OnceCell};
use tokio_util::sync::CancellationToken;
#[derive(Default)]
pub struct ReadCache {
    entries: Mutex<BTreeMap<String, Arc<OnceCell<Result<NativeResponse, Diagnostic>>>>>,
}
impl ReadCache {
    pub async fn read<F, Fut>(
        &self,
        key: String,
        cancel: &CancellationToken,
        read: F,
    ) -> Result<NativeResponse, Diagnostic>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<NativeResponse, Diagnostic>>,
    {
        self.read_with_source(key, cancel, read).await.map(|(response, _)| response)
    }

    /// Only the caller that executes the read may count network time and bytes.
    /// Waiters and later consumers receive the same body with a cache marker.
    pub async fn read_with_source<F, Fut>(
        &self,
        key: String,
        cancel: &CancellationToken,
        read: F,
    ) -> Result<(NativeResponse, bool), Diagnostic>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<NativeResponse, Diagnostic>>,
    {
        let cell = self
            .entries
            .lock()
            .await
            .entry(key)
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone();
        let mut executed = false;
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Diagnostic::new("CANCELED", "The shared read was canceled.")),
            result = cell.get_or_init(|| { executed = true; read() }) => result.clone(),
        };
        result.map(|response| (response, !executed))
    }
    /// Drop every retained body when the execution settles. Recovery creates a new cache.
    pub async fn clear(&self) {
        self.entries.lock().await.clear();
    }
    pub async fn is_empty(&self) -> bool {
        self.entries.lock().await.is_empty()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    fn response() -> NativeResponse {
        NativeResponse {
            raw: b"{}".to_vec(),
            value: json!({}),
            elapsed_ms: 0,
        }
    }
    #[tokio::test]
    async fn simultaneous_identical_reads_execute_once_and_new_runs_read_again() {
        let cache = ReadCache::default();
        let calls = AtomicUsize::new(0);
        let cancel = CancellationToken::new();
        let read = || async {
            calls.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            Ok(response())
        };
        let (a, b) = tokio::join!(
            cache.read("same".into(), &cancel, read),
            cache.read("same".into(), &cancel, read)
        );
        assert!(a.is_ok() && b.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        cache.read("different".into(), &cancel, read).await.unwrap();
        ReadCache::default()
            .read("same".into(), &cancel, read)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
    #[tokio::test]
    async fn only_the_executing_caller_counts_a_network_response() {
        let cache = ReadCache::default();
        let cancel = CancellationToken::new();
        let read = || async {
            tokio::task::yield_now().await;
            Ok(NativeResponse { elapsed_ms: 93, ..response() })
        };
        let (a, b) = tokio::join!(
            cache.read_with_source("body".into(), &cancel, read),
            cache.read_with_source("body".into(), &cancel, read),
        );
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_ne!(a.1, b.1);
        assert_eq!(a.0.raw, b.0.raw);
        let activity = crate::activity::Activity::default();
        for (response, reused) in [a, b] {
            if reused { activity.cached_response("report-styles-and-layouts", None, response.raw.len()); }
            else { activity.native_response("report-styles-and-layouts", None, response.elapsed_ms, response.raw.len(), &response.value); }
        }
        let (_, reused) = cache.read_with_source("body".into(), &cancel, read).await.unwrap();
        assert!(reused);
        activity.cached_response("report-styles-and-layouts", None, 2);
        let stats = activity.statistics();
        assert_eq!(stats["requests"], 1);
        assert_eq!(stats["requestMs"], 93);
        assert_eq!(stats["receivedBytes"], 2);
    }
    #[tokio::test]
    async fn one_failure_is_shared_and_a_new_execution_can_retry() {
        let cache = ReadCache::default();
        let cancel = CancellationToken::new();
        let calls = AtomicUsize::new(0);
        let read = || async {
            calls.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            Err(Diagnostic::new("REFUSED", "fixture"))
        };
        let (a, b) = tokio::join!(
            cache.read("retry".into(), &cancel, read),
            cache.read("retry".into(), &cancel, read)
        );
        assert_eq!(a.err().unwrap().code, "REFUSED");
        assert_eq!(b.err().unwrap().code, "REFUSED");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            cache
                .read("retry".into(), &cancel, || async {
                    panic!("Do not repeat the failed request for another item")
                })
                .await
                .is_err()
        );
        assert!(
            ReadCache::default()
                .read("retry".into(), &cancel, || async { Ok(response()) })
                .await
                .is_ok()
        );
    }
    #[tokio::test]
    async fn canceled_waiters_do_not_cancel_the_owner() {
        let cache = ReadCache::default();
        let cancel = CancellationToken::new();
        let waiting = CancellationToken::new();
        waiting.cancel();
        let (a, b) = tokio::join!(
            cache.read("work".into(), &cancel, || async {
                tokio::task::yield_now().await;
                Ok(response())
            }),
            cache.read("work".into(), &waiting, || async {
                panic!("Canceled caller must not start work")
            })
        );
        assert!(a.is_ok());
        assert_eq!(b.err().unwrap().code, "CANCELED");
        assert!(
            cache
                .read("work".into(), &cancel, || async {
                    panic!("Must reuse completed read")
                })
                .await
                .is_ok()
        );
    }
    #[tokio::test]
    async fn different_requests_run_concurrently() {
        let cache = ReadCache::default();
        let cancel = CancellationToken::new();
        let barrier = tokio::sync::Barrier::new(2);
        let read = || async {
            barrier.wait().await;
            Ok(response())
        };
        let work = async {
            tokio::join!(
                cache.read("first".into(), &cancel, read),
                cache.read("second".into(), &cancel, read)
            )
        };
        let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(5), work)
            .await
            .expect("Different reads must not hold one shared lock while waiting for the server");
        assert!(a.is_ok() && b.is_ok());
    }
}
