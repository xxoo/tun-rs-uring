# `UringDevice` 总体实施规划

本文档是 [URING_DEVICE_DESIGN.md](./URING_DEVICE_DESIGN.md) 的实施计划版本，目标是把设计拆成可以逐步编码、逐步验证、逐步收敛风险的执行路线。

当前已完成阶段与接续说明见 [URING_DEVICE_PROGRESS.md](./URING_DEVICE_PROGRESS.md)。

## 1. 规划目标

- 把 `UringDevice` 的实现拆成若干个可以独立落地和独立验证的里程碑
- 优先保证语义正确性、资源回收正确性和取消语义正确性，再追求性能优化
- 在实现路径上保持与 `tun-rs` 一致的 async 分层风格：
  - 对外只暴露单一 `UringDevice`
  - 内部按 feature 选择 `async_tokio` 或 `async_io` backend
  - 尽量把复杂逻辑沉淀在 runtime-agnostic 的共享 `core`

## 2. 总体原则

### 2.1 分层原则

- `core`
  - 负责 runtime-agnostic 的共享状态机与资源管理
  - 包括 RX/TX `io_uring` 上下文、provided buffer ring、`Packet`、`RxState`、`send_many()` 批次状态、取消/回收语义、内部队列与 waiter 状态
- `backend`
  - 负责 runtime-specific 的薄适配层
  - 包括 `writable()/send()` 的 readiness 适配
  - 包括后台 RX driver 的承载方式
  - 包括 facade 到具体实现的转发

### 2.2 交付顺序原则

每个能力里程碑按以下顺序完成：

1. 先实现 `core` 语义和最小可测试单元
2. 再接入 `async_tokio` backend
3. 再补齐 `async_io` backend
4. 最后补共享测试、压力测试、示例和文档

说明：

- 如果某个阶段为了降低复杂度，先用一个 backend 打通纵向切片，可以接受
- 但进入下一个能力阶段前，前一阶段的两个 backend 都必须至少编译通过，并完成最基本的语义验证
- 不允许因为先实现了某个 backend，就把共享语义写死到该 runtime 的 reactor 或专有类型上

### 2.3 完成判定原则

每个里程碑都必须同时满足以下条件才算完成：

- 代码层面：
  - 对应功能在共享 `core` 与 backend 边界上已经稳定
- 语义层面：
  - 与设计文档中该阶段涉及的 API 语义一致
- 验证层面：
  - 该阶段规定的单元测试、集成测试或压力测试已经具备并通过
- 文档层面：
  - 若该阶段调整了实现边界、限制条件或调用约束，应同步更新设计文档或实施规划

## 3. 预备工作

开始编码前先完成以下准备：

- 明确 crate feature 方案：
  - `async`
  - `async_tokio`
  - `async_io`
- 明确 feature 互斥规则：
  - 同一构建最多启用一个 backend
  - 同时启用多个 backend 时编译失败
- 明确测试环境要求：
  - Linux
  - 推荐内核版本不低于设计文档要求
  - 可创建并驱动 TUN 设备
- 明确验证分层：
  - 纯逻辑单元测试
  - 真实 TUN + `io_uring` 集成测试
  - 取消/恢复压力测试
  - 性能 smoke test

## 4. 里程碑规划

### M0. 项目骨架与 feature 矩阵

目标：

- 建立公开 facade、共享 `core`、runtime backend 的基本目录结构
- 锁定 feature 选择和编译期约束

产物：

- `UringDevice::new(SyncDevice, UringDeviceConfig)` 公开构造入口骨架
- `UringDevice` facade 类型
- `core` 模块骨架
- `async_tokio` backend 骨架
- `async_io` backend 骨架
- 多 backend 同开时报错的编译期约束

验证：

- 无 backend feature 时的行为符合预期
- 单独启用 `async_tokio` 能编译
- 单独启用 `async_io` 能编译
- 同时启用 `async_tokio` 与 `async_io` 会编译失败

退出条件：

- 对外 API 路径已经固定，不再需要为后续阶段重做模块边界

### M1. 共享基础设施

目标：

- 先完成与 runtime 无关、且后续所有阶段都会复用的基础设施

产物：

- `UringDeviceConfig` 校验
- `UringDeviceConfig::default()` 与链式设定 helper
- `UringDevice::new(...)` 的共享校验入口
- 公共错误 helper
- `RxState`
- `Packet` 基础结构外壳
- RX 单 waiter 槽位抽象
- TX 批次占用状态外壳
- 公共测试辅助设施

验证：

- 配置非法输入有单元测试
- 状态初值与状态转移 helper 有单元测试
- 基础资源对象 drop 行为可测试

退出条件：

- 后续阶段不需要再为配置、错误、状态抽象大范围返工

### M2. 单包发送路径

目标：

- 先打通最简单、最便宜的 async 发送纵向切片
- 保持“单包发送不强制使用 `io_uring`”这一设计前提

产物：

- `try_send()`
- `writable()`
- `send()`
- backend-specific readiness 适配

实现要求：

- `try_send()` 走 nonblocking fd 写
- `writable()` 与 `send()` 通过 facade 委托到当前 backend
- `writable()` 必须支持多 waiter
- 不得把这一阶段写死到某个 runtime 的专有类型上

验证：

- `WouldBlock -> writable -> send` 主路径可运行
- 多任务并发等待 `writable()` 语义正确
- `send()` 在两个 backend 上的对外语义一致

退出条件：

- 单包 TX 语义稳定，不需要依赖 RX ring 或 TX batch ring 才能工作

### M3. RX 生命周期与 driver 骨架

目标：

- 先搭好 RX ring、driver 生命周期与状态机，再进入数据面

产物：

- RX `io_uring` 初始化
- `IORING_FEAT_FAST_POLL` 检查
- RX CQ 通知源
- 后台 RX driver 承载与启动逻辑
- `start_rx()`
- `stop_rx()`

实现要求：

- `stop_rx()` 是唯一公开的 RX 请求取消入口
- `stop_rx()` 返回后不再注入新的 RX completion
- `start_rx()` / `stop_rx()` 必须满足幂等语义
- backend 只负责承载 driver，不负责重写 RX 状态机语义

验证：

- `AutoStart` / `ManualStart` 行为正确
- `start_rx()` 幂等
- `stop_rx()` 幂等
- `stop_rx()` 返回后不再注入新的 RX completion
- `Running -> Stopped -> Running` 主路径可用

退出条件：

- 不依赖完整数据面，也能证明 RX 生命周期控制模型成立

### M4. RX 数据面主路径

目标：

- 完成 multishot read + provided buffer ring + 内部队列 的主接收路径

产物：

- provided buffer ring 注册
- multishot read 提交与 CQ 消费
- 内部 `VecDeque<Packet>`
- `ready_len()`
- `readable()`
- `try_recv()`
- `recv()`
- `recv_many()`

实现要求：

- driver 先更新内部队列或 `RxState`，再 wake waiter
- `readable()/recv()/recv_many()` 只取消等待本身，不取消内核请求
- 接收侧继续维持单 waiter 槽位，不引入等待队列
- backend/driver 若需要额外持有同一个底层 TUN fd，不得依赖 `SyncDevice::try_clone()`；应使用普通 fd duplication，避免把实现错误地绑定到 multiqueue

分阶段落地说明：

- M4 第一阶段允许 driver 在消费 CQE 后先把数据复制到 owned `Packet` 并立即归还 provided buffer slot
- 这样可以先稳定 `ready_len()/readable()/try_recv()/recv()/recv_many()` 的 public 语义与跨 backend 一致性
- 该过渡阶段已经完成，当前代码已进入 M5 的 slot 生命周期收敛阶段

验证：

- `ready_len()` 与 `try_recv()` 行为一致
- `readable()` 成功返回后，下一次 `try_recv()` 成功
- `recv_many()` 一旦有至少一个包，不额外等待攒批
- `recv_many()` 会按 `out.len()` 限制单次提取数量，并在后续调用中继续 drain 当前 ready 队列
- 两个 backend 的对外接收语义一致
- 至少补一条基于真实单队列 TUN 的 public API 集成测试，覆盖 `ManualStart -> start_rx() -> readable() -> try_recv()/recv()`
- 至少补一条基于真实单队列 TUN 的 public API 集成测试，覆盖 `recv_many()` 和 `stop_rx()` 的对外行为边界

退出条件：

- RX happy path 可稳定接收和消费数据

### M5. RX 回收、故障与恢复

目标：

- 完成最容易出错的 RX slot 生命周期、自动恢复与故障路径

产物：

- ring-backed `Packet`
- `Packet::Drop()` 回收
- `Packet::detach()` 提前回收
- 回收幂等保证
- `ENOBUFS -> Faulted(ENOBUFS)` 语义
- `rx_auto_resume_after_recycled_slots`

实现要求：

- 若 M4 第一阶段已经采用 copy-to-owned `Packet`，M5 需要把实现推进到设计目标，而不是长期停留在复制型过渡方案
- slot 回收必须恰好发生一次
- 自动恢复与手动恢复共享同一状态机，不得分叉出两套逻辑
- `Faulted(ENOBUFS)` 不应被误当成不可恢复永久故障

分阶段落地说明：

- M5 第一阶段：
  - 把 CQE 直接封装为 ring-backed `Packet`
  - `Packet::Drop()` 回收 slot
  - `Packet::detach()` 复制到 owned storage 后提前回收 slot
  - 回收路径在 RX ring 已停用时必须静默失效，不能在已注销的 buf ring 上继续写回
- M5 第二阶段：
  - 补齐 `ENOBUFS -> Faulted(ENOBUFS)`
  - 实现 `rx_auto_resume_after_recycled_slots`
  - 完成恢复相关 live/压力测试

当前状态（截至 2026-04-25）：

- `ENOBUFS -> Faulted(ENOBUFS)` 已接入 driver 主路径
- recycled slot 驱动的自动恢复阈值已经接入 `Packet` recycle 路径
- 基于真实单队列 TUN 的 live exhaustion/recovery 用例已经补齐，覆盖：
  - `rx_auto_resume_after_recycled_slots == 0` 的手动恢复
  - `rx_auto_resume_after_recycled_slots > 0` 的自动恢复
- 已通过同一设备上的 4 轮 live stress 验证：
  - `ENOBUFS -> 手动恢复 -> stop/start`
  - `ENOBUFS -> 自动恢复 -> stop/start`
- M5 的主要退出条件已经满足；后续若继续加强这一阶段，重点应放在压力测试而不是基础控制语义

验证：

- `Drop()` 与 `detach()` 都只回收一次
- 可复现并验证 `ENOBUFS` 路径
- `rx_auto_resume_after_recycled_slots == 0` 与 `> 0` 两种模式都符合设计

退出条件：

- RX 不仅能“收”，还能在资源耗尽和回收场景下稳定收敛

### M6. `send_many()` 无序 happy path

目标：

- 先实现最小可用的批量发送，不把 timeout/cancel 与顺序控制一次混进来

产物：

- 共享 TX batch `io_uring`
- `tx_batch` 独占槽位
- `send_many()` 基础流程
- `user_data -> 输入索引` 映射
- 结果写回与 owned buffer 返回

实现要求：

- 先以 `keep_order == false` 为主路径
- `results.len()` 非法时不得提交 SQE
- 同一设备上最多一个批次占用 TX ring
- backend 主要负责 future 承载，不应复制 batch 状态机

验证：

- `results[i]` 与返回的 `bufs[i]` 稳定对应输入索引
- 非法 `results` 长度时不提交请求
- 两个并发 `send_many()` 调用会按 `Idle -> Running -> Idle` 串行化

退出条件：

- 批量发送主路径可用，索引映射和资源占用模型成立

当前状态（截至 2026-04-23）：

- public `send_many()` 已落地
- 共享 TX batch `io_uring` 已接入 core
- owned 输入使用 `bytes::Bytes`
- 已通过真实单队列 TUN live 测试验证 `send_many()` 可注入 IPv4/UDP 包并由内核 socket 收到
- `keep_order == true` 当前已进入 M8，并改为链式 `IOSQE_IO_LINK` chunk 提交保证顺序
- timeout/cancel/drop-cleanup 已进入 M7 跑通阶段

### M7. `send_many()` timeout、cancel 与 drop-cleanup

目标：

- 完成设计中最复杂、也是风险最高的批量发送收敛逻辑

产物：

- deadline 处理
- `cancel_requested`
- 取消未完成项
- drain 到终态 completion
- `Running/Cancelling/Idle` 批次阶段收敛

实现要求：

- `timeout` 是正式取消路径
- future drop 不得提前释放 `tx_batch`
- 已提交请求在终态收齐前不得返回
- cleanup 完成前新的 `send_many()` 不得开始

验证：

- deadline 到达前未提交任何请求时，可直接返还整批 `bufs`
- deadline 到达后若已有提交，必须 cancel + drain 后再返回
- future drop 后内部会进入 `Cancelling`，并在收敛后回到 `Idle`
- 下一批 `send_many()` 会被正确阻塞到 cleanup 完成之后

退出条件：

- `send_many()` 的资源安全、取消安全和返回路径安全都得到验证

当前状态（截至 2026-04-23）：

- `timeout` 已统一换算为 absolute deadline，覆盖等待 TX 批次槽位与批次执行
- deadline 到达后会标记未提交项为 `TimedOut`，对 pending write 提交 async cancel，并等待已提交项进入终态后再返回
- future drop 通过 `DropGuard` 设置 `cancel_requested` 并写 `eventfd` 唤醒 TX driver，避免提前释放 owned buffers
- cleanup 完成前 TX 批次槽位保持非 `Idle`，下一批 `send_many()` 会等到 release 后再开始
- `keep_order == true` 已改为链式 `IOSQE_IO_LINK` chunk 提交，并继续保留 timeout/drop cleanup 语义
- 已修正 TX 批次槽位等待的 check/register 竞态，避免无超时等待漏唤醒
- 已通过两个 backend 的 live 测试验证：drop future 后 followup batch 可发送，timeout 后 followup batch 可发送
- 已通过两个 backend 的 live 测试验证：等待 TX 批次槽位超时会直接返还整批 owned buffer、将结果标为 `TimedOut`，且不会误提交该批次
- 已通过两个 backend 的 live stress 测试验证：同一设备上 4 轮 `drop cleanup -> timeout -> busy-slot timeout` 连续执行后，TX 批次槽位仍能持续恢复并接受 followup batch
- 上述 stress 每轮后还会插入一次正常双包 `send_many()` 发送，验证 cleanup 路径不会影响后续普通批量发送
- 已通过两个 backend 的更长 live stress 测试验证：同一设备上 8 轮 mixed cleanup 连续执行后，TX 批次槽位仍能持续恢复
- stress 中插入的普通双包 `send_many()` 会在奇偶轮切换 `keep_order`，当前顺序路径的成功场景也已纳入覆盖
- 获取批次槽位时的 deadline 唤醒已改为 backend 注入的 runtime timer：`async_io::Timer::after` 或 `tokio::time::sleep`

### M8. `keep_order` 与收尾加固

目标：

- 在主路径稳定后，再补链式提交和更高层面的质量保障

产物：

- `keep_order == true` 的链式提交实现
- 行为差异说明
- 压力测试
- 示例代码
- 性能 smoke test

实现要求：

- 明确记录 `keep_order` 的并行度与尾延迟代价
- 不把 `keep_order` 语义扩展成全局 TX 顺序保证
- 文档与测试都必须覆盖链断裂和部分失败行为

验证：

- 链式模式保持输入顺序
- 前序失败会按预期影响后续请求
- 无序模式与有序模式的行为边界清晰

退出条件：

- 第一版功能、语义与文档达到可发布状态

当前状态（截至 2026-04-25）：

- `keep_order == true` 已改为链式 `IOSQE_IO_LINK` chunk 提交，而不是逐项 `submit()`
- 当前实现仍保持“单条链收敛后再提交下一条链”的保守策略，避免把 timeout/cancel 风险一次放大到整批
- 已通过两个 backend 的显式 live 测试验证：`keep_order == true` 的双包 `send_many()` 可成功注入并由 UDP socket 收到
- 已通过 core 单元测试验证：链断裂导致的同链尾项会保留 `ECANCELED`，而 timeout 驱动的 `ECANCELED` 仍统一映射为 `TimedOut`
- 已通过两个 backend 的显式 live 测试验证：`[有效包, 非法包, 有效包]` 的 `keep_order` 批次中，中间非法项会报错、链尾项会收到 `ECANCELED`，且不会误送到 UDP socket
- cleanup stress 中插入的普通双包 `send_many()` 已在奇偶轮切换 `keep_order`，顺序路径成功场景已纳入持续压力覆盖
- 已通过两个 backend 的同设备 6 轮 mixed live stress 验证：unordered/ordered 成功发送、ordered chain-break、drop cleanup、timeout、busy-slot timeout 可连续交错收敛
- 已补齐 `Packet::split_into(...)`，非 GSO 包按单段复制返回，GSO 包按 `tun-rs handle_virtio_read / gso_split` 语义拆段
- 已通过 core 单元测试验证：`split_into(...)` 的普通包 checksum 补全、IPv4/UDP GSO 拆段、以及 `detach()` 前后一致性
- 已把 virtio offload 元信息接回 RX 路径，但采用 lazy parse：`Packet` 初始化时只记录 payload offset，首次调用 `offload_info()` 时才解析 header
- 已通过两个 backend 的 live RX 测试验证：offload-enabled 设备上收到的普通 IPv4/UDP 包可正常返回 `Packet`，`as_bytes()` 不含 virtio header，`offload_info()` 可按需取到 metadata
- 已补齐 offload-enabled 设备上的单包 TX 兼容路径：`try_send()` / `send()` 会自动补一个默认零值的 virtio header，再发送原始 IP 包
- 已通过两个 backend 的显式 live 测试验证：offload-enabled 设备上的 `try_send()` / `send()` 可成功把普通 IPv4/UDP 包送到内核 UDP socket
- 当前 `send_many()` 仍未支持 offload-enabled TX；批量发送在该模式下仍返回 `Unsupported`
- 已补齐发布前收尾资产：
  - `README.md`
  - `examples/recv_manual_start.rs`
  - `examples/send_many.rs`
  - `examples/perf_smoke.rs`
  - 两个 backend 下的 example compile smoke
- 当前环境运行 live examples 时会因 TUN 权限不足打印明确 skip 提示，而不是裸错误退出
- M8 的发布前基础收尾已完成；后续若继续加强，重点应转向更正式 benchmark 或更长时长 soak

## 5. 验证矩阵

### 5.1 编译矩阵

- `async_tokio`
- `async_io`
- `async_tokio + async_io` 编译失败

### 5.2 测试矩阵

- 纯逻辑单元测试
  - 配置校验
  - 状态机
  - 索引映射
  - 结果数组约定
  - slot 回收幂等
- Linux 集成测试
  - 真实 TUN 设备
  - 真实 `io_uring`
  - 真实 RX/TX I/O
- 取消与恢复测试
  - `stop_rx()`
  - `send_many()` timeout
  - `send_many()` future drop
  - `ENOBUFS` 恢复
- 压力测试
  - 长时间 RX recycle
  - 并发 `writable()` waiter
  - 连续 `send_many()` cleanup 收敛

### 5.3 通过标准

- 所有共享语义测试在两个 backend 上都通过
- 所有请求取消路径都能收敛到稳定终态
- 不存在通过 drop future 触发的资源泄漏、UAF 或批次卡死

## 6. 风险排序

优先关注以下高风险区域：

1. `send_many()` 的 timeout / cancel / drop-cleanup 语义
2. RX slot recycle 与 `ENOBUFS` 自动恢复
3. backend 适配层是否意外侵入共享 `core`
4. `writable()` 多 waiter 语义在两个 backend 上的一致性

说明：

- 性能问题应排在语义正确性之后
- benchmark 只应在主路径稳定后引入，不能反过来驱动状态机设计

## 7. 实施建议

- 优先落地一条最小纵向切片：
  - M0 -> M1 -> M2 -> M3 -> M4
- 然后再补高风险语义：
  - M5 -> M6 -> M7
- 最后做有序发送与发布加固：
  - M8

建议节奏：

- 每完成一个里程碑，就冻结一次接口和语义，并补齐该阶段测试
- 在进入下一个里程碑前，先确保前一阶段的两个 backend 至少已经达到“可编译 + 基本语义通过”
- 不把多个高风险点一次性合并实现，例如不要把 `send_many()` 的 happy path、timeout、drop cleanup、`keep_order` 放在同一个大 patch 中
