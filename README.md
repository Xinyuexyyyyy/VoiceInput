# VoiceInput

Windows 10/11 的极简 Tauri 2 + Rust + React/TypeScript 语音输入原型。本仓库当前已完成：

- Slice 0：项目、MIT 许可、最小状态与脱敏日志边界；
- Slice 1：火山引擎旧版 `App ID + Access Token` 的本地 Protocol spike。
- Slice 2/3：单会话 Toggle、取消、120 秒上限、最终结果的 Windows 粘贴与剪贴板降级。

未实现设置页、托盘、状态胶囊、Windows Credential Manager、开机启动或安装包。

## 本地凭据

在本机编辑已忽略的 `.env.local`，填写：

```text
VOICEINPUT_VOLC_APP_ID=
VOICEINPUT_VOLC_ACCESS_TOKEN=
VOICEINPUT_VOLC_RESOURCE_ID=volc.seedasr.sauc.duration
```

也可使用同名环境变量。环境变量优先于 `.env.local`。真实凭据不得写入仓库、聊天、测试数据、截图或日志。

`.env.local` 只用于当前开发原型；正式应用会改用 Windows Credential Manager，不把本地明文文件作为发布方案。

## 开发原型

在项目根目录运行：

```powershell
npm run tauri dev
```

默认快捷键为 `Alt+Space`：首次按下开始，第二次按下结束并等待最终结果。该组合键会覆盖 Windows 的窗口系统菜单行为，只由 VoiceInput 消费；会话活动期间可按 `Esc` 取消。触发后会显示不抢焦点的状态提示框，明确展示启动、聆听、生成最终文字、写入/复制、失败或取消状态。最终结果只尝试写入开始录音时的同一前台窗口；窗口变化、模拟粘贴失败时会复制到剪贴板。界面不会显示或保存转写正文。

已知限制：`area` 热词真声测试结果为 0/5。用户已授权先推进基础可用性，但本项目不宣称英文 `area` 保留问题已经解决。

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

需要在不显示转写正文的情况下记录 Gate B 布尔结果时，使用：

```powershell
cargo run --bin voiceinput-spike -- --verify-area
cargo run --bin voiceinput-spike -- --verify-area --hotword area
```

`--verify-area` 只输出 final 是否非空以及英文 `area` 是否作为精确标识符出现；它不会输出转写正文。

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
