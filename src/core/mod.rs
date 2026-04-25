//! Runtime-agnostic shared core for the `UringDevice` implementation.

mod config;
pub(crate) mod error;
mod packet;
mod rx;
#[cfg(test)]
pub(crate) mod testutil;
mod tx;

use bytes::Bytes;
use std::fmt;
use std::future::Future;
use std::io;
use std::os::fd::AsRawFd;
use std::time::Duration;
use tun_rs::SyncDevice;

pub use self::config::UringDeviceConfig;
pub use self::packet::{GsoType, OffloadInfo, Packet};
pub use self::rx::{RxStartMode, RxState};
pub(crate) use self::{
    config::ValidatedConfig, packet::PacketRecycle, rx::RxController, tx::TxController,
};

/// Minimal shared device shell used by all runtime backends.
#[allow(dead_code)]
pub(crate) struct CoreDevice {
    device: SyncDevice,
    config: ValidatedConfig,
    packets_include_virtio_net_hdr: bool,
    rx: RxController,
    tx: TxController,
}

#[allow(dead_code)]
impl CoreDevice {
    pub(crate) fn new(
        device: SyncDevice,
        config: UringDeviceConfig,
        rx_driver_name: &'static str,
    ) -> std::io::Result<Self> {
        let config = ValidatedConfig::try_from(config)?;
        let packets_include_virtio_net_hdr = device.tcp_gso();
        let rx_buffer_len = config
            .rx_buffer_len
            .checked_add(if packets_include_virtio_net_hdr {
                tun_rs::VIRTIO_NET_HDR_LEN
            } else {
                0
            })
            .ok_or_else(|| error::invalid_input("effective rx buffer len overflows usize"))?;
        device.set_nonblocking(true)?;
        let tx_device = duplicate_device(&device)?;
        let tx_ring_entries = config.tx_ring_entries;
        let tx_submit_chunk_size = config.tx_submit_chunk_size;
        let rx = RxController::new(
            duplicate_device(&device)?,
            config.rx_ring_entries,
            rx_buffer_len,
            config.rx_buffer_count,
            packets_include_virtio_net_hdr,
            config.rx_auto_resume_after_recycled_slots,
            config.rx_start_mode,
            rx_driver_name,
        )?;

        Ok(Self {
            device,
            config,
            packets_include_virtio_net_hdr,
            rx,
            tx: TxController::new(tx_device, tx_ring_entries, tx_submit_chunk_size, "uring-tx")?,
        })
    }

    pub(crate) fn rx_state(&self) -> RxState {
        self.rx.state()
    }

    pub(crate) fn ready_len(&self) -> usize {
        self.rx.ready_len()
    }

    pub(crate) fn start_rx(&mut self) -> std::io::Result<()> {
        self.rx.start()
    }

    pub(crate) async fn stop_rx(&mut self) -> std::io::Result<()> {
        self.rx.stop().await
    }

    pub(crate) async fn readable(&self) -> std::io::Result<()> {
        self.rx.readable().await
    }

    pub(crate) fn try_recv(&self) -> std::io::Result<Packet> {
        self.rx.try_recv()
    }

    pub(crate) async fn recv(&self) -> std::io::Result<Packet> {
        self.rx.recv().await
    }

    pub(crate) async fn recv_many(&self, out: &mut [Option<Packet>]) -> std::io::Result<usize> {
        self.rx.recv_many(out).await
    }

    pub(crate) fn try_send(&self, buf: &[u8]) -> std::io::Result<usize> {
        self.device.send(buf)
    }

    pub(crate) async fn send_many<TimerFuture>(
        &self,
        bufs: Vec<Bytes>,
        results: &mut [Option<std::io::Result<usize>>],
        timeout: Duration,
        keep_order: bool,
        make_timer: impl FnOnce(Duration) -> TimerFuture,
    ) -> Vec<Bytes>
    where
        TimerFuture: Future,
    {
        self.tx
            .send_many(bufs, results, timeout, keep_order, make_timer)
            .await
    }

    pub(crate) fn duplicate_device(&self) -> std::io::Result<SyncDevice> {
        duplicate_device(&self.device)
    }
}

impl fmt::Debug for CoreDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoreDevice")
            .field("config", &self.config)
            .field(
                "packets_include_virtio_net_hdr",
                &self.packets_include_virtio_net_hdr,
            )
            .field("rx_state", &self.rx.state())
            .field("ready_len", &self.rx.ready_len())
            .field("rx_waiter_registered", &self.rx.waiter_registered())
            .field("tx_batch_phase", &self.tx.phase())
            .finish()
    }
}

pub(crate) fn duplicate_device(device: &SyncDevice) -> io::Result<SyncDevice> {
    let duplicated = unsafe { libc::fcntl(device.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if duplicated < 0 {
        return Err(io::Error::last_os_error());
    }

    unsafe { SyncDevice::from_fd(duplicated) }
}
