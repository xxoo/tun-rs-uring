mod common;

#[cfg(any(feature = "async_tokio", feature = "async_io"))]
use std::{
    io,
    net::Ipv4Addr,
    net::UdpSocket,
    os::fd::AsRawFd,
    thread,
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
    let warmup_rounds = parse_usize_arg(&args, "--warmup-rounds", 1);
    let batch_size = parse_usize_arg(&args, "--batch-size", 64);
    let keep_order = args.iter().any(|arg| arg == "--keep-order");

    let device = build_manual_start_device()?;
    let dst_port = 18_281;
    let socket = UdpSocket::bind(("10.26.1.100", dst_port))?;
    set_recv_buffer_size(&socket, 16 * 1024 * 1024)?;
    socket.set_read_timeout(Some(Duration::from_secs(10)))?;

    println!(
        "backend={} rounds={rounds} warmup_rounds={warmup_rounds} batch_size={batch_size} keep_order={keep_order}",
        tun_rs_uring::UringDevice::backend_name()
    );

    for round in 0..warmup_rounds {
        let stats = run_round(&device, &socket, round, batch_size, dst_port, keep_order).await?;
        println!(
            "warmup_round={} packets={} received={} bytes={} elapsed_us={}",
            round,
            stats.packets,
            stats.received_packets,
            stats.bytes,
            stats.elapsed.as_micros()
        );
    }

    let measured_start = Instant::now();
    let mut total_packets = 0usize;
    let mut total_bytes = 0usize;
    let mut round_times = Vec::with_capacity(rounds);

    for round in 0..rounds {
        let stats = run_round(
            &device,
            &socket,
            warmup_rounds + round,
            batch_size,
            dst_port,
            keep_order,
        )
        .await?;
        println!(
            "round={} packets={} received={} bytes={} elapsed_us={} packets_per_sec={:.0}",
            round,
            stats.packets,
            stats.received_packets,
            stats.bytes,
            stats.elapsed.as_micros(),
            stats.packets as f64 / stats.elapsed.as_secs_f64()
        );
        total_packets += stats.packets;
        total_bytes += stats.bytes;
        round_times.push(stats.elapsed);
    }

    let elapsed = measured_start.elapsed();
    let round_summary = RoundTimeSummary::new(&round_times);
    println!(
        "perf smoke complete: packets={} bytes={} elapsed_ms={} packets_per_sec={:.0} bytes_per_sec={:.0}",
        total_packets,
        total_bytes,
        elapsed.as_millis(),
        total_packets as f64 / elapsed.as_secs_f64(),
        total_bytes as f64 / elapsed.as_secs_f64()
    );
    println!(
        "round_elapsed_us: min={} p50={} p95={} max={}",
        round_summary.min.as_micros(),
        round_summary.p50.as_micros(),
        round_summary.p95.as_micros(),
        round_summary.max.as_micros()
    );

    Ok(())
}

#[cfg(any(feature = "async_tokio", feature = "async_io"))]
struct RoundStats {
    packets: usize,
    received_packets: usize,
    bytes: usize,
    elapsed: Duration,
}

#[cfg(any(feature = "async_tokio", feature = "async_io"))]
async fn run_round(
    device: &tun_rs_uring::UringDevice,
    socket: &UdpSocket,
    round: usize,
    batch_size: usize,
    dst_port: u16,
    keep_order: bool,
) -> io::Result<RoundStats> {
    let mut batch = Vec::with_capacity(batch_size);
    let mut expected_lens = Vec::with_capacity(batch_size);
    for index in 0..batch_size {
        let payload = format!("perf-smoke round={round} packet={index}");
        let src_port = 49_152 + ((round * batch_size + index) % 16_000) as u16;
        let packet = build_ipv4_udp_packet(
            Ipv4Addr::new(10, 26, 1, 101),
            Ipv4Addr::new(10, 26, 1, 100),
            src_port,
            dst_port,
            payload.as_bytes(),
        );
        expected_lens.push(packet.len());
        batch.push(packet);
    }

    let receiver = spawn_receiver(socket.try_clone()?, batch_size);
    let start = Instant::now();
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

    let mut bytes = 0usize;
    for (index, expected_len) in expected_lens.into_iter().enumerate() {
        let actual = results[index]
            .take()
            .ok_or_else(|| io::Error::other("missing perf smoke result"))??;
        if actual != expected_len {
            return Err(io::Error::other("unexpected perf smoke byte count"));
        }
        bytes += actual;
    }

    let received = receiver
        .join()
        .map_err(|_| io::Error::other("receiver thread panicked"))??;

    Ok(RoundStats {
        packets: batch_size,
        received_packets: received,
        bytes,
        elapsed: start.elapsed(),
    })
}

#[cfg(any(feature = "async_tokio", feature = "async_io"))]
fn spawn_receiver(
    socket: UdpSocket,
    expected_packets: usize,
) -> thread::JoinHandle<io::Result<usize>> {
    thread::spawn(move || {
        socket.set_read_timeout(Some(Duration::from_millis(100)))?;
        let mut recv_buf = [0u8; 2048];
        let mut received = 0usize;
        while received < expected_packets {
            match socket.recv(&mut recv_buf) {
                Ok(_) => received += 1,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(received);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(received)
    })
}

#[cfg(any(feature = "async_tokio", feature = "async_io"))]
struct RoundTimeSummary {
    min: Duration,
    p50: Duration,
    p95: Duration,
    max: Duration,
}

#[cfg(any(feature = "async_tokio", feature = "async_io"))]
impl RoundTimeSummary {
    fn new(round_times: &[Duration]) -> Self {
        if round_times.is_empty() {
            return Self {
                min: Duration::ZERO,
                p50: Duration::ZERO,
                p95: Duration::ZERO,
                max: Duration::ZERO,
            };
        }

        let mut sorted = round_times.to_vec();
        sorted.sort_unstable();
        Self {
            min: sorted[0],
            p50: percentile(&sorted, 50),
            p95: percentile(&sorted, 95),
            max: sorted[sorted.len() - 1],
        }
    }
}

#[cfg(any(feature = "async_tokio", feature = "async_io"))]
fn percentile(sorted: &[Duration], percentile: usize) -> Duration {
    let rank = ((sorted.len() * percentile).saturating_add(99) / 100).saturating_sub(1);
    sorted[rank.min(sorted.len() - 1)]
}

#[cfg(any(feature = "async_tokio", feature = "async_io"))]
fn set_recv_buffer_size(socket: &UdpSocket, size: usize) -> io::Result<()> {
    let size = libc::c_int::try_from(size)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "recv buffer is too large"))?;
    let result = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            (&size as *const libc::c_int).cast::<libc::c_void>(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
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
