# tun-rs-uring RX Reproducer

This is a Linux-only minimal reproducer for a `tun-rs-uring::UringDevice`
receive issue seen while debugging linq server egress.

## What It Tests

The program creates three temporary TUN devices:

- `tsync0`: `tun_rs::SyncDevice` baseline.
- `turing0`: `tun_rs_uring::UringDevice` with the fd left in blocking mode.
- `turing1`: `tun_rs_uring::UringDevice` after `set_nonblocking(true)`.

For each device it configures an IPv4 `/24`, starts a one-shot `ping` to an
unused peer IP in that subnet, and tries to receive the kernel-generated ICMP
packet from the TUN fd.

Expected healthy behavior: all three cases receive an IPv4 packet.

Observed failure on the linq Linux smoke host: `SyncDevice` receives the packet,
while both `UringDevice` cases time out.

The `UringDevice` cases call `start_rx()` explicitly after construction, so the
failure does not depend on whether `RxStartMode::AutoStart` was observed.

## Run

Run as root on Linux:

```sh
cargo run --release
```

Exit codes:

- `0`: issue reproduced, SyncDevice succeeded and at least one UringDevice case
  timed out.
- `1`: environment/setup failure, including SyncDevice baseline failure.
- `2`: not reproduced, both UringDevice cases received packets.

## Observed Output

On Debian aarch64 `6.12.85+deb13-arm64` with
`tun-rs-uring` rev `0ae67af6fd47a64d82705a6aa88e770b7d161189`:

```text
kernel: Linux debian 6.12.85+deb13-arm64 #1 SMP Debian 6.12.85-1 (2026-04-30) aarch64 GNU/Linux
uid: 0
sync baseline: pinging 10.254.71.2 through tsync0
sync baseline: received 84 bytes, prefix=[45, 00, 00, 54, a0, 85, 40, 00, 40, 01, f6, 24, 0a, fe, 47, 01, 0a, fe, 47, 02, 08, 00, 82, e2]
uring case: pinging 10.254.72.2 through turing0; fd_nonblocking=false
uring case: timed out without receiving a packet
uring case: pinging 10.254.73.2 through turing1; fd_nonblocking=true
uring case: timed out without receiving a packet
REPRODUCED: SyncDevice receives the kernel-generated TUN packet, but UringDevice does not.
```
