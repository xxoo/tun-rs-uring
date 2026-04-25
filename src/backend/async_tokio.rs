use crate::core::{write_fd, CoreDevice, Packet, RxState, UringDeviceConfig};
use bytes::Bytes;
use std::os::fd::{AsRawFd, OwnedFd};
use std::time::Duration;
use std::{fmt, io};
use tokio::io::unix::AsyncFd;
use tokio::time;
use tun_rs::SyncDevice;

pub(crate) struct BackendDevice {
    core: CoreDevice,
    io: AsyncFd<OwnedFd>,
}

impl BackendDevice {
    pub(crate) fn new_impl(device: SyncDevice, config: UringDeviceConfig) -> io::Result<Self> {
        let core = CoreDevice::new(device, config, "uring-rx-tokio")?;
        let io = AsyncFd::new(core.duplicate_fd()?)?;

        Ok(Self { core, io })
    }

    pub(crate) fn try_clone_impl(&self) -> io::Result<Self> {
        Self::new_impl(self.core.try_clone_device()?, self.core.config())
    }

    pub(crate) fn rx_state_impl(&self) -> RxState {
        self.core.rx_state()
    }

    pub(crate) fn ready_len_impl(&self) -> usize {
        self.core.ready_len()
    }

    pub(crate) fn start_rx_impl(&mut self) -> io::Result<()> {
        self.core.start_rx()
    }

    pub(crate) async fn stop_rx_impl(&mut self) -> io::Result<()> {
        self.core.stop_rx().await
    }

    pub(crate) fn try_send_impl(&self, buf: &[u8]) -> io::Result<usize> {
        self.core.try_send(buf)
    }

    pub(crate) async fn readable_impl(&mut self) -> io::Result<()> {
        self.core.readable().await
    }

    pub(crate) fn try_recv_impl(&mut self) -> io::Result<Packet> {
        self.core.try_recv()
    }

    pub(crate) async fn recv_impl(&mut self) -> io::Result<Packet> {
        self.core.recv().await
    }

    pub(crate) async fn recv_many_impl(&mut self, out: &mut [Option<Packet>]) -> io::Result<usize> {
        self.core.recv_many(out).await
    }

    pub(crate) async fn writable_impl(&self) -> io::Result<()> {
        self.io.writable().await.map(|_| ())
    }

    pub(crate) async fn send_impl(&self, buf: &[u8]) -> io::Result<usize> {
        loop {
            match self.core.try_send(buf) {
                Ok(written) => return Ok(written),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }

            let mut guard = self.io.writable().await?;
            match guard.try_io(|io| write_fd(io.get_ref().as_raw_fd(), buf)) {
                Ok(result) => return result,
                Err(_would_block) => continue,
            }
        }
    }

    pub(crate) async fn send_many_impl(
        &self,
        bufs: Vec<Bytes>,
        results: &mut [Option<io::Result<usize>>],
        timeout: Duration,
        keep_order: bool,
    ) -> Vec<Bytes> {
        self.core
            .send_many(bufs, results, timeout, keep_order, time::sleep)
            .await
    }
}

impl fmt::Debug for BackendDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BackendDevice")
            .field("core", &self.core)
            .finish()
    }
}
