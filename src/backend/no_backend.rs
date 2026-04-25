use crate::core::{error, Packet, RxState, UringDeviceConfig};
use bytes::Bytes;
use std::io;
use std::time::Duration;
use tun_rs::SyncDevice;

#[derive(Debug)]
pub(crate) struct BackendDevice {
    _private: (),
}

impl BackendDevice {
    pub(crate) fn new_impl(_device: SyncDevice, _config: UringDeviceConfig) -> io::Result<Self> {
        Err(error::no_async_backend())
    }

    pub(crate) fn rx_state_impl(&self) -> RxState {
        RxState::Stopped
    }

    pub(crate) fn ready_len_impl(&self) -> usize {
        0
    }

    pub(crate) fn start_rx_impl(&mut self) -> io::Result<()> {
        Err(error::no_async_backend())
    }

    pub(crate) async fn stop_rx_impl(&mut self) -> io::Result<()> {
        Err(error::no_async_backend())
    }

    pub(crate) fn try_send_impl(&self, _buf: &[u8]) -> io::Result<usize> {
        Err(error::no_async_backend())
    }

    pub(crate) async fn readable_impl(&mut self) -> io::Result<()> {
        Err(error::no_async_backend())
    }

    pub(crate) fn try_recv_impl(&mut self) -> io::Result<Packet> {
        Err(error::no_async_backend())
    }

    pub(crate) async fn recv_impl(&mut self) -> io::Result<Packet> {
        Err(error::no_async_backend())
    }

    pub(crate) async fn recv_many_impl(
        &mut self,
        _out: &mut [Option<Packet>],
    ) -> io::Result<usize> {
        Err(error::no_async_backend())
    }

    pub(crate) async fn writable_impl(&self) -> io::Result<()> {
        Err(error::no_async_backend())
    }

    pub(crate) async fn send_impl(&self, _buf: &[u8]) -> io::Result<usize> {
        Err(error::no_async_backend())
    }

    pub(crate) async fn send_many_impl(
        &self,
        bufs: Vec<Bytes>,
        results: &mut [Option<io::Result<usize>>],
        _timeout: Duration,
        _keep_order: bool,
    ) -> Vec<Bytes> {
        let len = bufs.len();
        for slot in results.iter_mut().take(len) {
            *slot = Some(Err(error::no_async_backend()));
        }
        bufs
    }
}
