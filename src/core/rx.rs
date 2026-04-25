use crate::core::{error, Packet, PacketRecycle};
use async_channel::{Receiver, Sender, TryRecvError};
use io_uring::{cqueue, opcode, squeue, types, IoUring};
use std::{
    collections::VecDeque,
    fmt,
    future::{poll_fn, Future},
    io, mem,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    pin::Pin,
    ptr,
    sync::{
        atomic::{fence, AtomicBool, AtomicU16, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll, Waker},
    thread::{self, JoinHandle},
};
use tun_rs::SyncDevice;

type DriverFuture<'a> = Pin<Box<dyn Future<Output = io::Result<()>> + Send + 'a>>;

const RX_BUFFER_GROUP_ID: u16 = 0;
const RX_READ_USER_DATA: u64 = 0x5258_5245_4144;
const RX_CANCEL_USER_DATA: u64 = 0x5258_4341_4e43;
const IO_URING_BUF_RING_HEADER_SIZE: usize = 16;

#[repr(C)]
#[derive(Clone, Copy)]
struct IoUringBuf {
    addr: u64,
    len: u32,
    bid: u16,
    resv: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RxStartMode {
    AutoStart,
    ManualStart,
}

#[derive(Clone, Debug)]
pub enum RxState {
    Running,
    Stopped,
    Faulted(Arc<io::Error>),
}

#[allow(dead_code)]
impl RxState {
    pub(crate) fn initial(start_mode: RxStartMode) -> Self {
        match start_mode {
            RxStartMode::AutoStart => Self::Running,
            RxStartMode::ManualStart => Self::Stopped,
        }
    }

    pub(crate) fn stop(&mut self) {
        *self = Self::Stopped;
    }

    pub(crate) fn restart(&mut self) {
        *self = Self::Running;
    }

    pub(crate) fn fault(&mut self, error: io::Error) {
        *self = Self::Faulted(Arc::new(error));
    }

    pub(crate) fn terminal_error(&self) -> Option<io::Error> {
        match self {
            Self::Running => None,
            Self::Stopped => Some(error::rx_stopped()),
            Self::Faulted(error) => Some(error::clone_io_error(error)),
        }
    }
}

pub(crate) trait RxDriverOps {
    fn start_sync(&self) -> io::Result<()>;
    fn stop_async(&self) -> DriverFuture<'_>;
}

pub(crate) struct RxController<D = RxDriverHandle> {
    shared: RxShared,
    driver: D,
}

impl RxController<RxDriverHandle> {
    pub(crate) fn new(
        device: SyncDevice,
        ring_entries: u32,
        buffer_len: usize,
        buffer_count: usize,
        packets_include_virtio_net_hdr: bool,
        auto_resume_after_recycled_slots: usize,
        start_mode: RxStartMode,
        thread_name: &'static str,
    ) -> io::Result<Self> {
        let shared = RxShared::default();
        let driver = RxDriverHandle::spawn(
            device,
            ring_entries,
            buffer_len,
            buffer_count,
            packets_include_virtio_net_hdr,
            auto_resume_after_recycled_slots,
            shared.clone(),
            thread_name,
        )?;
        let mut controller = Self { shared, driver };

        if matches!(start_mode, RxStartMode::AutoStart) {
            controller.start()?;
        }

        Ok(controller)
    }
}

impl<D> RxController<D>
where
    D: RxDriverOps,
{
    pub(crate) fn state(&self) -> RxState {
        self.shared.state()
    }

    pub(crate) fn ready_len(&self) -> usize {
        self.shared.ready_len()
    }

    pub(crate) fn waiter_registered(&self) -> bool {
        self.shared.waiter_registered()
    }

    pub(crate) fn start(&mut self) -> io::Result<()> {
        if matches!(self.shared.state(), RxState::Running) {
            return Ok(());
        }

        match self.driver.start_sync() {
            Ok(()) => {
                self.shared.set_running();
                Ok(())
            }
            Err(error) => {
                self.shared.set_faulted(error::clone_io_error(&error));
                Err(error)
            }
        }
    }

    pub(crate) async fn stop(&mut self) -> io::Result<()> {
        match self.shared.state() {
            RxState::Stopped | RxState::Faulted(_) => return Ok(()),
            RxState::Running => {}
        }

        match self.driver.stop_async().await {
            Ok(()) => {
                self.shared.set_stopped();
                Ok(())
            }
            Err(error) => {
                self.shared.set_faulted(error::clone_io_error(&error));
                Err(error)
            }
        }
    }

    pub(crate) async fn readable(&self) -> io::Result<()> {
        self.shared.readable().await
    }

    pub(crate) fn try_recv(&self) -> io::Result<Packet> {
        self.shared.try_recv()
    }

    pub(crate) async fn recv(&self) -> io::Result<Packet> {
        loop {
            match self.try_recv() {
                Ok(packet) => return Ok(packet),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => self.readable().await?,
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) fn try_recv_many(&self, out: &mut [Option<Packet>]) -> io::Result<usize> {
        self.shared.try_recv_many(out)
    }

    pub(crate) async fn recv_many(&self, out: &mut [Option<Packet>]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }

        loop {
            match self.try_recv_many(out) {
                Ok(count) => return Ok(count),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => self.readable().await?,
                Err(error) => return Err(error),
            }
        }
    }
}

impl<D> fmt::Debug for RxController<D>
where
    D: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RxController")
            .field("state", &self.shared.state())
            .field("ready_len", &self.shared.ready_len())
            .field("waiter_registered", &self.shared.waiter_registered())
            .field("driver", &self.driver)
            .finish()
    }
}

#[derive(Clone, Default)]
struct RxShared {
    inner: Arc<Mutex<RxSharedState>>,
}

#[derive(Default)]
struct RxSharedState {
    state: RxState,
    packets: VecDeque<Packet>,
    waiter: RxWaiterSlot,
}

impl Default for RxState {
    fn default() -> Self {
        Self::Stopped
    }
}

impl RxShared {
    #[cfg(test)]
    fn with_state(state: RxState) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RxSharedState {
                state,
                packets: VecDeque::new(),
                waiter: RxWaiterSlot::default(),
            })),
        }
    }

    fn state(&self) -> RxState {
        self.inner.lock().unwrap().state.clone()
    }

    fn ready_len(&self) -> usize {
        self.inner.lock().unwrap().packets.len()
    }

    fn waiter_registered(&self) -> bool {
        self.inner.lock().unwrap().waiter.is_registered()
    }

    fn set_running(&self) {
        self.inner.lock().unwrap().state.restart();
    }

    fn set_stopped(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.state.stop();
        inner.waiter.wake();
    }

    fn set_faulted(&self, error: io::Error) {
        let mut inner = self.inner.lock().unwrap();
        inner.state.fault(error);
        inner.waiter.wake();
    }

    fn push_packet(&self, packet: Packet) {
        let mut inner = self.inner.lock().unwrap();
        inner.packets.push_back(packet);
        inner.waiter.wake();
    }

    fn try_recv(&self) -> io::Result<Packet> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(packet) = inner.packets.pop_front() {
            return Ok(packet);
        }

        match inner.state.terminal_error() {
            Some(error) => Err(error),
            None => Err(io::Error::from(io::ErrorKind::WouldBlock)),
        }
    }

    fn try_recv_many(&self, out: &mut [Option<Packet>]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }

        let mut inner = self.inner.lock().unwrap();
        if inner.packets.is_empty() {
            return match inner.state.terminal_error() {
                Some(error) => Err(error),
                None => Err(io::Error::from(io::ErrorKind::WouldBlock)),
            };
        }

        let mut count = 0;
        while count < out.len() {
            match inner.packets.pop_front() {
                Some(packet) => {
                    out[count] = Some(packet);
                    count += 1;
                }
                None => break,
            }
        }

        Ok(count)
    }

    fn poll_readable(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut inner = self.inner.lock().unwrap();
        if !inner.packets.is_empty() {
            return Poll::Ready(Ok(()));
        }

        if let Some(error) = inner.state.terminal_error() {
            return Poll::Ready(Err(error));
        }

        inner.waiter.register(cx.waker());
        Poll::Pending
    }

    async fn readable(&self) -> io::Result<()> {
        poll_fn(|cx| self.poll_readable(cx)).await
    }
}

struct RxRingContext {
    ring: IoUring,
    cq_eventfd: OwnedFd,
    buffers: ProvidedBuffers,
}

impl RxRingContext {
    fn new(entries: u32, buffer_len: usize, buffer_count: usize) -> io::Result<Self> {
        let ring = IoUring::new(entries)?;
        if !ring.params().is_feature_fast_poll() {
            return Err(error::rx_fast_poll_required());
        }

        let cq_eventfd = new_eventfd()?;
        ring.submitter().register_eventfd(cq_eventfd.as_raw_fd())?;

        let buffers = ProvidedBuffers::new(buffer_len, buffer_count, RX_BUFFER_GROUP_ID)?;
        unsafe {
            ring.submitter().register_buf_ring_with_flags(
                buffers.ring_addr(),
                buffers.entry_count(),
                buffers.group_id(),
                0,
            )?;
        }
        buffers.publish_all();

        Ok(Self {
            ring,
            cq_eventfd,
            buffers,
        })
    }

    fn eventfd_raw_fd(&self) -> i32 {
        self.cq_eventfd.as_raw_fd()
    }
}

impl fmt::Debug for RxRingContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RxRingContext")
            .field("ring_fd", &self.ring.as_raw_fd())
            .field("cq_eventfd", &self.eventfd_raw_fd())
            .field("fast_poll", &self.ring.params().is_feature_fast_poll())
            .field("buffer_count", &self.buffers.buffer_count())
            .field("buffer_len", &self.buffers.buffer_len())
            .finish()
    }
}

impl Drop for RxRingContext {
    fn drop(&mut self) {
        self.buffers.deactivate();
        let _ = self
            .ring
            .submitter()
            .unregister_buf_ring(self.buffers.group_id());
        let _ = self.ring.submitter().unregister_eventfd();
    }
}

#[derive(Clone)]
struct RxAutoResume {
    inner: Arc<RxAutoResumeInner>,
}

struct RxAutoResumeInner {
    threshold: usize,
    pending_enobufs: AtomicBool,
    recycled_since_fault: AtomicUsize,
    wake_enqueued: AtomicBool,
    commands: Sender<RxDriverCommand>,
    command_eventfd: OwnedFd,
}

impl RxAutoResume {
    fn new(threshold: usize, commands: Sender<RxDriverCommand>, command_eventfd: OwnedFd) -> Self {
        Self {
            inner: Arc::new(RxAutoResumeInner {
                threshold,
                pending_enobufs: AtomicBool::new(false),
                recycled_since_fault: AtomicUsize::new(0),
                wake_enqueued: AtomicBool::new(false),
                commands,
                command_eventfd,
            }),
        }
    }

    fn threshold(&self) -> usize {
        self.inner.threshold
    }

    fn arm_enobufs(&self) {
        if self.threshold() == 0 {
            self.disarm();
            return;
        }

        self.inner.recycled_since_fault.store(0, Ordering::Release);
        self.inner.wake_enqueued.store(false, Ordering::Release);
        self.inner.pending_enobufs.store(true, Ordering::Release);
    }

    fn disarm(&self) {
        self.inner.pending_enobufs.store(false, Ordering::Release);
        self.inner.recycled_since_fault.store(0, Ordering::Release);
        self.inner.wake_enqueued.store(false, Ordering::Release);
    }

    fn note_recycled(&self) {
        if self.threshold() == 0 || !self.inner.pending_enobufs.load(Ordering::Acquire) {
            return;
        }

        let recycled = self
            .inner
            .recycled_since_fault
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        if recycled < self.threshold() {
            return;
        }

        if self.inner.wake_enqueued.swap(true, Ordering::AcqRel) {
            return;
        }

        if self
            .inner
            .commands
            .send_blocking(RxDriverCommand::AutoResumeCheck)
            .is_ok()
        {
            let _ = signal_eventfd(self.inner.command_eventfd.as_raw_fd());
        }
    }
}

impl fmt::Debug for RxAutoResume {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RxAutoResume")
            .field("threshold", &self.threshold())
            .field(
                "pending_enobufs",
                &self.inner.pending_enobufs.load(Ordering::Acquire),
            )
            .field(
                "recycled_since_fault",
                &self.inner.recycled_since_fault.load(Ordering::Acquire),
            )
            .finish()
    }
}

#[derive(Clone)]
struct ProvidedBuffers {
    inner: Arc<ProvidedBuffersInner>,
}

struct ProvidedBuffersInner {
    ring_memory: PageAlignedMemory,
    buffers: Vec<Box<[u8]>>,
    buffer_len: usize,
    entry_count: u16,
    mask: u16,
    group_id: u16,
    state: Mutex<ProvidedBuffersState>,
}

struct ProvidedBuffersState {
    tail: u16,
    active: bool,
}

impl ProvidedBuffers {
    fn new(buffer_len: usize, buffer_count: usize, group_id: u16) -> io::Result<Self> {
        let entry_count = u16::try_from(buffer_count)
            .map_err(|_| error::invalid_input("rx buffer count exceeds u16 range"))?;
        let ring_bytes = buffer_count
            .checked_mul(mem::size_of::<IoUringBuf>())
            .ok_or_else(|| error::invalid_input("rx buffer ring size overflows usize"))?;
        let ring_memory = PageAlignedMemory::new(ring_bytes)?;
        let buffers = (0..buffer_count)
            .map(|_| vec![0u8; buffer_len].into_boxed_slice())
            .collect();

        Ok(Self {
            inner: Arc::new(ProvidedBuffersInner {
                ring_memory,
                buffers,
                buffer_len,
                entry_count,
                mask: entry_count - 1,
                group_id,
                state: Mutex::new(ProvidedBuffersState {
                    tail: 0,
                    active: true,
                }),
            }),
        })
    }

    fn ring_addr(&self) -> u64 {
        self.inner.ring_memory.as_ptr() as u64
    }

    fn entry_count(&self) -> u16 {
        self.inner.entry_count
    }

    fn group_id(&self) -> u16 {
        self.inner.group_id
    }

    fn buffer_count(&self) -> usize {
        self.inner.buffers.len()
    }

    fn buffer_len(&self) -> usize {
        self.inner.buffer_len
    }

    fn publish_all(&self) {
        let mut state = self.inner.state.lock().unwrap();
        for bid in 0..self.inner.entry_count {
            self.write_buffer_descriptor(state.tail.wrapping_add(bid), bid);
        }
        self.advance_locked(&mut state, self.inner.entry_count);
    }

    fn recycle(&self, bid: u16) {
        let mut state = self.inner.state.lock().unwrap();
        if !state.active {
            return;
        }

        self.write_buffer_descriptor(state.tail, bid);
        self.advance_locked(&mut state, 1);
    }

    fn packet_ptr(&self, bid: u16) -> usize {
        self.inner.buffers[usize::from(bid)].as_ptr() as usize
    }

    fn packet_recycle(&self, bid: u16, auto_resume: RxAutoResume) -> Arc<dyn PacketRecycle> {
        Arc::new(ProvidedBufferRecycle {
            buffers: self.clone(),
            bid,
            auto_resume,
        })
    }

    fn deactivate(&self) {
        self.inner.state.lock().unwrap().active = false;
    }

    fn write_buffer_descriptor(&self, tail: u16, bid: u16) {
        let index = tail & self.inner.mask;
        let entry = unsafe {
            self.inner
                .ring_memory
                .as_ptr()
                .cast::<IoUringBuf>()
                .cast::<IoUringBuf>()
                .add(index as usize)
        };
        let buffer = &self.inner.buffers[usize::from(bid)];

        unsafe {
            ptr::write(
                entry,
                IoUringBuf {
                    addr: buffer.as_ptr() as u64,
                    len: self.inner.buffer_len as u32,
                    bid,
                    resv: 0,
                },
            );
        }
    }

    fn advance_locked(&self, state: &mut ProvidedBuffersState, count: u16) {
        state.tail = state.tail.wrapping_add(count);
        fence(Ordering::Release);
        unsafe {
            self.tail_ptr().store(state.tail, Ordering::Release);
        }
    }

    unsafe fn tail_ptr(&self) -> &AtomicU16 {
        &*(self
            .inner
            .ring_memory
            .as_ptr()
            .cast::<u8>()
            .add(IO_URING_BUF_RING_HEADER_SIZE - mem::size_of::<u16>())
            as *const AtomicU16)
    }
}

#[derive(Clone)]
struct ProvidedBufferRecycle {
    buffers: ProvidedBuffers,
    bid: u16,
    auto_resume: RxAutoResume,
}

impl PacketRecycle for ProvidedBufferRecycle {
    fn recycle(&self) {
        self.buffers.recycle(self.bid);
        self.auto_resume.note_recycled();
    }
}

struct PageAlignedMemory {
    addr: usize,
    size: usize,
}

impl PageAlignedMemory {
    fn new(size: usize) -> io::Result<Self> {
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page_size <= 0 {
            return Err(io::Error::last_os_error());
        }

        let mut ptr = ptr::null_mut();
        let result = unsafe { libc::posix_memalign(&mut ptr, page_size as usize, size) };
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result));
        }

        unsafe {
            ptr::write_bytes(ptr, 0, size);
        }

        Ok(Self {
            addr: ptr as usize,
            size,
        })
    }

    fn as_ptr(&self) -> *mut u8 {
        self.addr as *mut u8
    }
}

impl Drop for PageAlignedMemory {
    fn drop(&mut self) {
        unsafe {
            libc::free(self.addr as *mut libc::c_void);
        }
    }
}

impl fmt::Debug for PageAlignedMemory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageAlignedMemory")
            .field("addr", &self.addr)
            .field("size", &self.size)
            .finish()
    }
}

#[derive(Debug)]
enum RxDriverCommand {
    Start { ack: Sender<io::Result<()>> },
    Stop { ack: Sender<io::Result<()>> },
    Shutdown,
    AutoResumeCheck,
}

struct RxDriverThread {
    ring: RxRingContext,
    device: SyncDevice,
    shared: RxShared,
    commands: Receiver<RxDriverCommand>,
    command_eventfd: OwnedFd,
    running: bool,
    read_active: bool,
    stop_ack: Option<Sender<io::Result<()>>>,
    shutdown_requested: bool,
    auto_resume: RxAutoResume,
    packets_include_virtio_net_hdr: bool,
}

impl RxDriverThread {
    fn run(mut self) {
        loop {
            if let Err(error) = self.drain_commands() {
                self.fail(error);
            }

            if self.shutdown_requested && !self.read_active {
                break;
            }

            if !self.running && !self.read_active {
                match self.commands.recv_blocking() {
                    Ok(command) => {
                        let _ = drain_eventfd(self.command_eventfd.as_raw_fd());
                        if let Err(error) = self.handle_command(command) {
                            self.fail(error);
                        }
                    }
                    Err(_) => break,
                }
                continue;
            }

            if let Err(error) = self.wait_for_activity() {
                self.fail(error);
            }
        }
    }

    fn drain_commands(&mut self) -> io::Result<()> {
        loop {
            match self.commands.try_recv() {
                Ok(command) => self.handle_command(command)?,
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Closed) => return Err(error::rx_driver_closed()),
            }
        }
    }

    fn handle_command(&mut self, command: RxDriverCommand) -> io::Result<()> {
        match command {
            RxDriverCommand::Start { ack } => {
                let result = self.start_rx();
                let _ = ack.send_blocking(result);
            }
            RxDriverCommand::Stop { ack } => {
                let result = self.begin_stop(ack);
                if let Err(error) = result {
                    self.fail(error);
                }
            }
            RxDriverCommand::Shutdown => {
                self.auto_resume.disarm();
                self.shutdown_requested = true;
                self.running = false;
                if self.read_active {
                    self.submit_cancel()?;
                }
            }
            RxDriverCommand::AutoResumeCheck => self.try_auto_resume()?,
        }

        Ok(())
    }

    fn start_rx(&mut self) -> io::Result<()> {
        self.auto_resume.disarm();
        self.running = true;
        if !self.read_active {
            self.submit_multishot_read()?;
        }
        Ok(())
    }

    fn begin_stop(&mut self, ack: Sender<io::Result<()>>) -> io::Result<()> {
        self.auto_resume.disarm();
        self.running = false;
        if !self.read_active {
            let _ = ack.send_blocking(Ok(()));
            return Ok(());
        }

        self.stop_ack = Some(ack);
        self.submit_cancel()
    }

    fn enter_enobufs_fault(&mut self) {
        self.running = false;
        self.auto_resume.arm_enobufs();
        self.shared.set_faulted(error::rx_buffers_exhausted());
    }

    fn try_auto_resume(&mut self) -> io::Result<()> {
        if self.read_active
            || !self
                .auto_resume
                .inner
                .pending_enobufs
                .load(Ordering::Acquire)
        {
            return Ok(());
        }

        self.submit_multishot_read()?;
        self.running = true;
        self.auto_resume.disarm();
        self.shared.set_running();
        Ok(())
    }

    fn submit_multishot_read(&mut self) -> io::Result<()> {
        let entry = opcode::ReadMulti::new(
            types::Fd(self.device.as_raw_fd()),
            self.ring.buffers.buffer_len() as u32,
            self.ring.buffers.group_id(),
        )
        .build()
        .user_data(RX_READ_USER_DATA);
        self.submit_entry(entry)?;
        self.read_active = true;
        Ok(())
    }

    fn submit_cancel(&mut self) -> io::Result<()> {
        let entry = opcode::AsyncCancel::new(RX_READ_USER_DATA)
            .build()
            .user_data(RX_CANCEL_USER_DATA);
        self.submit_entry(entry)
    }

    fn submit_entry(&mut self, entry: squeue::Entry) -> io::Result<()> {
        loop {
            let mut submission = self.ring.ring.submission();
            match unsafe { submission.push(&entry) } {
                Ok(()) => break,
                Err(_) => {
                    drop(submission);
                    self.ring.ring.submit()?;
                }
            }
        }

        self.ring.ring.submit()?;
        Ok(())
    }

    fn wait_for_activity(&mut self) -> io::Result<()> {
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
            let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) };
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
            self.drain_commands()?;
        }

        if fds[1].revents & libc::POLLIN != 0 {
            drain_eventfd(self.ring.eventfd_raw_fd())?;
            self.process_cqes()?;
        }

        Ok(())
    }

    fn process_cqes(&mut self) -> io::Result<()> {
        let entries: Vec<cqueue::Entry> = {
            let completion = self.ring.ring.completion();
            completion.collect()
        };

        for entry in entries {
            match entry.user_data() {
                RX_READ_USER_DATA => self.handle_read_cqe(entry)?,
                RX_CANCEL_USER_DATA => self.handle_cancel_cqe(entry)?,
                _ => {}
            }
        }

        Ok(())
    }

    fn handle_read_cqe(&mut self, entry: cqueue::Entry) -> io::Result<()> {
        let result = entry.result();
        let flags = entry.flags();

        if result >= 0 {
            let bid = cqueue::buffer_select(flags).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "rx multishot completion missing selected buffer id",
                )
            })?;
            let len = result as usize;
            let recycle = self
                .ring
                .buffers
                .packet_recycle(bid, self.auto_resume.clone());
            let packet = if self.packets_include_virtio_net_hdr {
                if len <= tun_rs::VIRTIO_NET_HDR_LEN {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "rx offload packet is shorter than virtio header",
                    ));
                }
                Packet::from_ring_with_virtio_net_hdr(
                    self.ring.buffers.packet_ptr(bid),
                    len,
                    recycle,
                )
            } else {
                Packet::from_ring(self.ring.buffers.packet_ptr(bid), len, recycle, None)
            };
            self.shared.push_packet(packet);
        } else if result == -libc::ENOBUFS {
            self.enter_enobufs_fault();
        } else if result != -libc::ECANCELED || self.running {
            return Err(io::Error::from_raw_os_error(-result));
        }

        if !cqueue::more(flags) {
            self.read_active = false;
            if self.running {
                self.submit_multishot_read()?;
            } else if let Some(ack) = self.stop_ack.take() {
                let _ = ack.send_blocking(Ok(()));
                self.shared.set_stopped();
            }
        }

        Ok(())
    }

    fn handle_cancel_cqe(&mut self, entry: cqueue::Entry) -> io::Result<()> {
        let result = entry.result();
        if result < 0 && result != -libc::ENOENT {
            return Err(io::Error::from_raw_os_error(-result));
        }

        if !self.read_active {
            if let Some(ack) = self.stop_ack.take() {
                let _ = ack.send_blocking(Ok(()));
                self.shared.set_stopped();
            }
        }

        Ok(())
    }

    fn fail(&mut self, error: io::Error) {
        self.running = false;
        self.read_active = false;
        self.auto_resume.disarm();
        self.shared.set_faulted(error::clone_io_error(&error));
        if let Some(ack) = self.stop_ack.take() {
            let _ = ack.send_blocking(Err(error));
        }
    }
}

pub(crate) struct RxDriverHandle {
    commands: Sender<RxDriverCommand>,
    command_eventfd: OwnedFd,
    thread_name: &'static str,
    thread: Option<JoinHandle<()>>,
}

impl RxDriverHandle {
    fn spawn(
        device: SyncDevice,
        ring_entries: u32,
        buffer_len: usize,
        buffer_count: usize,
        packets_include_virtio_net_hdr: bool,
        auto_resume_after_recycled_slots: usize,
        shared: RxShared,
        thread_name: &'static str,
    ) -> io::Result<Self> {
        let ring = RxRingContext::new(ring_entries, buffer_len, buffer_count)?;
        let (commands_tx, commands_rx) = async_channel::unbounded();
        let command_eventfd_read = new_eventfd()?;
        let command_eventfd_write = dup_fd(&command_eventfd_read)?;
        let auto_resume = RxAutoResume::new(
            auto_resume_after_recycled_slots,
            commands_tx.clone(),
            dup_fd(&command_eventfd_read)?,
        );

        let thread = thread::Builder::new()
            .name(thread_name.to_string())
            .spawn(move || {
                RxDriverThread {
                    ring,
                    device,
                    shared,
                    commands: commands_rx,
                    command_eventfd: command_eventfd_read,
                    running: false,
                    read_active: false,
                    stop_ack: None,
                    shutdown_requested: false,
                    auto_resume,
                    packets_include_virtio_net_hdr,
                }
                .run();
            })?;

        Ok(Self {
            commands: commands_tx,
            command_eventfd: command_eventfd_write,
            thread_name,
            thread: Some(thread),
        })
    }

    fn send_command(&self, command: RxDriverCommand) -> io::Result<()> {
        self.commands
            .send_blocking(command)
            .map_err(|_| error::rx_driver_closed())?;
        signal_eventfd(self.command_eventfd.as_raw_fd())?;
        Ok(())
    }
}

impl RxDriverOps for RxDriverHandle {
    fn start_sync(&self) -> io::Result<()> {
        let (ack_tx, ack_rx) = async_channel::bounded(1);
        self.send_command(RxDriverCommand::Start { ack: ack_tx })?;
        ack_rx
            .recv_blocking()
            .map_err(|_| error::rx_driver_closed())?
    }

    fn stop_async(&self) -> DriverFuture<'_> {
        Box::pin(async move {
            let (ack_tx, ack_rx) = async_channel::bounded(1);
            self.send_command(RxDriverCommand::Stop { ack: ack_tx })?;
            ack_rx.recv().await.map_err(|_| error::rx_driver_closed())?
        })
    }
}

impl fmt::Debug for RxDriverHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RxDriverHandle")
            .field("thread_name", &self.thread_name)
            .finish()
    }
}

impl Drop for RxDriverHandle {
    fn drop(&mut self) {
        let _ = self.send_command(RxDriverCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Default)]
pub(crate) struct RxWaiterSlot {
    waker: Option<Waker>,
}

impl RxWaiterSlot {
    pub(crate) fn is_registered(&self) -> bool {
        self.waker.is_some()
    }

    pub(crate) fn register(&mut self, waker: &Waker) -> bool {
        if self
            .waker
            .as_ref()
            .is_some_and(|registered| registered.will_wake(waker))
        {
            return false;
        }

        self.waker = Some(waker.clone());
        true
    }

    #[cfg(test)]
    pub(crate) fn clear(&mut self) {
        self.waker = None;
    }

    pub(crate) fn wake(&mut self) -> bool {
        match self.waker.take() {
            Some(waker) => {
                waker.wake();
                true
            }
            None => false,
        }
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
        new_eventfd, DriverFuture, RxAutoResume, RxController, RxDriverCommand, RxDriverOps,
        RxRingContext, RxShared, RxStartMode, RxState, RxWaiterSlot,
    };
    use crate::{
        core::testutil::{block_on_immediate, WakeCounter},
        Packet,
    };
    use async_channel::TryRecvError;
    use std::{
        io,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        },
        task::{Context, Poll},
    };

    #[derive(Debug, Default)]
    struct FakeDriver {
        starts: Arc<AtomicUsize>,
        stops: Arc<AtomicUsize>,
        fail_start: AtomicBool,
        fail_stop: AtomicBool,
    }

    impl FakeDriver {
        fn new() -> Self {
            Self::default()
        }
    }

    fn supports_live_rx_driver_tests(error: &io::Error) -> bool {
        matches!(error.raw_os_error(), Some(libc::EPERM) | Some(libc::ENOSYS))
    }

    impl RxDriverOps for FakeDriver {
        fn start_sync(&self) -> io::Result<()> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            if self.fail_start.load(Ordering::SeqCst) {
                Err(io::Error::other("start failed"))
            } else {
                Ok(())
            }
        }

        fn stop_async(&self) -> DriverFuture<'_> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            let fail = self.fail_stop.load(Ordering::SeqCst);
            Box::pin(async move {
                if fail {
                    Err(io::Error::other("stop failed"))
                } else {
                    Ok(())
                }
            })
        }
    }

    #[test]
    fn initial_state_depends_on_start_mode() {
        assert!(matches!(
            RxState::initial(RxStartMode::AutoStart),
            RxState::Running
        ));
        assert!(matches!(
            RxState::initial(RxStartMode::ManualStart),
            RxState::Stopped
        ));
    }

    #[test]
    fn state_transitions_cover_stop_fault_and_restart() {
        let mut state = RxState::initial(RxStartMode::AutoStart);
        state.stop();
        assert!(matches!(state, RxState::Stopped));

        state.restart();
        assert!(matches!(state, RxState::Running));

        state.fault(io::Error::other("boom"));
        match state {
            RxState::Faulted(error) => assert_eq!(error.kind(), io::ErrorKind::Other),
            _ => panic!("expected faulted state"),
        }
    }

    #[test]
    fn terminal_error_reflects_state() {
        assert!(RxState::Running.terminal_error().is_none());

        let stopped = RxState::Stopped.terminal_error().unwrap();
        assert_eq!(stopped.kind(), io::ErrorKind::BrokenPipe);

        let mut state = RxState::Running;
        state.fault(io::Error::new(io::ErrorKind::TimedOut, "timeout"));
        let error = state.terminal_error().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn live_ring_initialization_registers_buffers() {
        match RxRingContext::new(8, 2048, 32) {
            Ok(context) => {
                assert_eq!(context.buffers.buffer_count(), 32);
                assert_eq!(context.buffers.buffer_len(), 2048);
            }
            Err(error) if supports_live_rx_driver_tests(&error) => {}
            Err(error) => panic!("unexpected rx ring init failure: {error}"),
        }
    }

    #[test]
    fn auto_start_starts_driver_and_enters_running() {
        let driver = FakeDriver::new();
        let starts = Arc::clone(&driver.starts);
        let mut controller = RxController {
            shared: RxShared::default(),
            driver,
        };

        controller.start().unwrap();

        assert!(matches!(controller.state(), RxState::Running));
        assert_eq!(starts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn start_is_idempotent_when_already_running() {
        let driver = FakeDriver::new();
        let starts = Arc::clone(&driver.starts);
        let mut controller = RxController {
            shared: RxShared::with_state(RxState::Running),
            driver,
        };

        controller.start().unwrap();

        assert_eq!(starts.load(Ordering::SeqCst), 0);
        assert!(matches!(controller.state(), RxState::Running));
    }

    #[test]
    fn stop_is_idempotent_when_already_stopped() {
        let driver = FakeDriver::new();
        let stops = Arc::clone(&driver.stops);
        let mut controller = RxController {
            shared: RxShared::default(),
            driver,
        };

        block_on_immediate(controller.stop()).unwrap();

        assert_eq!(stops.load(Ordering::SeqCst), 0);
        assert!(matches!(controller.state(), RxState::Stopped));
    }

    #[test]
    fn faulted_stop_keeps_faulted_state() {
        let driver = FakeDriver::new();
        let stops = Arc::clone(&driver.stops);
        let mut controller = RxController {
            shared: RxShared::with_state(RxState::Faulted(Arc::new(io::Error::other("boom")))),
            driver,
        };

        block_on_immediate(controller.stop()).unwrap();

        assert_eq!(stops.load(Ordering::SeqCst), 0);
        assert!(matches!(controller.state(), RxState::Faulted(_)));
    }

    #[test]
    fn running_stopped_running_round_trip_works() {
        let driver = FakeDriver::new();
        let starts = Arc::clone(&driver.starts);
        let stops = Arc::clone(&driver.stops);
        let mut controller = RxController {
            shared: RxShared::default(),
            driver,
        };

        controller.start().unwrap();
        block_on_immediate(controller.stop()).unwrap();
        controller.start().unwrap();

        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert_eq!(stops.load(Ordering::SeqCst), 1);
        assert!(matches!(controller.state(), RxState::Running));
    }

    #[test]
    fn failed_start_transitions_to_faulted() {
        let driver = FakeDriver::new();
        driver.fail_start.store(true, Ordering::SeqCst);
        let mut controller = RxController {
            shared: RxShared::default(),
            driver,
        };

        let error = controller.start().unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(matches!(controller.state(), RxState::Faulted(_)));
    }

    #[test]
    fn auto_resume_notifier_is_inert_until_enobufs_fault_is_armed() {
        let (commands_tx, commands_rx) = async_channel::unbounded();
        let auto_resume = RxAutoResume::new(2, commands_tx, new_eventfd().unwrap());

        auto_resume.note_recycled();

        assert!(matches!(commands_rx.try_recv(), Err(TryRecvError::Empty)));

        auto_resume.arm_enobufs();
        auto_resume.note_recycled();

        assert!(matches!(commands_rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn auto_resume_notifier_enqueues_single_wakeup_once_threshold_is_reached() {
        let (commands_tx, commands_rx) = async_channel::unbounded();
        let auto_resume = RxAutoResume::new(2, commands_tx, new_eventfd().unwrap());

        auto_resume.arm_enobufs();
        auto_resume.note_recycled();
        assert!(matches!(commands_rx.try_recv(), Err(TryRecvError::Empty)));

        auto_resume.note_recycled();
        match commands_rx.try_recv().unwrap() {
            RxDriverCommand::AutoResumeCheck => {}
            command => panic!("unexpected command: {command:?}"),
        }

        auto_resume.note_recycled();
        assert!(matches!(commands_rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn auto_resume_notifier_disarm_and_rearm_reset_threshold_tracking() {
        let (commands_tx, commands_rx) = async_channel::unbounded();
        let auto_resume = RxAutoResume::new(1, commands_tx, new_eventfd().unwrap());

        auto_resume.arm_enobufs();
        auto_resume.note_recycled();
        match commands_rx.try_recv().unwrap() {
            RxDriverCommand::AutoResumeCheck => {}
            command => panic!("unexpected command: {command:?}"),
        }

        auto_resume.disarm();
        auto_resume.note_recycled();
        assert!(matches!(commands_rx.try_recv(), Err(TryRecvError::Empty)));

        auto_resume.arm_enobufs();
        auto_resume.note_recycled();
        match commands_rx.try_recv().unwrap() {
            RxDriverCommand::AutoResumeCheck => {}
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn try_recv_returns_would_block_while_running_without_packets() {
        let shared = RxShared::with_state(RxState::Running);

        let error = shared.try_recv().unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    }

    #[test]
    fn try_recv_returns_terminal_error_when_queue_is_empty() {
        let stopped = RxShared::default().try_recv().unwrap_err();
        assert_eq!(stopped.kind(), io::ErrorKind::BrokenPipe);

        let faulted = RxShared::with_state(RxState::Faulted(Arc::new(io::Error::new(
            io::ErrorKind::TimedOut,
            "timeout",
        ))))
        .try_recv()
        .unwrap_err();
        assert_eq!(faulted.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn try_recv_many_drains_current_queue() {
        let shared = RxShared::with_state(RxState::Running);
        shared.push_packet(Packet::from_owned(vec![1, 2, 3]));
        shared.push_packet(Packet::from_owned(vec![4, 5, 6]));

        let mut out = [None, None, None];
        let count = shared.try_recv_many(&mut out).unwrap();

        assert_eq!(count, 2);
        assert_eq!(out[0].as_ref().unwrap().as_bytes(), &[1, 2, 3]);
        assert_eq!(out[1].as_ref().unwrap().as_bytes(), &[4, 5, 6]);
        assert!(out[2].is_none());
        assert_eq!(shared.ready_len(), 0);
    }

    #[test]
    fn readable_waits_until_packet_or_terminal_state() {
        let shared = RxShared::with_state(RxState::Running);
        let waker = WakeCounter::new();
        let task_waker = waker.waker();
        let mut context = Context::from_waker(&task_waker);

        assert!(matches!(shared.poll_readable(&mut context), Poll::Pending));
        assert_eq!(waker.count(), 0);

        shared.push_packet(Packet::from_owned(vec![7, 8, 9]));
        assert_eq!(waker.count(), 1);
    }

    #[test]
    fn waiter_slot_replaces_and_wakes_registered_waker() {
        let first = WakeCounter::new();
        let second = WakeCounter::new();
        let mut slot = RxWaiterSlot::default();

        assert!(slot.register(&first.waker()));
        assert!(slot.is_registered());
        assert!(!slot.register(&first.waker()));
        assert!(slot.register(&second.waker()));
        assert!(slot.wake());
        assert_eq!(first.count(), 0);
        assert_eq!(second.count(), 1);
        assert!(!slot.is_registered());
    }

    #[test]
    fn waiter_slot_clear_discards_registration() {
        let counter = WakeCounter::new();
        let mut slot = RxWaiterSlot::default();
        slot.register(&counter.waker());

        slot.clear();

        assert!(!slot.wake());
        assert_eq!(counter.count(), 0);
    }
}
