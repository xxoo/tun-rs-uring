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
use std::fs::OpenOptions;
use std::future::Future;
use std::io;
#[cfg(any(feature = "async_tokio", feature = "async_io"))]
use std::os::fd::RawFd;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::sync::Arc;
use std::time::Duration;
use tun_rs::SyncDevice;

pub use self::config::UringDeviceConfig;
pub use self::packet::{GsoType, OffloadInfo, Packet};
pub use self::rx::{RxStartMode, RxState};
pub(crate) use self::{
    config::ValidatedConfig,
    packet::PacketRecycle,
    rx::{RxController, RxControllerConfig},
    tx::TxController,
};

/// Minimal shared device shell used by all runtime backends.
#[allow(dead_code)]
pub(crate) struct CoreDevice {
    device: Arc<SyncDevice>,
    config: ValidatedConfig,
    packets_include_virtio_net_hdr: bool,
    rx: RxController,
    tx: TxController,
}

const FALLBACK_TUN_FLAGS: u16 = (libc::IFF_TUN | libc::IFF_NO_PI | libc::IFF_MULTI_QUEUE) as u16;
const FALLBACK_PERSISTENT_TUN_FLAGS: u16 = FALLBACK_TUN_FLAGS | libc::IFF_PERSIST as u16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NamespaceIdentity {
    device: libc::dev_t,
    inode: libc::ino_t,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TunIdentity {
    raw_name: [u8; libc::IFNAMSIZ],
    effective_flags: u16,
    if_index: u32,
    device_netns: NamespaceIdentity,
    thread_netns: NamespaceIdentity,
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
        let tx_ring_entries = config.tx_ring_entries;
        let tx_submit_chunk_size = config.tx_submit_chunk_size;
        let device = Arc::new(device);
        let rx = RxController::new(RxControllerConfig {
            device: duplicate_device_fd(&device)?,
            ring_entries: config.rx_ring_entries,
            buffer_len: rx_buffer_len,
            buffer_count: config.rx_buffer_count,
            packets_include_virtio_net_hdr,
            auto_resume_after_recycled_slots: config.rx_auto_resume_after_recycled_slots,
            start_mode: config.rx_start_mode,
            thread_name: rx_driver_name,
        })?;
        let tx = TxController::new(
            Arc::clone(&device),
            tx_ring_entries,
            tx_submit_chunk_size,
            "uring-tx",
        )?;

        Ok(Self {
            device,
            config,
            packets_include_virtio_net_hdr,
            rx,
            tx,
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

    pub(crate) fn try_clone_device(&self) -> std::io::Result<SyncDevice> {
        match self.device.try_clone() {
            Ok(device) => Ok(device),
            Err(error) if error.kind() == io::ErrorKind::Unsupported => {
                clone_plain_multiqueue_from_fd(&self.device)
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn config(&self) -> UringDeviceConfig {
        self.config.to_config()
    }

    pub(crate) fn duplicate_fd(&self) -> std::io::Result<OwnedFd> {
        duplicate_device_fd(&self.device)
    }
}

fn clone_plain_multiqueue_from_fd(device: &SyncDevice) -> io::Result<SyncDevice> {
    let source_before = tun_identity(device.as_raw_fd())?;
    validate_fallback_source(source_before)?;

    let new_fd: OwnedFd = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")?
        .into();
    let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
    unsafe {
        std::ptr::copy_nonoverlapping(
            source_before.raw_name.as_ptr(),
            request.ifr_name.as_mut_ptr().cast(),
            libc::IFNAMSIZ,
        );
    }
    request.ifr_ifru.ifru_flags = FALLBACK_TUN_FLAGS as libc::c_short;

    if unsafe { libc::ioctl(new_fd.as_raw_fd(), libc::TUNSETIFF, &mut request) } < 0 {
        return Err(io::Error::last_os_error());
    }

    let source_after = tun_identity(device.as_raw_fd())?;
    let attached = tun_identity(new_fd.as_raw_fd())?;
    validate_attached_queue(source_before, source_after, attached)?;

    unsafe { SyncDevice::from_fd(new_fd.into_raw_fd()) }
}

fn validate_fallback_source(identity: TunIdentity) -> io::Result<()> {
    if !matches!(
        identity.effective_flags,
        FALLBACK_TUN_FLAGS | FALLBACK_PERSISTENT_TUN_FLAGS
    ) || identity.if_index == 0
        || identity.device_netns != identity.thread_netns
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "device is not eligible for the multi-queue compatibility path",
        ));
    }
    Ok(())
}

fn validate_attached_queue(
    source_before: TunIdentity,
    source_after: TunIdentity,
    attached: TunIdentity,
) -> io::Result<()> {
    validate_fallback_source(source_after)?;
    validate_fallback_source(attached)?;
    if source_before != source_after || source_before != attached {
        return Err(io::Error::other(
            "TUN identity changed while attaching a queue",
        ));
    }
    Ok(())
}

fn tun_identity(fd: libc::c_int) -> io::Result<TunIdentity> {
    let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(fd, libc::TUNGETIFF, &mut request) } < 0 {
        return Err(io::Error::last_os_error());
    }

    let mut raw_name = [0; libc::IFNAMSIZ];
    unsafe {
        std::ptr::copy_nonoverlapping(
            request.ifr_name.as_ptr().cast(),
            raw_name.as_mut_ptr(),
            libc::IFNAMSIZ,
        );
    }
    if raw_name.first() == Some(&0) || !raw_name.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "TUNGETIFF returned an invalid interface name",
        ));
    }

    let device_netns = device_netns_identity(fd)?;
    let thread_netns_file = std::fs::File::open("/proc/thread-self/ns/net")?;
    let thread_netns = namespace_identity(thread_netns_file.as_raw_fd())?;
    if device_netns != thread_netns {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "TUN device and caller are in different network namespaces",
        ));
    }

    let if_index = unsafe { libc::if_nametoindex(request.ifr_name.as_ptr()) };
    if if_index == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "TUN interface index is unavailable",
        ));
    }

    Ok(TunIdentity {
        raw_name,
        effective_flags: unsafe { request.ifr_ifru.ifru_flags as u16 },
        if_index,
        device_netns,
        thread_netns,
    })
}

fn device_netns_identity(fd: libc::c_int) -> io::Result<NamespaceIdentity> {
    let namespace_fd = unsafe { libc::ioctl(fd, libc::TUNGETDEVNETNS) };
    if namespace_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let namespace_fd = unsafe { OwnedFd::from_raw_fd(namespace_fd) };
    namespace_identity(namespace_fd.as_raw_fd())
}

fn namespace_identity(fd: libc::c_int) -> io::Result<NamespaceIdentity> {
    let mut status: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut status) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(NamespaceIdentity {
        device: status.st_dev,
        inode: status.st_ino,
    })
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

pub(crate) fn duplicate_device_fd(device: &SyncDevice) -> io::Result<OwnedFd> {
    let duplicated = unsafe { libc::fcntl(device.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if duplicated < 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(unsafe { OwnedFd::from_raw_fd(duplicated) })
}

#[cfg(any(feature = "async_tokio", feature = "async_io"))]
pub(crate) fn write_fd(fd: RawFd, buf: &[u8]) -> io::Result<usize> {
    let written = unsafe { libc::write(fd, buf.as_ptr().cast::<libc::c_void>(), buf.len()) };
    if written < 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(written as usize)
}

#[cfg(test)]
mod clone_fallback_tests {
    use super::*;

    fn identity() -> TunIdentity {
        let mut raw_name = [0; libc::IFNAMSIZ];
        raw_name[..5].copy_from_slice(b"linq0");
        TunIdentity {
            raw_name,
            effective_flags: FALLBACK_TUN_FLAGS,
            if_index: 7,
            device_netns: NamespaceIdentity {
                device: 3,
                inode: 11,
            },
            thread_netns: NamespaceIdentity {
                device: 3,
                inode: 11,
            },
        }
    }

    #[test]
    fn fallback_seam_accepts_only_unchanged_plain_multiqueue_tun() {
        let expected = identity();
        assert!(validate_fallback_source(expected).is_ok());
        assert!(validate_attached_queue(expected, expected, expected).is_ok());

        let persistent = TunIdentity {
            effective_flags: FALLBACK_PERSISTENT_TUN_FLAGS,
            ..expected
        };
        assert!(validate_fallback_source(persistent).is_ok());
        assert!(validate_attached_queue(persistent, persistent, persistent).is_ok());

        for forbidden in [
            libc::IFF_TAP,
            libc::IFF_VNET_HDR,
            libc::IFF_NAPI,
            libc::IFF_NAPI_FRAGS,
            libc::IFF_TUN_EXCL,
            0x0040,
        ] {
            let mut candidate = expected;
            candidate.effective_flags |= forbidden as u16;
            assert_eq!(
                validate_fallback_source(candidate)
                    .expect_err("forbidden flag must be rejected")
                    .kind(),
                io::ErrorKind::Unsupported
            );
        }

        let mut candidate = expected;
        candidate.if_index += 1;
        assert!(validate_attached_queue(expected, expected, candidate).is_err());

        let mut candidate = expected;
        candidate.raw_name[0] = b'x';
        assert!(validate_attached_queue(expected, expected, candidate).is_err());

        let mut candidate = expected;
        candidate.device_netns.inode += 1;
        assert!(validate_fallback_source(candidate).is_err());
    }
}
