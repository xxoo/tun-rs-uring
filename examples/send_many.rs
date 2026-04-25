mod common;

#[cfg(any(feature = "async_tokio", feature = "async_io"))]
use std::{io, net::Ipv4Addr, net::UdpSocket, time::Duration};

#[cfg(any(feature = "async_tokio", feature = "async_io"))]
use common::{build_ipv4_udp_packet, build_manual_start_device, finish_live_example};

#[cfg(not(any(feature = "async_tokio", feature = "async_io")))]
fn main() {
    eprintln!("enable exactly one backend feature: `async_tokio` or `async_io`");
    std::process::exit(1);
}

#[cfg(all(feature = "async_tokio", not(feature = "async_io")))]
fn main() -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    finish_live_example(runtime.block_on(run()))
}

#[cfg(all(feature = "async_io", not(feature = "async_tokio")))]
fn main() -> io::Result<()> {
    finish_live_example(async_io::block_on(run()))
}

#[cfg(any(feature = "async_tokio", feature = "async_io"))]
async fn run() -> io::Result<()> {
    let keep_order = std::env::args().any(|arg| arg == "--keep-order");
    let device = build_manual_start_device()?;
    let socket = UdpSocket::bind(("10.26.1.100", 18_180))?;
    socket.set_read_timeout(Some(Duration::from_secs(3)))?;

    let first_payload = b"example send_many first".to_vec();
    let second_payload = b"example send_many second".to_vec();
    let first_packet = build_ipv4_udp_packet(
        Ipv4Addr::new(10, 26, 1, 101),
        Ipv4Addr::new(10, 26, 1, 100),
        40_500,
        18_180,
        &first_payload,
    );
    let second_packet = build_ipv4_udp_packet(
        Ipv4Addr::new(10, 26, 1, 101),
        Ipv4Addr::new(10, 26, 1, 100),
        40_501,
        18_180,
        &second_payload,
    );
    let expected_lens = [first_packet.len(), second_packet.len()];
    let mut results = [None, None];

    println!(
        "backend={} keep_order={keep_order}",
        tun_rs_uring::UringDevice::backend_name()
    );
    let returned = device
        .send_many(
            vec![first_packet.clone(), second_packet.clone()],
            &mut results,
            Duration::from_secs(3),
            keep_order,
        )
        .await;

    assert_eq!(returned.len(), 2);
    println!("send_many returned {} buffers", returned.len());
    for (index, expected_len) in expected_lens.into_iter().enumerate() {
        let actual = results[index]
            .take()
            .ok_or_else(|| io::Error::other("missing send_many result"))??;
        println!("result[{index}]={actual}");
        if actual != expected_len {
            return Err(io::Error::other("unexpected send_many byte count"));
        }
    }

    let mut recv_buf = [0u8; 2048];
    let mut seen_first = false;
    let mut seen_second = false;
    while !(seen_first && seen_second) {
        let received = socket.recv(&mut recv_buf)?;
        let payload = &recv_buf[..received];
        if payload == first_payload.as_slice() {
            seen_first = true;
        } else if payload == second_payload.as_slice() {
            seen_second = true;
        } else {
            return Err(io::Error::other(
                "received unexpected payload from kernel socket",
            ));
        }
    }

    println!("kernel socket received both payloads");
    Ok(())
}
