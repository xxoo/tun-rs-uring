mod common;

#[cfg(any(feature = "async_tokio", feature = "async_io"))]
use std::{
    io,
    net::Ipv4Addr,
    net::UdpSocket,
    time::{Duration, Instant},
};

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
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let rounds = parse_usize_arg(&args, "--rounds", 4);
    let batch_size = parse_usize_arg(&args, "--batch-size", 64);
    let keep_order = args.iter().any(|arg| arg == "--keep-order");

    let device = build_manual_start_device()?;
    let dst_port = 18_281;
    let socket = UdpSocket::bind(("10.26.1.100", dst_port))?;
    socket.set_read_timeout(Some(Duration::from_secs(3)))?;

    println!(
        "backend={} rounds={rounds} batch_size={batch_size} keep_order={keep_order}",
        tun_rs_uring::UringDevice::backend_name()
    );

    let start = Instant::now();
    let mut total_packets = 0usize;
    let mut total_bytes = 0usize;

    for round in 0..rounds {
        let mut batch = Vec::with_capacity(batch_size);
        let mut expected_lens = Vec::with_capacity(batch_size);
        for index in 0..batch_size {
            let payload = format!("perf-smoke round={round} packet={index}");
            let packet = build_ipv4_udp_packet(
                Ipv4Addr::new(10, 26, 1, 101),
                Ipv4Addr::new(10, 26, 1, 100),
                50_000 + (round * batch_size + index) as u16,
                dst_port,
                payload.as_bytes(),
            );
            expected_lens.push(packet.len());
            batch.push(packet);
        }

        let mut results = std::iter::repeat_with(|| None)
            .take(batch.len())
            .collect::<Vec<_>>();
        let returned = device
            .send_many(batch, &mut results, Duration::from_secs(3), keep_order)
            .await;

        if returned.len() != batch_size {
            return Err(io::Error::other(
                "send_many returned an unexpected buffer count",
            ));
        }

        for (index, expected_len) in expected_lens.into_iter().enumerate() {
            let actual = results[index]
                .take()
                .ok_or_else(|| io::Error::other("missing perf smoke result"))??;
            if actual != expected_len {
                return Err(io::Error::other("unexpected perf smoke byte count"));
            }
            total_bytes += actual;
        }

        let mut recv_buf = [0u8; 2048];
        for _ in 0..batch_size {
            let _ = socket.recv(&mut recv_buf)?;
        }

        total_packets += batch_size;
    }

    let elapsed = start.elapsed();
    println!(
        "perf smoke complete: packets={} bytes={} elapsed_ms={} packets_per_sec={:.0}",
        total_packets,
        total_bytes,
        elapsed.as_millis(),
        total_packets as f64 / elapsed.as_secs_f64()
    );

    Ok(())
}

#[cfg(any(feature = "async_tokio", feature = "async_io"))]
fn parse_usize_arg(args: &[String], flag: &str, default: usize) -> usize {
    let Some(index) = args.iter().position(|arg| arg == flag) else {
        return default;
    };
    let Some(value) = args.get(index + 1) else {
        return default;
    };
    value.parse().unwrap_or(default)
}
