# Slice 0/1 验证记录

日期：2026-07-23

## 已完成的静态验证

| 检查 | 结果 |
| --- | --- |
| `npm run check` | 通过 |
| `npm run build` | 通过 |
| `cargo fmt --check` | 通过 |
| `cargo test --locked` | 9/9 通过 |
| `cargo check --locked` | 通过 |
| `cargo +1.88.0 check --locked` | 通过 |
| `.env.local` Git 忽略规则 | 通过 |
| 构建产物 Git 忽略规则 | 通过 |
| 未配置凭据失败路径 | 通过，分类为 `credentials`；未启动麦克风或网络会话 |

## 审查修复

- 热词字段已改为 `request.corpus.context`，并验证不存在旧的 `request.context`。
- WebSocket 每次写入有 5 秒 deadline；结束阶段的排空与末帧共享 5 秒总 deadline，超时归类为 `network`。
- 音频与服务端帧通道改为有界；音频回调队列满时立即报告 `audio_backpressure`，不静默丢帧。
- CPAL 异步设备失败会通知主循环立即停止；48 kHz 等降采样路径在重采样前经过跨 callback 保持状态的低通 FIR。
- `licenses/OpenLess-LICENSE` 包含 OpenLess 原始 MIT 版权与许可文本；MSRV 已提升并实测为 Rust 1.88，Windows CI 会同时验证 Rust 1.88 与 stable。

## Gate A：真实旧版凭据

状态：**Blocked**

本机尚未配置非空的旧版 App ID 与 Access Token，因而没有尝试建立会话，也没有取得 final。未记录、输出或上传任何真实凭据。

最小复测命令：

```powershell
cd H:\workspace-daily\voice-input\src-tauri
cargo run --bin voiceinput-spike
```

在项目根目录的已忽略 `.env.local` 填入本地凭据后运行。程序按 Enter 开始录音、再按 Enter 结束；只有服务端 final 会临时显示在终端。

## Gate B：`area` 真声测试

状态：**Blocked**

依赖 Gate A 的真实 WebSocket 会话；尚未进行真声测试。以下表格不保存转写正文，只记录 final 是否非空及 `area` 是否精确保留英文。

| 组 | 句子 | 无热词 final 非空 | 无热词 `area` 精确 | 热词 final 非空 | 热词 `area` 精确 |
| --- | --- | --- | --- | --- | --- |
| 1 | 我需要调整这个 area 的大小。 | 未运行 | 未运行 | 未运行 | 未运行 |
| 2 | 请把 area 的边界标记出来。 | 未运行 | 未运行 | 未运行 | 未运行 |
| 3 | 这个 area 需要重新规划。 | 未运行 | 未运行 | 未运行 | 未运行 |
| 4 | 我们先讨论 area 的颜色。 | 未运行 | 未运行 | 未运行 | 未运行 |
| 5 | 把结果记录到 area 配置里。 | 未运行 | 未运行 | 未运行 | 未运行 |

无热词运行：

```powershell
cd H:\workspace-daily\voice-input\src-tauri
cargo run --bin voiceinput-spike
```

带热词运行：

```powershell
cd H:\workspace-daily\voice-input\src-tauri
cargo run --bin voiceinput-spike -- --hotword area
```

带热词结果至少 4/5 精确保留 `area` 前，不得进入 Slice 2。
