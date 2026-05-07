# `UringDevice` 设计文档

## 1. 目标

本扩展 crate 基于 `tun-rs` 的 Linux `SyncDevice`，提供一个仅面向 Linux 的高性能异步设备类型 `UringDevice`。

实施路线见 [URING_DEVICE_IMPLEMENTATION_PLAN.md](./URING_DEVICE_IMPLEMENTATION_PLAN.md)。

核心目标：

- 使用 `io_uring + read_multishot + provided buffer ring` 优化接收路径
- 保持与 `tun-rs` 现有 `readable + try_recv + recv` 范式一致
- 参考 `tun-rs` 的 async 分层方式，对外保持单一公开类型，内部按 async backend 选择具体实现
- 默认不做 GSO 拆包，始终返回内核交付的完整原始包
- 将 GSO 拆包延迟到 `Packet` 方法，由调用方自行决定何时执行
- 单包发送继续保留简单直接的 async/nonblocking 语义
- 批量发送通过 `send_many` 使用 `io_uring` 批量提交以减少 syscall 成本
- `send_many` 支持批次级超时，并在超时后取消未完成请求且等待取消完成后再返回
- `send_many` 使用 owned 输入，设备内部保存批次所有权直到批次终态收敛
- `Packet` 在 `Drop` 时自动归还 ring buffer，避免调用方遗忘归还
- 核心状态机与 `io_uring` 资源管理不依赖特定 async runtime

## 2. 非目标

第一版不解决以下问题：

- 非 Linux 平台支持
- 自动 GSO 拆包
- 自动 GRO/GSO 发送聚合
- 多消费者并发接收
- 面向 `Stream` 的高层封装
- 对所有接收错误自动恢复

## 3. 平台与版本要求

- 平台：Linux
- 接收实现依赖 `io_uring` 的 multishot read 和 provided buffer ring
- RX 与 TX 使用的 `io_uring` 上下文都必须具备 `IORING_FEAT_FAST_POLL`
- 推荐最低内核版本：Linux 6.7+

说明：

- provided buffer ring 在更早内核已可用，但本设计依赖 `read_multishot` 对可轮询 fd 的稳定支持
- 根据 `io_uring_setup(2)`，`IORING_FEAT_FAST_POLL` 表示 ring 具备内建 poll 机制，可驱动读写的数据/空间就绪，而不是把等待读写就绪的请求交给异步线程
- 本设计要求 RX 和 TX 都基于这种内部 poll 能力来等待 TUN fd 的可读/可写
- 本 crate 应在初始化阶段检查运行环境，不满足要求时返回明确错误

## 4. 总体设计

`UringDevice` 分为两个相对独立的部分：

- RX 路径：`io_uring` multishot read + provided buffer ring + 后台 RX driver + 内部 `VecDeque<Packet>`
- TX 路径：
  - 单包发送：nonblocking `send/try_send/writable`
  - 批量发送：`send_many` 使用每个 `UringDevice` 内部共享的单个 TX `io_uring` 批量提交 one-shot write
  - 同一时刻最多只有一个 `send_many` 批次占用该 TX `io_uring`
  - `send_many` 的 owned 输入会保存在设备内部单一批次槽位中，直到批次终态收敛
  - 批量发送支持可选顺序约束 `keep_order`

接收侧采用单消费者模型，因此接收方法使用 `&mut self`。

发送侧不消费 RX 状态，因此发送方法可以使用 `&self`。

阶段性实现注记（截至 2026-04-25）：

- backend 或 driver 若需要在同一个 TUN fd 上做额外的 readiness/driver glue，不应依赖 `SyncDevice::try_clone()`
- 在 Linux 上，`SyncDevice::try_clone()` 依赖 `IFF_MULTI_QUEUE`，而 multiqueue 不在本期实现范围内
- 因此当前实现使用普通 `dup` 复制同一个底层 fd 来构造 backend glue/driver 持有者，而不是创建新的 TUN queue
- 当前 RX 主路径已经使用 `read_multishot + provided buffer ring` 返回 ring-backed `Packet`
- `Packet` 会一直持有 provided buffer slot，直到 `Drop()` 或 `detach()` 发生时才归还
- 当前代码已经把 `-ENOBUFS` 特判为 `Faulted(ENOBUFS)`，并把 recycle 事件接回自动恢复控制器
- 当前仓库已补齐基于真实单队列 TUN 的 live exhaustion/recovery 测试，覆盖手动恢复与自动恢复
- 当前仓库已通过真实单队列 TUN live 测试验证：`recv_many()` 会按 `out.len()` 限制批量取包，并在后续调用中继续 drain 剩余 ready 队列
- 当前仓库已通过真实单队列 TUN live 测试验证：`stop_rx()` 返回后不会再产生新的用户态 RX completion，直到后续显式 `start_rx()`
- 当前仓库已通过同一设备上的 4 轮 RX recovery live stress 验证：`ENOBUFS -> 手动恢复 -> stop/start` 与 `ENOBUFS -> 自动恢复 -> stop/start` 都可连续收敛
- 当前仓库已接入 `send_many()` 的共享 TX batch `io_uring` 基础路径，并通过真实单队列 TUN 验证可发送 IPv4/UDP 包
- 当前 `send_many()` 已跑通 timeout 与 future drop 后的内部 cleanup，并验证 cleanup 后下一批发送可继续推进
- 当前仓库已通过真实单队列 TUN 验证：当 TX 批次槽位被占用时，后续 `send_many()` 会按 timeout 预算等待 `Idle`，超时后直接返还整批 owned buffer 且不会误提交
- 当前仓库已通过同一设备上的 4 轮 live stress 验证：`drop cleanup / timeout / busy-slot timeout` 连续执行后，TX 批次槽位仍可恢复并继续处理后续批次
- 上述 stress 每轮后还会插入一次正常双包 `send_many()` 发送，验证 cleanup 路径不会影响后续普通批量发送
- 当前仓库已通过同一设备上的 8 轮 live long stress 验证：更长轮次下 TX 批次槽位仍可恢复并继续处理后续批次
- 当前仓库已通过同一设备上的 6 轮 TX mixed live stress 验证：unordered/ordered 成功发送、ordered chain-break、drop cleanup、timeout、busy-slot timeout 可连续交错收敛
- stress 中插入的普通双包 `send_many()` 会在奇偶轮切换 `keep_order`，当前顺序路径的成功场景也已纳入覆盖
- 当前 `keep_order == true` 已使用链式 `IOSQE_IO_LINK` chunk 提交，而不是逐项 `submit()`
- 当前仓库已通过 core 单元测试与真实单队列 TUN live 测试覆盖 `keep_order` 的链断裂/部分失败语义：中间非法包报错、同链尾项收到 `ECANCELED`、且尾项不会误送到 UDP socket
- 当前仓库已补齐 `Packet::split_into(...)`：无 offload_info 返回错误, 非 GSO 反回单段，GSO 包按 `tun-rs handle_virtio_read / gso_split` 语义拆段，且 `detach()` 前后结果一致
- 当前 RX 路径已接回 virtio offload 元信息，但采用 lazy parse：`Packet` 初始化时只记录 payload offset，不解析 header；首次调用 `offload_info()` 或 `split_into()` 时才解析
- 当前 offload-enabled RX `Packet` 对外 `as_bytes()/len()` 只暴露 virtio header 之后的原始 IP 包，不向调用方泄露 12-byte header
- 当前还未暴露显式 TX offload metadata API；`try_send()/send()/send_many()` 假设用户已在为 offload-enabled fd 处理好 virtio header
- 当前获取 TX 批次槽位时的 deadline 唤醒已改为 backend/runtime-native timer，不再为等待超时额外创建短生命周期线程
- 当前仓库已补齐发布前基础资产：`README`、可运行 `examples/`、以及可配置 round/batch 的 `perf_smoke` example
- 后续若继续演进，剩余工作主要转向更长时长的 soak 压测或更正式的 benchmark

### 4.1 async backend 分层

为与 `tun-rs` 的使用方式保持一致，建议：

- 对外始终只暴露单一公开类型 `UringDevice`
- 具体 async backend 通过 feature 选择，而不是在 public API 中暴露 Tokio/async-io 特有类型
- 同一构建中最多启用一个 async backend；若同时启用多个 backend，应在编译期报错

feature 约定建议与 `tun-rs` 对齐：

- `async`
  - 作为 `async_tokio` 的别名
- `async_tokio`
  - 使用 Tokio backend
- `async_io`
  - 使用 `async-io` backend，兼容 `async-std`、`smol` 等生态

内部建议拆分为两层：

- `core`
  - runtime-agnostic 的共享核心
  - 负责 RX/TX `io_uring` 上下文、provided buffer ring、`Packet`、`RxState`、`send_many()` 批次状态、取消/回收语义、内部队列与 waiter 状态
- `backend`
  - runtime-specific 的薄适配层
  - 负责把对外暴露的方法委托到选定 backend
  - 负责 `writable()/send()` 之类依赖具体 readiness 机制的部分
  - 负责承载后台 RX driver 的执行方式，例如使用对应 runtime 的任务/调度设施来运行 driver 循环

对外结构建议：

- `UringDevice` 作为 facade，内部持有一个由 feature 选定的具体实现
- public async 方法只暴露统一语义，例如 `readable()/recv()/writable()/send()/send_many()`
- facade 方法本身不应引用 Tokio/async-io 特有类型；具体行为由 backend 内部实现

设计约束：

- 不应在 public API 中暴露 `tokio::io::unix::AsyncFd`、`async_io::Async` 等 backend 专有类型
- 不应把核心 RX/TX 状态机正确性建立在特定 runtime reactor 之上
- 若两个 backend 都需要同一份语义，应优先抽到 `core` 复用，而不是复制两份独立逻辑
- backend 差异应尽量限制在 readiness 适配、任务承载与少量 glue code 上
- 若调用方需要 timeout，接口说明应使用“所选 runtime 提供的 timeout wrapper”这类中性表述，而不是把设计文档绑定到某个 runtime

## 5. 公开类型

### 5.1 `UringDevice`

```rust
pub struct UringDevice {
    // private
}
```

语义：

- 表示一个面向 Linux TUN/TAP fd 的异步设备
- 读写能力共存，但 RX 生命周期可单独控制
- 对外保持单一公开类型
- 具体内部实现由 feature 选定的 async backend 决定
- 通过消费一个 `tun-rs` 的 Linux `SyncDevice` 实例来创建
- 底层接收使用 `io_uring`
- 底层单包发送不强制使用 `io_uring`

### 5.2 `Packet`

```rust
pub struct Packet {
    // private
}
```

语义：

- 表示一次接收得到的完整原始包
- 默认不自动拆 GSO
- 绑定某个 ring buffer slot 的生命周期
- 在 `Drop` 时自动归还底层 buffer
- 可通过 `detach()` 将数据复制到内部 owned storage，并提前归还 ring slot
- 不可 `Clone`

阶段性实现说明：

- 当前实现已经支持 ring-backed 与 detached/owned 两种状态
- driver 在消费 CQE 后直接把 provided buffer slot 绑定到 `Packet`
- `Packet::Drop()` 会归还 slot，`Packet::detach()` 会先复制到 owned storage 再提前归还 slot
- 当前实现也已接入 `ENOBUFS -> Faulted(ENOBUFS)` 与 recycled-slot 驱动的自动恢复控制路径
- 当前仓库也已补齐 manual restart / auto resume 的 live exhaustion 验证
- 当前仓库也已接入 `send_many()` 基础 TX batch 路径
- 剩余工作主要转向更高强度压力测试、示例代码与性能 smoke test

### 5.3 `UringDeviceConfig`

```rust
#[derive(Clone, Debug)]
pub struct UringDeviceConfig {
    pub rx_buffer_len: usize,
    pub rx_buffer_count: usize,
    pub rx_ring_entries: u32,
    pub tx_ring_entries: u32,
    pub rx_auto_resume_after_recycled_slots: usize,
    pub rx_start_mode: RxStartMode,
    pub tx_submit_chunk_size: usize,
}
```

语义：

- `rx_buffer_len`
  - 每个 RX provided buffer slot 的字节长度
  - 决定单包可直接承载的最大原始长度上限
  - 过小会导致大包无法直接接收，过大则增加内存占用

- `rx_buffer_count`
  - RX provided buffer slot 的总数量
  - 同时也是内部 `Packet` 队列可占用的最大 slot 数量上限
  - 该值越小，越容易在上层处理变慢时触发 `-ENOBUFS`

- `rx_ring_entries`
  - RX `io_uring` 的 ring 深度
  - 至少要能容纳 multishot read、cancel、重提交通道以及必要控制请求

- `tx_ring_entries`
  - TX batch `io_uring` 的 ring 深度
  - 限制单轮 `send_many()` 能高效推进的请求数量

- `rx_auto_resume_after_recycled_slots`
  - 表示 RX 因 provided buffer 耗尽而中断后，在后台统计到多少次 slot 回收后自动尝试恢复
  - 默认值为 `0`
  - `0` 表示不自动恢复，调用方需要显式调用 `start_rx()`
  - `N > 0` 表示当后台累计观察到 `N` 个 slot 被归还后，自动尝试重提交通道

- `rx_start_mode`
  - 控制 `UringDevice` 创建完成后，RX 是立即启动还是初始保持停止
  - `AutoStart` 表示构造完成后立即建立 RX driver 并提交通道
  - `ManualStart` 表示初始进入 `Stopped`，等待调用方显式 `start_rx()`

- `tx_submit_chunk_size`
  - `send_many()` 在单轮向 TX ring 提交 SQE 时的最大分块大小
  - 若输入包数超过该值，则实现可以分多轮提交
  - 用于控制单次提交成本、ring 占用和 cancel/drain 压力

推荐约束：

- `rx_buffer_count > 0`
- `rx_buffer_len > 0`
- `rx_ring_entries > 0`
- `tx_ring_entries > 0`
- `tx_submit_chunk_size > 0`

回收计数来源：

- `Packet::Drop()` 归还 ring-backed slot
- `Packet::detach()` 提前归还 ring-backed slot

配置创建方式：

- `UringDeviceConfig` 应实现 `Default`
- 默认值应选择“保守但可直接使用”的一组推荐参数
- 调用方可以在 `default()` 基础上通过链式 helper 覆盖个别字段
- 字段仍可保持可读；链式 helper 主要用于让调用点更紧凑、减少样板代码

建议 API 草案：

```rust
impl Default for UringDeviceConfig {
    fn default() -> Self;
}

impl UringDeviceConfig {
    pub fn with_rx_buffer_len(self, value: usize) -> Self;
    pub fn with_rx_buffer_count(self, value: usize) -> Self;
    pub fn with_rx_ring_entries(self, value: u32) -> Self;
    pub fn with_tx_ring_entries(self, value: u32) -> Self;
    pub fn with_rx_auto_resume_after_recycled_slots(self, value: usize) -> Self;
    pub fn with_rx_start_mode(self, value: RxStartMode) -> Self;
    pub fn with_tx_submit_chunk_size(self, value: usize) -> Self;
}
```

示例：

```rust
let config = UringDeviceConfig::default()
    .with_rx_buffer_len(4096)
    .with_rx_buffer_count(1024)
    .with_rx_start_mode(RxStartMode::ManualStart);
```

### 5.4 `RxStartMode`

```rust
#[derive(Clone, Copy, Debug)]
pub enum RxStartMode {
    AutoStart,
    ManualStart,
}
```

### 5.5 `RxState`

```rust
#[derive(Clone, Debug)]
pub enum RxState {
    Running,
    Stopped,
    Faulted(std::sync::Arc<std::io::Error>),
}
```

语义：

- `Running`
  - 当前 RX 正在运行
  - 已提交 multishot read
  - 新 completion 会继续进入 CQ
- `Stopped`
  - 当前 RX 已停止
  - 不再接收新包
  - 当前已 ready 的 completion 仍可继续读取
- `Faulted(err)`
  - RX 因错误中断
  - 不再接收新包
  - 当前已 ready 的 completion 仍可继续读取
  - 调用方可通过 `start_rx()` 尝试恢复

补充：

- 若 multishot read 因 provided buffer 耗尽而收到 `-ENOBUFS`
  - 推荐进入 `Faulted(ENOBUFS)`
  - 这是一种可恢复 fault，而不是永久故障
  - 是否在后续 slot 回收后自动恢复，取决于 `rx_auto_resume_after_recycled_slots`
- 若 `rx_start_mode == ManualStart`
  - 设备初始化后 RX 可直接处于 `Stopped`
  - 调用方需显式调用 `start_rx()`

### 5.6 `OffloadInfo`

```rust
pub struct OffloadInfo {
    pub gso_type: GsoType,
    pub gso_size: u16,
    pub hdr_len: u16,
    pub csum_start: u16,
    pub csum_offset: u16,
    pub needs_csum: bool,
}
```

语义：

- 表示 Linux TUN 开启 offload 后从 virtio header 中解析出来的元信息
- `Packet` 对外暴露该信息，但不自动拆包

### 5.7 `GsoType`

```rust
pub enum GsoType {
    None,
    TcpV4,
    TcpV6,
    UdpL4,
    Other(u8),
}
```

## 6. 核心 API

以下为建议的公开 API 草案。

```rust
impl UringDevice {
    pub fn new(device: SyncDevice, config: UringDeviceConfig) -> std::io::Result<Self>;

    pub fn rx_state(&self) -> RxState;

    pub fn ready_len(&mut self) -> usize;

    pub async fn readable(&mut self) -> std::io::Result<()>;
    pub fn try_recv(&mut self) -> std::io::Result<Packet>;
    pub async fn recv(&mut self) -> std::io::Result<Packet>;
    pub async fn recv_many(
        &mut self,
        out: &mut [Option<Packet>],
    ) -> std::io::Result<usize>;

    pub async fn writable(&self) -> std::io::Result<()>;
    pub fn try_send(&self, buf: &[u8]) -> std::io::Result<usize>;
    pub async fn send(&self, buf: &[u8]) -> std::io::Result<usize>;
    pub async fn send_many(
        &self,
        bufs: Vec<bytes::Bytes>,
        results: &mut [Option<std::io::Result<usize>>],
        timeout: std::time::Duration,
        keep_order: bool,
    ) -> Vec<bytes::Bytes>;

    pub async fn stop_rx(&mut self) -> std::io::Result<()>;
    pub fn start_rx(&mut self) -> std::io::Result<()>;
}

impl Packet {
    pub fn as_bytes(&self) -> &[u8];
    pub fn len(&self) -> usize;
    pub fn is_detached(&self) -> bool;
    pub fn detach(&mut self);

    pub fn offload_info(&self) -> Option<&OffloadInfo>;
    pub fn is_gso(&self) -> bool;

    pub fn split_into<B: AsMut<[u8]>>(
        &self,
        out: &mut [B],
        sizes: &mut [usize],
        offset: usize,
    ) -> std::io::Result<usize>;
}
```

### 6.1 创建语义

`UringDevice::new(device, config)` 的建议语义：

- 按值接收 `SyncDevice`，并接管其底层 TUN/TAP fd 的后续生命周期管理
- 调用方一旦把 `SyncDevice` 传入 `new()`，就不应再保留原设备上的独立 I/O 语义假设
- backend/driver 如果需要额外持有同一个底层 fd，应基于普通 fd duplication，而不是依赖 multiqueue clone
- 构造阶段应先统一完成配置校验、平台检查和 ring 能力检查，再决定是否启动 backend 侧承载逻辑
- 若配置非法、运行环境不满足要求或初始化任一步骤失败，`new()` 应直接返回错误，不暴露部分初始化成功的 `UringDevice`
- 若 `config.rx_start_mode == RxStartMode::AutoStart`，则 `new()` 只有在 RX driver 已完成启动并进入可运行状态后才返回成功
- 若 `config.rx_start_mode == RxStartMode::ManualStart`，则 `new()` 成功返回时 RX 初始状态应为 `Stopped`
- 构造入口对外只暴露统一签名，不在 public API 中泄漏 Tokio/async-io backend 专有类型

补充：

- 内部实现可以根据需要从 `SyncDevice` 中提取、共享或封装底层 fd，但这应对调用方透明
- `Default + with_*` 的配置方式不改变配置校验规则；所有最终约束仍在 `new()` 中统一校验

## 7. 接收语义

### 7.1 `ready_len(&mut self) -> usize`

语义：

- 返回当前内部 `Packet` 队列中的包数量
- 该值对应已经由后台 RX driver 从 CQ 中提取、校验并成功 materialize 的包数量

因此：

- `ready_len()` 反映的是当前可立即被 `try_recv()` 消费的精确数量
- `ready_len() > 0` 时，下一次 `try_recv()` 应成功
- `ready_len() == 0` 时，只说明当前内部 `Packet` 队列为空

### 7.2 外部超时约定

对以下不涉及内部请求取消的 async 方法：

- `readable()`
- `recv()`
- `recv_many()`
- `writable()`
- `send()`

约定：

- 如果入口时条件已满足，则应立即成功返回
- 这些方法不内建 `timeout` 参数
- 若调用方需要 deadline/timeout，推荐在设备外部使用所选 runtime 提供的 timeout wrapper

例外：

- `stop_rx()` 是控制型 async 方法，不提供超时参数
- `send_many()` 保留显式 `timeout` 参数，因为这里的超时不仅是等待预算，也是该 API 定义的批次取消路径

实现建议：

- 对 `send_many()`，内部应统一换算为绝对 deadline
- 不要在多个内部等待点分别重新消费相对 `Duration`

补充：

- 对 `send_many()` 而言，`timeout` 只约束“等待获取共享 TX ring”和“正常完成批次”的预算
- 如果 `send_many()` 在 deadline 到达后已经提交了部分或全部 SQE，则必须先启动取消，再等待这些请求都到达终态后才能返回
- 因此 `send_many()` 的实际返回时间可能略晚于 `timeout`
- 对 `send_many()` 而言，future drop 不会立即释放输入 buffer；设备必须先完成内部 cleanup 并回到空闲态，下一个 `send_many()` 才能开始

### 7.2.1 RX 等待取消与请求取消

在 RX 侧需要区分两类“取消”：

- 取消“等待”本身
- 取消已经提交到内核的 `io_uring` 请求

这两者不是同一件事。

就公开 RX API 而言：

- `readable()`、`recv()`、`recv_many()` 只涉及“取消等待”本身
- `stop_rx()` 才涉及取消已经提交到内核的 multishot read 请求

对于 RX 纯等待型方法：

- `readable()`
- `recv()`
- `recv_many()`

推荐语义：

- 先走一次非阻塞 fast path，直接检查当前内部队列或状态是否已经满足条件
- 若在等待过程中 RX 状态变为 `Stopped` 或 `Faulted`，应优先返回对应状态错误
- 如果条件未满足，则把当前任务注册为本地 waiter，并返回 `Poll::Pending`
- 一旦后台 driver 推进了内部状态，例如：
  - RX 队列从空变为非空
  - RX 状态从 `Running` 变为 `Stopped/Faulted`
  再唤醒 waiter 重新 poll

实现建议：

- 不要在这些 public async 方法内部直接使用 `io_uring_wait_cqe()`、`io_uring_wait_cqe_timeout()` 之类会阻塞当前线程的等待 helper
- 推荐为 RX ring 注册 `eventfd`，把它作为后台 RX driver 的 CQ 通知源
- `eventfd` 通知只能视为 hint，被唤醒后仍需由后台 driver 自行 drain CQ，不能假设通知次数与 completion 数量一一对应
- 由于接收侧为单消费者模型，内部只需要一个本地 waiter 槽位，不需要等待队列

取消语义：

- 若调用方在条件满足前 drop 这些等待型 future，只应移除本地 waiter
- 这种 drop 不应触发额外的内核 cancel
- 因此这类“纯等待 future”应尽量设计为 cancel-safe

请求取消：

- `stop_rx()` 会主动取消已有 multishot read

### 7.2.2 TX 单包发送等待与请求取消

`writable()` 与 `send()` 是多消费者模型，不应要求内部自建单一 waiter 槽位。也不依赖 `io_uring` CQ 通知源。

推荐语义：

- 对外暴露的 `UringDevice::writable()` / `send()` 可作为 facade，转发到当前选定 backend 的具体实现
- `writable()` 应继续基于底层 TUN fd 的普通 readiness wait 实现
- `send()` 应继续基于 `try_send() + writable()` 的标准 async 包装
- 由于方法签名使用 `&self`，允许多个任务并发等待 `writable()`；实现应兼容多 waiter，而不是套用 RX 单消费者模型
- 这部分实现允许因 backend 不同而变化，但对外语义应保持一致

取消语义：

- 若调用方在 `writable()` 或 `send()` 等待期间 drop future，只应取消本次用户态等待
- 这种 drop 不应触发额外的内核 cancel
- 若实现仍是 nonblocking fd + readiness wait，则它们应尽量保持 cancel-safe

请求取消：

- `send_many()` 会主动提交多个 write 请求，可能在取消后发起 cancel 请求并继续等待直到缓存不被内核占用

### 7.3 `readable(&mut self)`

语义：

- 如果当前内部 `Packet` 队列非空，立即返回成功
- 如果当前内部 `Packet` 队列为空且 `RxState::Running`，等待直到内部队列中至少有 1 个包
- 如果当前内部 `Packet` 队列为空且 `RxState::Stopped`，立即返回 `rx stopped` 错误
- 如果当前内部 `Packet` 队列为空且 `RxState::Faulted(err)`，立即返回该错误

约束：

- 成功返回后，下一次 `try_recv()` 应成功

### 7.4 `try_recv(&mut self)`

语义：

- 如果内部 `Packet` 队列非空，则弹出 1 个 `Packet` 并返回
- 如果当前内部队列为空且 `RxState::Running`，返回 `WouldBlock`
- 如果当前内部队列为空且 `RxState::Stopped`，返回 `rx stopped`
- 如果当前内部队列为空且 `RxState::Faulted(err)`，返回 `err`

### 7.5 `recv(&mut self)`

语义：

- 与 `try_recv + readable` 范式保持一致
- 如果当前内部队列已有至少 1 个包，立即返回 1 个包
- 否则等待到至少有 1 个包，然后返回 1 个包
- 不为等更多包而额外等待

### 7.6 `recv_many(&mut self, out)`

语义：

- 与 `recv()` 保持相同等待策略
- 如果当前内部队列已有至少 1 个包，立即返回，不额外等待
- 如果当前内部队列为空且 `RxState::Running`，等待直到至少有 1 个包
- 一旦有数据，立即从内部 `Packet` 队列中弹出一批包并写入 `out`，实际返回条目数受 `out.len()` 限制

返回值：

- 返回本次实际写入 `out` 的数量

输出数组约定：

- 仅前 `n` 个元素被写入为 `Some(Packet)`
- 其余元素保持调用前状态，不做额外保证

实现建议：

- `recv_many()` 直接批量消费当前内部 `Packet` 队列
- 后台 driver 负责把 CQE 提前转换成队列，不应由 `recv_many()` 自己再去看 CQ
- 但不应为了“攒批次”延迟返回

## 8. 发送语义

### 8.1 `try_send(&self, buf)`

语义：

- 走 nonblocking 单包发送路径
- fd 当前不可写时返回 `WouldBlock`

### 8.2 `writable(&self)`

语义：

- 与 `tun-rs` 现有 async 发送语义保持一致
- 等待到下一次 `try_send()` 不再返回 `WouldBlock`

### 8.3 `send(&self, buf)`

语义：

- 基于 `try_send + writable` 的标准 async 包装
- 单包发送默认不强制使用 `io_uring`

原因：

- 单次 one-shot `io_uring` 写请求不一定比 nonblocking `write` 明显更省 syscall
- 本设计中 `io_uring` 的明确收益主要来自 multishot RX 和批量 TX 提交

### 8.4 `send_many(&self, bufs, results, timeout, keep_order)`

语义：

- 每个 `UringDevice` 维护一个共享的 TX batch `io_uring`
- 每个 `UringDevice` 还维护一个内部单一 `tx_batch` 槽位，用于保存当前批次的 owned 输入和运行状态
- 同一时刻最多只允许一个 `send_many` 批次使用该 TX ring 和 `tx_batch` 槽位
- 若调用进入时已有其他 `send_many` 批次在 `Running` 或 `Cancelling`，则当前调用必须先等待设备回到 `Idle`
- 使用该共享 TX ring 批量提交多个发送请求
- 每个发送请求都是独立的 one-shot write
- 允许 `io_uring` 的内部 poll 机制在批次超时预算内等待可写
- 当批次超时到达时，所有尚未完成的发送请求都应被取消
- 一旦本次调用已经提交过请求，则不得在取消尚未收敛时提前返回
- 必须在所有已提交请求都进入终态后才能返回给调用方
- 通过一次批量提交减少 syscall 与调度往返

返回值：

- 正常 `await` 完成时，始终直接返回原始 owned `bufs`
- 返回向量中的第 `i` 项对应输入 `bufs[i]`
- 逐包发送结果仍通过 `results[i]` 返回
- 即使整批在进入 `Idle` 前就已经超时，或提交后部分失败，只要本次调用最终正常返回，调用方都能通过返回值拿回整批 buffer
- 空输入可直接返回空向量

超时语义：

- `timeout.is_zero()` 表示整个调用不限时, 否则表示整个调用的最大主动等待预算为 `timeout`
- 该预算同时覆盖：
  - 等待设备内部 `tx_batch` 槽位回到 `Idle`
  - 获取 TX ring 后等待本批请求自然完成
- 如果在预算耗尽前仍未等到 `Idle`，则整批输入应直接通过返回值返还，并把对应 `results` 写为 `TimedOut`
- 如果预算耗尽时本批已经提交了请求，则必须：
  - 对所有未完成请求发起取消
  - 继续等待这些请求的终态 completion
  - 仅在终态全部收齐后返回
- 因批次超时而 `ECANCELED` 的条目，建议统一在 `results` 中映射为 `TimedOut`
- `send_many()` 返回时必须保证本批次不再存在依赖这些 owned buffer 的 in-flight SQE

结果数组约定：

- `results.len()` 应不小于 `bufs.len()`
- 正常完成时，前 `bufs.len()` 个 `results[i]` 与返回的 `bufs[i]` 一一对应
- 若 `results.len() < bufs.len()`，则不应提交任何 SQE，并应直接返回原始 `bufs`
- 在这种输入不合法场景下，允许实现对可写入的前缀结果槽做 best-effort `InvalidInput` 标记

future 取消语义：

- `timeout` 是 `send_many()` 支持的正式取消路径
- 一旦本次调用已经把 owned 输入移入设备内部 `tx_batch` 槽位，future 被外部 drop 也不应立即释放这些 buffer
- future drop 的效果仅应是：
  - 标记当前批次需要取消
  - 唤醒内部 driver 执行 cancel/drain
- future drop 后，批次会进入 `Cancelling`
- 只有当内部 cleanup 完成并回到 `Idle` 后，新的 `send_many()` 才能开始
- 即使采用 owned 输入，`send_many()` 仍不应声明为 cancel-safe
- 原因不是内存安全，而是 future drop 后不会再有结果返回路径；只能保证内部安全 cleanup，而不能保证继续把结果返还给已 drop 的调用方

共享 TX ring 语义：

- `send_many()` 不为每次调用创建独立的 `io_uring`
- 所有 `send_many()` 调用共享同一个 TX ring，以避免额外内存和注册成本
- 由于共享 TX ring 需要保证请求与 completion 的归属清晰，因此同一设备上批量发送按“单批次独占”执行
- 这种独占范围覆盖：
  - `Running`
  - `Cancelling`
  - 直到内部 `tx_batch` 槽位被清空并重新回到 `Idle`
- `&self` 允许多个任务同时发起发送，但并不意味着多个 `send_many()` 可以同时占用同一个 TX ring

内部批次状态建议：

- `Idle`
  - 当前没有批次占用 `tx_batch`
  - 允许新的 `send_many()` 开始
- `Running`
  - 当前批次已获取 `tx_batch`
  - owned buffer 已移入设备内部
  - SQE 可能已部分或全部提交
- `Cancelling`
  - 当前批次已收到 timeout 或 future drop 触发的取消请求
  - 设备仍在等待 cancel/drain 收敛
  - 在此状态下不允许新的 `send_many()` 开始

`keep_order` 语义：

- `keep_order == false`
  - 请求可并发提交
  - completion 顺序不代表输入顺序
  - 只要求返回向量中的 `bufs[i]` 与 `results[i]` 稳定对应输入第 `i` 个包
- `keep_order == true`
  - 发送必须按输入顺序推进
  - 推荐实现为 `IOSQE_IO_LINK` 链式提交，并将批次超时绑定到整条链
  - 必须在文档中明确说明：该模式会降低并行度，增加尾延迟，并且由于链条在前序错误时会被打断，可能提升整批失败率
  - 当前实现按 `tx_submit_chunk_size` 构造单条链；链内前序失败会让同链尾项收到 `ECANCELED`，下一条链则在前一条链收敛后再继续提交

跨 API 顺序说明：

- `keep_order` 只约束当前这一次 `send_many()` 内部的包顺序
- 它不自动约束与并发 `send()/try_send()` 调用之间的全局发送顺序
- 若调用方需要所有 TX 操作的全局顺序，应在设备外部自行串行化

设计说明：

- `keep_order` 对应设计讨论中的 `keepOrder` 概念
- 公开 Rust API 使用 `snake_case`

完成顺序：

- `io_uring` completion 顺序不能假定与提交顺序相同
- 实现必须通过 `user_data` 将 completion 映射回输入索引
- 对外返回向量中的第 `i` 项以及 `results[i]` 都必须与输入 `bufs[i]` 对应

为什么 `send_many` 有价值：

- 批量提交多个 write 请求时，可以将多个发送操作合并到更少的 syscall 中
- 这是 TX 路径上使用 `io_uring` 最明确的收益点
- 在 `IORING_FEAT_FAST_POLL` 可用时，暂不可写的主路径应交给内核 poll 机制处理
- 若单个 write 仍以错误完成，应作为该条目的最终逐包结果写入 `results[i]`
- 批次超时模型适合对时效敏感的数据包发送

## 9. `Packet` 语义

### 9.1 生命周期

- `Packet` 绑定一个 RX ring buffer slot
- 在 `Drop` 时自动归还 slot
- 调用方不需要手动 `giveBack`
- 若调用 `detach()`，则应将当前包内容复制到 `Packet` 自身持有的内存，并立即归还 ring slot
- `detach()` 之后，`Packet` 继续可用，但数据来源改为内部 owned storage

### 9.1.1 `detach()` 与 `is_detached()`

`detach()` 的语义：

- 如果当前 `Packet` 仍为 ring-backed：
  - 复制当前包数据到 `Packet` 内部拥有的内存
  - 保留现有 `OffloadInfo`
  - 立即归还 ring slot
  - 将内部状态切换为 detached/owned
- 如果当前 `Packet` 已经 detached：
  - 直接 no-op

`is_detached()` 的语义：

- 返回该 `Packet` 当前是否已经脱离 ring-backed 状态

设计目的：

- 保留默认零额外分配的快路径
- 为需要长期持有、跨任务传递或排队处理的调用方提供显式脱离路径
- 避免仅依赖 `Drop` 导致 ring buffer slot 被长期占用

### 9.2 内容语义

- `Packet` 始终表示内核交付的完整原始包
- 默认不自动 GSO 拆包
- 若 Linux offload 开启，`Packet` 可暴露 `OffloadInfo`
- 若底层 RX 包前带有 virtio header，`as_bytes()/len()` 仅暴露 header 之后的 IP 包本体
- `OffloadInfo` 当前按 lazy parse 方式提供，而不是在 `Packet` 初始化时立即解析

### 9.3 GSO 拆包

`split_into(...)` 的语义：

- 若没有 offload_info，返回错误
- 若不是 GSO 包，返回单段
- 若是 GSO 包，则执行用户态拆包
- 拆包行为与 `tun-rs` 现有 Linux `handle_virtio_read / gso_split` 逻辑保持一致

注意：

- `split_into(...)` 不消费 `Packet`
- `Packet` 原始内容仍然保持不变
- `detach()` 前后，`split_into(...)` 的结果应保持一致

## 10. 接收状态控制

### 10.1 `stop_rx(&mut self)`

语义：

- 停止接收新数据
- 取消当前 in-flight multishot read
- 返回后不再向 RX 缓存注入新的 completion
- 已经进入缓存的数据仍然可以继续读取

是否幂等：

- 是
- 若当前已为 `Stopped`，返回 `Ok(())`
- 若当前为 `Faulted`，返回 `Ok(())`，保持原状态不变

为什么使用 async：

- 停止 RX 可能需要等待 cancellation completion
- `async fn` 能保证在返回时达到更强的一致性语义

### 10.2 `start_rx(&mut self)`

语义：

- 若当前不是 `Running`，重新提交 multishot read
- 成功后进入 `Running`
- 若提交失败，则进入或保持 `Faulted(err)`

是否幂等：

- 是
- 若当前已为 `Running`，返回 `Ok(())`

### 10.3 状态转移

正常状态转移：

- `Running -> Stopped`
  - `stop_rx()` 成功
- `Stopped -> Running`
  - `start_rx()` 成功
- `Running -> Faulted(err)`
  - RX 运行时发生错误
- `Faulted(err) -> Running`
  - `start_rx()` 恢复成功
- `Faulted(err_old) -> Faulted(err_new)`
  - `start_rx()` 恢复失败并更新错误

缓存与状态的关系：

- `Stopped` 或 `Faulted` 仅表示不再接收新包
- 不代表当前内部 `Packet` 队列为空
- 接收 API 必须优先消费当前内部 `Packet` 队列，再根据状态决定是否返回错误

### 10.4 自动重提交流程

若后台 RX driver 在运行中遇到如下可恢复情况：

- provided buffer 已全部被当前内部队列和用户持有的 `Packet` 占满
- 内核因没有可用 provided buffer 返回 `-ENOBUFS`
- 当前 multishot read 因上述原因终止

则建议语义为：

- 这类情况优先视为可恢复 fault，推荐对外表现为 `Faulted(ENOBUFS)`
- 是否自动恢复由 `rx_auto_resume_after_recycled_slots` 控制
- 当该配置为 `0` 时：
  - 不自动恢复
  - 后续即便有 slot 被归还，也只更新内部可恢复条件
  - 调用方需要显式调用 `start_rx()`
- 当该配置为 `N > 0` 时：
  - 后台 driver 应在 fault 发生后开始累计回收计数
  - 每当一个 ring-backed slot 被归还，就把计数加一
  - 当累计值达到 `N` 时，自动尝试重提交通道
  - 若自动恢复成功，则进入 `Running`
  - 若自动恢复失败，则保持或更新为 `Faulted(err)`
- 一旦自动恢复或手动 `start_rx()` 成功，回收计数应清零
- 只有不可恢复的真正 I/O 错误，才进入一般意义上的 `Faulted(err)`

## 11. 错误语义

### 11.1 RX 停止错误

当 `RxState::Stopped` 且当前没有可立即消费的 completion 时：

- `readable()`
- `recv()`
- `recv_many()`
- `try_recv()`

都应返回统一的“RX 已停止”错误，而不是 `WouldBlock`。

推荐实现：

- 使用统一 helper 构造错误
- 避免到处复制错误字符串

例如：

```rust
fn rx_stopped_error() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "rx stopped")
}
```

### 11.2 RX 故障错误

当 `RxState::Faulted(err)` 且当前没有可立即消费的 completion 时：

- 直接返回该错误

### 11.3 `WouldBlock`

只有一种情况返回 `WouldBlock`：

- `RxState::Running`
- 当前内部 `Packet` 队列为空
- 当前无可立即返回的数据

即：

- `WouldBlock` 表示“以后可能还有数据”
- `Stopped/Faulted` 表示“当前不会自动变得可读”

### 11.4 `send_many` 的批次错误与逐包错误

- `send_many()` 不通过外层 `io::Result` 表示整批结果，而是直接返回整批 `bufs`
- 逐包发送结果通过 `results` 返回
- 若等待设备回到 `Idle` 时超时，则整批输入应按原顺序返回，并把对应 `results` 统一写为 `TimedOut`
- 若提交批次前出现其它可恢复的协调错误，也应优先通过返回值把整批 owned buffer 返还给调用方
- 一旦本次调用已经成功提交一个或多个发送请求：
  - 不应再因为批次超时而丢失返回路径
  - 而应等待所有已提交请求进入终态
  - 并把逐包结果写入 `results`，同时按原顺序返还 `bufs`
- 因 deadline 触发取消而未发送成功的条目，可统一映射为 `results[i] = TimedOut`

### 11.5 Future cancel-safety

推荐按以下方式约束各 async 方法：

- `readable()`、`recv()`、`recv_many()`
  - 若只是等待条件成立而尚未消费内核借用资源，则应设计为 cancel-safe
  - future 被 drop 时，只移除本地 waiter
- `writable()`、`send()`
  - 若实现仍是 nonblocking fd + readiness wait，则也应尽量保持 cancel-safe
- `stop_rx()`
  - 一旦已经开始发起 multishot read 的取消流程，就不应把它当成普通 cancel-safe future
  - 推荐文档明确要求：调用方在开始 `stop_rx()` 后应 await 到完成
- `send_many()`
  - 即使采用 owned 输入，也不应承诺 cancel-safe
  - 支持的是“timeout 或 future drop 触发内部 cancel，并等待收敛”的受控 cleanup 语义
  - 只有正常 await 完成的调用，才能通过返回值拿回 owned `bufs`，并通过 `results` 读取逐包发送结果

## 12. 内部实现建议

### 12.1 RX 结构

建议内部至少维护：

- 一个 provided buffer ring
- 一个 `rx_buffer_len` 配置值
- 一个 `rx_buffer_count` 配置值
- 一个 in-flight multishot read 请求标识
- 一个 RX 状态字段
- 一个 cancellation/in-flight 控制字段
- 一个后台 RX driver
- 一个内部 `VecDeque<Packet>`
- 一个 `rx_ring_entries` 配置值
- 一个 `rx_auto_resume_after_recycled_slots` 配置值
- 一个 `rx_start_mode` 配置值
- 一个“自上次 `ENOBUFS` 后已回收 slot 数”计数器
- 必要的 CQ 消费 bookkeeping
- 一个 CQ 通知源，例如注册到 RX ring 的 `eventfd`
- 一个本地 waiter 槽位，用于挂起 `readable/recv/recv_many` 的任务

说明：

- 后台 RX driver 应持续等待 CQ 通知并尽量把当前可见 CQE 全部读完
- 成功的数据 CQE 应立即转换成内部 `Packet` 队列项
- 错误、终止和控制型 CQE 应立即转化为内部状态更新，而不是留给 `try_recv()` 临时判断
- `Packet` 不再是按需从 CQE materialize，而是由后台 driver 提前构造并入队
- `ready_len()` 直接等于内部 `VecDeque<Packet>` 的当前长度
- CQ notification 只作为“需要重新检查 CQ”的提示，不能直接当作 completion 计数
- RX ring 初始化时应明确检查 `IORING_FEAT_FAST_POLL`
- `rx_buffer_count` 应与 provided buffer ring 中注册的 slot 数一致
- `rx_buffer_len` 应与每个 slot 的实际分配大小一致
- `rx_start_mode == ManualStart` 时，后台 driver 可已创建但不应主动提交通道
- `-ENOBUFS` 后若自动恢复阈值为 `0`，driver 不应自行重提交通道
- `-ENOBUFS` 后若自动恢复阈值大于 `0`，driver 应在 slot 回收计数达到阈值时尝试自动恢复

### 12.2 `ready_len()` 的实现建议

`ready_len()` 建议：

- 直接返回内部 `VecDeque<Packet>` 的长度
- 它不负责触发 CQ drain，也不应重新扫描 CQ
- 它是一个纯观察内部队列状态的操作

因此它不是阻塞操作，也不需要推进底层 CQ。

### 12.3 `Packet` 回收

建议设计为：

- `Packet` 内部保存共享的 slot 回收上下文
- `Packet` 内部至少支持 ring-backed 与 detached/owned 两种存储状态
- `Drop` 时仅在 ring-backed 状态执行 slot recycle
- recycle 必须是幂等的，避免重复归还
- `detach()` 触发状态切换时，也必须保证 slot 恰好归还一次
- 若当前 RX 因无 buffer 可用而进入 `Faulted(ENOBUFS)`：
  - buffer recycle 应通知后台 RX driver
  - driver 是否自动重提交通道，取决于 `rx_auto_resume_after_recycled_slots`
  - recycle 事件应参与自动恢复阈值计数

### 12.4 TX 结构

单包发送：

- 直接使用 nonblocking fd 写
- `writable()` 推荐继续走 readiness wait，而不是在 public async 方法里阻塞等待 `io_uring` CQE

批量发送：

- 维护一个共享的 TX batch `io_uring`
- 为该共享 TX ring 提供 CQ 通知源，例如注册 `eventfd`
- TX ring 初始化时应明确检查 `IORING_FEAT_FAST_POLL`
- 应使用 `tx_ring_entries` 配置 ring 深度
- 维护一个内部单一 `tx_batch` 槽位，例如 `Option<TxBatchState>`
- `TxBatchState` 至少应包含：
  - owned buffers
  - per-packet results
  - in-flight 计数
  - `cancel_requested`
  - 当前阶段，例如 `Running/Cancelling`
  - 唤醒等待下一批次的 waiter
- 用一个独占控制原语保证同一时刻最多只有一个 `send_many()` 批次占用该 ring 与 `tx_batch`
- 新的 `send_many()` 若发现设备不在 `Idle`，则先等待空闲
- 等待设备回到 `Idle` 的 async 路径应基于本地 waiter + wake 机制，而不是直接阻塞线程
- 在拿到独占权后，把 owned 输入移入 `tx_batch`，再按 `tx_submit_chunk_size` 分块构建 SQE
- 为每个 SQE 记录输入索引到 `user_data`
- 为整批请求绑定超时控制
- `keep_order == true` 时，优先采用链式提交
- `keep_order == false` 时，请求可独立提交，但超时后仍需取消未完成项
- 若 deadline 到达且本批已有提交，则继续 drain，直到所有已提交请求进入终态
- 若 future 被 drop，则只应设置 `cancel_requested` 并唤醒内部 driver，不得提前释放 `tx_batch`
- 只有在本批次在共享 TX ring 上完全收敛、owned buffers 已安全释放或可安全返还、并且 `tx_batch` 被清空后，才释放独占权
- drain completion 后按索引写入 `results`，并把原始 `bufs` 容器直接作为返回值返还
- 不应把“drop `send_many()` future”当作立即释放资源的终止路径
- 当前实现已采用 `DropGuard + eventfd` 唤醒 TX driver 的方式处理 future drop；driver 收到取消请求后会标记未提交项、取消 pending write，并在收敛后释放批次槽位
- 当前实现已修正 TX 批次槽位等待的 check/register 竞态，空闲检查与 waiter 注册在同一把锁下完成，避免无限等待调用漏唤醒

## 13. 与 `tun-rs` 的关系

建议本 crate 复用 `tun-rs` 的以下能力：

- `SyncDevice`
- Linux offload 元数据结构和语义
- 现有 GSO 拆包逻辑

其中：

- 普通接收：返回原始包，不复用 `tun-rs::recv_multiple` 的“自动拆包”语义
- `Packet::split_into(...)`：可参考并复用 `tun-rs` 现有 Linux 拆包逻辑

## 14. 命名约束

为避免与 `tun-rs` 现有接口混淆：

- 顶层类型命名为 `UringDevice`
- 批量发送命名为 `send_many`
- 批量接收命名为 `recv_many`

不建议直接使用以下名称：

- `AsyncReceiver`
- `recv_multiple`
- `send_multiple`

原因：

- `tun-rs` 现有 `recv_multiple/send_multiple` 已有 Linux offload 相关语义
- 新 crate 中这两个名字更适合保留给“批量返回/批量发送”的一般含义

## 15. 第一版建议范围

建议第一版只实现：

- `UringDevice`
- `Packet`
- `RxState`
- `OffloadInfo`
- `ready_len`
- `readable`
- `try_recv`
- `recv`
- `recv_many`
- `stop_rx`
- `start_rx`
- `writable`
- `try_send`
- `send`
- `send_many`

可以延后实现：

- 更细粒度的错误分类 helper
- 更高层的 stream/framed API
- 发送侧 GSO/GRO 批量优化
- benchmark 驱动的 SQPOLL 变体

## 16. 设计结论

本设计的关键决策如下：

- RX 使用 `io_uring` multishot 以获得明确收益
- TX 单包发送保持 nonblocking 简单语义
- TX 批量发送通过共享 TX ring 上的 `send_many` 使用 `io_uring`
- 对外保持单一 `UringDevice` 类型，内部按 feature 选择 `async_tokio` 或 `async_io` backend
- `UringDevice` 的创建入口按值消费 `SyncDevice`，并显式接收 `UringDeviceConfig`
- 核心 `io_uring` 状态机与资源管理应尽量沉淀在 runtime-agnostic 的共享 `core`
- `send_many` 通过超时控制避免批量请求无限排队，但超时后仍需等待取消完成再返回
- RX 纯等待型接收方法应通过本地 waiter + CQ 通知源实现，而 TX 单包发送继续走 fd readiness wait
- `send_many` 使用 owned 输入，并把批次所有权保存在设备内部直到 cleanup 完成
- `send_many` 即使使用 owned 输入也不承诺 future cancel-safe
- RX/TX 两侧 ring 初始化都要求 `IORING_FEAT_FAST_POLL`
- 默认不自动拆 GSO
- `Packet` 在 `Drop` 时自动归还 buffer
- 接收状态使用单一 `RxState` enum 表示
- 接收 API 一律优先 drain 缓存，再看状态
- `recv` 与 `recv_many` 使用完全一致的等待策略
- `ready_len()` 暴露“至少可立即读取的包数量”，允许做非阻塞 peek/drain

这份设计文档可作为第一版实现的直接依据。
