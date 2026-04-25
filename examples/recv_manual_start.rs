mod common;

#[cfg(any(feature = "async_tokio", feature = "async_io"))]
use std::{io, time::Duration};

#[cfg(any(feature = "async_tokio", feature = "async_io"))]
use common::{
    build_manual_start_device, finish_live_example, packet_has_ipv4_udp_payload,
    spawn_ipv4_udp_sender,
};

#[cfg(not(any(feature = "async_tokio", feature = "async_io")))]
fn main() {
    eprintln!("enable exactly one backend feature: `async_tokio` or `async_io`");
    std::process::exit(1);
}

#[cfg(all(feature = "async_tokio", not(feature = "async_io")))]
fn main() -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()?;
    finish_live_example(runtime.block_on(run()))
}

#[cfg(all(feature = "async_io", not(feature = "async_tokio")))]
fn main() -> io::Result<()> {
    finish_live_example(async_io::block_on(run()))
}

#[cfg(any(feature = "async_tokio", feature = "async_io"))]
async fn run() -> io::Result<()> {
    let mut device = build_manual_start_device()?;
    let payload = b"tun-rs-uring example recv";

    println!("backend={}", tun_rs_uring::UringDevice::backend_name());
    println!("starting RX");
    device.start_rx()?;

    let (stop, sender) = spawn_ipv4_udp_sender(
        "10.26.1.100:0",
        "10.26.1.101:28080",
        payload,
        Duration::from_millis(20),
        Duration::from_secs(3),
    );

    let received = device.recv().await;
    stop.store(true, std::sync::atomic::Ordering::Release);
    sender.join().unwrap()?;

    let packet = received?;
    println!("received packet bytes={}", packet.len());
    if !packet_has_ipv4_udp_payload(packet.as_bytes(), payload) {
        return Err(io::Error::other("received unexpected UDP payload"));
    }

    device.stop_rx().await?;
    println!("stopped RX");
    Ok(())
}
