use std::{
    io,
    sync::mpsc::{self, SyncSender},
    thread,
    time::{Duration, Instant},
};

struct Request<T, R> {
    value: T,
    deadline: Instant,
    reply: SyncSender<R>,
}

/// A fixed worker and a bounded queue, even when a foreign provider never returns.
/// Expired requests never reach the processor. An optional expiration callback
/// lets an ordered capture consumer invalidate a prefix after losing an event.
pub(crate) struct BoundedWorker<T, R> {
    sender: SyncSender<Request<T, R>>,
}

impl<T: Send + 'static, R: Send + 'static> BoundedWorker<T, R> {
    pub(crate) fn start<F, I>(name: &str, initialize: I) -> io::Result<Self>
    where
        I: FnOnce() -> F + Send + 'static,
        F: FnMut(T) -> R + 'static,
    {
        Self::start_with_expiration(name, 1, initialize, |_| {})
    }

    pub(crate) fn start_with_expiration<F, I, E>(
        name: &str,
        capacity: usize,
        initialize: I,
        mut expired: E,
    ) -> io::Result<Self>
    where
        I: FnOnce() -> F + Send + 'static,
        F: FnMut(T) -> R + 'static,
        E: FnMut(T) + Send + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel::<Request<T, R>>(capacity);
        thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let mut process = initialize();
                while let Ok(request) = receiver.recv() {
                    if Instant::now() >= request.deadline {
                        expired(request.value);
                        continue;
                    }
                    let result = process(request.value);
                    if Instant::now() < request.deadline {
                        let _ = request.reply.send(result);
                    }
                }
            })?;
        Ok(Self { sender })
    }

    /// Enqueue without waiting for a provider or allocating another worker.
    pub(crate) fn submit(&self, value: T, timeout: Duration) -> bool {
        let (reply, _receiver) = mpsc::sync_channel(1);
        self.sender
            .try_send(Request {
                value,
                deadline: Instant::now() + timeout,
                reply,
            })
            .is_ok()
    }

    pub(crate) fn request(&self, value: T, timeout: Duration) -> Option<R> {
        let (reply, receiver) = mpsc::sync_channel(1);
        self.sender
            .try_send(Request {
                value,
                deadline: Instant::now() + timeout,
                reply,
            })
            .ok()?;
        receiver.recv_timeout(timeout).ok()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]
    use super::*;

    #[test]
    fn returns_worker_result() {
        let worker = BoundedWorker::start("probe-test", || |value: u32| value + 1).unwrap();
        assert_eq!(worker.request(2, Duration::from_secs(1)), Some(3));
    }

    #[test]
    fn hung_provider_does_not_create_more_workers_or_block_caller() {
        let (release, blocked) = mpsc::channel();
        let (started, observed) = mpsc::channel();
        let worker = BoundedWorker::start("blocked-probe-test", move || {
            move |value: u32| {
                started.send(value).unwrap();
                blocked.recv().unwrap();
                value
            }
        })
        .unwrap();
        let start = Instant::now();
        assert_eq!(worker.request(1, Duration::from_millis(30)), None);
        assert_eq!(observed.recv_timeout(Duration::from_secs(1)).unwrap(), 1);
        assert_eq!(worker.request(2, Duration::from_millis(30)), None);
        assert_eq!(worker.request(3, Duration::from_millis(30)), None);
        assert!(start.elapsed() < Duration::from_secs(1));
        release.send(()).unwrap();
        // The expired queued request must never reach the provider.
        assert!(observed.recv_timeout(Duration::from_millis(80)).is_err());
    }

    #[test]
    fn asynchronous_capture_is_bounded_and_expired_prefix_is_reported() {
        let (release, blocked) = mpsc::channel();
        let (started, observed) = mpsc::channel();
        let (expired, expired_events) = mpsc::channel();
        let worker = BoundedWorker::start_with_expiration(
            "capture-test",
            1,
            move || {
                move |value: u32| {
                    started.send(value).unwrap();
                    blocked.recv().unwrap();
                    value
                }
            },
            move |value| {
                expired.send(value).unwrap();
            },
        )
        .unwrap();

        assert!(worker.submit(1, Duration::from_secs(1)));
        assert_eq!(observed.recv_timeout(Duration::from_secs(1)).unwrap(), 1);
        let start = Instant::now();
        assert!(worker.submit(2, Duration::ZERO));
        assert!(!worker.submit(3, Duration::from_secs(1)));
        assert!(start.elapsed() < Duration::from_millis(100));
        release.send(()).unwrap();
        assert_eq!(
            expired_events.recv_timeout(Duration::from_secs(1)).unwrap(),
            2
        );
        assert!(observed.try_recv().is_err());
    }
}
