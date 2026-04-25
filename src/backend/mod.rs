#[cfg(all(feature = "async_io", not(feature = "async_tokio")))]
mod async_io;
#[cfg(all(feature = "async_tokio", not(feature = "async_io")))]
mod async_tokio;
#[cfg(not(any(feature = "async_tokio", feature = "async_io")))]
mod no_backend;

#[cfg(all(feature = "async_io", not(feature = "async_tokio")))]
pub(crate) use self::async_io::BackendDevice;
#[cfg(all(feature = "async_tokio", not(feature = "async_io")))]
pub(crate) use self::async_tokio::BackendDevice;
#[cfg(not(any(feature = "async_tokio", feature = "async_io")))]
pub(crate) use self::no_backend::BackendDevice;

#[cfg(not(all(feature = "async_tokio", feature = "async_io")))]
use crate::core::{Packet, RxState, UringDeviceConfig};
#[cfg(not(all(feature = "async_tokio", feature = "async_io")))]
use bytes::Bytes;
#[cfg(not(all(feature = "async_tokio", feature = "async_io")))]
use std::io;
#[cfg(not(all(feature = "async_tokio", feature = "async_io")))]
use std::time::Duration;
#[cfg(not(all(feature = "async_tokio", feature = "async_io")))]
use tun_rs::SyncDevice;

#[cfg(all(feature = "async_io", not(feature = "async_tokio")))]
pub(crate) const fn backend_name() -> &'static str {
    "async_io"
}

#[cfg(all(feature = "async_tokio", not(feature = "async_io")))]
pub(crate) const fn backend_name() -> &'static str {
    "async_tokio"
}

#[cfg(not(any(feature = "async_tokio", feature = "async_io")))]
pub(crate) const fn backend_name() -> &'static str {
    "none"
}

#[cfg(not(all(feature = "async_tokio", feature = "async_io")))]
impl BackendDevice {
    pub(crate) fn new(device: SyncDevice, config: UringDeviceConfig) -> io::Result<Self> {
        Self::new_impl(device, config)
    }

    pub(crate) fn rx_state(&self) -> RxState {
        self.rx_state_impl()
    }

    pub(crate) fn ready_len(&self) -> usize {
        self.ready_len_impl()
    }

    pub(crate) fn start_rx(&mut self) -> io::Result<()> {
        self.start_rx_impl()
    }

    pub(crate) async fn stop_rx(&mut self) -> io::Result<()> {
        self.stop_rx_impl().await
    }

    pub(crate) fn try_send(&self, buf: &[u8]) -> io::Result<usize> {
        self.try_send_impl(buf)
    }

    pub(crate) async fn readable(&mut self) -> io::Result<()> {
        self.readable_impl().await
    }

    pub(crate) fn try_recv(&mut self) -> io::Result<Packet> {
        self.try_recv_impl()
    }

    pub(crate) async fn recv(&mut self) -> io::Result<Packet> {
        self.recv_impl().await
    }

    pub(crate) async fn recv_many(&mut self, out: &mut [Option<Packet>]) -> io::Result<usize> {
        self.recv_many_impl(out).await
    }

    pub(crate) async fn writable(&self) -> io::Result<()> {
        self.writable_impl().await
    }

    pub(crate) async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        self.send_impl(buf).await
    }

    pub(crate) async fn send_many(
        &self,
        bufs: Vec<Bytes>,
        results: &mut [Option<io::Result<usize>>],
        timeout: Duration,
        keep_order: bool,
    ) -> Vec<Bytes> {
        self.send_many_impl(bufs, results, timeout, keep_order)
            .await
    }
}
