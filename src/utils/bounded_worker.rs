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

/// One worker and one queued request, even when a foreign provider never returns.
/// The worker only reads data: expired requests cannot perform delayed input.
pub(crate) struct BoundedWorker<T, R> {
    sender: SyncSender<Request<T, R>>,
}

impl<T: Send + 'static, R: Send + 'static> BoundedWorker<T, R> {
    pub(crate) fn start<F, I>(name: &str, initialize: I) -> io::Result<Self>
    where
        I: FnOnce() -> F + Send + 'static,
        F: FnMut(T) -> R + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel::<Request<T, R>>(1);
        thread::Builder::new().name(name.to_owned()).spawn(move || {
            let mut process = initialize();
            while let Ok(request) = receiver.recv() {
                if Instant::now() >= request.deadline {
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
        }).unwrap();
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
}
