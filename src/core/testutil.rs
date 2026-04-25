use std::{
    future::Future,
    mem::ManuallyDrop,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};
#[cfg(any(feature = "async_tokio", feature = "async_io"))]
use std::{
    task::Wake,
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
pub(crate) struct WakeCounter {
    count: Arc<AtomicUsize>,
}

impl WakeCounter {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    pub(crate) fn waker(&self) -> Waker {
        unsafe { Waker::from_raw(raw_waker(Arc::clone(&self.count))) }
    }
}

fn raw_waker(count: Arc<AtomicUsize>) -> RawWaker {
    RawWaker::new(Arc::into_raw(count) as *const (), &VTABLE)
}

unsafe fn clone_waker(data: *const ()) -> RawWaker {
    let count = ManuallyDrop::new(Arc::from_raw(data.cast::<AtomicUsize>()));
    raw_waker(Arc::clone(&count))
}

unsafe fn wake_waker(data: *const ()) {
    let count = Arc::from_raw(data.cast::<AtomicUsize>());
    count.fetch_add(1, Ordering::SeqCst);
}

unsafe fn wake_by_ref_waker(data: *const ()) {
    let count = ManuallyDrop::new(Arc::from_raw(data.cast::<AtomicUsize>()));
    count.fetch_add(1, Ordering::SeqCst);
}

unsafe fn drop_waker(data: *const ()) {
    drop(Arc::from_raw(data.cast::<AtomicUsize>()));
}

static VTABLE: RawWakerVTable =
    RawWakerVTable::new(clone_waker, wake_waker, wake_by_ref_waker, drop_waker);

pub(crate) fn block_on_immediate<F>(future: F) -> F::Output
where
    F: Future,
{
    let waker = WakeCounter::new().waker();
    let mut future = Pin::from(Box::new(future));
    let mut context = Context::from_waker(&waker);

    match future.as_mut().poll(&mut context) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("future unexpectedly returned Poll::Pending"),
    }
}

#[cfg(any(feature = "async_tokio", feature = "async_io"))]
struct ThreadWaker {
    thread: thread::Thread,
}

#[cfg(any(feature = "async_tokio", feature = "async_io"))]
impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.thread.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.thread.unpark();
    }
}

#[cfg(any(feature = "async_tokio", feature = "async_io"))]
pub(crate) fn block_on_timeout<F>(future: F, timeout: Duration) -> Option<F::Output>
where
    F: Future,
{
    let current_thread = thread::current();
    let waker = Waker::from(Arc::new(ThreadWaker {
        thread: current_thread,
    }));
    let mut future = Pin::from(Box::new(future));
    let start = Instant::now();

    loop {
        let mut context = Context::from_waker(&waker);
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return Some(output),
            Poll::Pending => {
                let elapsed = start.elapsed();
                if elapsed >= timeout {
                    return None;
                }

                let remaining = timeout - elapsed;
                thread::park_timeout(remaining.min(Duration::from_millis(50)));
            }
        }
    }
}
