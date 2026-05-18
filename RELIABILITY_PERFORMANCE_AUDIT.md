# Reliability And Performance Audit

## Scope

This audit covers the current `audit/reliability-performance` branch. The goal was to remove low-risk reliability blockers, make live tests non-silent when required, add repeatable audit automation, and capture a root live performance smoke baseline.

## Environment

- Timestamp: 2026-05-19T01:17:21+08:00
- Host: Linux debian 6.12.86+deb13-arm64 aarch64
- Rust: rustc 1.95.0, cargo 1.95.0
- Root live path: `ssh root@127.0.0.1`
- Root environment for live stages: `TUN_RS_URING_REQUIRE_LIVE=1 RUST_TEST_THREADS=1`
- Root policy: root executed only ordinary-user-built test/example binaries. Root did not run Cargo and did not write project or Cargo cache artifacts.

## Changes Audited

- Added `Packet::is_empty()`.
- Kept `Packet::as_bytes()` and `Packet::len()` as complete-buffer APIs, including the virtio net header when RX offload is enabled.
- Fixed `Packet::split_into()` to split the header-stripped payload while preserving `as_bytes()` as the complete buffer for direct forwarding/reinjection use cases.
- Derived `Default` for `RxState`.
- Replaced long RX constructor argument lists with internal config structs.
- Fixed RX auto-resume so a pending `ENOBUFS` auto-resume updates running state even when a multishot read is already active.
- Added strict live mode through `TUN_RS_URING_REQUIRE_LIVE=1`.
- Made Tokio test timeout helpers use a Tokio runtime with IO and time enabled.
- Hardened live tests against unrelated packets and timing-sensitive TX cleanup assumptions.
- Expanded `perf_smoke` with warmup rounds, per-round latency, throughput, bytes/sec, min/p50/p95/max summaries, and best-effort UDP receive counts.
- Added `scripts/audit_reliability_performance.sh`.

## Command Results

The full audit script passed:

```bash
./scripts/audit_reliability_performance.sh
```

Coverage from the script:

- `cargo fmt --check`: pass
- `cargo test --no-default-features`: pass, 50 tests
- `cargo test --no-default-features --features async_tokio`: pass, 68 tests
- `cargo test --no-default-features --features async_io`: pass, 68 tests
- `cargo check --examples --no-default-features --features async_tokio`: pass
- `cargo check --examples --no-default-features --features async_io`: pass
- `cargo check --no-default-features --features async_tokio,async_io`: failed as expected with the mutually exclusive feature compile error
- `cargo clippy --no-default-features --all-targets --features async_tokio -- -D warnings`: pass
- `cargo clippy --no-default-features --all-targets --features async_io -- -D warnings`: pass
- `cargo package --list --allow-dirty`: pass
- `cargo package --allow-dirty --no-verify`: pass
- Root live Tokio test binary over SSH: pass, 68 tests, forced live mode
- Root live async-io test binary over SSH: pass, 68 tests, forced live mode
- Root-owned file check under the repository after root stages: pass, no files found

## Performance Smoke Results

The numbers below are from root SSH runs of `target/release/examples/perf_smoke`. UDP receive counts are best-effort observability for loopback delivery under burst; pass/fail is based on `send_many()` completion and byte-count validation.

Interpretation note: these raw smoke totals should not be treated as a precise backend comparison. Each round's elapsed time currently includes `send_many()` completion, result validation, and joining the UDP receiver thread. When the receiver misses burst packets, the round includes the receiver's 100 ms read timeout. In the 512-packet runs, the timeout-affected round counts were 4 for Tokio unordered, 5 for Tokio ordered, 4 for async-io unordered, and 3 for async-io ordered. Excluding those timeout-affected rounds, the 512-packet fast-round averages were about 423 us, 424 us, 434 us, and 491 us respectively. That is a better indication that `keep_order` was not actually faster in the measured send path.

| Backend | Batch | Rounds | Keep Order | Packets/s | Bytes/s | Round Latency us min/p50/p95/max |
| --- | ---: | ---: | --- | ---: | ---: | --- |
| async_tokio | 512 | 32 | false | 38,478 | 2,213,815 | 223 / 352 / 102,933 / 104,070 |
| async_tokio | 512 | 32 | true | 31,040 | 1,785,908 | 272 / 371 / 103,444 / 104,609 |
| async_tokio | 1 | 256 | false | 8,377 | 474,178 | 8 / 44 / 306 / 2,425 |
| async_tokio | 2048 | 8 | false | 75,536 | 4,340,160 | 960 / 1,225 / 104,341 / 104,341 |
| async_io | 512 | 32 | false | 38,482 | 2,214,080 | 251 / 429 / 102,440 / 105,107 |
| async_io | 512 | 32 | true | 50,658 | 2,914,604 | 240 / 410 / 102,239 / 103,385 |
| async_io | 1 | 256 | false | 9,982 | 565,022 | 11 / 38 / 365 / 1,299 |
| async_io | 2048 | 8 | false | 145,088 | 8,336,451 | 917 / 1,060 / 104,246 / 104,246 |

## Packaging

`cargo package --list --allow-dirty` was checked to confirm the package includes the source, examples, audit script, and documentation expected for the audit branch. After both audit reports were added, `cargo package --allow-dirty --no-verify` packaged 29 files successfully.

## Residual Risks

- `perf_smoke` is a repeatable smoke benchmark for `send_many()` throughput, not a lossless UDP benchmark. Large bursts can exceed socket receive behavior, so receive counts are reported but not used as the pass condition.
- `cargo-audit`, `cargo-deny`, and `perf` were not installed in this environment. Dependency policy/security and low-level profiling should be run when those tools are available.
- `cargo-miri` exists, but Miri was not used for live `io_uring`/TUN paths because those tests depend on Linux kernel behavior and file descriptors.
- The root live checks depend on local SSH access to `root@127.0.0.1` and the host's ability to create/configure TUN devices.
