# App / UI / Loop / Session 实施清单

用户已明确授权整套架构分阶段落地 Rust。以下记录已落地边界与验收结果。

## 方案优先级

用户导出的 `app-loop-design.json`（2026-10-02）为字段选择基线；对话中明确确认的修订优先。导出中的空 access 表示尚未指定，不表示禁止其他使用者访问。

## 已确认目标

- App 负责装配，持有 UI 与 Loop。保留 AgentSurface、ToolProvider/ToolRegistry、TurnHook 等既有端口，不让 Runner 依赖 UI/App。
- UI 管全局交互及 Panel，Panel 管局部显示状态。Sidebar 持有自身区域。ActivityPage 合并 ActivityBrowserState 与 ActivityPanel。
- Loop 管跨回合调度、待处理请求、执行阶段与 Runner；待处理文本及图片作为完整请求处理。
- Session 是会话领域状态，包含 Conversation、权威 TodoList、诊断状态、执行设置和 checkpoint 管理。磁盘 DTO 与运行对象必须区别，不改变旧存档兼容性。
- 模型、work_mode、thinking_level、vision_enabled 跟会话走，App 配置给默认值，每轮使用设置快照。
- UI 的 TodoList 只是只读投影，修改经操作提交，不保留独立可写业务事实。
- tasks 与 agents 跟随会话关闭/切换停止并收尾；普通 Esc 只取消当前轮，然后按统一可启动条件继续队列，不等于关闭会话。
- checkpoint store 归会话，当前 checkpoint ID 归执行上下文。
- diagnostics_state 合入 lsp_configured，删除 App.diag 和单字段包装；reset 诊断结果不清除配置标记。
- MCP 管理者统一管理连接、连接状态、reload generation 与过期结果处理；管理者寿命覆盖重载。ProviderManager 管理模型发现状态。
- plan_phase 归 Loop，plan_review_selected 归 UI 计划确认栏。
- PeerInbox 管在线登记、收发、轻量回答、本地处理记账；Loop 管委派批准、排队、claim、取消；UI 统一提问交互；Session 保存归档记录。
- 批准不等于执行或完成，不跨重启恢复执行授权。关闭 PeerInbox 不删除持久化收件箱；不新增会话互相唤醒循环阻断。

## 阶段与验收

1. 基线与边界检查：已运行离线测试，855 单元测试通过、2 忽略，1 集成测试通过。
2. 已落地：App.ui / UiState、App.agent_loop / LoopState、SessionState；显式字段路径，没有 Deref 兼容伪装。Session 持权威 Conversation、Todo、诊断、执行设置、checkpoints、tasks/agents；磁盘 DTO 已更名 SessionSnapshot，JSON 兼容测试通过。
3. 已落地：完整 UserRequest 队列与阶段归 Loop；Loop 保留当前 Runner 快照和工作句柄。取消/失败部分响应由 Runner 归档，Panel 只交接显示缓存。任务与子 agent 有异步关闭屏障；关闭失败不恢复执行准入；/new 与 rewind fork 经过关闭与资源隔离。普通 Esc 仍保持当前轮取消与队列接续。
4. 已落地：Sidebar 区域、ActivityPage、Provider 状态、常驻 McpRuntime 重载代数、PeerInbox / Loop 授权 / UI 提问拆分。Todo 编辑通过操作提交，快照刷新收敛选择位置。
5. 最近验收：`CARGO_INCREMENTAL=0 cargo test --offline --quiet`，905 单元测试通过、2 忽略，1 集成测试通过；`cargo clippy --offline --all-targets -- -D warnings`、`cargo fmt -- --check`、`git diff --check` 通过。三套 Playwright 设计预览测试通过；新版关系图覆盖 15 个节点、22 条有向边及源码字段验证，桌面/窄屏截图已检查。

## 收尾完成

- 单一 hydration：main 获取 session 锁后加载 SessionSeed，App::new 只消费 seed，不再重复读磁盘。恢复历史、设置、tasks/todos/skills 与错误不覆盖存档已有回归测试。
- Panel mutation facade 已移除；App 调用 Session 的 Conversation 领域操作，再显式通知滚动与显示缓存。显示通知不修改历史/usage；更换 Conversation 时清空历史缓存，防止不同对象版本相同而复用旧文本。
- /clear 同样等待后台工作退出并隔离资源；任务事件转发器归 Loop 管理并在关闭时 join。替换 TaskManager 同步绑定 Panel，保持实时输出。
- 新版实际关系图：`architecture-ownership.html`，包含所有权与执行/共享两个视图、明确的箭头与关系标注。旧 HTML 及其 localStorage 草案保留，其基线测试校验旧提交而不是当前文件行号。
- 已更新 PROGRAMMER.md、README 与对应测试；没有新增依赖、提交或发布。

## 验证边界

验证是本地单元/集成、Clippy、格式及浏览器交互测试，未进行真实模型或远程 MCP 服务端到端验收。App 仍承担组合根和事件编排，LoopState/SessionState 类型仍定义于 app 模块；这不是额外拆 crate 的授权。

## 必须保护

实施前已有 PROGRAMMER.md、README.md、src/memory/dream.rs 用户修改，不覆盖。禁止提交、发布和额外源码备份。UI 外观与交互不因所有权重构擅自改动。

测试应覆盖恢复/保存兼容、旧事件不污染新会话、队列与草稿/审批阻塞、会话关闭的实际任务终止、取消后队列继续、只读 UI 投影、审批身份与一次性决定、MCP 过期重载及 provider 状态。每阶段记录真实结果，不将局部通过写成整套完成。
