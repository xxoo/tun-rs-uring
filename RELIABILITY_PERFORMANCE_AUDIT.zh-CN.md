# 可靠性与性能审计报告

## 范围

本次审计覆盖当前 `audit/reliability-performance` 分支。目标是修复低风险可靠性阻塞项，在需要 live 测试时避免静默跳过，增加可重复的审计自动化，并记录 root live 性能 smoke 基线。

## 环境

- 时间戳：2026-05-19T01:17:21+08:00
- 主机：Linux debian 6.12.86+deb13-arm64 aarch64
- Rust：rustc 1.95.0，cargo 1.95.0
- root live 路径：`ssh root@127.0.0.1`
- live 阶段 root 环境变量：`TUN_RS_URING_REQUIRE_LIVE=1 RUST_TEST_THREADS=1`
- root 策略：root 只执行普通用户已构建好的测试/示例二进制。root 未运行 Cargo，也未写入项目目录或 Cargo cache 产物。

## 已审计变更

- 新增 `Packet::is_empty()`。
- 保持 `Packet::as_bytes()` 和 `Packet::len()` 的完整 buffer 语义；启用 RX offload 时会包含 virtio net header。
- 修复 `Packet::split_into()`，让它拆分剥离 header 后的 payload，同时保留 `as_bytes()` 作为完整 buffer，方便直接转发或重新写入支持 offload 的设备。
- 为 `RxState` 派生 `Default`。
- 用内部 config 结构替换 RX 构造路径中的过长参数列表。
- 修复 RX auto-resume：存在待处理 `ENOBUFS` auto-resume 时，即使 multishot read 已处于 active 状态，也会正确更新 running 状态。
- 新增严格 live 模式：`TUN_RS_URING_REQUIRE_LIVE=1`。
- 让 Tokio 测试 timeout helper 使用启用 IO 和 time 的 Tokio runtime。
- 加固 live 测试，避免无关包和 TX cleanup 时序假设导致不稳定。
- 增强 `perf_smoke`，增加 warmup 轮次、每轮延迟、吞吐、bytes/sec、min/p50/p95/max 摘要和 best-effort UDP 接收计数。
- 新增 `scripts/audit_reliability_performance.sh`。

## 命令结果

完整审计脚本已通过：

```bash
./scripts/audit_reliability_performance.sh
```

脚本覆盖范围：

- `cargo fmt --check`：通过
- `cargo test --no-default-features`：通过，50 个测试
- `cargo test --no-default-features --features async_tokio`：通过，68 个测试
- `cargo test --no-default-features --features async_io`：通过，68 个测试
- `cargo check --examples --no-default-features --features async_tokio`：通过
- `cargo check --examples --no-default-features --features async_io`：通过
- `cargo check --no-default-features --features async_tokio,async_io`：按预期失败，触发 mutually exclusive feature 编译错误
- `cargo clippy --no-default-features --all-targets --features async_tokio -- -D warnings`：通过
- `cargo clippy --no-default-features --all-targets --features async_io -- -D warnings`：通过
- `cargo package --list --allow-dirty`：通过
- `cargo package --allow-dirty --no-verify`：通过
- 通过 SSH 执行 root live Tokio 测试二进制：通过，68 个测试，强制 live 模式
- 通过 SSH 执行 root live async-io 测试二进制：通过，68 个测试，强制 live 模式
- root 阶段后检查仓库中的 root-owned 文件：通过，未发现文件

## 性能 Smoke 结果

以下数据来自 root SSH 执行的 `target/release/examples/perf_smoke`。UDP 接收计数只用于观察 burst 下 loopback 投递情况，属于 best-effort 指标；通过条件基于 `send_many()` 完成和 byte count 校验。

解读说明：这些原始 smoke 总吞吐不应视为精确的 backend 横向对比。当前每轮耗时包含 `send_many()` 完成、结果校验，以及等待 UDP 接收线程退出。当接收线程在 burst 下漏收包时，该轮会包含接收线程的 100 ms read timeout。512 包场景中，受 timeout 影响的轮数分别是 Tokio unordered 4 轮、Tokio ordered 5 轮、async-io unordered 4 轮、async-io ordered 3 轮。排除这些 timeout 轮后，512 包 fast-round 平均耗时约为 423 us、424 us、434 us、491 us；这个结果更能说明 measured send path 中 `keep_order` 并没有真正更快。

| Backend | Batch | Rounds | Keep Order | Packets/s | Bytes/s | 每轮延迟 us min/p50/p95/max |
| --- | ---: | ---: | --- | ---: | ---: | --- |
| async_tokio | 512 | 32 | false | 38,478 | 2,213,815 | 223 / 352 / 102,933 / 104,070 |
| async_tokio | 512 | 32 | true | 31,040 | 1,785,908 | 272 / 371 / 103,444 / 104,609 |
| async_tokio | 1 | 256 | false | 8,377 | 474,178 | 8 / 44 / 306 / 2,425 |
| async_tokio | 2048 | 8 | false | 75,536 | 4,340,160 | 960 / 1,225 / 104,341 / 104,341 |
| async_io | 512 | 32 | false | 38,482 | 2,214,080 | 251 / 429 / 102,440 / 105,107 |
| async_io | 512 | 32 | true | 50,658 | 2,914,604 | 240 / 410 / 102,239 / 103,385 |
| async_io | 1 | 256 | false | 9,982 | 565,022 | 11 / 38 / 365 / 1,299 |
| async_io | 2048 | 8 | false | 145,088 | 8,336,451 | 917 / 1,060 / 104,246 / 104,246 |

## 打包

已检查 `cargo package --list --allow-dirty`，确认 package 包含本审计分支预期的源码、示例、审计脚本和文档。加入两份审计报告后，`cargo package --allow-dirty --no-verify` 已成功打包 29 个文件。

## 残余风险

- `perf_smoke` 是可重复的 `send_many()` 吞吐 smoke benchmark，不是无丢包 UDP benchmark。大 burst 可能超过 socket 接收行为，因此接收计数只报告，不作为通过条件。
- 当前环境未安装 `cargo-audit`、`cargo-deny` 和 `perf`。依赖策略/安全审计和低层 profiling 应在这些工具可用后补跑。
- 环境中存在 `cargo-miri`，但 live `io_uring`/TUN 路径依赖 Linux 内核行为和文件描述符，因此未使用 Miri 覆盖这些路径。
- root live 检查依赖本机 `root@127.0.0.1` SSH 可用，并依赖主机能创建和配置 TUN 设备。
