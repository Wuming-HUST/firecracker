# Free Page Reporting (FPR) 自测文档

本文档用于验证移植到 `feat-fpr` 分支的 **virtio-balloon Free Page Reporting** 特性。
当前开发沙箱缺少 `/dev/kvm` 与 `clippy`，因此需要 KVM 主机才能跑通端到端与 `test_attach_balloon_device`。

---

## 0. 改动范围速览（被测对象）

只引入了 **FPR（Free Page Reporting）**，未引入 FPH（Free Page Hinting）。

| 文件 | 改动 |
|---|---|
| `devices/virtio/balloon/mod.rs` | 新增特性位 `VIRTIO_BALLOON_F_FREE_PAGE_REPORTING = 5` |
| `devices/virtio/balloon/device.rs` | `Balloon::new` 增加 `free_page_reporting` 参数；`queue_evts` 改 `Vec`；按需动态构造队列；新增 `process_free_page_reporting_queue[_event]`、`free_page_reporting()`、`free_page_reporting_idx()`；`BalloonConfig` 加字段 |
| `devices/virtio/balloon/event_handler.rs` | 注册并分发 reporting 队列事件（token `PROCESS_VIRTQ_REPORTING = 5`） |
| `devices/virtio/balloon/metrics.rs` | 新增 `free_page_report_count` / `_fails` / `_freed` |
| `devices/virtio/balloon/persist.rs` | 快照恢复时按 `avail_features` 重建 reporting 队列布局 |
| `vmm_config/balloon.rs` | `BalloonDeviceConfig` 加 `free_page_reporting`（`#[serde(default)]`），透传到 `Balloon::new` |

**关键设计**：reporting 队列恒为最后一个，索引随 stats 浮动 —— stats 关：index **2**；stats 开：index **3**。
释放复用已有的 `util::remove_range` → `MADV_DONTNEED`（快照恢复态下改为 `mmap` 打洞）。

---

## 1. 编译 + 静态检查

```bash
cd <repo>

# 编译（库 + 测试代码）
cargo build -p vmm
cargo test  -p vmm --no-run

# 格式（CI 会卡）
cargo fmt --all -- --check

# Lint（CI 用 -D warnings；本沙箱未装 clippy，到 KVM 环境补跑）
cargo clippy -p vmm --all-targets -- -D warnings
```

**通过标准**：全部零错误、零 warning。

---

## 2. 单元测试

> ⚠️ **必须单线程**：balloon 的 `METRICS` 是进程内全局静态，多线程并行时
> `deflate_count` 等会被其它用例污染，导致 `test_deflate` 偶发 `left:2 right:1`。
> 这是既有现象，与 FPR 无关，但会干扰判断。

```bash
# 全部 balloon 相关用例
cargo test -p vmm balloon -- --test-threads=1
```

**预期**：除 `builder::tests::test_attach_balloon_device`（需 `/dev/kvm`）外全过；
在 KVM 主机上该用例也应通过。

### 2.1 FPR 针对性用例（本次新增）

```bash
cargo test -p vmm --lib balloon::device::tests::test_free_page_reporting_layout -- --test-threads=1
cargo test -p vmm --lib balloon::device::tests::test_process_reporting        -- --test-threads=1
```

- **`test_free_page_reporting_layout`**：验证特性位与队列布局
  - reporting 关 → 2 个队列；
  - reporting 开 / stats 关 → 3 个队列，`free_page_reporting_idx() == 2`；
  - reporting 开 / stats 开 → 4 个队列，`free_page_reporting_idx() == 3`。
- **`test_process_reporting`**：驱动 reporting 队列处理
  - 未触发 eventfd → `event_fails` 计数 +1、页内容不变；
  - 触发后处理 → 上报的页对齐 range 被 `MADV_DONTNEED` 清零、descriptor 归还、
    `IrqType::Vring` 中断挂起、`free_page_report_freed` 增长一个页大小。

### 2.2 回归相关用例（确认未破坏既有行为）

```bash
cargo test -p vmm --lib balloon::persist::tests::test_persistence       -- --test-threads=1
cargo test -p vmm --lib balloon::device::tests::test_virtio_features    -- --test-threads=1
cargo test -p vmm --lib vmm_config::balloon::tests                      -- --test-threads=1
```

---

## 3. API / 配置 round-trip（需运行 firecracker，可无 guest 内核）

FPR 仅支持**启动时配置**，不能运行时开关。

```bash
# 启动 firecracker（略），配置 boot source / drives / machine-config 后：
curl --unix-socket /tmp/fc.sock -X PUT 'http://localhost/balloon' \
  -H 'Content-Type: application/json' \
  -d '{
        "amount_mib": 0,
        "deflate_on_oom": false,
        "stats_polling_interval_s": 1,
        "free_page_reporting": true
      }'

# 回显应包含 "free_page_reporting": true —— 验证 config 透传 + serde
curl --unix-socket /tmp/fc.sock 'http://localhost/balloon'
```

**通过标准**：
- `PUT` 返回 204；
- `GET /balloon` 的响应里 `free_page_reporting` 为 `true`；
- 不带该字段的旧配置仍可启动（`#[serde(default)]` 向后兼容，默认为 `false`）。

---

## 4. 端到端：RSS 下降（核心功能验证，需 KVM + 支持 FPR 的 guest 内核）

**前提**：guest 内核 `CONFIG_VIRTIO_BALLOON=y` 且支持 free page reporting
（Linux ≥ 5.7，含 `mm/page_reporting`）。

```bash
# 1) 用 free_page_reporting:true 启动并 InstanceStart（见第 3 节配置）

# 2) 宿主侧持续观察 firecracker 进程 RSS
FC_PID=$(pgrep -n firecracker)
watch -n1 "grep VmRSS /proc/$FC_PID/status"

# 3) 在 guest 内分配再释放大块内存
#    guest 内执行（任选其一）：
stress-ng --vm 1 --vm-bytes 512M --timeout 10s
# 或
python3 -c "x=bytearray(512*1024*1024); del x; import time; time.sleep(5)"
```

**通过标准**：guest 释放内存后，宿主侧 `VmRSS` 在数秒内**明显回落**
（FPR 关闭时 RSS 不会自动回落，可对照实验）。

---

## 5. 快照 save / restore（验证 persist 路径）

```bash
# Pause → CreateSnapshot
curl --unix-socket /tmp/fc.sock -X PATCH 'http://localhost/vm' \
  -H 'Content-Type: application/json' -d '{"state": "Paused"}'
curl --unix-socket /tmp/fc.sock -X PUT 'http://localhost/snapshot/create' \
  -H 'Content-Type: application/json' \
  -d '{"snapshot_path":"/tmp/snap","mem_file_path":"/tmp/mem","snapshot_type":"Full"}'

# 新进程 LoadSnapshot → Resume，然后：
curl --unix-socket /tmp/fc2.sock 'http://localhost/balloon'   # 仍应 free_page_reporting:true
```

**通过标准**：
- 恢复后 `GET /balloon` 仍 `free_page_reporting: true`；
- guest 继续上报、RSS 仍能回落（restore 时按 `avail_features` 中的 FPR 位
  重建了 reporting 队列，队列数与保存前一致）；
- 不报 `QueueRestoreError`。

---

## 6. 指标核对（宿主侧 metrics）

```bash
# 触发一次 metrics flush 后，从 metrics 文件中过滤：
grep -o '"free_page_report_[a-z]*":[0-9]*' <firecracker-metrics-file>
```

**通过标准**：
- `free_page_report_count` 随上报递增；
- `free_page_report_freed` 随释放字节数递增；
- `free_page_report_fails` 保持 **0**（若非 0，多半是 guest 上报了非页对齐的
  range，检查 guest 内核 / 大页配置）。

---

## 7. （可选）Python 集成测试

```bash
tools/devtool test -- integration_tests/functional/test_balloon.py
```

可在该文件新增一条：用 `free_page_reporting:true` 启动 → guest 分配/释放 →
断言宿主 RSS 下降 + `free_page_report_*` 指标变化。

---

## 8. 已知环境问题（非缺陷）

| 现象 | 原因 | 处理 |
|---|---|---|
| `test_attach_balloon_device` panic：`Cannot create Kvm: Kvm(Error(2))` | 无 `/dev/kvm` | 在 KVM 主机上跑 |
| `test_deflate` 偶发 `left:2 right:1` | 全局静态 `METRICS` 多线程争用 | 加 `--test-threads=1` |
| `cargo clippy` 报未安装 | 沙箱 toolchain 缺组件 | `rustup component add clippy` |

---

## 9. 验收清单

- [ ] `cargo build -p vmm` / `cargo test -p vmm --no-run` 通过
- [ ] `cargo fmt --all -- --check` 通过
- [ ] `cargo clippy -p vmm --all-targets -- -D warnings` 通过
- [ ] `cargo test -p vmm balloon -- --test-threads=1` 仅 KVM 用例失败（KVM 主机上全过）
- [ ] `test_free_page_reporting_layout` / `test_process_reporting` 通过
- [ ] `GET /balloon` 回显 `free_page_reporting: true`
- [ ] guest 释放内存后宿主 `VmRSS` 回落
- [ ] 快照恢复后特性 + 队列布局一致，RSS 仍回落
- [ ] `free_page_report_freed` 增长、`free_page_report_fails == 0`
