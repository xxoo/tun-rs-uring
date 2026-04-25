//! Async TUN/TAP device built around `io_uring`.

#[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
compile_error!("`tun-rs-uring` currently supports Linux only");

#[cfg(all(feature = "async_tokio", feature = "async_io"))]
compile_error!("features `async_tokio` and `async_io` are mutually exclusive");

mod backend;
pub(crate) mod core;

pub use crate::core::{GsoType, OffloadInfo, Packet, RxStartMode, RxState, UringDeviceConfig};
use bytes::Bytes;
use std::time::Duration;
use tun_rs::SyncDevice;

/// Public async device facade. The concrete backend stays internal and is
/// selected at compile time through crate features.
#[derive(Debug)]
pub struct UringDevice {
    inner: backend::BackendDevice,
}

impl UringDevice {
    /// Creates a new `UringDevice` by consuming the provided `SyncDevice`.
    pub fn new(device: SyncDevice, config: UringDeviceConfig) -> std::io::Result<Self> {
        Ok(Self {
            inner: backend::BackendDevice::new(device, config)?,
        })
    }

    /// Returns the compile-time selected backend name.
    pub const fn backend_name() -> &'static str {
        backend::backend_name()
    }

    /// Returns the current receive state.
    pub fn rx_state(&self) -> RxState {
        self.inner.rx_state()
    }

    /// Returns the exact number of packets currently available to `try_recv()`.
    pub fn ready_len(&mut self) -> usize {
        self.inner.ready_len()
    }

    /// Starts RX if it is currently stopped or faulted.
    pub fn start_rx(&mut self) -> std::io::Result<()> {
        self.inner.start_rx()
    }

    /// Stops RX and waits until the current stop request is observed.
    pub async fn stop_rx(&mut self) -> std::io::Result<()> {
        self.inner.stop_rx().await
    }

    /// Attempts a nonblocking single-packet send.
    pub fn try_send(&self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.try_send(buf)
    }

    /// Waits until at least one packet is available to receive.
    pub async fn readable(&mut self) -> std::io::Result<()> {
        self.inner.readable().await
    }

    /// Attempts to receive one packet without waiting.
    pub fn try_recv(&mut self) -> std::io::Result<Packet> {
        self.inner.try_recv()
    }

    /// Receives one packet using the standard `try_recv() + readable()` loop.
    pub async fn recv(&mut self) -> std::io::Result<Packet> {
        self.inner.recv().await
    }

    /// Receives as many packets as are immediately available, up to `out.len()`.
    pub async fn recv_many(&mut self, out: &mut [Option<Packet>]) -> std::io::Result<usize> {
        self.inner.recv_many(out).await
    }

    /// Waits until the device becomes writable.
    pub async fn writable(&self) -> std::io::Result<()> {
        self.inner.writable().await
    }

    /// Sends one packet using the standard `try_send() + writable()` loop.
    pub async fn send(&self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.send(buf).await
    }

    /// Sends a batch of owned packet buffers through the shared TX ring.
    pub async fn send_many(
        &self,
        bufs: Vec<Bytes>,
        results: &mut [Option<std::io::Result<usize>>],
        timeout: Duration,
        keep_order: bool,
    ) -> Vec<Bytes> {
        self.inner
            .send_many(bufs, results, timeout, keep_order)
            .await
    }
}

#[cfg(test)]
mod tests {
    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    const RX_RECOVERY_STRESS_ROUNDS: u16 = 4;
    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    const TX_MIXED_STRESS_ROUNDS: u16 = 6;
    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    const CLEANUP_STRESS_ROUNDS: u16 = 4;
    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    const CLEANUP_LONG_STRESS_ROUNDS: u16 = 8;

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    use super::{GsoType, Packet, RxState};
    use super::{RxStartMode, UringDevice, UringDeviceConfig};
    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    use crate::core::testutil::block_on_timeout;
    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    use bytes::Bytes;
    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    use std::{
        future::Future,
        io,
        net::{Ipv4Addr, UdpSocket},
        pin::Pin,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        task::{Context, Poll},
        thread,
        time::{Duration, Instant},
    };
    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    use tun_rs::DeviceBuilder;

    #[test]
    fn facade_exposes_selected_backend() {
        #[cfg(feature = "async_tokio")]
        assert_eq!(UringDevice::backend_name(), "async_tokio");

        #[cfg(feature = "async_io")]
        assert_eq!(UringDevice::backend_name(), "async_io");

        #[cfg(not(any(feature = "async_tokio", feature = "async_io")))]
        assert_eq!(UringDevice::backend_name(), "none");
    }

    #[test]
    fn config_builder_overrides_selected_fields() {
        let config = UringDeviceConfig::default()
            .with_rx_buffer_len(4096)
            .with_rx_buffer_count(128)
            .with_rx_start_mode(RxStartMode::ManualStart);

        assert_eq!(config.rx_buffer_len, 4096);
        assert_eq!(config.rx_buffer_count, 128);
        assert!(matches!(config.rx_start_mode, RxStartMode::ManualStart));
        assert_eq!(
            config.tx_ring_entries,
            UringDeviceConfig::default().tx_ring_entries
        );
    }

    #[test]
    fn linux_only_compile_guard_is_in_effect() {
        assert!(cfg!(all(target_os = "linux", not(target_env = "ohos"))));
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn should_skip_live_public_rx_test(error: &io::Error) -> bool {
        matches!(
            error.raw_os_error(),
            Some(libc::EPERM | libc::EACCES | libc::ENOSYS | libc::ENOENT | libc::ENODEV)
        ) || matches!(
            error.kind(),
            io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
        )
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn packet_has_ipv4_udp_payload(packet: &[u8], expected_payload: &[u8]) -> bool {
        if packet.len() < 20 || packet[0] >> 4 != 4 {
            return false;
        }

        let ip_header_len = usize::from(packet[0] & 0x0f) * 4;
        if packet.len() < ip_header_len + 8 || packet[9] != libc::IPPROTO_UDP as u8 {
            return false;
        }

        let udp_len = u16::from_be_bytes([packet[ip_header_len + 4], packet[ip_header_len + 5]]);
        let udp_len = usize::from(udp_len);
        if udp_len < 8 || packet.len() < ip_header_len + udp_len {
            return false;
        }

        &packet[ip_header_len + 8..ip_header_len + udp_len] == expected_payload
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn ipv4_checksum(header: &[u8]) -> u16 {
        let mut sum = 0u32;
        for chunk in header.chunks_exact(2) {
            sum += u32::from(u16::from_be_bytes([chunk[0], chunk[1]]));
        }

        while sum > 0xffff {
            sum = (sum & 0xffff) + (sum >> 16);
        }

        !(sum as u16)
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn build_ipv4_udp_packet(
        src: Ipv4Addr,
        dst: Ipv4Addr,
        src_port: u16,
        dst_port: u16,
        payload: &[u8],
    ) -> Bytes {
        let ip_header_len = 20usize;
        let udp_header_len = 8usize;
        let total_len = ip_header_len + udp_header_len + payload.len();
        let mut packet = vec![0u8; total_len];

        packet[0] = 0x45;
        packet[1] = 0;
        packet[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
        packet[4..6].copy_from_slice(&0u16.to_be_bytes());
        packet[6..8].copy_from_slice(&0u16.to_be_bytes());
        packet[8] = 64;
        packet[9] = libc::IPPROTO_UDP as u8;
        packet[12..16].copy_from_slice(&src.octets());
        packet[16..20].copy_from_slice(&dst.octets());
        let checksum = ipv4_checksum(&packet[..ip_header_len]);
        packet[10..12].copy_from_slice(&checksum.to_be_bytes());

        let udp_start = ip_header_len;
        packet[udp_start..udp_start + 2].copy_from_slice(&src_port.to_be_bytes());
        packet[udp_start + 2..udp_start + 4].copy_from_slice(&dst_port.to_be_bytes());
        packet[udp_start + 4..udp_start + 6]
            .copy_from_slice(&((udp_header_len + payload.len()) as u16).to_be_bytes());
        packet[udp_start + 6..udp_start + 8].copy_from_slice(&0u16.to_be_bytes());
        packet[udp_start + udp_header_len..].copy_from_slice(payload);

        Bytes::from(packet)
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn spawn_ipv4_udp_sender(
        bind_addr: &str,
        target_addr: &str,
        payload: &[u8],
    ) -> (Arc<AtomicBool>, thread::JoinHandle<io::Result<()>>) {
        spawn_ipv4_udp_sender_with_timing(
            bind_addr,
            target_addr,
            payload,
            Duration::from_millis(50),
            Duration::from_secs(3),
        )
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn spawn_ipv4_udp_sender_with_timing(
        bind_addr: &str,
        target_addr: &str,
        payload: &[u8],
        interval: Duration,
        duration: Duration,
    ) -> (Arc<AtomicBool>, thread::JoinHandle<io::Result<()>>) {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let bind_addr = bind_addr.to_string();
        let target_addr = target_addr.to_string();
        let payload = payload.to_vec();

        let handle = thread::spawn(move || {
            let socket = UdpSocket::bind(&bind_addr)?;
            let deadline = Instant::now() + duration;

            while !stop_for_thread.load(Ordering::Acquire) && Instant::now() < deadline {
                let _ = socket.send_to(&payload, &target_addr);
                thread::sleep(interval);
            }

            Ok(())
        });

        (stop, handle)
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn build_manual_start_device() -> io::Result<UringDevice> {
        build_manual_start_device_with_config(
            UringDeviceConfig::default()
                .with_rx_start_mode(RxStartMode::ManualStart)
                .with_rx_buffer_count(128),
        )
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn build_manual_start_device_with_config(config: UringDeviceConfig) -> io::Result<UringDevice> {
        let device = DeviceBuilder::new()
            .ipv4("10.26.1.100", 24, None)
            .build_sync()?;

        UringDevice::new(device, config)
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn build_manual_start_offload_device_with_config(
        config: UringDeviceConfig,
    ) -> io::Result<Option<UringDevice>> {
        let device = DeviceBuilder::new()
            .ipv4("10.26.1.100", 24, None)
            .offload(true)
            .build_sync()?;

        if !device.tcp_gso() {
            return Ok(None);
        }

        UringDevice::new(device, config).map(Some)
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn wait_for_condition(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if condition() {
                return true;
            }

            thread::sleep(Duration::from_millis(10));
        }

        condition()
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn is_faulted_with_raw_os_error(state: RxState, raw_os_error: i32) -> bool {
        match state {
            RxState::Faulted(error) => error.raw_os_error() == Some(raw_os_error),
            _ => false,
        }
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    async fn recv_packets(device: &mut UringDevice, count: usize) -> io::Result<Vec<Packet>> {
        let mut packets = Vec::with_capacity(count);
        while packets.len() < count {
            let packet = device.recv().await?;
            packets.push(packet);
        }
        Ok(packets)
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_offload_metadata_is_lazily_available(
        mut device: UringDevice,
        payload: &[u8],
    ) -> io::Result<()> {
        device.start_rx()?;

        let (stop, sender) = spawn_ipv4_udp_sender("10.26.1.100:0", "10.26.1.101:18084", payload);
        let packet =
            block_on_timeout(device.recv(), Duration::from_secs(3)).ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "timed out waiting for RX packet")
            })??;
        stop.store(true, Ordering::Release);
        sender.join().unwrap()?;

        assert!(packet_has_ipv4_udp_payload(packet.as_bytes(), payload));
        let offload = packet.offload_info().ok_or_else(|| {
            io::Error::other("missing offload metadata on offload-enabled RX packet")
        })?;
        assert_eq!(offload.gso_type, GsoType::None);
        assert_eq!(offload.csum_start, 20);
        assert_eq!(offload.csum_offset, 6);
        assert_eq!(offload.hdr_len, 28);
        assert!(!packet.is_gso());
        assert!(!packet.is_detached());

        Ok(())
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn drain_stopped_rx_queue(device: &mut UringDevice, expected_payload: &[u8]) -> io::Result<()> {
        loop {
            match device.try_recv() {
                Ok(packet) => {
                    if !packet_has_ipv4_udp_payload(packet.as_bytes(), expected_payload) {
                        return Err(io::Error::other(
                            "drained stopped RX queue contained an unexpected payload",
                        ));
                    }
                    drop(packet);
                }
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => break,
                Err(error) => return Err(error),
            }
        }

        assert_eq!(device.ready_len(), 0);
        Ok(())
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_recv_many_limits_and_drains_ready_packets(
        mut device: UringDevice,
        payload_prefix: &str,
    ) -> io::Result<()> {
        let send_socket = UdpSocket::bind(("10.26.1.100", 0))?;
        let target_addr = ("10.26.1.101", 8_083);
        let payloads = vec![
            format!("{payload_prefix} one").into_bytes(),
            format!("{payload_prefix} two").into_bytes(),
            format!("{payload_prefix} three").into_bytes(),
        ];

        assert!(matches!(device.rx_state(), RxState::Stopped));
        device.start_rx()?;
        assert!(matches!(device.rx_state(), RxState::Running));

        for payload in &payloads {
            send_socket.send_to(payload, target_addr)?;
        }

        if !wait_for_condition(Duration::from_secs(3), || {
            device.ready_len() >= payloads.len()
        }) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for recv_many packets to become ready",
            ));
        }

        let mut first_out = [None, None];
        let first_count = block_on_timeout(
            async { device.recv_many(&mut first_out).await },
            Duration::from_secs(2),
        )
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "timed out on first recv_many"))??;
        assert_eq!(first_count, 2);
        assert_eq!(device.ready_len(), 1);

        let mut second_out = [None, None];
        let second_count = block_on_timeout(
            async { device.recv_many(&mut second_out).await },
            Duration::from_millis(500),
        )
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::TimedOut, "timed out on second recv_many")
        })??;
        assert_eq!(second_count, 1);
        assert_eq!(device.ready_len(), 0);

        let received_packets = first_out
            .into_iter()
            .chain(second_out)
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(received_packets.len(), payloads.len());

        let mut matched = vec![false; payloads.len()];
        for packet in received_packets {
            let Some(index) = payloads
                .iter()
                .position(|payload| packet_has_ipv4_udp_payload(packet.as_bytes(), payload))
            else {
                return Err(io::Error::other(
                    "recv_many returned a packet with unexpected UDP payload",
                ));
            };

            if matched[index] {
                return Err(io::Error::other(
                    "recv_many returned the same payload more than once",
                ));
            }

            matched[index] = true;
        }
        assert!(matched.into_iter().all(|seen| seen));

        block_on_timeout(async { device.stop_rx().await }, Duration::from_secs(2))
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "timed out stopping rx"))??;
        assert!(matches!(device.rx_state(), RxState::Stopped));

        Ok(())
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_stop_rx_prevents_new_completions_until_restart(
        mut device: UringDevice,
        payload: &[u8],
    ) -> io::Result<()> {
        assert!(matches!(device.rx_state(), RxState::Stopped));
        device.start_rx()?;
        assert!(matches!(device.rx_state(), RxState::Running));

        let (stop, sender) = spawn_ipv4_udp_sender_with_timing(
            "10.26.1.100:0",
            "10.26.1.101:8084",
            payload,
            Duration::from_millis(1),
            Duration::from_secs(8),
        );

        let scenario = (|| -> io::Result<()> {
            let packet = block_on_timeout(async { device.recv().await }, Duration::from_secs(3))
                .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out receiving packet before stop_rx",
                )
            })??;
            assert!(packet_has_ipv4_udp_payload(packet.as_bytes(), payload));
            drop(packet);

            block_on_timeout(async { device.stop_rx().await }, Duration::from_secs(2))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "timed out stopping rx")
                })??;
            assert!(matches!(device.rx_state(), RxState::Stopped));

            loop {
                match device.try_recv() {
                    Ok(packet) => {
                        assert!(packet_has_ipv4_udp_payload(packet.as_bytes(), payload));
                        drop(packet);
                    }
                    Err(error) if error.kind() == io::ErrorKind::BrokenPipe => break,
                    Err(error) => return Err(error),
                }
            }

            assert_eq!(device.ready_len(), 0);
            thread::sleep(Duration::from_millis(150));
            assert_eq!(device.ready_len(), 0);
            let error = device.try_recv().unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);

            device.start_rx()?;
            assert!(matches!(device.rx_state(), RxState::Running));

            let packet = block_on_timeout(async { device.recv().await }, Duration::from_secs(3))
                .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out receiving packet after restarting rx",
                )
            })??;
            assert!(packet_has_ipv4_udp_payload(packet.as_bytes(), payload));
            drop(packet);

            block_on_timeout(async { device.stop_rx().await }, Duration::from_secs(2))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "timed out stopping rx")
                })??;
            assert!(matches!(device.rx_state(), RxState::Stopped));

            Ok(())
        })();

        stop.store(true, Ordering::Release);
        sender.join().unwrap().unwrap();
        scenario
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_manual_restart_after_enobufs(
        mut device: UringDevice,
        payload: &[u8],
    ) -> io::Result<()> {
        exercise_manual_restart_after_enobufs_on_port(&mut device, payload, 8_081)
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_manual_restart_after_enobufs_on_port(
        mut device: &mut UringDevice,
        payload: &[u8],
        dst_port: u16,
    ) -> io::Result<()> {
        const BUFFER_COUNT: usize = 4;

        assert!(matches!(device.rx_state(), RxState::Stopped));
        device.start_rx()?;
        assert!(matches!(device.rx_state(), RxState::Running));

        let target_addr = format!("10.26.1.101:{dst_port}");

        let (stop, sender) = spawn_ipv4_udp_sender_with_timing(
            "10.26.1.100:0",
            &target_addr,
            payload,
            Duration::from_millis(1),
            Duration::from_secs(8),
        );

        let scenario = (|| -> io::Result<()> {
            let held_packets = block_on_timeout(
                async { recv_packets(&mut device, BUFFER_COUNT).await },
                Duration::from_secs(4),
            )
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "timed out receiving held packets")
            })??;
            let mut held_packets = held_packets;

            assert_eq!(held_packets.len(), BUFFER_COUNT);
            for packet in &held_packets {
                assert!(!packet.is_detached());
                assert!(packet_has_ipv4_udp_payload(packet.as_bytes(), payload));
            }

            if !wait_for_condition(Duration::from_secs(3), || {
                is_faulted_with_raw_os_error(device.rx_state(), libc::ENOBUFS)
            }) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out waiting for rx to fault with ENOBUFS",
                ));
            }

            assert_eq!(device.ready_len(), 0);
            let error = device.try_recv().unwrap_err();
            assert_eq!(error.raw_os_error(), Some(libc::ENOBUFS));

            drop(held_packets.drain(..).collect::<Vec<_>>());

            thread::sleep(Duration::from_millis(100));
            assert!(is_faulted_with_raw_os_error(
                device.rx_state(),
                libc::ENOBUFS
            ));

            device.start_rx()?;
            assert!(matches!(device.rx_state(), RxState::Running));

            let packet = block_on_timeout(async { device.recv().await }, Duration::from_secs(3))
                .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out receiving packet after manual restart",
                )
            })??;
            assert!(!packet.is_detached());
            assert!(packet_has_ipv4_udp_payload(packet.as_bytes(), payload));
            drop(packet);

            block_on_timeout(async { device.stop_rx().await }, Duration::from_secs(2))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "timed out stopping rx")
                })??;
            assert!(matches!(device.rx_state(), RxState::Stopped));
            drain_stopped_rx_queue(device, payload)?;

            Ok(())
        })();

        stop.store(true, Ordering::Release);
        sender.join().unwrap().unwrap();
        scenario
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_auto_resume_after_enobufs(
        mut device: UringDevice,
        payload: &[u8],
    ) -> io::Result<()> {
        exercise_auto_resume_after_enobufs_on_port(&mut device, payload, 8_082)
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_auto_resume_after_enobufs_on_port(
        mut device: &mut UringDevice,
        payload: &[u8],
        dst_port: u16,
    ) -> io::Result<()> {
        const BUFFER_COUNT: usize = 4;
        const AUTO_RESUME_THRESHOLD: usize = 2;

        assert!(matches!(device.rx_state(), RxState::Stopped));
        device.start_rx()?;
        assert!(matches!(device.rx_state(), RxState::Running));

        let target_addr = format!("10.26.1.101:{dst_port}");

        let (stop, sender) = spawn_ipv4_udp_sender_with_timing(
            "10.26.1.100:0",
            &target_addr,
            payload,
            Duration::from_millis(1),
            Duration::from_secs(8),
        );

        let scenario = (|| -> io::Result<()> {
            let held_packets = block_on_timeout(
                async { recv_packets(&mut device, BUFFER_COUNT).await },
                Duration::from_secs(4),
            )
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "timed out receiving held packets")
            })??;
            let mut held_packets = held_packets;

            if !wait_for_condition(Duration::from_secs(3), || {
                is_faulted_with_raw_os_error(device.rx_state(), libc::ENOBUFS)
            }) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out waiting for rx to fault with ENOBUFS",
                ));
            }

            assert_eq!(device.ready_len(), 0);
            let error = device.try_recv().unwrap_err();
            assert_eq!(error.raw_os_error(), Some(libc::ENOBUFS));

            let recycled = held_packets
                .drain(..AUTO_RESUME_THRESHOLD)
                .collect::<Vec<_>>();
            drop(recycled);

            if !wait_for_condition(Duration::from_secs(3), || {
                matches!(device.rx_state(), RxState::Running)
            }) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out waiting for rx auto-resume",
                ));
            }

            let packet = block_on_timeout(async { device.recv().await }, Duration::from_secs(3))
                .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out receiving packet after auto-resume",
                )
            })??;
            assert!(!packet.is_detached());
            assert!(packet_has_ipv4_udp_payload(packet.as_bytes(), payload));
            drop(packet);
            drop(held_packets);

            block_on_timeout(async { device.stop_rx().await }, Duration::from_secs(2))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "timed out stopping rx")
                })??;
            assert!(matches!(device.rx_state(), RxState::Stopped));
            drain_stopped_rx_queue(device, payload)?;

            Ok(())
        })();

        stop.store(true, Ordering::Release);
        sender.join().unwrap().unwrap();
        scenario
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_manual_restart_after_enobufs_stress(
        mut device: UringDevice,
        payload_prefix: &str,
    ) -> io::Result<()> {
        for round in 0..RX_RECOVERY_STRESS_ROUNDS {
            let payload = format!("{payload_prefix} manual round {round}");
            exercise_manual_restart_after_enobufs_on_port(
                &mut device,
                payload.as_bytes(),
                8_090 + round,
            )?;
        }

        Ok(())
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_auto_resume_after_enobufs_stress(
        mut device: UringDevice,
        payload_prefix: &str,
    ) -> io::Result<()> {
        for round in 0..RX_RECOVERY_STRESS_ROUNDS {
            let payload = format!("{payload_prefix} auto round {round}");
            exercise_auto_resume_after_enobufs_on_port(
                &mut device,
                payload.as_bytes(),
                8_100 + round,
            )?;
        }

        Ok(())
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_send_many_delivers_ipv4_udp(
        device: &UringDevice,
        payload: &[u8],
    ) -> io::Result<()> {
        let mut second_payload = payload.to_vec();
        second_payload.extend_from_slice(b" second");
        exercise_send_many_delivers_ipv4_udp_on_port(
            device,
            payload,
            &second_payload,
            18_181,
            40_000,
            40_001,
            false,
        )
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_send_many_keep_order_delivers_ipv4_udp(
        device: &UringDevice,
        payload: &[u8],
    ) -> io::Result<()> {
        let mut second_payload = payload.to_vec();
        second_payload.extend_from_slice(b" second");
        exercise_send_many_delivers_ipv4_udp_on_port(
            device,
            payload,
            &second_payload,
            18_186,
            40_010,
            40_011,
            true,
        )
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_send_many_keep_order_chain_breaks_after_invalid_packet(
        device: &UringDevice,
        payload: &[u8],
    ) -> io::Result<()> {
        exercise_send_many_keep_order_chain_breaks_after_invalid_packet_on_port(
            device, payload, 18_187, 40_020, 40_021,
        )
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_send_many_keep_order_chain_breaks_after_invalid_packet_on_port(
        device: &UringDevice,
        payload: &[u8],
        dst_port: u16,
        first_src_port: u16,
        third_src_port: u16,
    ) -> io::Result<()> {
        let socket = UdpSocket::bind(("10.26.1.100", dst_port))?;
        socket.set_read_timeout(Some(Duration::from_secs(3)))?;

        let first_packet = build_ipv4_udp_packet(
            Ipv4Addr::new(10, 26, 1, 101),
            Ipv4Addr::new(10, 26, 1, 100),
            first_src_port,
            dst_port,
            payload,
        );
        let invalid_packet = Bytes::from_static(b"\x00");
        let mut third_payload = payload.to_vec();
        third_payload.extend_from_slice(b" linked tail");
        let third_packet = build_ipv4_udp_packet(
            Ipv4Addr::new(10, 26, 1, 101),
            Ipv4Addr::new(10, 26, 1, 100),
            third_src_port,
            dst_port,
            &third_payload,
        );
        let mut results = [None, None, None];

        let returned = block_on_timeout(
            async {
                device
                    .send_many(
                        vec![
                            first_packet.clone(),
                            invalid_packet.clone(),
                            third_packet.clone(),
                        ],
                        &mut results,
                        Duration::from_secs(2),
                        true,
                    )
                    .await
            },
            Duration::from_secs(4),
        )
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for keep_order chain-break batch",
            )
        })?;

        assert_eq!(returned.len(), 3);
        assert_eq!(returned[0].as_ref(), first_packet.as_ref());
        assert_eq!(returned[1].as_ref(), invalid_packet.as_ref());
        assert_eq!(returned[2].as_ref(), third_packet.as_ref());
        assert_eq!(results[0].take().unwrap()?, first_packet.len());
        results[1]
            .take()
            .ok_or_else(|| io::Error::other("missing invalid keep_order result"))?
            .expect_err("invalid keep_order packet unexpectedly succeeded");
        let linked_tail_error = results[2]
            .take()
            .ok_or_else(|| io::Error::other("missing linked-tail keep_order result"))?
            .expect_err("linked tail packet unexpectedly succeeded");
        assert_eq!(linked_tail_error.raw_os_error(), Some(libc::ECANCELED));

        let mut recv_buf = [0u8; 2048];
        let received = socket.recv(&mut recv_buf)?;
        if &recv_buf[..received] != payload {
            return Err(io::Error::other(
                "unexpected payload received before keep_order chain break settled",
            ));
        }

        socket.set_read_timeout(Some(Duration::from_millis(300)))?;
        match socket.recv(&mut recv_buf) {
            Ok(received) if &recv_buf[..received] == third_payload.as_slice() => {
                Err(io::Error::other(
                    "linked-tail packet was unexpectedly delivered after keep_order chain break",
                ))
            }
            Ok(_) => Err(io::Error::other(
                "received unexpected payload while checking keep_order chain break",
            )),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_send_many_delivers_ipv4_udp_on_port(
        device: &UringDevice,
        first_payload: &[u8],
        second_payload: &[u8],
        dst_port: u16,
        first_src_port: u16,
        second_src_port: u16,
        keep_order: bool,
    ) -> io::Result<()> {
        let socket = UdpSocket::bind(("10.26.1.100", dst_port))?;
        socket.set_read_timeout(Some(Duration::from_secs(3)))?;

        let first_packet = build_ipv4_udp_packet(
            Ipv4Addr::new(10, 26, 1, 101),
            Ipv4Addr::new(10, 26, 1, 100),
            first_src_port,
            dst_port,
            first_payload,
        );
        let second_packet = build_ipv4_udp_packet(
            Ipv4Addr::new(10, 26, 1, 101),
            Ipv4Addr::new(10, 26, 1, 100),
            second_src_port,
            dst_port,
            second_payload,
        );
        let expected_lens = [first_packet.len(), second_packet.len()];
        let mut results = [None, None];

        let returned = block_on_timeout(
            async {
                device
                    .send_many(
                        vec![first_packet.clone(), second_packet.clone()],
                        &mut results,
                        Duration::from_secs(2),
                        keep_order,
                    )
                    .await
            },
            Duration::from_secs(4),
        )
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::TimedOut, "timed out waiting for send_many")
        })?;

        assert_eq!(returned.len(), 2);
        assert_eq!(returned[0].as_ref(), first_packet.as_ref());
        assert_eq!(returned[1].as_ref(), second_packet.as_ref());
        assert_eq!(results[0].take().unwrap()?, expected_lens[0]);
        assert_eq!(results[1].take().unwrap()?, expected_lens[1]);

        let mut recv_buf = [0u8; 2048];
        let mut seen_first = false;
        let mut seen_second = false;
        while !(seen_first && seen_second) {
            let received = socket.recv(&mut recv_buf)?;
            let received = &recv_buf[..received];
            if received == first_payload {
                seen_first = true;
            } else if received == second_payload {
                seen_second = true;
            }
        }

        Ok(())
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn poll_once<F>(future: &mut Pin<Box<F>>) -> Poll<F::Output>
    where
        F: Future,
    {
        let waker = crate::core::testutil::WakeCounter::new().waker();
        let mut context = Context::from_waker(&waker);
        future.as_mut().poll(&mut context)
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_send_many_drop_cleanup_allows_followup(
        device: &UringDevice,
        payload: &[u8],
    ) -> io::Result<()> {
        exercise_send_many_drop_cleanup_allows_followup_on_port(
            device, payload, 18_182, 41_000, 41_001,
        )
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_send_many_drop_cleanup_allows_followup_on_port(
        device: &UringDevice,
        payload: &[u8],
        dst_port: u16,
        warmup_src_port: u16,
        followup_src_port: u16,
    ) -> io::Result<()> {
        let socket = UdpSocket::bind(("10.26.1.100", dst_port))?;
        socket.set_read_timeout(Some(Duration::from_secs(3)))?;

        let first_packet = build_ipv4_udp_packet(
            Ipv4Addr::new(10, 26, 1, 101),
            Ipv4Addr::new(10, 26, 1, 100),
            warmup_src_port,
            dst_port,
            b"drop-cleanup warmup",
        );
        let warmup_batch = (0..512).map(|_| first_packet.clone()).collect::<Vec<_>>();
        let mut warmup_results = std::iter::repeat_with(|| None)
            .take(warmup_batch.len())
            .collect::<Vec<_>>();
        let mut future = Box::pin(device.send_many(
            warmup_batch,
            &mut warmup_results,
            Duration::from_secs(10),
            true,
        ));

        match poll_once(&mut future) {
            Poll::Pending => {}
            Poll::Ready(_) => {
                return Err(io::Error::other(
                    "send_many warmup batch completed before drop cleanup could be exercised",
                ));
            }
        }

        drop(future);
        drop(warmup_results);

        let followup_packet = build_ipv4_udp_packet(
            Ipv4Addr::new(10, 26, 1, 101),
            Ipv4Addr::new(10, 26, 1, 100),
            followup_src_port,
            dst_port,
            payload,
        );
        let mut results = [None];
        let returned = block_on_timeout(
            async {
                device
                    .send_many(
                        vec![followup_packet.clone()],
                        &mut results,
                        Duration::from_secs(3),
                        false,
                    )
                    .await
            },
            Duration::from_secs(5),
        )
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for followup send_many after drop cleanup",
            )
        })?;

        assert_eq!(returned.len(), 1);
        assert_eq!(returned[0].as_ref(), followup_packet.as_ref());
        assert_eq!(results[0].take().unwrap()?, followup_packet.len());

        let mut recv_buf = [0u8; 2048];
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let received = socket.recv(&mut recv_buf)?;
            if &recv_buf[..received] == payload {
                break;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out receiving followup payload after drop cleanup",
                ));
            }
        }

        Ok(())
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_send_many_timeout_allows_followup(
        device: &UringDevice,
        payload: &[u8],
    ) -> io::Result<()> {
        exercise_send_many_timeout_allows_followup_on_port(device, payload, 18_183, 42_000, 42_001)
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_send_many_timeout_allows_followup_on_port(
        device: &UringDevice,
        payload: &[u8],
        dst_port: u16,
        timeout_src_port: u16,
        followup_src_port: u16,
    ) -> io::Result<()> {
        let socket = UdpSocket::bind(("10.26.1.100", dst_port))?;
        socket.set_read_timeout(Some(Duration::from_secs(3)))?;

        let timeout_packet = build_ipv4_udp_packet(
            Ipv4Addr::new(10, 26, 1, 101),
            Ipv4Addr::new(10, 26, 1, 100),
            timeout_src_port,
            dst_port,
            b"timeout warmup",
        );
        let timeout_batch = (0..2048)
            .map(|_| timeout_packet.clone())
            .collect::<Vec<_>>();
        let mut timeout_results = std::iter::repeat_with(|| None)
            .take(timeout_batch.len())
            .collect::<Vec<_>>();
        let returned = block_on_timeout(
            async {
                device
                    .send_many(
                        timeout_batch.clone(),
                        &mut timeout_results,
                        Duration::from_millis(1),
                        true,
                    )
                    .await
            },
            Duration::from_secs(5),
        )
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for timeout batch",
            )
        })?;

        assert_eq!(returned.len(), timeout_batch.len());
        assert!(timeout_results.iter().any(|result| {
            matches!(
                result,
                Some(Err(error)) if error.kind() == io::ErrorKind::TimedOut
            )
        }));

        let followup_packet = build_ipv4_udp_packet(
            Ipv4Addr::new(10, 26, 1, 101),
            Ipv4Addr::new(10, 26, 1, 100),
            followup_src_port,
            dst_port,
            payload,
        );
        let mut results = [None];
        let returned = block_on_timeout(
            async {
                device
                    .send_many(
                        vec![followup_packet.clone()],
                        &mut results,
                        Duration::from_secs(3),
                        false,
                    )
                    .await
            },
            Duration::from_secs(5),
        )
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for followup send_many after timeout",
            )
        })?;

        assert_eq!(returned.len(), 1);
        assert_eq!(returned[0].as_ref(), followup_packet.as_ref());
        assert_eq!(results[0].take().unwrap()?, followup_packet.len());

        let mut recv_buf = [0u8; 2048];
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let received = socket.recv(&mut recv_buf)?;
            if &recv_buf[..received] == payload {
                break;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out receiving followup payload after timeout",
                ));
            }
        }

        Ok(())
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_send_many_waiting_for_busy_slot_times_out(
        device: &UringDevice,
        payload: &[u8],
    ) -> io::Result<()> {
        exercise_send_many_waiting_for_busy_slot_times_out_on_ports(
            device, payload, 18_184, 18_185, 43_000, 43_001, 43_002,
        )
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_send_many_waiting_for_busy_slot_times_out_on_ports(
        device: &UringDevice,
        payload: &[u8],
        block_port: u16,
        dst_port: u16,
        blocking_src_port: u16,
        timeout_src_port: u16,
        followup_src_port: u16,
    ) -> io::Result<()> {
        let socket = UdpSocket::bind(("10.26.1.100", dst_port))?;
        socket.set_read_timeout(Some(Duration::from_millis(200)))?;

        let blocking_packet = build_ipv4_udp_packet(
            Ipv4Addr::new(10, 26, 1, 101),
            Ipv4Addr::new(10, 26, 1, 100),
            blocking_src_port,
            block_port,
            b"busy-slot warmup",
        );
        let blocking_batch = (0..1024)
            .map(|_| blocking_packet.clone())
            .collect::<Vec<_>>();
        let mut blocking_results = std::iter::repeat_with(|| None)
            .take(blocking_batch.len())
            .collect::<Vec<_>>();
        let mut blocking_future = Box::pin(device.send_many(
            blocking_batch,
            &mut blocking_results,
            Duration::from_secs(10),
            true,
        ));

        match poll_once(&mut blocking_future) {
            Poll::Pending => {}
            Poll::Ready(_) => {
                return Err(io::Error::other(
                    "blocking send_many batch completed before busy-slot timeout could be exercised",
                ));
            }
        }

        let timed_out_packet = build_ipv4_udp_packet(
            Ipv4Addr::new(10, 26, 1, 101),
            Ipv4Addr::new(10, 26, 1, 100),
            timeout_src_port,
            dst_port,
            b"busy-slot timeout",
        );
        let mut timeout_results = [None];
        let returned = block_on_timeout(
            async {
                device
                    .send_many(
                        vec![timed_out_packet.clone()],
                        &mut timeout_results,
                        Duration::from_millis(1),
                        false,
                    )
                    .await
            },
            Duration::from_secs(2),
        )
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for busy-slot timeout batch",
            )
        })?;

        assert_eq!(returned.len(), 1);
        assert_eq!(returned[0].as_ref(), timed_out_packet.as_ref());
        let error = timeout_results[0]
            .take()
            .ok_or_else(|| io::Error::other("missing busy-slot timeout result"))?
            .expect_err("busy-slot timeout batch unexpectedly succeeded");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);

        let mut recv_buf = [0u8; 2048];
        match socket.recv(&mut recv_buf) {
            Ok(received) if &recv_buf[..received] == b"busy-slot timeout" => {
                return Err(io::Error::other(
                    "busy-slot timeout batch was unexpectedly submitted",
                ));
            }
            Ok(_) => {
                return Err(io::Error::other(
                    "received unexpected packet while checking busy-slot timeout batch",
                ));
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error),
        }

        drop(blocking_future);
        drop(blocking_results);

        socket.set_read_timeout(Some(Duration::from_secs(3)))?;
        let followup_packet = build_ipv4_udp_packet(
            Ipv4Addr::new(10, 26, 1, 101),
            Ipv4Addr::new(10, 26, 1, 100),
            followup_src_port,
            dst_port,
            payload,
        );
        let mut results = [None];
        let returned = block_on_timeout(
            async {
                device
                    .send_many(
                        vec![followup_packet.clone()],
                        &mut results,
                        Duration::from_secs(3),
                        false,
                    )
                    .await
            },
            Duration::from_secs(5),
        )
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for followup send_many after busy-slot timeout",
            )
        })?;

        assert_eq!(returned.len(), 1);
        assert_eq!(returned[0].as_ref(), followup_packet.as_ref());
        assert_eq!(results[0].take().unwrap()?, followup_packet.len());

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let received = socket.recv(&mut recv_buf)?;
            if &recv_buf[..received] == payload {
                break;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out receiving followup payload after busy-slot timeout",
                ));
            }
        }

        Ok(())
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_send_many_cleanup_stress(
        device: &UringDevice,
        payload_prefix: &str,
    ) -> io::Result<()> {
        exercise_send_many_cleanup_stress_with_rounds(
            device,
            payload_prefix,
            CLEANUP_STRESS_ROUNDS,
            18_190,
            44_000,
        )
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_send_many_cleanup_long_stress(
        device: &UringDevice,
        payload_prefix: &str,
    ) -> io::Result<()> {
        exercise_send_many_cleanup_stress_with_rounds(
            device,
            payload_prefix,
            CLEANUP_LONG_STRESS_ROUNDS,
            18_240,
            48_000,
        )
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_send_many_mixed_stress(
        device: &UringDevice,
        payload_prefix: &str,
    ) -> io::Result<()> {
        for round in 0..TX_MIXED_STRESS_ROUNDS {
            let round_port_base = 18_300 + round * 7;
            let round_src_port_base = 52_000 + round * 16;

            let unordered_first_payload = format!("{payload_prefix} unordered first round {round}");
            let unordered_second_payload =
                format!("{payload_prefix} unordered second round {round}");
            exercise_send_many_delivers_ipv4_udp_on_port(
                device,
                unordered_first_payload.as_bytes(),
                unordered_second_payload.as_bytes(),
                round_port_base,
                round_src_port_base,
                round_src_port_base + 1,
                false,
            )?;

            let ordered_first_payload = format!("{payload_prefix} ordered first round {round}");
            let ordered_second_payload = format!("{payload_prefix} ordered second round {round}");
            exercise_send_many_delivers_ipv4_udp_on_port(
                device,
                ordered_first_payload.as_bytes(),
                ordered_second_payload.as_bytes(),
                round_port_base + 1,
                round_src_port_base + 2,
                round_src_port_base + 3,
                true,
            )?;

            let chain_break_payload = format!("{payload_prefix} chain break round {round}");
            exercise_send_many_keep_order_chain_breaks_after_invalid_packet_on_port(
                device,
                chain_break_payload.as_bytes(),
                round_port_base + 2,
                round_src_port_base + 4,
                round_src_port_base + 5,
            )?;

            let drop_payload = format!("{payload_prefix} drop round {round}");
            exercise_send_many_drop_cleanup_allows_followup_on_port(
                device,
                drop_payload.as_bytes(),
                round_port_base + 3,
                round_src_port_base + 6,
                round_src_port_base + 7,
            )?;

            let timeout_payload = format!("{payload_prefix} timeout round {round}");
            exercise_send_many_timeout_allows_followup_on_port(
                device,
                timeout_payload.as_bytes(),
                round_port_base + 4,
                round_src_port_base + 8,
                round_src_port_base + 9,
            )?;

            let busy_slot_payload = format!("{payload_prefix} busy round {round}");
            exercise_send_many_waiting_for_busy_slot_times_out_on_ports(
                device,
                busy_slot_payload.as_bytes(),
                round_port_base + 5,
                round_port_base + 6,
                round_src_port_base + 10,
                round_src_port_base + 11,
                round_src_port_base + 12,
            )?;
        }

        Ok(())
    }

    #[cfg(all(
        any(feature = "async_tokio", feature = "async_io"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    fn exercise_send_many_cleanup_stress_with_rounds(
        device: &UringDevice,
        payload_prefix: &str,
        rounds: u16,
        port_base: u16,
        src_port_base: u16,
    ) -> io::Result<()> {
        for round in 0..rounds {
            let round_port_base = port_base + round * 5;
            let round_src_port_base = src_port_base + round * 10;

            let drop_payload = format!("{payload_prefix} drop round {round}");
            exercise_send_many_drop_cleanup_allows_followup_on_port(
                device,
                drop_payload.as_bytes(),
                round_port_base,
                round_src_port_base,
                round_src_port_base + 1,
            )?;

            let timeout_payload = format!("{payload_prefix} timeout round {round}");
            exercise_send_many_timeout_allows_followup_on_port(
                device,
                timeout_payload.as_bytes(),
                round_port_base + 1,
                round_src_port_base + 2,
                round_src_port_base + 3,
            )?;

            let busy_slot_payload = format!("{payload_prefix} busy round {round}");
            exercise_send_many_waiting_for_busy_slot_times_out_on_ports(
                device,
                busy_slot_payload.as_bytes(),
                round_port_base + 2,
                round_port_base + 3,
                round_src_port_base + 4,
                round_src_port_base + 5,
                round_src_port_base + 6,
            )?;

            let delivery_first_payload = format!("{payload_prefix} deliver first round {round}");
            let delivery_second_payload = format!("{payload_prefix} deliver second round {round}");
            exercise_send_many_delivers_ipv4_udp_on_port(
                device,
                delivery_first_payload.as_bytes(),
                delivery_second_payload.as_bytes(),
                round_port_base + 4,
                round_src_port_base + 7,
                round_src_port_base + 8,
                round % 2 == 1,
            )?;
        }

        Ok(())
    }

    #[cfg(all(feature = "async_io", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_rx_api_receives_ipv4_packet_on_single_queue_tun_async_io() {
        let mut device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };

        assert!(matches!(device.rx_state(), RxState::Stopped));
        assert_eq!(device.ready_len(), 0);
        device.start_rx().unwrap();
        assert!(matches!(device.rx_state(), RxState::Running));

        let payload = b"tun-rs-uring async-io rx";
        let (stop, sender) = spawn_ipv4_udp_sender("10.26.1.100:0", "10.26.1.101:8080", payload);

        let result = block_on_timeout(
            async {
                device.readable().await?;
                let ready_len = device.ready_len();
                assert!(ready_len > 0);
                let packet = device.try_recv()?;
                assert!(!packet.is_detached());
                assert!(packet_has_ipv4_udp_payload(packet.as_bytes(), payload));
                device.stop_rx().await?;
                Ok::<(), io::Error>(())
            },
            Duration::from_secs(4),
        );

        stop.store(true, Ordering::Release);
        sender.join().unwrap().unwrap();

        match result {
            Some(Ok(())) => assert!(matches!(device.rx_state(), RxState::Stopped)),
            Some(Err(error)) if should_skip_live_public_rx_test(&error) => {}
            Some(Err(error)) => panic!("unexpected public RX API failure: {error}"),
            None => panic!("timed out waiting for public RX API to receive a packet"),
        }
    }

    #[cfg(all(feature = "async_io", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_rx_offload_packet_exposes_lazy_offload_info_async_io() {
        let device = match build_manual_start_offload_device_with_config(
            UringDeviceConfig::default()
                .with_rx_start_mode(RxStartMode::ManualStart)
                .with_rx_buffer_count(128),
        ) {
            Ok(Some(device)) => device,
            Ok(None) => return,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };

        if let Err(error) = exercise_offload_metadata_is_lazily_available(
            device,
            b"tun-rs-uring async-io offload rx",
        ) {
            panic!("unexpected offload metadata live failure: {error}");
        }
    }

    #[cfg(all(feature = "async_io", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_rx_faults_with_enobufs_until_manual_restart_async_io() {
        let device = match build_manual_start_device_with_config(
            UringDeviceConfig::default()
                .with_rx_start_mode(RxStartMode::ManualStart)
                .with_rx_buffer_count(4)
                .with_rx_auto_resume_after_recycled_slots(0),
        ) {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };

        if let Err(error) =
            exercise_manual_restart_after_enobufs(device, b"tun-rs-uring async-io enobufs manual")
        {
            panic!("unexpected manual restart ENOBUFS failure: {error}");
        }
    }

    #[cfg(all(feature = "async_io", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_rx_auto_resumes_after_recycled_slots_async_io() {
        let device = match build_manual_start_device_with_config(
            UringDeviceConfig::default()
                .with_rx_start_mode(RxStartMode::ManualStart)
                .with_rx_buffer_count(4)
                .with_rx_auto_resume_after_recycled_slots(2),
        ) {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };

        if let Err(error) =
            exercise_auto_resume_after_enobufs(device, b"tun-rs-uring async-io enobufs auto")
        {
            panic!("unexpected auto-resume ENOBUFS failure: {error}");
        }
    }

    #[cfg(all(feature = "async_io", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_rx_recv_many_limits_and_drains_ready_packets_async_io() {
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };

        if let Err(error) = exercise_recv_many_limits_and_drains_ready_packets(
            device,
            "tun-rs-uring async-io recv_many",
        ) {
            panic!("unexpected recv_many live failure: {error}");
        }
    }

    #[cfg(all(feature = "async_io", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_rx_stop_prevents_new_completions_until_restart_async_io() {
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };

        if let Err(error) = exercise_stop_rx_prevents_new_completions_until_restart(
            device,
            b"tun-rs-uring async-io stop-rx",
        ) {
            panic!("unexpected stop_rx live failure: {error}");
        }
    }

    #[cfg(all(feature = "async_io", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_rx_manual_restart_stress_async_io() {
        let device = match build_manual_start_device_with_config(
            UringDeviceConfig::default()
                .with_rx_start_mode(RxStartMode::ManualStart)
                .with_rx_buffer_count(4)
                .with_rx_auto_resume_after_recycled_slots(0),
        ) {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };

        if let Err(error) = exercise_manual_restart_after_enobufs_stress(
            device,
            "tun-rs-uring async-io rx recovery stress",
        ) {
            panic!("unexpected manual restart stress failure: {error}");
        }
    }

    #[cfg(all(feature = "async_io", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_rx_auto_resume_stress_async_io() {
        let device = match build_manual_start_device_with_config(
            UringDeviceConfig::default()
                .with_rx_start_mode(RxStartMode::ManualStart)
                .with_rx_buffer_count(4)
                .with_rx_auto_resume_after_recycled_slots(2),
        ) {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };

        if let Err(error) = exercise_auto_resume_after_enobufs_stress(
            device,
            "tun-rs-uring async-io rx auto-resume stress",
        ) {
            panic!("unexpected auto-resume stress failure: {error}");
        }
    }

    #[cfg(all(feature = "async_io", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_send_many_delivers_ipv4_udp_packet_async_io() {
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };

        if let Err(error) =
            exercise_send_many_delivers_ipv4_udp(&device, b"tun-rs-uring async-io send_many")
        {
            panic!("unexpected send_many failure: {error}");
        }
    }

    #[cfg(all(feature = "async_io", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_send_many_keep_order_delivers_ipv4_udp_packet_async_io() {
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };

        if let Err(error) = exercise_send_many_keep_order_delivers_ipv4_udp(
            &device,
            b"tun-rs-uring async-io send_many keep_order",
        ) {
            panic!("unexpected keep_order send_many failure: {error}");
        }
    }

    #[cfg(all(feature = "async_io", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_send_many_keep_order_chain_breaks_after_invalid_packet_async_io() {
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };

        if let Err(error) = exercise_send_many_keep_order_chain_breaks_after_invalid_packet(
            &device,
            b"tun-rs-uring async-io send_many keep_order chain break",
        ) {
            panic!("unexpected keep_order chain-break failure: {error}");
        }
    }

    #[cfg(all(feature = "async_io", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_send_many_drop_cleanup_allows_followup_batch_async_io() {
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };

        if let Err(error) = exercise_send_many_drop_cleanup_allows_followup(
            &device,
            b"tun-rs-uring async-io send_many drop cleanup",
        ) {
            panic!("unexpected send_many drop cleanup failure: {error}");
        }
    }

    #[cfg(all(feature = "async_io", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_send_many_timeout_allows_followup_batch_async_io() {
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };

        if let Err(error) = exercise_send_many_timeout_allows_followup(
            &device,
            b"tun-rs-uring async-io send_many timeout",
        ) {
            panic!("unexpected send_many timeout failure: {error}");
        }
    }

    #[cfg(all(feature = "async_io", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_send_many_waiting_for_busy_slot_times_out_async_io() {
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };

        if let Err(error) = exercise_send_many_waiting_for_busy_slot_times_out(
            &device,
            b"tun-rs-uring async-io send_many busy-slot timeout",
        ) {
            panic!("unexpected send_many busy-slot timeout failure: {error}");
        }
    }

    #[cfg(all(feature = "async_io", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_send_many_cleanup_stress_async_io() {
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };

        if let Err(error) =
            exercise_send_many_cleanup_stress(&device, "tun-rs-uring async-io cleanup stress")
        {
            panic!("unexpected send_many cleanup stress failure: {error}");
        }
    }

    #[cfg(all(feature = "async_io", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_send_many_cleanup_long_stress_async_io() {
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };

        if let Err(error) = exercise_send_many_cleanup_long_stress(
            &device,
            "tun-rs-uring async-io cleanup long stress",
        ) {
            panic!("unexpected send_many cleanup long stress failure: {error}");
        }
    }

    #[cfg(all(feature = "async_io", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_send_many_mixed_stress_async_io() {
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };

        if let Err(error) =
            exercise_send_many_mixed_stress(&device, "tun-rs-uring async-io tx mixed stress")
        {
            panic!("unexpected send_many mixed stress failure: {error}");
        }
    }

    #[cfg(all(feature = "async_tokio", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_rx_api_receives_ipv4_packet_on_single_queue_tun_async_tokio() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let mut device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };
        drop(_guard);

        assert!(matches!(device.rx_state(), RxState::Stopped));
        assert_eq!(device.ready_len(), 0);
        device.start_rx().unwrap();
        assert!(matches!(device.rx_state(), RxState::Running));

        let payload = b"tun-rs-uring async-tokio rx";
        let (stop, sender) = spawn_ipv4_udp_sender("10.26.1.100:0", "10.26.1.101:8080", payload);

        let result = block_on_timeout(
            async {
                device.readable().await?;
                let ready_len = device.ready_len();
                assert!(ready_len > 0);
                let packet = device.try_recv()?;
                assert!(!packet.is_detached());
                assert!(packet_has_ipv4_udp_payload(packet.as_bytes(), payload));
                device.stop_rx().await?;
                Ok::<(), io::Error>(())
            },
            Duration::from_secs(4),
        );

        stop.store(true, Ordering::Release);
        sender.join().unwrap().unwrap();

        match result {
            Some(Ok(())) => assert!(matches!(device.rx_state(), RxState::Stopped)),
            Some(Err(error)) if should_skip_live_public_rx_test(&error) => {}
            Some(Err(error)) => panic!("unexpected public RX API failure: {error}"),
            None => panic!("timed out waiting for public RX API to receive a packet"),
        }
    }

    #[cfg(all(feature = "async_tokio", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_rx_offload_packet_exposes_lazy_offload_info_async_tokio() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let device = match build_manual_start_offload_device_with_config(
            UringDeviceConfig::default()
                .with_rx_start_mode(RxStartMode::ManualStart)
                .with_rx_buffer_count(128),
        ) {
            Ok(Some(device)) => device,
            Ok(None) => return,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };
        drop(_guard);

        if let Err(error) = exercise_offload_metadata_is_lazily_available(
            device,
            b"tun-rs-uring async-tokio offload rx",
        ) {
            panic!("unexpected offload metadata live failure: {error}");
        }
    }

    #[cfg(all(feature = "async_tokio", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_rx_faults_with_enobufs_until_manual_restart_async_tokio() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let device = match build_manual_start_device_with_config(
            UringDeviceConfig::default()
                .with_rx_start_mode(RxStartMode::ManualStart)
                .with_rx_buffer_count(4)
                .with_rx_auto_resume_after_recycled_slots(0),
        ) {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };
        drop(_guard);

        if let Err(error) = exercise_manual_restart_after_enobufs(
            device,
            b"tun-rs-uring async-tokio enobufs manual",
        ) {
            panic!("unexpected manual restart ENOBUFS failure: {error}");
        }
    }

    #[cfg(all(feature = "async_tokio", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_rx_auto_resumes_after_recycled_slots_async_tokio() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let device = match build_manual_start_device_with_config(
            UringDeviceConfig::default()
                .with_rx_start_mode(RxStartMode::ManualStart)
                .with_rx_buffer_count(4)
                .with_rx_auto_resume_after_recycled_slots(2),
        ) {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };
        drop(_guard);

        if let Err(error) =
            exercise_auto_resume_after_enobufs(device, b"tun-rs-uring async-tokio enobufs auto")
        {
            panic!("unexpected auto-resume ENOBUFS failure: {error}");
        }
    }

    #[cfg(all(feature = "async_tokio", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_rx_recv_many_limits_and_drains_ready_packets_async_tokio() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };
        drop(_guard);

        if let Err(error) = exercise_recv_many_limits_and_drains_ready_packets(
            device,
            "tun-rs-uring async-tokio recv_many",
        ) {
            panic!("unexpected recv_many live failure: {error}");
        }
    }

    #[cfg(all(feature = "async_tokio", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_rx_stop_prevents_new_completions_until_restart_async_tokio() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };
        drop(_guard);

        if let Err(error) = exercise_stop_rx_prevents_new_completions_until_restart(
            device,
            b"tun-rs-uring async-tokio stop-rx",
        ) {
            panic!("unexpected stop_rx live failure: {error}");
        }
    }

    #[cfg(all(feature = "async_tokio", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_rx_manual_restart_stress_async_tokio() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let device = match build_manual_start_device_with_config(
            UringDeviceConfig::default()
                .with_rx_start_mode(RxStartMode::ManualStart)
                .with_rx_buffer_count(4)
                .with_rx_auto_resume_after_recycled_slots(0),
        ) {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };
        drop(_guard);

        if let Err(error) = exercise_manual_restart_after_enobufs_stress(
            device,
            "tun-rs-uring async-tokio rx recovery stress",
        ) {
            panic!("unexpected manual restart stress failure: {error}");
        }
    }

    #[cfg(all(feature = "async_tokio", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_rx_auto_resume_stress_async_tokio() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let device = match build_manual_start_device_with_config(
            UringDeviceConfig::default()
                .with_rx_start_mode(RxStartMode::ManualStart)
                .with_rx_buffer_count(4)
                .with_rx_auto_resume_after_recycled_slots(2),
        ) {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };
        drop(_guard);

        if let Err(error) = exercise_auto_resume_after_enobufs_stress(
            device,
            "tun-rs-uring async-tokio rx auto-resume stress",
        ) {
            panic!("unexpected auto-resume stress failure: {error}");
        }
    }

    #[cfg(all(feature = "async_tokio", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_send_many_delivers_ipv4_udp_packet_async_tokio() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };
        drop(_guard);

        if let Err(error) =
            exercise_send_many_delivers_ipv4_udp(&device, b"tun-rs-uring async-tokio send_many")
        {
            panic!("unexpected send_many failure: {error}");
        }
    }

    #[cfg(all(feature = "async_tokio", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_send_many_keep_order_delivers_ipv4_udp_packet_async_tokio() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };
        drop(_guard);

        if let Err(error) = exercise_send_many_keep_order_delivers_ipv4_udp(
            &device,
            b"tun-rs-uring async-tokio send_many keep_order",
        ) {
            panic!("unexpected keep_order send_many failure: {error}");
        }
    }

    #[cfg(all(feature = "async_tokio", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_send_many_keep_order_chain_breaks_after_invalid_packet_async_tokio() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };
        drop(_guard);

        if let Err(error) = exercise_send_many_keep_order_chain_breaks_after_invalid_packet(
            &device,
            b"tun-rs-uring async-tokio send_many keep_order chain break",
        ) {
            panic!("unexpected keep_order chain-break failure: {error}");
        }
    }

    #[cfg(all(feature = "async_tokio", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_send_many_drop_cleanup_allows_followup_batch_async_tokio() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };
        drop(_guard);

        if let Err(error) = exercise_send_many_drop_cleanup_allows_followup(
            &device,
            b"tun-rs-uring async-tokio send_many drop cleanup",
        ) {
            panic!("unexpected send_many drop cleanup failure: {error}");
        }
    }

    #[cfg(all(feature = "async_tokio", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_send_many_timeout_allows_followup_batch_async_tokio() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };
        drop(_guard);

        if let Err(error) = exercise_send_many_timeout_allows_followup(
            &device,
            b"tun-rs-uring async-tokio send_many timeout",
        ) {
            panic!("unexpected send_many timeout failure: {error}");
        }
    }

    #[cfg(all(feature = "async_tokio", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_send_many_waiting_for_busy_slot_times_out_async_tokio() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };
        drop(_guard);

        if let Err(error) = exercise_send_many_waiting_for_busy_slot_times_out(
            &device,
            b"tun-rs-uring async-tokio send_many busy-slot timeout",
        ) {
            panic!("unexpected send_many busy-slot timeout failure: {error}");
        }
    }

    #[cfg(all(feature = "async_tokio", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_send_many_cleanup_stress_async_tokio() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };
        drop(_guard);

        if let Err(error) =
            exercise_send_many_cleanup_stress(&device, "tun-rs-uring async-tokio cleanup stress")
        {
            panic!("unexpected send_many cleanup stress failure: {error}");
        }
    }

    #[cfg(all(feature = "async_tokio", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_send_many_cleanup_long_stress_async_tokio() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };
        drop(_guard);

        if let Err(error) = exercise_send_many_cleanup_long_stress(
            &device,
            "tun-rs-uring async-tokio cleanup long stress",
        ) {
            panic!("unexpected send_many cleanup long stress failure: {error}");
        }
    }

    #[cfg(all(feature = "async_tokio", target_os = "linux", not(target_env = "ohos")))]
    #[test]
    fn public_send_many_mixed_stress_async_tokio() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let device = match build_manual_start_device() {
            Ok(device) => device,
            Err(error) if should_skip_live_public_rx_test(&error) => return,
            Err(error) => panic!("unexpected UringDevice init failure: {error}"),
        };
        drop(_guard);

        if let Err(error) =
            exercise_send_many_mixed_stress(&device, "tun-rs-uring async-tokio tx mixed stress")
        {
            panic!("unexpected send_many mixed stress failure: {error}");
        }
    }
}
