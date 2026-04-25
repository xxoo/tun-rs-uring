use crate::core::error;
use async_channel::{Receiver, Sender, TryRecvError};
use bytes::Bytes;
use io_uring::{cqueue, opcode, squeue, types, IoUring};
use std::{
    fmt,
    future::poll_fn,
    future::Future,
    io, mem,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    task::Waker,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tun_rs::SyncDevice;

const TX_WRITE_KIND: u64 = 0x5458_5700_0000_0000;
const TX_CANCEL_KIND: u64 = 0x5458_4300_0000_0000;
const TX_KIND_MASK: u64 = 0xffff_ff00_0000_0000;
const TX_INDEX_MASK: u64 = 0x0000_00ff_ffff_ffff;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) enum TxBatchPhase {
    Idle,
    Running,
    Cancelling,
}

#[derive(Default)]
struct TxBatchShared {
    inner: Mutex<TxBatchSharedState>,
}

struct TxBatchSharedState {
    phase: TxBatchPhase,
    waiters: Vec<Waker>,
}

impl Default for TxBatchSharedState {
    fn default() -> Self {
        Self {
            phase: TxBatchPhase::Idle,
            waiters: Vec::new(),
        }
    }
}

impl TxBatchShared {
    fn phase(&self) -> TxBatchPhase {
        self.inner.lock().unwrap().phase
    }

    fn try_acquire(&self) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.phase != TxBatchPhase::Idle {
            return false;
        }

        inner.phase = TxBatchPhase::Running;
        true
    }

    fn try_acquire_or_register(&self, waker: &Waker) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.phase == TxBatchPhase::Idle {
            inner.phase = TxBatchPhase::Running;
            return true;
        }

        if !inner
            .waiters
            .iter()
            .any(|registered| registered.will_wake(waker))
        {
            inner.waiters.push(waker.clone());
        }

        false
    }

    fn request_cancel(&self) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.phase != TxBatchPhase::Running {
            return false;
        }

        inner.phase = TxBatchPhase::Cancelling;
        true
    }

    #[cfg(test)]
    fn register_waiter(&self, waker: &Waker) {
        let mut inner = self.inner.lock().unwrap();
        if inner
            .waiters
            .iter()
            .any(|registered| registered.will_wake(waker))
        {
            return;
        }

        inner.waiters.push(waker.clone());
    }

    fn release_to_idle(&self) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.phase == TxBatchPhase::Idle {
            return false;
        }

        inner.phase = TxBatchPhase::Idle;
        for waker in inner.waiters.drain(..) {
            waker.wake();
        }
        true
    }
}

#[derive(Debug)]
enum TxDriverCommand {
    RunBatch(TxBatchRequest),
    Shutdown,
}

#[derive(Debug)]
struct TxBatchRequest {
    bufs: Vec<Bytes>,
    deadline: Option<Instant>,
    keep_order: bool,
    cancel_requested: Arc<AtomicBool>,
    response: Sender<TxBatchResponse>,
}

#[derive(Debug)]
struct TxBatchResponse {
    bufs: Vec<Bytes>,
    results: Vec<Option<io::Result<usize>>>,
}

pub(crate) struct TxController {
    shared: Arc<TxBatchShared>,
    commands: Sender<TxDriverCommand>,
    command_eventfd: OwnedFd,
    thread_name: &'static str,
    thread: Option<JoinHandle<()>>,
}

impl TxController {
    pub(crate) fn new(
        device: SyncDevice,
        ring_entries: u32,
        submit_chunk_size: usize,
        thread_name: &'static str,
    ) -> io::Result<Self> {
        let shared = Arc::new(TxBatchShared::default());
        let ring = TxRingContext::new(ring_entries)?;
        let (commands_tx, commands_rx) = async_channel::unbounded();
        let command_eventfd_read = new_eventfd()?;
        let command_eventfd_write = dup_fd(&command_eventfd_read)?;
        let shared_for_thread = Arc::clone(&shared);

        let thread = thread::Builder::new()
            .name(thread_name.to_string())
            .spawn(move || {
                TxDriverThread {
                    ring,
                    device,
                    commands: commands_rx,
                    command_eventfd: command_eventfd_read,
                    shared: shared_for_thread,
                    submit_chunk_size,
                    shutdown_requested: false,
                }
                .run();
            })?;

        Ok(Self {
            shared,
            commands: commands_tx,
            command_eventfd: command_eventfd_write,
            thread_name,
            thread: Some(thread),
        })
    }

    pub(crate) fn phase(&self) -> TxBatchPhase {
        self.shared.phase()
    }

    pub(crate) async fn send_many<TimerFuture>(
        &self,
        mut bufs: Vec<Bytes>,
        results: &mut [Option<io::Result<usize>>],
        timeout: Duration,
        keep_order: bool,
        make_timer: impl FnOnce(Duration) -> TimerFuture,
    ) -> Vec<Bytes>
    where
        TimerFuture: Future,
    {
        let len = bufs.len();
        if len == 0 {
            return bufs;
        }

        if results.len() < len {
            for slot in results.iter_mut() {
                *slot = Some(Err(error::invalid_input(
                    "send_many results slice is shorter than input buffers",
                )));
            }
            return bufs;
        }

        clear_result_prefix(results, len);

        let deadline = if timeout.is_zero() {
            None
        } else {
            Some(Instant::now() + timeout)
        };

        if !self.acquire(deadline, make_timer).await {
            fill_result_prefix(results, len, || {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out waiting for tx batch slot",
                ))
            });
            return bufs;
        }

        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            self.shared.release_to_idle();
            fill_result_prefix(results, len, || {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out before tx batch submission",
                ))
            });
            return bufs;
        }

        let (response_tx, response_rx) = async_channel::bounded(1);
        let cancel_requested = Arc::new(AtomicBool::new(false));
        let mut drop_guard = TxBatchDropGuard::new(
            Arc::clone(&cancel_requested),
            self.command_eventfd.as_raw_fd(),
        );
        let request = TxBatchRequest {
            bufs,
            deadline,
            keep_order,
            cancel_requested,
            response: response_tx,
        };

        if let Err(command) = self.try_send_command(TxDriverCommand::RunBatch(request)) {
            self.shared.release_to_idle();
            if let TxDriverCommand::RunBatch(request) = command {
                fill_result_prefix(results, len, || Err(error::rx_driver_closed()));
                return request.bufs;
            }
            return Vec::new();
        }

        let response = match response_rx.recv().await {
            Ok(response) => {
                drop_guard.disarm();
                response
            }
            Err(_) => {
                self.shared.release_to_idle();
                return Vec::new();
            }
        };

        for (slot, result) in results.iter_mut().take(len).zip(response.results) {
            *slot = result;
        }

        bufs = response.bufs;
        bufs
    }

    async fn acquire<TimerFuture>(
        &self,
        deadline: Option<Instant>,
        make_timer: impl FnOnce(Duration) -> TimerFuture,
    ) -> bool
    where
        TimerFuture: Future,
    {
        acquire_batch_slot(Arc::clone(&self.shared), deadline, make_timer).await
    }

    fn try_send_command(&self, command: TxDriverCommand) -> Result<(), TxDriverCommand> {
        self.commands
            .send_blocking(command)
            .map_err(|error| error.0)?;
        signal_eventfd(self.command_eventfd.as_raw_fd()).map_err(|_| TxDriverCommand::Shutdown)?;
        Ok(())
    }

    fn send_command(&self, command: TxDriverCommand) -> io::Result<()> {
        self.try_send_command(command)
            .map_err(|_| error::rx_driver_closed())
    }
}

async fn acquire_batch_slot<TimerFuture>(
    shared: Arc<TxBatchShared>,
    deadline: Option<Instant>,
    make_timer: impl FnOnce(Duration) -> TimerFuture,
) -> bool
where
    TimerFuture: Future,
{
    if shared.try_acquire() {
        return true;
    }

    let Some(deadline) = deadline else {
        poll_fn(|cx| {
            if shared.try_acquire_or_register(cx.waker()) {
                std::task::Poll::Ready(true)
            } else {
                std::task::Poll::Pending
            }
        })
        .await;
        return true;
    };

    if Instant::now() >= deadline {
        return false;
    }

    let mut timer = Box::pin(make_timer(
        deadline.saturating_duration_since(Instant::now()),
    ));
    poll_fn(|cx| {
        if shared.try_acquire_or_register(cx.waker()) {
            return std::task::Poll::Ready(true);
        }

        if timer.as_mut().poll(cx).is_ready() {
            return std::task::Poll::Ready(false);
        }

        std::task::Poll::Pending
    })
    .await
}

struct TxBatchDropGuard {
    cancel_requested: Arc<AtomicBool>,
    eventfd: i32,
    armed: bool,
}

impl TxBatchDropGuard {
    fn new(cancel_requested: Arc<AtomicBool>, eventfd: i32) -> Self {
        Self {
            cancel_requested,
            eventfd,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TxBatchDropGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        self.cancel_requested.store(true, Ordering::Release);
        let _ = signal_eventfd(self.eventfd);
    }
}

impl fmt::Debug for TxController {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TxController")
            .field("thread_name", &self.thread_name)
            .field("phase", &self.phase())
            .finish()
    }
}

impl Drop for TxController {
    fn drop(&mut self) {
        let _ = self.send_command(TxDriverCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct TxRingContext {
    ring: IoUring,
    cq_eventfd: OwnedFd,
}

impl TxRingContext {
    fn new(entries: u32) -> io::Result<Self> {
        let ring = IoUring::new(entries)?;
        if !ring.params().is_feature_fast_poll() {
            return Err(error::unsupported(
                "tx io_uring requires IORING_FEAT_FAST_POLL",
            ));
        }

        let cq_eventfd = new_eventfd()?;
        ring.submitter().register_eventfd(cq_eventfd.as_raw_fd())?;

        Ok(Self { ring, cq_eventfd })
    }

    fn eventfd_raw_fd(&self) -> i32 {
        self.cq_eventfd.as_raw_fd()
    }
}

impl Drop for TxRingContext {
    fn drop(&mut self) {
        let _ = self.ring.submitter().unregister_eventfd();
    }
}

struct TxDriverThread {
    ring: TxRingContext,
    device: SyncDevice,
    commands: Receiver<TxDriverCommand>,
    command_eventfd: OwnedFd,
    shared: Arc<TxBatchShared>,
    submit_chunk_size: usize,
    shutdown_requested: bool,
}

impl TxDriverThread {
    fn run(mut self) {
        while !self.shutdown_requested {
            match self.commands.recv_blocking() {
                Ok(command) => {
                    let _ = drain_eventfd(self.command_eventfd.as_raw_fd());
                    self.handle_command(command);
                }
                Err(_) => break,
            }
        }
    }

    fn handle_command(&mut self, command: TxDriverCommand) {
        match command {
            TxDriverCommand::RunBatch(request) => {
                let response = self.run_batch(request);
                let _ = response.0.send_blocking(response.1);
                self.shared.release_to_idle();
            }
            TxDriverCommand::Shutdown => {
                self.shutdown_requested = true;
            }
        }
    }

    fn run_batch(&mut self, request: TxBatchRequest) -> (Sender<TxBatchResponse>, TxBatchResponse) {
        let response = request.response.clone();
        let mut batch = ActiveTxBatch::new(request);

        if batch.deadline_expired() {
            batch.mark_unsubmitted_timed_out();
            return (response, batch.into_response());
        }

        loop {
            if !batch.cancel_requested {
                if batch.keep_order {
                    if let Err(error) = self.submit_next_ordered(&mut batch) {
                        batch.mark_driver_error(error);
                    }
                } else if let Err(error) = self.submit_unordered(&mut batch) {
                    batch.mark_driver_error(error);
                }
            }

            if batch.is_complete() {
                return (response, batch.into_response());
            }

            if batch.external_cancel_requested() && !batch.cancel_requested {
                self.shared.request_cancel();
                batch.cancel_requested = true;
                batch.mark_unsubmitted_cancelled();
                if let Err(error) = self.submit_cancels(&mut batch) {
                    batch.mark_driver_error(error);
                }
                if batch.is_complete() {
                    return (response, batch.into_response());
                }
            }

            if batch.deadline_expired() && !batch.cancel_requested {
                self.shared.request_cancel();
                batch.cancel_requested = true;
                batch.mark_unsubmitted_timed_out();
                if let Err(error) = self.submit_cancels(&mut batch) {
                    batch.mark_driver_error(error);
                }
                if batch.is_complete() {
                    return (response, batch.into_response());
                }
            }

            if let Err(error) = self.wait_for_activity(batch.wait_timeout()) {
                batch.mark_driver_error(error);
                return (response, batch.into_response());
            }

            if let Err(error) = self.process_cqes(&mut batch) {
                batch.mark_driver_error(error);
                return (response, batch.into_response());
            }
        }
    }

    fn submit_unordered(&mut self, batch: &mut ActiveTxBatch) -> io::Result<()> {
        let mut submitted_this_round = 0;
        while batch.next_to_submit < batch.bufs.len()
            && submitted_this_round < self.submit_chunk_size
        {
            self.submit_write(batch, batch.next_to_submit)?;
            batch.next_to_submit += 1;
            submitted_this_round += 1;
        }

        if submitted_this_round > 0 {
            self.ring.ring.submit()?;
        }

        Ok(())
    }

    fn submit_next_ordered(&mut self, batch: &mut ActiveTxBatch) -> io::Result<()> {
        if batch.in_flight != 0 || batch.next_to_submit >= batch.bufs.len() {
            return Ok(());
        }

        self.submit_ordered_linked(batch)
    }

    fn submit_write(&mut self, batch: &mut ActiveTxBatch, index: usize) -> io::Result<()> {
        let entry = build_write_entry(self.device.as_raw_fd(), batch, index, false);

        self.submit_entry(entry)?;
        batch.submitted[index] = true;
        batch.pending[index] = true;
        batch.in_flight += 1;
        Ok(())
    }

    fn submit_ordered_linked(&mut self, batch: &mut ActiveTxBatch) -> io::Result<()> {
        loop {
            let mut submission = self.ring.ring.submission();
            let available = submission.capacity().saturating_sub(submission.len());
            let fd = self.device.as_raw_fd();
            let chunk_len = batch
                .remaining_to_submit()
                .min(self.submit_chunk_size)
                .min(available);

            if chunk_len == 0 {
                drop(submission);
                self.ring.ring.submit()?;
                continue;
            }

            let mut entries = Vec::with_capacity(chunk_len);
            for offset in 0..chunk_len {
                let index = batch.next_to_submit + offset;
                let link_to_next = offset + 1 < chunk_len;
                entries.push(build_write_entry(fd, batch, index, link_to_next));
            }

            match unsafe { submission.push_multiple(&entries) } {
                Ok(()) => {
                    for offset in 0..chunk_len {
                        let index = batch.next_to_submit + offset;
                        batch.submitted[index] = true;
                        batch.pending[index] = true;
                    }
                    batch.next_to_submit += chunk_len;
                    batch.in_flight += chunk_len;
                    drop(submission);
                    self.ring.ring.submit()?;
                    return Ok(());
                }
                Err(_) => {
                    drop(submission);
                    self.ring.ring.submit()?;
                }
            }
        }
    }

    fn submit_cancels(&mut self, batch: &mut ActiveTxBatch) -> io::Result<()> {
        let pending_indices: Vec<usize> = batch
            .pending
            .iter()
            .enumerate()
            .filter_map(|(index, pending)| (*pending).then_some(index))
            .collect();

        for index in pending_indices {
            let entry = opcode::AsyncCancel::new(write_user_data(index))
                .build()
                .user_data(cancel_user_data(index));
            self.submit_entry(entry)?;
        }

        self.ring.ring.submit()?;
        Ok(())
    }

    fn submit_entry(&mut self, entry: squeue::Entry) -> io::Result<()> {
        loop {
            let mut submission = self.ring.ring.submission();
            match unsafe { submission.push(&entry) } {
                Ok(()) => return Ok(()),
                Err(_) => {
                    drop(submission);
                    self.ring.ring.submit()?;
                }
            }
        }
    }

    fn process_cqes(&mut self, batch: &mut ActiveTxBatch) -> io::Result<()> {
        let entries: Vec<cqueue::Entry> = {
            let completion = self.ring.ring.completion();
            completion.collect()
        };

        for entry in entries {
            match user_data_kind(entry.user_data()) {
                Some(TxUserDataKind::Write(index)) => batch.complete_write(index, entry.result()),
                Some(TxUserDataKind::Cancel(_)) | None => {}
            }
        }

        Ok(())
    }

    fn wait_for_activity(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        let timeout_ms = timeout.map(duration_to_poll_timeout_ms).unwrap_or(-1);
        let mut fds = [
            libc::pollfd {
                fd: self.command_eventfd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.ring.eventfd_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];

        loop {
            let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, timeout_ms) };
            if result >= 0 {
                break;
            }

            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }

        if fds[0].revents & libc::POLLIN != 0 {
            drain_eventfd(self.command_eventfd.as_raw_fd())?;
            self.drain_shutdown_commands();
        }

        if fds[1].revents & libc::POLLIN != 0 {
            drain_eventfd(self.ring.eventfd_raw_fd())?;
        }

        Ok(())
    }

    fn drain_shutdown_commands(&mut self) {
        loop {
            match self.commands.try_recv() {
                Ok(TxDriverCommand::Shutdown) => {
                    self.shutdown_requested = true;
                    return;
                }
                Ok(TxDriverCommand::RunBatch(request)) => {
                    let mut batch = ActiveTxBatch::new(request);
                    batch.mark_unsubmitted_error(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "tx driver shutting down",
                    ));
                    let response = batch.response.clone();
                    let _ = response.send_blocking(batch.into_response());
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Closed) => {
                    self.shutdown_requested = true;
                    return;
                }
            }
        }
    }
}

struct ActiveTxBatch {
    bufs: Vec<Bytes>,
    results: Vec<Option<io::Result<usize>>>,
    submitted: Vec<bool>,
    pending: Vec<bool>,
    next_to_submit: usize,
    in_flight: usize,
    completed: usize,
    deadline: Option<Instant>,
    keep_order: bool,
    cancel_requested: bool,
    external_cancel_requested: Arc<AtomicBool>,
    response: Sender<TxBatchResponse>,
}

impl ActiveTxBatch {
    fn new(request: TxBatchRequest) -> Self {
        let len = request.bufs.len();
        Self {
            bufs: request.bufs,
            results: std::iter::repeat_with(|| None).take(len).collect(),
            submitted: vec![false; len],
            pending: vec![false; len],
            next_to_submit: 0,
            in_flight: 0,
            completed: 0,
            deadline: request.deadline,
            keep_order: request.keep_order,
            cancel_requested: false,
            external_cancel_requested: request.cancel_requested,
            response: request.response,
        }
    }

    fn external_cancel_requested(&self) -> bool {
        self.external_cancel_requested.load(Ordering::Acquire)
    }

    fn deadline_expired(&self) -> bool {
        self.deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }

    fn wait_timeout(&self) -> Option<Duration> {
        if self.cancel_requested {
            return None;
        }

        self.deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    fn is_complete(&self) -> bool {
        self.completed == self.bufs.len()
    }

    fn remaining_to_submit(&self) -> usize {
        self.bufs.len().saturating_sub(self.next_to_submit)
    }

    fn complete_write(&mut self, index: usize, result: i32) {
        if index >= self.bufs.len() || !self.pending[index] {
            return;
        }

        self.pending[index] = false;
        self.in_flight = self.in_flight.saturating_sub(1);
        self.completed += 1;
        self.results[index] = Some(cqe_result_to_io_result(result, self.cancel_requested));
    }

    fn mark_unsubmitted_timed_out(&mut self) {
        self.mark_unsubmitted_error(io::Error::new(
            io::ErrorKind::TimedOut,
            "tx batch timed out",
        ));
    }

    fn mark_unsubmitted_cancelled(&mut self) {
        self.mark_unsubmitted_error(io::Error::new(
            io::ErrorKind::Interrupted,
            "tx batch future was dropped",
        ));
    }

    fn mark_unsubmitted_error(&mut self, error: io::Error) {
        for index in 0..self.bufs.len() {
            if self.submitted[index] || self.results[index].is_some() {
                continue;
            }

            self.results[index] = Some(Err(error::clone_io_error(&error)));
            self.completed += 1;
        }
    }

    fn mark_driver_error(&mut self, error: io::Error) {
        for index in 0..self.bufs.len() {
            if self.results[index].is_some() {
                continue;
            }

            self.results[index] = Some(Err(error::clone_io_error(&error)));
            if self.pending[index] {
                self.pending[index] = false;
                self.in_flight = self.in_flight.saturating_sub(1);
            }
            self.completed += 1;
        }
    }

    fn into_response(self) -> TxBatchResponse {
        TxBatchResponse {
            bufs: self.bufs,
            results: self.results,
        }
    }
}

fn build_write_entry(
    fd: i32,
    batch: &ActiveTxBatch,
    index: usize,
    link_to_next: bool,
) -> squeue::Entry {
    let mut entry = opcode::Write::new(
        types::Fd(fd),
        batch.bufs[index].as_ptr(),
        batch.bufs[index].len() as u32,
    )
    .build()
    .user_data(write_user_data(index));
    if link_to_next {
        entry = entry.flags(squeue::Flags::IO_LINK);
    }
    entry
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TxUserDataKind {
    Write(usize),
    Cancel(usize),
}

fn write_user_data(index: usize) -> u64 {
    TX_WRITE_KIND | (index as u64 & TX_INDEX_MASK)
}

fn cancel_user_data(index: usize) -> u64 {
    TX_CANCEL_KIND | (index as u64 & TX_INDEX_MASK)
}

fn user_data_kind(user_data: u64) -> Option<TxUserDataKind> {
    let index = (user_data & TX_INDEX_MASK) as usize;
    match user_data & TX_KIND_MASK {
        TX_WRITE_KIND => Some(TxUserDataKind::Write(index)),
        TX_CANCEL_KIND => Some(TxUserDataKind::Cancel(index)),
        _ => None,
    }
}

fn cqe_result_to_io_result(result: i32, cancel_requested: bool) -> io::Result<usize> {
    if result >= 0 {
        return Ok(result as usize);
    }

    let error_code = -result;
    if cancel_requested && error_code == libc::ECANCELED {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "tx batch timed out",
        ));
    }

    Err(io::Error::from_raw_os_error(error_code))
}

fn clear_result_prefix(results: &mut [Option<io::Result<usize>>], len: usize) {
    for slot in results.iter_mut().take(len) {
        *slot = None;
    }
}

fn fill_result_prefix(
    results: &mut [Option<io::Result<usize>>],
    len: usize,
    mut result: impl FnMut() -> io::Result<usize>,
) {
    for slot in results.iter_mut().take(len) {
        *slot = Some(result());
    }
}

fn duration_to_poll_timeout_ms(duration: Duration) -> i32 {
    if duration.is_zero() {
        return 0;
    }

    let millis = duration.as_millis();
    if millis > i32::MAX as u128 {
        i32::MAX
    } else {
        millis.max(1) as i32
    }
}

fn new_eventfd() -> io::Result<OwnedFd> {
    let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn dup_fd(fd: &OwnedFd) -> io::Result<OwnedFd> {
    let duplicated = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if duplicated < 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(unsafe { OwnedFd::from_raw_fd(duplicated) })
}

fn drain_eventfd(fd: i32) -> io::Result<()> {
    loop {
        let mut value = 0u64;
        let read = unsafe {
            libc::read(
                fd,
                (&mut value as *mut u64).cast::<libc::c_void>(),
                mem::size_of::<u64>(),
            )
        };
        if read == mem::size_of::<u64>() as isize {
            continue;
        }
        if read < 0 {
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::WouldBlock => return Ok(()),
                io::ErrorKind::Interrupted => continue,
                _ => return Err(error),
            }
        }
        return Ok(());
    }
}

fn signal_eventfd(fd: i32) -> io::Result<()> {
    let value = 1u64;
    loop {
        let written = unsafe {
            libc::write(
                fd,
                (&value as *const u64).cast::<libc::c_void>(),
                mem::size_of::<u64>(),
            )
        };
        if written == mem::size_of::<u64>() as isize {
            return Ok(());
        }
        if written < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        duration_to_poll_timeout_ms, user_data_kind, ActiveTxBatch, TxBatchPhase, TxBatchRequest,
        TxBatchShared, TxUserDataKind,
    };
    use crate::core::testutil::{block_on_immediate, WakeCounter};
    use bytes::Bytes;
    use std::{
        future,
        sync::atomic::AtomicBool,
        sync::Arc,
        time::{Duration, Instant},
    };

    fn new_test_batch(len: usize) -> ActiveTxBatch {
        let (response, _response_rx) = async_channel::bounded(1);
        let bufs = (0..len)
            .map(|index| Bytes::from(vec![index as u8 + 1; 8]))
            .collect();

        ActiveTxBatch::new(TxBatchRequest {
            bufs,
            deadline: None,
            keep_order: true,
            cancel_requested: Arc::new(AtomicBool::new(false)),
            response,
        })
    }

    fn mark_range_pending(batch: &mut ActiveTxBatch, start: usize, end: usize) {
        for index in start..end {
            batch.submitted[index] = true;
            batch.pending[index] = true;
            batch.in_flight += 1;
        }
        batch.next_to_submit = batch.next_to_submit.max(end);
    }

    #[test]
    fn batch_slot_transitions_idle_running_cancelling_idle() {
        let slot = TxBatchShared::default();

        assert_eq!(slot.phase(), TxBatchPhase::Idle);
        assert!(slot.try_acquire());
        assert_eq!(slot.phase(), TxBatchPhase::Running);
        assert!(!slot.try_acquire());
        assert!(slot.request_cancel());
        assert_eq!(slot.phase(), TxBatchPhase::Cancelling);
        assert!(!slot.request_cancel());
        assert!(slot.release_to_idle());
        assert_eq!(slot.phase(), TxBatchPhase::Idle);
    }

    #[test]
    fn release_wakes_all_waiters() {
        let slot = TxBatchShared::default();
        let first = WakeCounter::new();
        let second = WakeCounter::new();

        assert!(slot.try_acquire());
        slot.register_waiter(&first.waker());
        slot.register_waiter(&second.waker());
        slot.release_to_idle();

        assert_eq!(first.count(), 1);
        assert_eq!(second.count(), 1);
    }

    #[test]
    fn acquire_or_register_checks_phase_and_registers_under_one_lock() {
        let slot = TxBatchShared::default();
        let waiter = WakeCounter::new();

        assert!(slot.try_acquire_or_register(&waiter.waker()));
        assert_eq!(slot.phase(), TxBatchPhase::Running);
        assert!(!slot.try_acquire_or_register(&waiter.waker()));

        slot.release_to_idle();
        assert_eq!(waiter.count(), 1);
    }

    #[test]
    fn acquire_batch_slot_uses_supplied_timer_when_busy() {
        let slot = Arc::new(TxBatchShared::default());

        assert!(slot.try_acquire());
        let acquired = block_on_immediate(super::acquire_batch_slot(
            Arc::clone(&slot),
            Some(Instant::now() + Duration::from_secs(1)),
            |_| future::ready(()),
        ));

        assert!(!acquired);
        assert_eq!(slot.phase(), TxBatchPhase::Running);
        slot.release_to_idle();
    }

    #[test]
    fn user_data_round_trip_preserves_kind_and_index() {
        assert_eq!(
            user_data_kind(super::write_user_data(42)),
            Some(TxUserDataKind::Write(42))
        );
        assert_eq!(
            user_data_kind(super::cancel_user_data(7)),
            Some(TxUserDataKind::Cancel(7))
        );
    }

    #[test]
    fn poll_timeout_rounds_up_sub_millisecond_duration() {
        assert_eq!(duration_to_poll_timeout_ms(Duration::ZERO), 0);
        assert_eq!(duration_to_poll_timeout_ms(Duration::from_nanos(1)), 1);
        assert_eq!(duration_to_poll_timeout_ms(Duration::from_millis(12)), 12);
    }

    #[test]
    fn ecanceled_maps_to_timeout_only_for_explicit_batch_cancel() {
        let timeout_error =
            super::cqe_result_to_io_result(-libc::ECANCELED, true).expect_err("expected timeout");
        assert_eq!(timeout_error.kind(), std::io::ErrorKind::TimedOut);

        let linked_error = super::cqe_result_to_io_result(-libc::ECANCELED, false)
            .expect_err("expected linked-chain cancellation");
        assert_eq!(linked_error.raw_os_error(), Some(libc::ECANCELED));
    }

    #[test]
    fn ordered_chunk_failure_leaves_later_chunks_unsubmitted() {
        let mut batch = new_test_batch(4);
        mark_range_pending(&mut batch, 0, 2);

        batch.complete_write(0, 64);
        batch.complete_write(1, -libc::EINVAL);

        assert_eq!(
            *batch.results[0]
                .as_ref()
                .unwrap()
                .as_ref()
                .expect("expected first ordered chunk entry to succeed"),
            64usize
        );
        assert_eq!(
            batch.results[1]
                .as_ref()
                .unwrap()
                .as_ref()
                .expect_err("expected ordered chunk failure")
                .raw_os_error(),
            Some(libc::EINVAL)
        );
        assert!(batch.results[2].is_none());
        assert!(batch.results[3].is_none());
        assert_eq!(batch.remaining_to_submit(), 2);
        assert_eq!(batch.in_flight, 0);
        assert!(!batch.is_complete());
    }

    #[test]
    fn ordered_chain_break_maps_linked_tail_to_ecanceled() {
        let mut batch = new_test_batch(3);
        mark_range_pending(&mut batch, 0, 3);

        batch.complete_write(0, 96);
        batch.complete_write(1, -libc::EINVAL);
        batch.complete_write(2, -libc::ECANCELED);

        assert_eq!(
            *batch.results[0]
                .as_ref()
                .unwrap()
                .as_ref()
                .expect("expected first linked entry to succeed"),
            96usize
        );
        assert_eq!(
            batch.results[1]
                .as_ref()
                .unwrap()
                .as_ref()
                .expect_err("expected chain failure")
                .raw_os_error(),
            Some(libc::EINVAL)
        );
        assert_eq!(
            batch.results[2]
                .as_ref()
                .unwrap()
                .as_ref()
                .expect_err("expected linked tail cancellation")
                .raw_os_error(),
            Some(libc::ECANCELED)
        );
        assert_eq!(batch.in_flight, 0);
        assert!(batch.is_complete());
    }
}
