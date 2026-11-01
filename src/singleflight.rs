use std::future::Future;

use futures::future::BoxFuture;
use futures::FutureExt;
use tokio::sync::Mutex;

struct Flight<T> {
    key: String,
    fut: futures::future::Shared<BoxFuture<'static, Result<T, String>>>,
}

/// 同一个 key 只跑一次工厂函数，其余调用者等待同一次结果。
pub struct SingleFlight<T> {
    inner: Mutex<Option<Flight<T>>>,
}

impl<T> SingleFlight<T>
where
    T: Clone + Send + Sync + 'static,
{
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    pub async fn run<F, Fut>(&self, key: &str, factory: F) -> Result<T, String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, String>> + Send + 'static,
    {
        let fut = {
            let mut guard = self.inner.lock().await;
            if let Some(existing) = guard.as_ref() {
                if existing.key == key {
                    existing.fut.clone()
                } else {
                    let fut = factory().boxed().shared();
                    *guard = Some(Flight {
                        key: key.to_string(),
                        fut: fut.clone(),
                    });
                    fut
                }
            } else {
                let fut = factory().boxed().shared();
                *guard = Some(Flight {
                    key: key.to_string(),
                    fut: fut.clone(),
                });
                fut
            }
        };
        let result = fut.await;
        let mut guard = self.inner.lock().await;
        if guard.as_ref().is_some_and(|flight| flight.key == key) {
            *guard = None;
        }
        result
    }
}

impl<T> Default for SingleFlight<T>
where
    T: Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn factory_runs_once_for_the_same_key() {
        let flight = Arc::new(SingleFlight::<String>::new());
        let hits = Arc::new(AtomicUsize::new(0));

        let make = |hits: Arc<AtomicUsize>| {
            let hits = hits.clone();
            move || {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    Ok("apk".to_string())
                }
            }
        };

        let left = flight.clone();
        let right = flight.clone();
        let left_hits = hits.clone();
        let right_hits = hits.clone();
        let (left_result, right_result) = tokio::join!(
            left.run("2.7.91", make(left_hits)),
            right.run("2.7.91", make(right_hits)),
        );
        assert_eq!(left_result.as_deref(), Ok("apk"));
        assert_eq!(right_result.as_deref(), Ok("apk"));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
