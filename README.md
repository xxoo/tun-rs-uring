# `tun-rs-uring`

Linux-only async TUN/TAP device built around `io_uring`.

The crate keeps a single public type, `UringDevice`, and splits backend-specific glue behind feature flags:

- `async_tokio`
- `async_io`
- `async` as an alias of `async_tokio`

Internally, RX uses `read_multishot + provided buffer ring`, while TX keeps single-packet send simple and moves batch send to `send_many()`.

## Requirements

- Linux only
- RX and TX both require `IORING_FEAT_FAST_POLL`
- Linux `6.7+` is the recommended baseline
- Running live examples or tests requires permission to create and configure a TUN device

## Feature Selection

Pick exactly one backend:

```bash
cargo check --no-default-features --features async_tokio
cargo check --no-default-features --features async_io
```

Enabling both backends in one build is rejected at compile time.

## Behavior Boundaries

- RX is single-consumer and keeps a single waiter slot
- `Packet` is ring-backed until `detach()` or `Drop`
- `send_many()` owns all input buffers until the batch reaches a terminal state
- Only one `send_many()` batch may occupy the shared TX ring at a time
- `keep_order` only constrains packet order inside one `send_many()` call
- `keep_order` does not provide global ordering against concurrent `send()` or `try_send()`
- `keep_order` currently uses conservative linked chunks: one chain settles before the next chain is submitted
- `Packet::offload_info()` is lazily parsed from the virtio header only when first used
- `Packet::as_bytes()` and `len()` exclude the virtio header even when RX offload is enabled
- `Packet::split_into()` is available for manual GSO splitting on offload-enabled RX packets
- All send methods assume you handled virtio net headers correctly on offload-enabled devices
- In ordered mode, a link break can propagate `ECANCELED` to later entries in the same linked chunk
- `send_many()` timeout is a real cancellation path: once requests were submitted, the API waits for cancel/drain before returning
- multiqueue is not part of the current implementation scope

Detailed design notes live in:

- [URING_DEVICE_DESIGN.md](./URING_DEVICE_DESIGN.md)
- [URING_DEVICE_IMPLEMENTATION_PLAN.md](./URING_DEVICE_IMPLEMENTATION_PLAN.md)
- [URING_DEVICE_PROGRESS.md](./URING_DEVICE_PROGRESS.md)

## Examples

The repository includes runnable examples under [`examples/`](./examples):

- `recv_manual_start`
  Shows `ManualStart -> start_rx() -> recv() -> stop_rx()`
- `send_many`
  Shows two-packet `send_many()` with optional `--keep-order`
- `perf_smoke`
  Runs a small `send_many()` throughput smoke loop with configurable batch size and round count

Run them with one backend enabled:

```bash
cargo run --example recv_manual_start --no-default-features --features async_tokio
cargo run --example send_many --no-default-features --features async_io -- --keep-order
cargo run --example perf_smoke --no-default-features --features async_tokio -- --rounds 4 --batch-size 64
```

If the current environment cannot create a TUN device, the examples print a clear skip note instead of failing with an unexplained raw OS error.

## Test Matrix

Core checks:

```bash
cargo test --no-default-features
cargo test --no-default-features --features async_tokio
cargo test --no-default-features --features async_io
```

Example compile smoke:

```bash
cargo check --examples --no-default-features --features async_tokio
cargo check --examples --no-default-features --features async_io
```

## Current Status

The current tree has:

- RX lifecycle, recycle, `ENOBUFS`, manual restart and auto-resume
- TX `send_many()` happy path, timeout, drop cleanup and busy-slot timeout
- ordered `send_many()` linked submission
- ordered chain-break behavior coverage
- RX recovery stress and TX mixed stress live coverage
- RX lazy `offload_info()` live coverage on offload-enabled devices
- runnable examples and a configurable `perf_smoke` example
- successful `cargo package --allow-dirty` packaging preflight

The first release baseline is in place. Further work is mainly formal benchmarking and longer soak testing rather than core state-machine correctness.
