# `UringDevice` 阶段总结（M0-M8 第七阶段）

本文档记录截至 2026-04-25 当前仓库已经落地的能力边界，目的是让后续开发可以直接沿着现状继续推进，而不是再根据更早的骨架状态重新判断。

对应文档：

- 设计文档：[URING_DEVICE_DESIGN.md](./URING_DEVICE_DESIGN.md)
- 实施计划：[URING_DEVICE_IMPLEMENTATION_PLAN.md](./URING_DEVICE_IMPLEMENTATION_PLAN.md)

## 1. 当前快照

当前已完成：

- M0. 项目骨架与 feature 矩阵
- M1. 共享基础设施
- M2. 单包发送路径
- M3. RX 生命周期与 driver 骨架
- M4. RX 数据面主路径
- M5. RX 回收、故障与恢复
- M5 加固：RX exhaustion/recovery 压力验证
- M6 第一阶段：TX `send_many()` 基础路径
- M7 跑通阶段：TX `send_many()` timeout、cancel 与 drop-cleanup
- M7 加固：取消路径压力验证
- M8 第一阶段：`keep_order` 链式 SQE 提交
- M8 第二阶段：`keep_order` 链断裂/部分失败行为验证
- M8 第三阶段：TX mixed continuous stress
- M8 第四阶段：发布前收尾（README / examples / perf smoke / package preflight）
- M8 第五阶段：`Packet::split_into()` API 补齐
- M8 第六阶段：RX lazy `offload_info()` 接回
- M8 第七阶段：offload-enabled 单包 TX 兼容与 `dead_code` cfg 收口

当前 crate 形态：

- 平台限制为 Linux only，并在编译期做了保护
- 对外只暴露单一 `UringDevice`
- backend 通过 feature 选择：
  - `async` -> `async_tokio`
  - `async_tokio`
  - `async_io`
- 同一构建中启用 `async_tokio` 与 `async_io` 会直接编译失败
- multiqueue 不在当前实现范围内，也不是初始化或 backend glue 的前提

当前公开 API：

- `UringDevice::new(device, config)`
- `UringDevice::backend_name()`
- `UringDevice::rx_state()`
- `UringDevice::ready_len()`
- `UringDevice::start_rx()`
- `UringDevice::stop_rx().await`
- `UringDevice::readable().await`
- `UringDevice::try_recv()`
- `UringDevice::recv().await`
- `UringDevice::recv_many().await`
- `UringDevice::try_send()`
- `UringDevice::writable().await`
- `UringDevice::send().await`
- `UringDevice::send_many().await`

## 2. 各阶段完成情况

### M0. 项目骨架与 feature 矩阵

已落地内容：

- 建立了 facade/core/backend 的基本目录结构
- `src/lib.rs` 中实现了 Linux-only 编译保护
- feature 选择与 backend 入口已经固定
- `backend/mod.rs` 统一了 facade 到具体 backend 的转发
- 无 backend feature 时提供 `no_backend` 占位实现
- 多 backend 同开时使用 `compile_error!` 直接失败

### M1. 共享基础设施

已落地内容：

- `UringDeviceConfig`
  - 实现了 `Default`
  - 实现了链式 builder helper
  - 实现了 `ValidatedConfig` 校验入口
  - 已补充 RX provided buffer ring 需要的约束：
    - `rx_buffer_count` 必须是 2 的幂
    - `rx_buffer_count <= 32768`
- 公共错误 helper
- `RxStartMode`
- `RxState`
- `Packet` 基础类型
  - 已有 `as_bytes()/len()/is_empty()/detach()/offload_info()/is_gso()/split_into()`
  - 已具备 ring-backed 与 detached/owned 两种内部状态
- `RxWaiterSlot`
  - 单 waiter 槽位语义已实现
- `TxController`
  - 共享 TX batch `io_uring` 已实现
  - 批次独占槽位已实现，支持 waiter 注册与 release 唤醒

### M2. 单包发送路径

已落地内容：

- `try_send()` 走 `SyncDevice::send()` 的 nonblocking 写路径
- `CoreDevice::new()` 会把底层设备切到 nonblocking 模式
- `writable()` 与 `send()` 已在两个 backend 上打通

关键修正：

- backend 构造 async wrapper 时，不再依赖 `SyncDevice::try_clone()`
- 当前实现改为对同一个底层 TUN fd 做普通 `dup`
- 原因是 `tun-rs` 在 Linux 上的 `try_clone()` 依赖 `IFF_MULTI_QUEUE`，而 multiqueue 不在本期范围

### M3. RX 生命周期与 driver 骨架

已落地内容：

- `start_rx()` / `stop_rx()` / `rx_state()`
- `RxController`
  - 统一维护 `Running / Stopped / Faulted` 状态
  - `start_rx()` 幂等
  - `stop_rx()` 幂等
- RX `io_uring` 初始化
- `IORING_FEAT_FAST_POLL` 检查
- CQ 通知 `eventfd` 注册
- runtime-agnostic 的后台 RX driver 线程

### M4. RX 数据面主路径

已落地内容：

- RX `provided buffer ring` 注册
- `read_multishot` 提交
- CQE 消费
- 内部 `VecDeque<Packet>`
- `ready_len()`
- `readable()`
- `try_recv()`
- `recv()`
- `recv_many()`

当前对外语义：

- `ready_len()` 反映当前内部队列中可立即被 `try_recv()` 消费的精确数量
- `readable()` 成功返回后，下一次 `try_recv()` 成功
- `recv()` 使用标准 `try_recv() + readable()` 等待策略
- `recv_many()` 一旦至少有一个包，不会为了攒批额外等待
- 接收侧继续维持单 waiter 槽位，不引入等待队列

### M5. RX 回收、故障与恢复

已落地内容：

- CQE 处理路径直接生成 ring-backed `Packet`
- `Packet::Drop()` 会通过共享回收句柄把 slot 写回 provided buffer ring
- `Packet::detach()` 会先复制到 owned storage，再提前归还 slot
- RX ring 关闭时会先停用回收路径，再执行 `unregister_buf_ring`
- driver 会把 `-ENOBUFS` 特判为 `Faulted(ENOBUFS)`，而不是落入一般 I/O 错误路径
- `rx_auto_resume_after_recycled_slots` 已接入真实 `Packet` recycle 事件
- 当自动恢复阈值大于 `0` 时，回收计数达到阈值会唤醒 driver 尝试重提交通道
- 当自动恢复阈值为 `0` 时，回收仍会归还 slot，但不会自动重提交通道
- 手动 `start_rx()` 与自动恢复共享同一条重提交通道路径，成功后会清零恢复计数

已验证能力：

- `rx_auto_resume_after_recycled_slots == 0`
  - RX 在 `ENOBUFS` 后保持 `Faulted(ENOBUFS)`
  - 归还 slot 后不会自动恢复
  - 显式 `start_rx()` 后可恢复收包
- `rx_auto_resume_after_recycled_slots > 0`
  - RX 在 `ENOBUFS` 后进入 `Faulted(ENOBUFS)`
  - 回收计数达到阈值后会自动恢复到 `Running`
  - 恢复后可继续收包

### M6 第一阶段：TX `send_many()` 基础路径

已落地内容：

- public `UringDevice::send_many(...)`
- backend facade 到 core 的转发
- `bytes::Bytes` owned 输入
- 共享 TX batch `io_uring`
- TX CQ `eventfd`
- 单批次独占控制，避免多个 `send_many()` 同时占用同一个 TX ring
- 每个 buffer 对应一个 one-shot write SQE
- 通过 `user_data` 维护 completion 到输入下标的映射
- `results[i]` 与返回的 `bufs[i]` 稳定对应
- 空输入直接返回
- `results.len() < bufs.len()` 时不提交 SQE，按 `InvalidInput` 标记可写入的结果前缀并返还原始输入
- `keep_order == false` 时按 `tx_submit_chunk_size` 分块提交
- `keep_order == true` 时按 `tx_submit_chunk_size` 构造 `IOSQE_IO_LINK` 链式 chunk 提交

当前边界：

- 已具备可工作的 TX batch happy path
- timeout/cancel 控制路径已跑通，并具备 live followup 验证
- future drop 后的内部 cancel/drain cleanup 已跑通，并具备 live followup 验证
- `keep_order == true` 当前已改为保守的链式 `IOSQE_IO_LINK` chunk 提交；单条链收敛后再提交下一条链

### M7 跑通阶段：TX `send_many()` timeout、cancel 与 drop-cleanup

已落地内容：

- `timeout` 会被转换为 absolute deadline，覆盖等待 TX 批次槽位与批次执行
- deadline 到达后，未提交项会被标记为 `TimedOut`
- 已提交且仍 pending 的 write 会提交 async cancel，并继续 drain 到终态后再释放批次槽位
- future drop 通过 `DropGuard` 设置 `cancel_requested` 并写 `eventfd` 唤醒 TX driver
- cleanup 完成前批次槽位保持 `Running/Cancelling`，下一批 `send_many()` 不会提前开始
- TX 批次槽位等待的 check/register 已合并到同一把锁下，避免无超时等待漏唤醒
- TX 批次槽位等待的 deadline 唤醒已由 backend 注入 runtime timer，`async_io` 使用 `async_io::Timer::after`，`async_tokio` 使用 `tokio::time::sleep`

已验证能力：

- `send_many()` future 被 drop 后，后续 followup batch 可以继续发送并由 UDP socket 收到
- `send_many()` timeout 后，结果中可观察到 `TimedOut`，后续 followup batch 可以继续发送并由 UDP socket 收到
- `send_many()` 在等待 TX 批次槽位期间超时后，会直接返还整批 owned buffer、将结果标为 `TimedOut`，且不会误提交该批次
- 同一设备上 4 轮 `drop cleanup -> timeout -> busy-slot timeout` 连续执行后，TX 批次槽位仍可持续恢复并接受 followup batch
- 上述 stress 每轮后插入一次正常双包 `send_many()` 发送，验证 cleanup 路径不会影响后续普通批量发送
- 同一设备上 8 轮 mixed cleanup long stress 连续执行后，TX 批次槽位仍可持续恢复
- stress 中插入的普通双包 `send_many()` 会在奇偶轮切换 `keep_order`，当前顺序路径的成功场景也已纳入覆盖
- 上述场景已分别覆盖 `async_io` 与 `async_tokio`

当前边界：

- 获取 TX 批次槽位时的 deadline 唤醒已改为 backend 注入的 runtime timer，不再为等待超时额外创建短生命周期线程
- timeout/drop cleanup 已有 live smoke 覆盖，但还需要更高强度压力测试
- `keep_order == true` 已使用链式 `IOSQE_IO_LINK` chunk 提交，并已补充针对链断裂/部分失败的 core + live 行为测试

## 3. 当前代码结构与关键入口

建议下阶段优先阅读：

- [src/lib.rs](/home/xx/tun-rs-uring/src/lib.rs)
- [src/backend/mod.rs](/home/xx/tun-rs-uring/src/backend/mod.rs)
- [src/backend/async_tokio.rs](/home/xx/tun-rs-uring/src/backend/async_tokio.rs)
- [src/backend/async_io.rs](/home/xx/tun-rs-uring/src/backend/async_io.rs)
- [src/core/mod.rs](/home/xx/tun-rs-uring/src/core/mod.rs)
- [src/core/config.rs](/home/xx/tun-rs-uring/src/core/config.rs)
- [src/core/rx.rs](/home/xx/tun-rs-uring/src/core/rx.rs)
- [src/core/packet.rs](/home/xx/tun-rs-uring/src/core/packet.rs)
- [src/core/tx.rs](/home/xx/tun-rs-uring/src/core/tx.rs)

## 4. 已验证结果

2026-04-25 在当前仓库状态下执行并通过：

```bash
cargo check --no-default-features
cargo check --no-default-features --features async_tokio
cargo check --no-default-features --features async_io

cargo test --no-default-features
cargo test --no-default-features --features async_tokio
cargo test --no-default-features --features async_io
```

另已验证：

```bash
cargo check --no-default-features --features async_tokio,async_io
```

该命令按预期失败，并触发互斥 feature 的 `compile_error!`。

发布前收尾阶段另已验证：

```bash
cargo check --examples --no-default-features --features async_tokio
cargo check --examples --no-default-features --features async_io
cargo package --allow-dirty
```

结果为：

- 两个 backend 下的 examples compile smoke 均通过
- `cargo package --allow-dirty` 已成功完成打包与 verify，当前包内容可正常构建
- 当前环境直接运行 live examples 时，会因缺少 TUN 权限打印明确 skip 提示，而不是裸错误退出

验证结果：

- `--no-default-features` 为 `49 passed`
- `--features async_tokio` 为 `66 passed`
- `--features async_io` 为 `66 passed`

基于真实单队列 TUN 的 public live 测试包括：

- `public_rx_api_receives_ipv4_packet_on_single_queue_tun_async_tokio`
- `public_rx_api_receives_ipv4_packet_on_single_queue_tun_async_io`
- `public_rx_faults_with_enobufs_until_manual_restart_async_tokio`
- `public_rx_faults_with_enobufs_until_manual_restart_async_io`
- `public_rx_auto_resumes_after_recycled_slots_async_tokio`
- `public_rx_auto_resumes_after_recycled_slots_async_io`
- `public_rx_recv_many_limits_and_drains_ready_packets_async_tokio`
- `public_rx_recv_many_limits_and_drains_ready_packets_async_io`
- `public_rx_stop_prevents_new_completions_until_restart_async_tokio`
- `public_rx_stop_prevents_new_completions_until_restart_async_io`
- `public_rx_manual_restart_stress_async_tokio`
- `public_rx_manual_restart_stress_async_io`
- `public_rx_auto_resume_stress_async_tokio`
- `public_rx_auto_resume_stress_async_io`
- `public_rx_offload_packet_exposes_lazy_offload_info_async_tokio`
- `public_rx_offload_packet_exposes_lazy_offload_info_async_io`
- `public_send_many_delivers_ipv4_udp_packet_async_tokio`
- `public_send_many_delivers_ipv4_udp_packet_async_io`
- `public_send_many_keep_order_delivers_ipv4_udp_packet_async_tokio`
- `public_send_many_keep_order_delivers_ipv4_udp_packet_async_io`
- `public_send_many_keep_order_chain_breaks_after_invalid_packet_async_tokio`
- `public_send_many_keep_order_chain_breaks_after_invalid_packet_async_io`
- `public_send_many_drop_cleanup_allows_followup_batch_async_tokio`
- `public_send_many_drop_cleanup_allows_followup_batch_async_io`
- `public_send_many_timeout_allows_followup_batch_async_tokio`
- `public_send_many_timeout_allows_followup_batch_async_io`
- `public_send_many_waiting_for_busy_slot_times_out_async_tokio`
- `public_send_many_waiting_for_busy_slot_times_out_async_io`
- `public_send_many_cleanup_stress_async_tokio`
- `public_send_many_cleanup_stress_async_io`
- `public_send_many_cleanup_long_stress_async_tokio`
- `public_send_many_cleanup_long_stress_async_io`
- `public_send_many_mixed_stress_async_tokio`
- `public_send_many_mixed_stress_async_io`

这些用例覆盖了：

- 单队列 TUN 上 `UringDevice::new()` 可成功初始化
- `RxStartMode::ManualStart -> start_rx()` 的 public 生命周期
- `readable()` 成功返回后 `try_recv()` 可立即取包
- `try_recv()` 返回的首个 `Packet` 仍是 ring-backed 状态，而不是提前 detached
- `recv_many()` 会按 `out.len()` 限制单次提取数量，并在后续调用中继续 drain 当前 ready 队列
- `stop_rx()` 返回后，在重新 `start_rx()` 之前不会再产生新的用户态 RX completion
- 通过持有 packet 不释放 slot 的方式稳定构造 `ENOBUFS`
- `threshold == 0` 时只能手动恢复
- `threshold > 0` 时会自动恢复
- 同一设备上连续 4 轮 `ENOBUFS -> 手动恢复 -> stop/start` 后，RX 仍可持续恢复
- 同一设备上连续 4 轮 `ENOBUFS -> 自动恢复 -> stop/start` 后，RX 仍可持续恢复
- offload-enabled RX 包的 `Packet::as_bytes()` 不包含 virtio header，`offload_info()` 在首次使用时可懒解析出 metadata
- `send_many()` 可通过共享 TX ring 把 IPv4/UDP 包注入 TUN，并由内核 UDP socket 收到
- `send_many()` 会返还原始 owned buffer，并在 `results` 中写入逐包发送长度
- `send_many()` future drop 后内部 cleanup 会释放批次槽位，下一批发送可继续
- `send_many()` timeout 后内部 cleanup 会释放批次槽位，下一批发送可继续
- `send_many()` 在等待 TX 批次槽位时若超时，会在不提交任何 SQE 的前提下返还 owned buffer，并把结果写为 `TimedOut`
- 同一设备上连续 4 轮 `drop cleanup / timeout / busy-slot timeout` 后，后续发送仍可继续推进
- cleanup 压力场景中的每轮普通双包发送仍可成功到达 UDP socket
- 同一设备上连续 8 轮 mixed cleanup long stress 后，后续发送仍可继续推进
- 同一设备上连续 6 轮 mixed TX stress 后，unordered/ordered 成功发送、ordered chain-break、drop cleanup、timeout、busy-slot timeout 仍可连续交错收敛
- cleanup 压力场景中的普通双包发送在 `keep_order == false/true` 两种模式下都可成功到达 UDP socket
- `keep_order == true` 的显式双包 `send_many()` live 用例已成功覆盖链式顺序发送
- `keep_order == true` 的 `[有效包, 非法包, 有效包]` live 用例已验证：中间非法项报错、同链尾项收到 `ECANCELED`、尾包不会误送到 UDP socket
- 不依赖 multiqueue

另有 `RxAutoResume` 单元测试覆盖：

- 未进入 `ENOBUFS` fault 前 recycle 事件不会误触发自动恢复
- 回收计数达到阈值时只会入队一次自动恢复唤醒
- `disarm()/re-arm()` 会重置阈值计数

另有 `Packet` 单元测试覆盖：

- virtio header 上的 `OffloadInfo` 会按 lazy parse 方式接回 `Packet`
- `split_into(...)` 对无 offload_info 包会返回错误
- `split_into(...)` 对非 GSO 包会返回单段，并在 `needs_csum == true` 时补完 checksum
- `split_into(...)` 可把 IPv4/UDP GSO 包按 `gso_size` 拆成多个输出段
- `split_into(...)` 不消费 `Packet`，且 `detach()` 前后拆段结果保持一致

另有 TX 单元测试覆盖：

- TX batch 槽位 `Idle -> Running -> Cancelling -> Idle`
- release 会唤醒等待者
- TX batch 槽位等待的 phase check 与 waiter register 在同一把锁下完成
- TX batch 槽位忙且 deadline 未到时，会使用注入的 timer future 触发超时返回
- TX `user_data` 编码可区分 write/cancel 并保留下标
- poll timeout 会对亚毫秒 duration 向上取整
- `ECANCELED` 只会在 timeout/drop cleanup 语义下映射为 `TimedOut`；链断裂导致的同链尾项仍保留原始 `ECANCELED`
- 保守的 ordered chunk 策略下，当前链失败不会提前标记后续未提交 chunk；driver 会在前链收敛后再决定是否继续推进

## 5. 当前真实边界

当前仓库已达到第一版可发布状态，但下面这些点仍属于明确的能力边界或后续可增强项：

- multiqueue 仍不在当前实现范围内
- `keep_order == true` 仍采用保守的“单条链收敛后再提交下一条链”策略，而不是一次铺满整批
- offload-enabled 设备上的单包 TX 已支持兼容模式：`try_send()` / `send()` 会自动补一个默认零值的 virtio header，再发送原始 IP 包
- 已通过两个 backend 的 live 测试验证：offload-enabled 设备上的 `try_send()` / `send()` 可成功把普通 IPv4/UDP 包送到内核 UDP socket
- `send_many()` 在 offload-enabled 设备上仍保持 `Unsupported`，本轮未扩展到批量 TX offload 兼容
- 已把宽泛的 `#[allow(dead_code)]` 收敛到 backend/test 对应的 `cfg_attr`，并把 examples 公共辅助改成更窄的 item-level 标注
- 当前 `perf_smoke` 是发布前基础 smoke，不是正式 benchmark
- live examples 在无 TUN 权限的环境下会打印 skip 提示并退出；真实 I/O 路径仍需要在具备建链路权限的 Linux 环境中运行

## 6. 后续建议

后续如果继续推进，建议按下面顺序演进：

1. 补更正式的 benchmark
   - 分离 unordered / ordered 模式的基线对比
   - 记录不同 `tx_submit_chunk_size` 的影响
2. 补更长时长 soak 压测
   - 放大 RX exhaustion/recovery 与 TX mixed stress 的轮次
   - 观察更长时间运行下的状态机收敛

特别注意：

- 当前 RX wait 仍然只有 `RxWaiterSlot` 单槽；如果继续沿用设计文档，就不要扩成等待队列
- backend 的职责应继续限制在 readiness/glue code，不要把 RX/TX 正确性拆散到两个 backend 中分别维护
- 后续如果继续演进 RX/TX 数据面，不要重新引入对 `SyncDevice::try_clone()` / multiqueue 的依赖
