# UringDevice does not receive kernel-generated TUN packets while SyncDevice does

## Summary

On Linux aarch64, `tun_rs::SyncDevice` can receive kernel-generated packets from
a TUN device, but `tun_rs_uring::UringDevice` times out in the same setup.

The repro uses a one-shot `ping` to an unused peer IP in the TUN subnet. This
causes the kernel to emit an ICMP echo request to the TUN fd. The SyncDevice
baseline receives that IPv4 packet. Both UringDevice cases time out:

- UringDevice with the fd left in blocking mode.
- UringDevice after `SyncDevice::set_nonblocking(true)`.

The UringDevice cases call `start_rx()` explicitly after construction.

## Environment

- OS: Debian Linux aarch64
- Kernel: `6.12.85+deb13-arm64`
- `tun-rs`: `2.8.3`
- `tun-rs-uring`: `0ae67af6fd47a64d82705a6aa88e770b7d161189`
- Run as `root`

## Repro

```sh
git clone <repo-with-this-repro>
cd repros/tun-rs-uring-rx
cargo run --release
```

## Observed Output

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

## Expected

Both UringDevice cases should receive the same kind of IPv4 packet as the
SyncDevice baseline, or should return a concrete error explaining why RX cannot
start on this TUN fd.

## Notes

This was found while debugging a VPN server that uses Linux TUN. The production
path recovered by switching the server TUN worker back to nonblocking
`SyncDevice`, while preserving one owner per TUN queue.
