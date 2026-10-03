# 当前所有权图

打开 `design-demos/architecture-ownership.html`。独立、离线单 HTML，无依赖、外部字体、网络请求或 localStorage；不修改旧版 `architecture-review.html` 的字段与草案。

## 阅读方式

- **所有权**：12 个节点，从 App 向 UiState / LoopState / ProviderManager / McpRuntime 展开；LoopState 直接持有 SessionState 与 PeerInbox。
- **执行与共享**：9 个节点，展示客户端解析、Runner 的 Conversation 借用、AgentSurface 回调的 TUI 适配与 MCP 工具注册表分支。
- 共 **15 个不同节点、22 条有方向且带标注的边**。实线是直接值所有权；虚线是 Arc 共享、借用或调用，不把 Arc 指向的资源画成独占值。
- 点击节点或用 Tab + Enter / Space 选中；方向键、Home、End 导航。节点选择器会定位窄屏画布。详情显示职责、真实字段、双视图关联和源码路径 / 符号。
- 窄屏保留 1160px 画布以维持标签可读性，使用局部双向滚动；页面本身不横向溢出。

## 核实边界

数据来自当前工作树，不是未来方案。源码定位以路径 + 类型 / 字段 / 表达式为准，不固定易漂移的行号。

- `src/app/mod.rs`：App、UiState、LoopState、SessionState 及 `build_runner`。SessionState 是会话权威入口；其 Conversation 与 todos 通过 Arc 共享。
- `src/ui/components/conversation_panel/conversation_panel.rs`：ConversationPanel 的 `conversation` 共享句柄和显示状态仍真实存在；没有宣称 Panel 完全不接触领域模型。
- `src/runner/mod.rs`：TurnRunner 没有 App / UiState 字段，生产代码不引用 app / ui；`run_turn` 接收 Conversation、CancellationToken、AgentSurface。集成测试使用 TuiSurface，不属于生产依赖。
- `src/app/surface.rs` 与 `src/app/events/mod.rs`：TuiSurface 实现 AgentSurface，通过事件通道发送带 operation_id 的 AppEvent，事件处理更新面板。图上 Runner → TuiSurface 表达经 trait 的回调运行路径，不是具体类型依赖。
- `src/providers/mod.rs`：ProviderManager 指模型服务管理器。客户端由 App 组装时克隆给 Runner，ProviderManager 本身不执行 Runner。
- `src/mcp/runtime.rs`、`src/mcp/mod.rs`、`src/tools/provider.rs`：McpRuntime 是稳定重载所有者；已有 Runner 可通过工具 Provider 保留旧连接 Arc 快照。
- `src/app/peers.rs`：PeerInbox 属于 LoopState；本地授权在独立的 `peer_delegations`，不伪装成持久化数据。
- `src/app/session.rs`、`src/session/mod.rs`：SessionSnapshot 是构建并保存的磁盘数据，不是 SessionState 的嵌套字段；输入历史和 suggestion 仍来自 UI。仅画保存方向，不猜测并行重构中的 hydration 流程。

图有意只列代表性字段、仅展开 MCP 工具分支，不代表完整字段清单或所有调用路径。

## 回归验证

使用已有 npm 缓存和 Chromium，不安装项目依赖：

```sh
npm exec --offline --package=playwright -- sh -c 'NODE_PATH="$(dirname "$(dirname "$(command -v playwright)")")" node design-demos/architecture-ownership.test.cjs'
```

测试逐次从磁盘检查所有节点的准确 struct 内字段与每条边的源码表达式，检查生产 Runner 不反向依赖 App / UI，并验证：

- 两视图的节点 / 边数量、ID 唯一、边端点落在正确节点边界、箭头存在、实虚线语义；
- 路径不穿过节点，边标签不遮挡节点；
- 每个节点的点击、详情与源码显示，方向键 / Home / End / Enter、选择器和重载；
- 1440px 桌面与 390px 窄屏无页面横向溢出，窄屏定位可用；
- 离线模式，零 HTTP(S) 请求、零页面 / 控制台错误、零 localStorage 写入。

实际测试通过，四张截图已生成并通过 `read_image` 逐张目检：

- `architecture-ownership-ownership.png`
- `architecture-ownership-execution.png`
- `architecture-ownership-ownership-mobile.png`
- `architecture-ownership-execution-mobile.png`

本任务仅新增预览、测试、本文档和截图，不修改 Rust 或根文档。`PROGRAMMER.md` / 根 README 的整体重构事实由主 agent 统一同步；此预览不改变应用行为。
