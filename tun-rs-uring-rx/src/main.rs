#[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
fn main() {
    eprintln!("this reproducer is Linux-only");
    std::process::exit(1);
}

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    linux::run().await
}

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
mod linux {
    use std::io;
    use std::net::Ipv4Addr;
    use std::time::{Duration, Instant};

    use tokio::process::Command;
    use tun_rs::DeviceBuilder;
    use tun_rs_uring::{UringDevice, UringDeviceConfig};

    const POLL_INTERVAL: Duration = Duration::from_millis(10);
    const READ_TIMEOUT: Duration = Duration::from_millis(1500);

    pub async fn run() -> io::Result<()> {
        println!("kernel: {}", command_output("uname", &["-a"]).await?);
        println!("uid: {}", command_output("id", &["-u"]).await?);

        let sync_ok = sync_baseline().await?;
        if !sync_ok {
            eprintln!("sync baseline failed; TUN/ping environment is not valid");
            std::process::exit(1);
        }

        let uring_blocking_ok = uring_case(
            "turing0",
            Ipv4Addr::new(10, 254, 72, 1),
            Ipv4Addr::new(10, 254, 72, 2),
            false,
        )
        .await?;
        let uring_nonblocking_ok = uring_case(
            "turing1",
            Ipv4Addr::new(10, 254, 73, 1),
            Ipv4Addr::new(10, 254, 73, 2),
            true,
        )
        .await?;

        if !uring_blocking_ok || !uring_nonblocking_ok {
            println!(
                "REPRODUCED: SyncDevice receives the kernel-generated TUN packet, but UringDevice does not."
            );
            std::process::exit(0);
        }

        println!(
            "NOT REPRODUCED: UringDevice received packets in both blocking and nonblocking fd modes."
        );
        std::process::exit(2);
    }

    async fn sync_baseline() -> io::Result<bool> {
        let name = "tsync0";
        let local = Ipv4Addr::new(10, 254, 71, 1);
        let peer = Ipv4Addr::new(10, 254, 71, 2);
        let device = build_tun(name, local)?;
        device.set_nonblocking(true)?;

        println!("sync baseline: pinging {peer} through {name}");
        spawn_ping(peer);
        let mut buf = vec![0u8; 2048];
        let result = poll_sync_recv(&device, &mut buf, peer, READ_TIMEOUT).await;
        report_packet("sync baseline", result.as_deref());
        Ok(result.is_some())
    }

    async fn uring_case(
        name: &str,
        local: Ipv4Addr,
        peer: Ipv4Addr,
        nonblocking: bool,
    ) -> io::Result<bool> {
        let device = build_tun(name, local)?;
        if nonblocking {
            device.set_nonblocking(true)?;
        }
        let mut device = UringDevice::new(device, UringDeviceConfig::default())?;
        device.start_rx()?;

        println!("uring case: pinging {peer} through {name}; fd_nonblocking={nonblocking}");
        spawn_ping(peer);
        let result = poll_uring_recv(&mut device, peer, READ_TIMEOUT).await;
        report_packet("uring case", result.as_deref());
        Ok(result.is_some())
    }

    fn build_tun(name: &str, local: Ipv4Addr) -> io::Result<tun_rs::SyncDevice> {
        DeviceBuilder::new()
            .name(name)
            .packet_information(false)
            .ipv4(local, 24, None)
            .enable(true)
            .build_sync()
    }

    async fn poll_sync_recv(
        device: &tun_rs::SyncDevice,
        buf: &mut [u8],
        peer: Ipv4Addr,
        timeout: Duration,
    ) -> Option<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match device.recv(buf) {
                Ok(n) if is_ipv4_packet_to(&buf[..n], peer) => return Some(buf[..n].to_vec()),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => {
                    eprintln!("sync recv error: {error}");
                    return None;
                }
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        None
    }

    async fn poll_uring_recv(
        device: &mut UringDevice,
        peer: Ipv4Addr,
        timeout: Duration,
    ) -> Option<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match tokio::time::timeout(POLL_INTERVAL, device.readable()).await {
                Ok(Ok(())) | Err(_) => {}
                Ok(Err(error)) if error.kind() == io::ErrorKind::WouldBlock => {}
                Ok(Err(error)) => {
                    eprintln!("uring readable error: {error}");
                    return None;
                }
            }

            loop {
                match device.try_recv() {
                    Ok(packet) if is_ipv4_packet_to(packet.as_bytes(), peer) => {
                        return Some(packet.as_bytes().to_vec());
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) => {
                        eprintln!("uring try_recv error: {error}");
                        return None;
                    }
                }
            }
        }
        None
    }

    fn is_ipv4_packet_to(packet: &[u8], dst: Ipv4Addr) -> bool {
        packet.len() >= 20 && packet[0] >> 4 == 4 && packet[16..20] == dst.octets()
    }

    fn spawn_ping(peer: Ipv4Addr) {
        tokio::spawn(async move {
            let _ = Command::new("ping")
                .args(["-c", "1", "-W", "1", &peer.to_string()])
                .status()
                .await;
        });
    }

    fn report_packet(label: &str, packet: Option<&[u8]>) {
        match packet {
            Some(packet) => {
                let prefix_len = packet.len().min(24);
                println!(
                    "{label}: received {} bytes, prefix={:02x?}",
                    packet.len(),
                    &packet[..prefix_len]
                );
            }
            None => println!("{label}: timed out without receiving a packet"),
        }
    }

    async fn command_output(program: &str, args: &[&str]) -> io::Result<String> {
        let output = Command::new(program).args(args).output().await?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}
