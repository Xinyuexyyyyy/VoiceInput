# Slice 0/1 验证记录

日期：2026-07-26

## 已完成的静态验证

| 检查 | 结果 |
| --- | --- |
| `npm run check` | 通过 |
| `npm run build` | 通过 |
| `cargo fmt --all --check` | 通过 |
| `cargo test --locked` | 17/17 通过 |
| `cargo +1.88.0 test --locked` | 17/17 通过 |
| `.env.local` Git 忽略规则 | 通过 |
| 构建产物 Git 忽略规则 | 通过 |
| 未配置凭据失败路径 | 通过，分类为 `credentials`；未启动麦克风或网络会话 |

## 审查修复

- 热词字段已改为 `request.corpus.context`，并验证不存在旧的 `request.context`。
- WebSocket 每次写入有 5 秒 deadline；结束阶段的排空与末帧共享 5 秒总 deadline，超时归类为 `network`。
- 音频与服务端帧通道改为有界；音频回调队列满时立即报告 `audio_backpressure`，不静默丢帧。
- CPAL 异步设备失败会通知主循环立即停止；48 kHz 等降采样路径在重采样前经过跨 callback 保持状态的低通 FIR。
- `licenses/OpenLess-LICENSE` 包含 OpenLess 原始 MIT 版权与许可文本；MSRV 已提升并实测为 Rust 1.88，Windows CI 会同时验证 Rust 1.88 与 stable。
- 为 Rustls 0.23 显式安装 `ring` crypto provider，避免 Windows 上 TLS 握手因缺少默认 provider 发生 panic。
- 服务端会在识别前发送 `FullServerResponse` / `flags=0` 的 JSON 确认帧；该帧现在被安全忽略，只有 `flags=3` 的结果会作为 final。
- `--verify-area` 只输出 `final_nonempty` 和 `exact_token_preserved` 布尔值，用于真声验收时避免把转写正文带入输出或记录。

## Gate A：真实旧版凭据

状态：**Pass**

使用已忽略的本地凭据完成真实 WebSocket 会话。会话录制并上传 119 个 200ms PCM 音频帧，进程以 `completed` 结束；该状态仅会在收到非空 final 后发出。凭据和转写正文均未进入仓库、日志、截图或本验证记录。

最小复测命令：

```powershell
cd H:\workspace-daily\voice-input\src-tauri
cargo run --bin voiceinput-spike
```

在项目根目录的已忽略 `.env.local` 填入本地凭据后运行。程序按 Enter 开始录音、再按 Enter 结束；只有服务端 final 会临时显示在终端。

## Gate B：`area` 真声测试

状态：**Fail**

已完成五组中英混合真声对照。每一有效样本都收到非空 final；以下表格不保存转写正文，只记录 final 是否非空及 `area` 是否精确保留英文。

| 组 | 句子 | 无热词 final 非空 | 无热词 `area` 精确 | 热词 final 非空 | 热词 `area` 精确 |
| --- | --- | --- | --- | --- | --- |
| 1 | 我需要调整这个 area 的大小。 | 是 | 否 | 是 | 否 |
| 2 | 请把 area 的边界标记出来。 | 是 | 否 | 是 | 否 |
| 3 | 这个 area 需要重新规划。 | 是 | 否 | 是 | 否 |
| 4 | 我们先讨论 area 的颜色。 | 是 | 否 | 是 | 否 |
| 5 | 把结果记录到 area 配置里。 | 是 | 否 | 是 | 否 |

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

热词组精确保留结果为 **0/5**，未达到至少 4/5 的硬闸门。已复核 `enable_nonstream`、`request.corpus.context` 热词 JSON、服务端 `flags=3` final 选择和默认 Resource ID；在参数正确但结果仍失败的情况下，按 PRD Stop Condition 停止后续 Slice，并重新评估 ASR 路线。不得进入 Slice 2。
