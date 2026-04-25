#![allow(dead_code)]

use bytes::Bytes;
use std::{
    io,
    net::Ipv4Addr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use tun_rs::DeviceBuilder;
use tun_rs_uring::{RxStartMode, UringDevice, UringDeviceConfig};

pub fn build_manual_start_device() -> io::Result<UringDevice> {
    let device = DeviceBuilder::new()
        .ipv4("10.26.1.100", 24, None)
        .build_sync()?;

    UringDevice::new(
        device,
        UringDeviceConfig::default()
            .with_rx_start_mode(RxStartMode::ManualStart)
            .with_rx_buffer_count(128),
    )
}

pub fn finish_live_example(result: io::Result<()>) -> io::Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if should_skip_live_example(&error) => {
            eprintln!(
                "live example skipped: {error}. this example needs permission to create and configure a TUN device."
            );
            Ok(())
        }
        Err(error) => Err(error),
    }
}

pub fn packet_has_ipv4_udp_payload(packet: &[u8], expected_payload: &[u8]) -> bool {
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

pub fn build_ipv4_udp_packet(
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

pub fn spawn_ipv4_udp_sender(
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
        let socket = std::net::UdpSocket::bind(&bind_addr)?;
        let deadline = Instant::now() + duration;

        while !stop_for_thread.load(Ordering::Acquire) && Instant::now() < deadline {
            let _ = socket.send_to(&payload, &target_addr);
            thread::sleep(interval);
        }

        Ok(())
    });

    (stop, handle)
}

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

fn should_skip_live_example(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EPERM | libc::EACCES | libc::ENOSYS | libc::ENOENT | libc::ENODEV)
    ) || matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
    )
}
