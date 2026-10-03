# App / Loop 所有权设计台

> 本页及 HTML 保留 `0715627` 重构前快照，不代表当前 Rust 实现；原 localStorage 草案不重置。当前结构见 [新版所有权图](architecture-ownership.html)，验收见 [实施清单](architecture-implementation.md)。

直接打开 `app-loop-workbench.html`，离线使用，不改变 Rust。

- 左侧列出生成时 `src/app/mod.rs::App` 的全部 61 个直接字段，带类型、源码位置及原有注释。另列 ConversationPanel 的四个拆分候选和 App 构造的 TurnRunner，共 66 项。
- 右侧在 App 容器中并列展示保留字段、UI 与 Loop，避免 Loop 被长字段清单挤到屏幕外。TurnRunner 的内部草案跟随其归属移动。
- 从左侧或右侧拖到 App / UI / Loop / TurnRunner；也可使用归属下拉框，支持键盘和触屏。左侧事实不删除。
- 主归属表示生命周期管理责任，不表示独占访问。展开每项的「其他使用方」，可为其他层指定只读、读写或调用；不自动选择 Arc、借用或消息传递。访问关系独立持久化，导出 JSON v2 同时包含归属与关系，保留旧版归属草案。
- 初始方案按讨论设置：quit_requested_at 归 UI；conversation、pending_message、phase、TurnRunner 归 Loop；receiving_response 归 TurnRunner。其他字段暂留 App，不代表已经审查其归属。
- 搜索只过滤左侧。自动保存到当前浏览器 localStorage，失败会提示。导出 JSON 可留档；目前不支持导入。恢复起点需确认。
- 这是字段归属草案，不执行重构，不检查依赖闭包；嵌套字段拆出后，父对象表示剩余职责。实际共享权限、生命周期和业务行为仍需另行设计。

## 验证

```sh
npm exec --offline --package=playwright -- sh -c 'NODE_PATH="$(dirname "$(dirname "$(command -v playwright)")")" node design-demos/app-loop-workbench.test.cjs'
```

覆盖源码 App 字段完整性、真实拖拽、UI 分层、多使用方访问关系的保存与导出、下拉迁移、Runner 内部跟随、刷新恢复、搜索、导出 JSON、确认/取消重置、窄屏溢出与离线无浏览器错误。生成 `app-loop-workbench.png`。

仅新增设计预览和测试，未改变运行时架构、工具链或配置；根 `PROGRAMMER.md` 与 `README.md` 无需因本次更新而修改，保留其已有修改。
