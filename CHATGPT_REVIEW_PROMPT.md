# ChatGPT 审查提示词

请以资深 Windows/Rust/Tauri 安全与实时音频工程师的身份，审查这个公开仓库：

https://github.com/Xinyuexyyyyy/VoiceInput

审查提交：`cad542f`（如仓库后续有新提交，请明确说明你审查的 commit）。

## 背景与严格范围

这是 VoiceInput V1 的前两个切片，不是完整产品：

- Slice 0：最小 Windows-only Tauri 2 + Rust + React/TypeScript 骨架、MIT 许可、状态/错误和脱敏日志边界。
- Slice 1：火山引擎旧版 `App ID + Access Token` 的本地 Protocol spike。

明确不在本次范围内：设置页、托盘、状态胶囊、全局热键、文字写入、开机启动、正式安装包、历史记录、LLM、翻译、润色、多 ASR provider、本地模型或跨平台实现。请不要把缺少这些功能作为缺陷。

## 预期协议与行为

- Endpoint：`wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async`。
- 只使用旧版请求头：`X-Api-App-Key`、`X-Api-Access-Key`、`X-Api-Resource-Id`、`X-Api-Connect-Id`。
- 音频为 PCM、16 kHz、16-bit、mono；应按 200 ms 即 6,400 bytes 分包。
- 请求必须启用 `enable_itn`、`enable_punc`、`show_utterances`、`enable_nonstream`。
- 热词为请求级 JSON；去空、英文大小写不敏感去重，最多 80 个。
- 停止录音后先排空音频队列、发送协议 last frame，再最多等待 12 秒 final。
- partial 只能在内存中暂存，绝不能冒充 final、写入持久日志或作为最终文本输出。
- 必须区分凭据拒绝、限流、网络失败、麦克风失败、无 final 和 final 超时。

## 凭据与隐私红线

- 真实 App ID 和 Access Token 只能在本机环境变量或 `.gitignore` 排除的 `.env.local` 中。
- 代码、测试、日志、截图和审查回复都不得要求、展示、推测或复述真实凭据。
- 音频、部分文本和最终文本不得写入持久日志或文件；本 spike 允许只在当前终端临时显示最终文本供人工判断。

## 已执行的检查

- `npm run check`：通过。
- `npm run build`：通过。
- `cargo fmt --check`：通过。
- `cargo test`：6/6 通过。
- `cargo check`：通过。
- `.env.local` 和构建产物已验证不被 Git 跟踪。
- 未配置凭据时，spike 正确以 `credentials` 类别退出，未启动麦克风或网络会话。

真实验收尚未执行，不能视为通过：

- Gate A：尚未使用真实旧版凭据建立 WebSocket 并取得非空 final。
- Gate B：尚未完成五组中文上下文中英混合 `area` 真声测试；带热词达到至少 4/5 精确保留英文 `area` 前，不得进入 Slice 2。

## 请重点审查

1. `src-tauri/src/spike/protocol.rs`：握手、二进制帧、序列号、末帧、并发读写、重试、错误分类、final 解析、热词 payload 是否与上述约束一致；找出可能导致尾音丢失、假 final、连接悬挂、限流误重试或凭据泄露的问题。
2. `src-tauri/src/spike/audio.rs`：默认麦克风选择、不同采样格式、立体声 downmix、重采样、PCM 量化、停止录音和异步错误处理是否正确；重点检查音频是否可能静默丢失、格式错误或内存无界增长。
3. `src-tauri/src/bin/voiceinput-spike.rs`：录音开始/结束的显式命令、队列排空、final 超时、partial 边界、终端输出与错误路径是否正确。
4. `src-tauri/src/spike/credentials.rs`、`.gitignore`、`.env.example`、`safe_log.rs`：凭据加载优先级、路径、日志脱敏和 Git 忽略规则是否存在泄露风险。
5. `Cargo.toml`、`tauri.conf.json` 和许可证文件：依赖、Windows-only 约束、最小 Tauri 骨架、OpenLess MIT 归属是否合理。OpenLess 仅选择性改编了帧布局、旧版握手、PCM 分包、末帧收尾和 final 等待思路。

## 回复格式

请先列出发现，按严重度排序：`P0`、`P1`、`P2`。每项必须包含：

- 文件路径和精确行号；
- 可复现的失败/风险场景；
- 为什么违反上述协议、隐私边界或 Gate；
- 最小修复建议；
- 建议补充的测试。

然后单列：

1. 已做得正确且有证据支持的部分。
2. 因没有真实凭据或真声测试而无法判断的事项。
3. 是否可以认为代码“可进入 Gate A 测试”，以及是否允许进入 Slice 2。

不要把静态代码审查或编译通过写成 Gate A/Gate B 通过；不要建议扩展到本轮范围之外的产品功能。
