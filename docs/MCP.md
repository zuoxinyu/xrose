# Xrose MCP 服务设计

**设计提案，尚未实现。** 本机 Agent 可以查看 Xrose 保存的配置和实际运行状态，解释某条规则、某个节点或某项设置为何生效，并在授权范围内提交变更。典型任务是启用一条分流规则，预先了解它对当前工作模式和内核的影响，最后确认正在运行的内核是否采用了这次编辑。

## 接入与状态归属

```mermaid
flowchart LR
    host["Agent 宿主"] -->|MCP / stdio| bridge["xrose-cli mcp serve"]
    bridge -->|本机 IPC| service["AppService"]
    desktop["桌面与托盘"] --> service
    service --> config["配置与设置"]
    service --> runtime["内核、组网与系统状态"]
```

宿主注册 stdio 服务时，启动配置为 `{"command":"xrose-cli","args":["mcp","serve"]}`。拟议命令支持 `--home <目录>`，CLI 按与桌面相同的 `AppPaths` 定位服务。IPC 在后台调用桌面的 `AppService`，共享内核操作锁和热重载流程。耗时操作登记 `busy` 后排队，界面照常读取快照。桥接进程退出只关闭自己的连接。首版连接已启动的桌面实例，桌面未运行时返回 `owner_unavailable` 和启动提示。后续无界面部署可由用户显式启动前台所有者，沿用同一套接口和互斥规则。

| 边界 | 设计决定 |
| --- | --- |
| 数据目录 | 使用 `AppPaths::instance_id()` 区分目录。在启用 MCP 的目录，所有者持有目录写锁。打开可写存储的 GUI 和 CLI 入口须在构造 `AppService::with_paths` 前参与协调。有所有者时，CLI 将支持的写操作转交它，其余写操作返回占用提示。多实例模式无法确定唯一所有者时，返回 `owner_conflict`。 |
| 本机 IPC | Unix 使用用户私有目录中的 Unix domain socket，双向核验对端用户。Windows 使用限当前用户的本机命名管道，校验对端 SID 并拒绝远端连接。端点与窗口唤醒通道、提权助手 IPC 各自独立。 |
| 权限归属 | 普通用户进程中的 `AppService` 核验授权、组装可见字段、执行操作。提权进程不发布此端点。桥接器仅转发有类型的请求。客户端自报的名称、工具注解、客户端传入的路径均不能授予访问权。 |
| MCP 兼容性 | 目标为 `2026-07-28` 和 `2025-11-25`。前者支持 `server/discover` 和逐请求版本元数据，后者使用 `initialize` 与 `notifications/initialized`。两者共享业务契约。stdio 的标准输出专用于协议消息，诊断经脱敏后送到标准错误。[版本兼容](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning) · [stdio](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio) · [旧版生命周期](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle) |

现有 CLI 的部分命令会打开自己的服务，构造时会初始化存储和后台任务。直接复用这些命令会得到另一份内存状态。`UiSnapshot` 还持有节点、节点订阅、日志和编译后的 JSON。MCP 应从组件所属的服务读取所需字段，在所有者进程内生成专用视图。[服务与快照](../crates/xrose-core/src/service.rs) · [路径与实例标识](../crates/xrose-core/src/paths.rs) · [单实例与窗口唤醒](../crates/xrose-core/src/system/single_instance.rs)

## 资源与组件覆盖

入口为 `xrose://local/catalog`。目录列出当前版本已实现、对当前授权可见的组件、URI、操作、参数约束和所需权限。固定入口包括 `status`（版本、活动配置、工作模式、内核与待应用状态）和 `profiles`（可访问的配置档）。配置资源使用 `profiles/{profile_id}/{component}` 或 `global/{component}`，观测资源使用 `runtime/{component}`。下表路径均省略 `xrose://local/` 前缀。

| 资源 | Agent 可获取与审查 | 配置编辑或运行操作的设计范围 |
| --- | --- | --- |
| `profiles`、`profiles/{id}`、`runtime/mode` | 配置档、活动项、保存与实际工作模式、默认出站和组网系统出口 | 创建、重命名、删除或切换配置档，选择直连、代理或组网出口。沿用运行中限制和至少保留一个配置档的约束。 |
| `profiles/{id}/nodes`、`chains`、`groups` | 节点来源、启用状态、协议支持、代理链跳点、出站组成员、策略、首选与实选节点 | 编辑手动节点、编排代理链、增删出站组、调整成员与切换策略。显式选择节点按其实际切换效果执行。直连虚拟节点和自动生成的组网出口标明来源。 |
| `profiles/{id}/routing`、`rule_sets` | 规则分组、有效顺序、匹配条件、最终动作、出站引用和规则集缓存状态 | 增删、启停、排序规则与规则分组，编辑规则集来源。下载或更新规则集是独立动作。 |
| `profiles/{id}/app_proxy`、`runtime/app_proxy` | 在提供分应用代理的版本中，读取启用状态、已登记应用与出站组的绑定、平台可用性 | 调整已登记应用的出站组，预览对新连接的影响。系统接管需要系统控制权限，应用身份由本机选择。 |
| `profiles/{id}/dns` | 上游、分域、最终服务器、Hosts、Fake-IP 和出站依赖 | 编辑 DNS 预设、服务器、Hosts 或最终服务器。保留标签唯一、最终服务器有效和引用保护。 |
| `profiles/{id}/inbounds`、`global/settings`、`runtime/system_proxy` | 有效入站、端口、LAN 认证是否配置、TUN、系统代理和 PAC 的意图与实况，以及外观、语言、托盘和通知设置 | 编辑已声明的设置项。mixed 端口与 TUN 开关写入权威的 `Settings`，由 `sync_inbounds` 派生入站。系统代理和 PAC 沿用系统应用与还原流程。 |
| `global/subscriptions` | 节点订阅的启用状态、更新周期、最近结果、配额和节点数 | 编辑名称、周期、启用状态，刷新指定订阅。刷新绑定位于计划中的活动配置，并列出可能替换的订阅节点。订阅来源通过本机私密输入设置。 |
| `global/mesh`、`runtime/mesh` | EasyTier / Tailscale 配置、会话状态、可用出口、子网、转发规则与共享状态摘要 | 编辑组网配置、重连策略和转发规则，连接、断开或选择出口。区分分流出口与接管系统流量的组网模式。登录和共享码交由本机用户处理。 |
| `runtime/core`、`runtime/helper`、`runtime/updates` | 内核种类、已登记版本、进程状态、能力、提权助手可用性和更新状态 | 选择已登记的内核版本，启动、停止、校验或应用配置。安装提权助手、指定外部可执行文件和应用升级保留在本机管理入口。 |
| `runtime/health`、`connections`、`traffic`、`diagnostics`、`logs` | 节点健康与 HTTP 测速、连接和失败分类、流量聚合、已有诊断结果、脱敏日志事件 | 按已知节点执行 HTTP 测速、按连接 ID 断开、运行已定义的诊断。采集开关和探测预算归入设置。流量计数、健康分、日志与进程状态均由数据源产生。 |

配置档资源可以按 ID 读取，编辑活动配置时必须显式给出其 ID。非活动配置的读取不会切换桌面配置。节点订阅、组网配置和客户端设置各有独立的全局授权范围。只具备某个配置档的权限时，相关全局对象仅返回获准的引用摘要。公开标识随对象保持稳定。DNS 标签、设备标识或连接地址不适宜直接公开时，服务返回授权范围内的不透明 ID，供后续读取和编辑使用。

资源返回 `uri`、`observed_at`、`availability` 和 `data`。可编辑资源带 `revision`，可执行动作的资源带 `state_token`。状态摘要分别给出 `desired`（已保存意图）、`effective`（已观测实况）、`config_dirty` 和 `auto_reload_pending`。内核停止或缺少观测时，`effective` 中对应值为 `null` 并附原因。例如 Xray 的连接明细标为 `unsupported`，采集关闭标为 `not_collected`，sing-box 成功采集到零条连接才返回可用的空列表。

服务只解析已登记的 `xrose://local/` URI。列表默认每页 50 项，上限 200 项，返回 `next_cursor`。游标绑定授权、查询和快照，续页数据已失效时返回 `stale_cursor`，提示重读第一页。对每份授权限制未提交计划数、返回字节和并发任务数。繁忙时返回 `rate_limited` 及重试等待时间。

## 工具契约

MCP 发布 `resources/list`、`resources/templates/list`、`resources/read` 和下列四个工具。只支持工具的宿主可从 `xrose_read` 的说明取得目录 URI。它与 `resources/read` 使用相同的读取、分页、鉴权和脱敏逻辑。首版通过轮询读取变化。含用户数据的结果仅限当前授权使用，现代协议的相应资源读取返回 `cacheScope: private` 和 `ttlMs: 0`。[MCP 资源](https://modelcontextprotocol.io/specification/2026-07-28/server/resources) · [MCP 工具](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)

| 工具 | 输入 | 结果 |
| --- | --- | --- |
| `xrose_read` | `uri`，可选 `cursor`、`limit` | 组件、对象或操作回执的结构化视图。资源 URI 可用同名查询参数翻页。 |
| `xrose_review` | `uri` 和 `plan_id` 二选一。配置审查可指定 `cores`，路由审查可带本地匹配样本 `sample` | 检查阶段、目标内核、发现项、未执行的检查和原因。可审查现状或候选，不改配置，也不发起网络探测。 |
| `xrose_plan` | 配置编辑传 `kind=config`、`target`、`base_revision`、有序 `changes`。运行动作传 `kind=action`、`target`、`expected_state` 和一个 `action` | 脱敏的逐字段差异、连带影响、所需权限和审查结果。有效候选获得限时 `plan_id`。只读授权也可以预览其可读范围内的变更。 |
| `xrose_commit` | 仅 `plan_id` | 提交同一份已审查的内容，返回 `operation_uri`。从该 URI 读取保存结果、生效结果和失败原因。 |

工具声明 `inputSchema` 和 `outputSchema`。工具结果使用 `structuredContent`，同时在 `content` 提供同一脱敏结果的文本表示。每个 `op` 对应一个有类型的参数分支，拒绝未知字段。首批契约包含节点启停、出站组成员、规则启停、最终 DNS 服务器及显式应用配置。对象由 ID 标识，排序操作使用相邻对象 ID。公开字段与双内核适配器使用的原始配置相互独立。[首批计划输入的 JSON Schema](mcp/plan.schema.json)

协议报文格式错误及未知工具使用 JSON-RPC 错误。工具执行失败使用 `isError: true`，业务结果为 `{ok: false, error: {code, message, retryable}}`，有定位信息时附对象 URI 和字段。服务层沿用 `crate::error::Error`，对外提供稳定错误码和可操作的中文说明。例如 `stale_revision` 要求重读后重新预览，`busy` 可稍后重试，`forbidden` 指明缺少的授权，`unsupported` 描述受限的内核或平台。跨授权访问的私有计划和操作按未找到处理。

## 审查、提交与生效

- **对象与引用。** 检查对象是否存在、是否允许编辑，以及最终候选的引用关系。例如删除仍被 DNS 上游使用的出站组，返回 `group_in_use`、上游对象引用和“请先更改该上游的出站组”。检查也覆盖禁用规则、代理链及其他组件持有的引用。[模型校验](../crates/xrose-core/src/model/mod.rs) · [DNS 校验](../crates/xrose-core/src/dns.rs)
- **实际编译效果。** 用工作模式、目标内核版本及组网上下文在内存中编译候选，把 `CompiledConfig::warnings` 的降级语义映射成结构化发现项。例如 Xray 的 Fake-IP 降级是警告。sing-box 沿用现有的 1.12+ 适配，版本未知时列出无法确定的能力。本地路由样本复用 `route::evaluate`，依赖规则集而无法判定时返回 `needs_core`。[路由预览](../crates/xrose-core/src/route.rs)
- **内核自身校验。** `core_check` 单独标记为 `passed`、`failed` 或 `not_run`。静态审查不调用会写运行目录的 `CoreManager::prepare`。显式内核校验应使用已登记内核和私有临时文件，缺少二进制或资产时说明原因。[内核准备与校验](../crates/xrose-core/src/core/mod.rs)

每条发现项给出稳定的 `code`、`severity`（`error`、`warning` 或 `info`）、对象 URI、相关字段、中文原因和修正建议。适配器原始错误或警告可能含用户输入，输出时走与资源相同的字段保护。其他内核的对照结果单列，提交依据实际目标内核判定。

1. **取得一致基线。** 所有者为持久化域维护带实例代次的单调版本，从同一读取边界返回数据和版本。同一配置档的组件共享配置档修订。GUI、CLI、节点订阅刷新和会持久化选择的后台策略均须经过相同的写入边界。长时间打开的编辑表单同样检查基线，后台刷新仅合并它负责的字段。秒级 `updated_at` 不能区分连续编辑。流量采样不推进配置版本。运行资源的 `state_token` 绑定与动作有关的状态，例如 `core.apply` 绑定内核运行代次、活动配置及将要应用的修订。
2. **生成计划。** 在副本上按序编辑，复用领域校验，计算最终差异。计划绑定授权、数据目录、活动配置、依赖版本、已审查的副作用以及到期时间。单计划限 32 项，首版有效期 5 分钟。只接受属于同一持久化事务的配置编辑，运行操作单独建计划。跨 SQLite 与设置文件的组合在预览时返回 `unsupported_transaction`。
3. **在所有者处提交。** 在同一写入临界区内重验授权、基线、动作前置条件及副作用。界面已切换配置、内核状态改变导致新增副作用、依赖项已更新时，旧计划失效。配置档批量操作只保存一次，持久化成功后才发布内存状态，再按生效范围触发一次应用。跨表级联复用加密 SQLite 事务，设置使用原子文件写入。具有跨存储连带修改的命令，须先具备可恢复的专用服务入口才加入目录。[出站组编辑](../crates/xrose-core/src/service/outbound_groups.rs) · [事务式导入](../crates/xrose-core/src/service/config_import.rs) · [存储事务](../crates/xrose-core/src/storage/mod.rs)
4. **跟踪结果。** 所有者在产生副作用前原子登记提交和操作回执，并发提交同一计划只执行一次。同一授权重连后可再次提交原计划取回同一回执。运行中的回执保留至完成，终态至少保留 1 小时，优先于计划到期检查。所有者重启后旧计划失效，无法证明是否完成的提交返回 `outcome_unknown`，调用方先核对配置与实况。耗时任务由服务后台执行，回执取自该次任务的完成结果。全局 `busy` 和桌面提示仅用于界面反馈。

回执分别报告任务阶段 `phase`（`queued`、`running`、`succeeded` 或 `failed`）、保存结果 `persistence` 和生效结果 `application`。内核停止时，保存编辑返回 `application=not_running`。运行中且热重载关闭时，保存任务以 `succeeded` 和 `pending` 结束，可另建 `core.apply` 计划。纯客户端设置返回 `not_required`。

热重载开启时，先校验候选，再替换进程。校验失败保留原进程，替换失败尝试恢复上一次运行配置。回执区分 `validation_failed`、`restored`、`restore_failed`，并保留新保存的编辑供修正。只有确认运行中的内核采用目标版本时才报告 `applied`。若防抖合并期间新编辑替代了目标，回执报告 `superseded` 和实际采用的版本。[现有热重载行为](../crates/xrose-core/src/service/reload.rs)

## 一次完整调用

假设 sing-box 正在运行且热重载已开启。以下标识和返回值均为虚构示例，JSON 展示工具调用的 `name` 与 `arguments`。Agent 先用 `xrose_read` 读取 `xrose://local/profiles/p_demo/routing`，得到修订 `rev_demo_1`，发现规则 `rule_demo` 已禁用。

```json
{
  "name": "xrose_plan",
  "arguments": {
    "kind": "config",
    "target": "xrose://local/profiles/p_demo",
    "base_revision": "rev_demo_1",
    "changes": [
      {"op": "routing.set_rule_enabled", "rule_id": "rule_demo", "enabled": true}
    ]
  }
}
```

计划结果给出这次修改需要的权限和对运行中内核的影响。

```json
{
  "plan_id": "plan_demo",
  "expires_in_seconds": 300,
  "diff": [
    {"object_id": "rule_demo", "field": "enabled", "before": false, "after": true}
  ],
  "effects": ["reload_core"],
  "required_scopes": ["config.write", "runtime.control"],
  "review": {"core": "sing_box", "findings": [], "core_check": "not_run"}
}
```

Agent 可以用 `xrose_review({"plan_id":"plan_demo"})` 查看完整发现项，也可以依据已返回的审查结果提交。

```json
{"name": "xrose_commit", "arguments": {"plan_id": "plan_demo"}}
```

提交返回 `operation_uri`。Agent 使用 `xrose_read` 查询该 URI，示例终态如下。若用户在预览后先改了该规则，提交会返回 `stale_revision`，用户的修改得以保留。

```json
{
  "uri": "xrose://local/operations/op_demo",
  "observed_at": "2026-01-01T00:00:00Z",
  "availability": "available",
  "data": {
    "phase": "succeeded",
    "persistence": "saved",
    "saved_revision": "rev_demo_2",
    "application": "applied",
    "applied_revision": "rev_demo_2",
    "core_check": "passed"
  }
}
```

## 授权与隐私

用户在 Xrose 本机设置中启用服务，为宿主选择组件和权限，签发或撤销授权。服务默认关闭，新授权默认只读。授权绑定数据目录、配置档或全局组件。桥接器从宿主私密环境继承 `XROSE_MCP_GRANT`，所有者只保存其校验摘要。授权值不进入工具参数、资源 URI 或日志。撤销即时作用于后续读取、提交和排队动作。该授权约束经 MCP 发起的操作，操作系统账户仍是本机信任边界。

| 权限 | 可见内容或可执行操作 |
| --- | --- |
| `read` | 脱敏资源、静态审查和计划预览。默认显示对象引用、类型、启停、顺序、计数及引用关系。 |
| `read.details` | 仅按获准组件展示审查所需的非凭据字段，例如规则的匹配域名和网段。桌面隐私开关不能扩大该授权。 |
| `config.write` | 提交允许编辑的持久化字段。全局组件需要相应的全局授权。 |
| `runtime.control` | 启停内核、应用配置、HTTP 测速或断开指定连接。配置编辑会触发热重载时同时需要此权限。 |
| `system.control` | 改变系统代理、TUN、LAN 暴露或组网系统出口。涉及保存及运行的动作还需对应的写入和运行权限，沿用现有提权路径。 |

权限按整项变更及其连带效果计算。例如刷新节点订阅可能写入节点，打开热重载可能应用已有待生效修改。即使内核已停止，保存将来会放开 LAN 监听的设置也需要 `system.control`。若启动或应用配置需要安装提权助手，返回 `requires_local_setup` 并说明需在本机完成的准备工作。权限不足时可以查看获准范围内的预览，提交不会写入。服务不提供通用 JSON 路径写入、任意程序、文件读取或助手 IPC 转发。

读取、差异、审查发现、错误和回执共用字段白名单。节点凭据、订阅链接及请求头、私钥、共享码、完整编译配置和解密备份均不进入 MCP。缺省字段在编辑时原样保留，脱敏占位符不可写回。创建带凭据的节点、替换订阅来源或更改已绑定凭据的目的地时，由本机私密输入完成绑定，Agent 仅使用绑定到目标和用途、短时单次有效的引用。节点名称和日志正文按不可信数据处理，无法可靠脱敏的日志只给出事件类别、等级、次数和时间。审计只记操作类型、时间、结果和本次操作编号。

## 实施顺序与验收

1. **只读接入。** 在 `xrose-core` 的服务层增加组件视图和有类型的本机 IPC，在 `xrose-app` 注册普通用户端点。在 `xrose-cli` 接入官方 Rust SDK 的服务端与 stdio 功能，依赖统一声明在工作区。先打通目录、状态、配置档、路由、DNS、节点与出站组的读取和静态审查。两个协议版本和仅支持工具的客户端都应读到相同结果。[官方 Rust SDK](https://github.com/modelcontextprotocol/rust-sdk)
2. **活动配置编辑。** 接入共同写锁、修订检查、批量事务、计划与回执，实现首批输入契约。为现有仅发提示或就地修改模型的方法提取可返回 `Result` 的服务入口，供 GUI 与 MCP 共用。验证审查前后存储、运行配置和进程均保持一致。用两个客户端和一次 GUI 编辑验证并发冲突，用存储故障验证整批回滚，用丢失提交响应验证重试只产生一次操作。
3. **补齐组件与运行控制。** 按表扩展全局设置、节点订阅、组网、系统代理和诊断。每个新操作随同字段投影、依赖检查、所需权限与真实完成回执发布。覆盖热重载关闭、候选校验失败、进程恢复失败、Xray 缺少连接明细、撤销授权以及同目录多实例。测试遍历资源、差异、错误、日志与标准错误，确保私密测试标记均不可见。

集成验收使用临时 `--home`、模拟内核和本机 IPC，不依赖网络、显示器或管理员权限。两个隔离目录必须只能看到各自的状态。普通构建还须在新的空目录运行 `xrose-cli mesh list --home <目录>` 并得到 `[]`。
