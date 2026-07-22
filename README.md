# VoiceInput

Windows 10/11 的极简 Tauri 2 + Rust + React/TypeScript 骨架。本仓库当前只完成：

- Slice 0：项目、MIT 许可、最小状态与脱敏日志边界；
- Slice 1：火山引擎旧版 `App ID + Access Token` 的本地 Protocol spike。

未实现设置页、托盘、状态胶囊、全局热键、文字写入、开机启动或安装包。

## 本地凭据

在本机编辑已忽略的 `.env.local`，填写：

```text
VOICEINPUT_VOLC_APP_ID=
VOICEINPUT_VOLC_ACCESS_TOKEN=
VOICEINPUT_VOLC_RESOURCE_ID=volc.seedasr.sauc.duration
```

也可使用同名环境变量。环境变量优先于 `.env.local`。真实凭据不得写入仓库、聊天、测试数据、截图或日志。

## Protocol spike

先安装依赖，再运行：

```powershell
npm install
cd src-tauri
cargo run --bin voiceinput-spike
```

程序会使用系统默认麦克风；按 Enter 开始录音，再按 Enter 结束。它只在本次终端临时显示服务端最终文本，不写入文件或日志。

Gate A：先用任意自然句跑出一次非空最终文本。

Gate B：分别执行五次无热词和五次带热词测试。每次带热词测试这样运行：

```powershell
cargo run --bin voiceinput-spike -- --hotword area
```

五句建议依次为：

1. 我需要调整这个 area 的大小。
2. 请把 area 的边界标记出来。
3. 这个 area 需要重新规划。
4. 我们先讨论 area 的颜色。
5. 把结果记录到 area 配置里。

带热词时至少 4/5 最终文本精确保留英文 `area`，才允许进入 Slice 2。测试结论只记录通过数和错误类别，不保存文本正文。

## 检查

```powershell
npm run check
npm run build
cd src-tauri
cargo fmt --check
cargo test
cargo check
```

`cargo check` 和真实麦克风测试只支持 Windows；本项目使用 Rust `stable` 工具链。
