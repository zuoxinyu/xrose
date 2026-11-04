# DESIGN.md — Xross UI 规范

---

## 0. 使用规则

1. **MUST** = 违反即 bug；**SHOULD** = 无充分理由不要偏离。
2. 条目后的 `⚠K##` 表示"当前实现偏离目标"，见 §11。按目标做，**不要照抄当前的错误**。
3. 不要发明新样式。先复用 §5 的组件；缺组件时先提议（Problem / 已有组件为何不够 / 用到的 token），再实现。
4. 颜色、间距、圆角、字号只能引用 token。组件代码里出现裸 hex / 魔法数字即不合格。
5. 截图与本文冲突时，不要静默二选一：在 PR 说明里写出冲突点。
6. 文中 CSS 变量为对实现的映射：§3 的数值取自 `ui/theme.rs`、`ui/widgets.rs`、`window_effects.rs` 与 gpui-kit 0.8.1 默认主题；与实现冲突时以源码为准。

---

## 1. 产品与气质

Xross：桌面端代理客户端 GUI，内核可选 sing-box / Xray。页面：节点、订阅、路由、DNS、组网、连接、测试、日志、设置。

性质：**工具型、信息密集、低频配置 + 高频看状态**。

气质：克制、冷灰底 + 白卡片、单一蓝绿 accent、系统字体、macOS 原生感（侧栏 vibrancy）。

禁止：渐变、装饰插画、营销式大标题、纯装饰动效、饱和度高的大色块。

---

## 2. 原则

1. **状态先于装饰**：core 运行状态、当前选中项、启用 / 禁用，快速可辨。
2. **一种语义，一种控件、一个图标、一种颜色。**
3. **危险操作远离主操作，且可撤销。**
4. **颜色不单独承载信息**（色弱、灰度截图）：必须有图标、文字或形状冗余。
5. **高频可见，低频收进 `⋯`。**
6. **有副作用的操作**（重启 core、切节点会断连）操作前后都要有反馈。
7. **跨平台**：不依赖平台独有术语（菜单栏 / 托盘）、材质。

---

## 3. Tokens

### 3.1 颜色（取自当前实现）

来源：`crates/xross-app/src/ui/theme.rs`（`Palette`、`interaction()`、`interaction_tint()`）与 `window_effects.rs::make_surface_translucent`（覆盖 gpui-kit 主题）。颜色以 HSL 定义，下列 hex 为换算值（±1）。未覆盖的 token 直接取 gpui-kit 0.8.1（gpui-component `default-theme.json`，neutral / Tailwind 色阶）。

```css
:root {
  /* surface —— Palette.bg / panel / panel_alt / border */
  --bg-app:    hsl(210 40% 97.5%);  /* #f6f9fb 页面底 */
  --surface:   #ffffff;             /* 卡片、标题栏（浅色） */
  --surface-2: hsl(210 30% 94%);    /* #ebf0f4 Segmented 轨道、hover 底 */
  --border:    hsl(210 25% 88%);    /* #d9e0e8 1px 描边 */
  --text:      #0a0a0a;             /* neutral-950（gpui-kit foreground） */
  --text-2:    hsl(0 0% 42%);       /* #6b6b6b muted_foreground（覆盖值） */
  --input-border: hsl(0 0% 58%);    /* #949494 输入框描边（覆盖值） */

  /* accent：选中、开启、聚焦、主按钮、进度；gpui-kit 的 primary / ring / accent 均被覆盖 */
  --primary:        hsl(192 81% 31%);  /* #0f758f */
  --primary-hover:  hsl(192 81% 27%);  /* #0d667d */
  --primary-active: hsl(192 81% 23%);  /* #0b576a */
  --primary-fg:     #ffffff;
  --accent-soft:    hsl(190 75% 93%);  /* #e0f6fb 选中行 / 导航选中底 */
  --ring:           var(--primary);

  /* 语义（Palette.*_text 用于文字；success / warning / danger / info 取 gpui-kit 主题色） */
  --success-text: hsl(142 72% 29%);  /* #157f3c */
  --warning-text: hsl(26 90% 37%);   /* #b35309 */
  --danger-text:  hsl(0 74% 42%);    /* #ba1c1c */
  --success: #22c55e; --warning: #eab308; --danger: #ef4444; --info: #06b6d4;  /* green/yellow/red/cyan-500 */

  /* 数据 */
  --data-down: hsl(221 83% 53%);   /* #2463eb */
  --data-up:   hsl(262 83% 58%);   /* #7c3bed ⚠K24 */

  /* 侧栏（有模糊背景时） */
  --sidebar: hsl(210 30% 98% / .28);  /* 无模糊背景时回退为 --surface；Windows Mica 为透明 */
}
```

```css
@media (prefers-color-scheme: dark) { /* 实际由 Theme 模式切换，非媒体查询 */
  :root {
    --bg-app:    hsl(220 10% 9%);   /* #151619 */
    --surface:   #262626;           /* neutral-800（gpui-kit secondary） */
    --surface-2: #262626;           /* neutral-800（gpui-kit muted） */
    --border:    #262626;           /* neutral-800 */
    --text:      #fafafa;           /* neutral-50 */
    --text-2:    hsl(0 0% 65%);     /* #a6a6a6 */
    --input-border: hsl(0 0% 38%);  /* #616161 */

    --primary:        hsl(188 86% 53%);  /* #20d3ee */
    --primary-hover:  hsl(188 86% 60%);
    --primary-active: hsl(188 86% 46%);
    --primary-fg:     hsl(192 80% 10%);  /* #05262e */
    --accent-soft:    hsl(192 50% 17%);  /* #163841 */

    --success-text: hsl(142 69% 58%);  /* #4ade80 */
    --warning-text: hsl(43 96% 56%);   /* #fbbd23 */
    --danger-text:  hsl(0 91% 71%);    /* #f87272 */
    --success: #4ade80; --warning: #facc15; --danger: #f87171; --info: #22d3ee;  /* *-400 */

    --data-down: hsl(213 94% 68%);   /* #61a6fa */
    --data-up:   hsl(255 92% 76%);   /* #a689fa */
    --sidebar:   hsl(220 18% 8% / .62);
  }
}
```

- 页面底色 `--bg-app` 位于内容列；标题栏浅色用 `--surface`，深色用 `--bg-app`。
- 当前没有 `--ink`（近黑）主按钮：主按钮即 `--primary` 实心。下文中 ink / CTA 相关描述为目标态，见 `⚠K25`。
- 当前没有 `--*-soft` 语义浅底 token；徽标浅底由语义色 + alpha 叠加（如 `danger.opacity(.14)`）得到。
- 关闭态 switch 轨道取 gpui-kit：浅色 `#d4d4d4`，深色 `#404040`（`⚠K05`）。
- 焦点环：gpui-kit 的 `ring` 被覆盖为 `--primary`。

规则：
- **accent（蓝绿）= 状态与交互**：选中、开启、聚焦、进度、主按钮。目标态中页面主 CTA 改用近黑 ink，`启动` 保持 accent。`⚠K25`
- **绿色只表示"成功 / 健康 / 运行中 / 低延迟"**，不得用于"直连"等中性分类。
- **红色只表示 danger / 阻止 / 错误 / 高延迟**。accent 不得使用红色色相（避免与 danger 混淆）。
- 语义色做**浅底 + 深色文字**，不要实心饱和底 + 白字。 `⚠K04`
- **暗色**：每个颜色 token 必须有暗色值；surface 比 bg-app 亮一档；soft 色用 alpha 叠加。暗色值已在上方列出，**改动后必须在暗色下截图自检**。
- 延迟分级（`theme.rs`）：<120ms `--success-text`；<250ms 正文色；<500ms `--warning-text`；≥500ms `--danger-text`；未测 `--text-2`；探测失败 `--danger-text`；超过 10 分钟的结果调淡。

### 3.2 字体

来源：gpui-kit `Theme`（`font_family = ".SystemUIFont"`、`font_size = 16px`、`mono_font_family` = 平台默认等宽字体，未安装时自动回退、`mono_font_size = 13px`）。应用未覆盖字体；根节点仅设置 `font_family(theme.font_family)`。

GPUI 将 `rem` 设为 `theme.font_size`（16px），Tailwind 风格字号按 rem 换算：

| GPUI 方法 | 字号 |
|---|---|
| `text_xs` | 12px |
| `text_sm` | 14px |
| `text_base` | 16px |
| `text_lg` | 18px |
| `text_xl` | 20px |

```css
--font-sans: system-ui;   /* GPUI ".SystemUIFont"：macOS SF Pro / PingFang SC，Windows Segoe UI / 微软雅黑 UI */
--font-mono: 平台默认等宽字体;   /* 通过 theme.mono_font_family 取用 */
```

| 角色 | 字号 / 字重 | 实现 | 用法 |
|---|---|---|---|
| title | 18 / 600 | `text_lg` + `SEMIBOLD`（`section_title`） | 卡片标题 |
| stat | 20 / 600 | `text_xl` + `SEMIBOLD`（连接页速率） | 大号数值 |
| row-title | 14 / 500 | `text_sm` + `MEDIUM` | 列表项名称、侧栏当前项 |
| body | 14 / 400 | `text_sm` | 正文、侧栏项、副标题（`--text-2`） |
| caption | 12 / 400 | `text_xs` | 说明、徽标、次要信息 |
| data | 13 mono | `mono_font_family`（`mono_font_size`） | 延迟、速率、端口、日志 |

- 全局使用最多的是 `text_xs`（12）与 `text_sm`（14）；`text_base`（16）仅偶发。
- 数字列固定 `w(px(..))`（节点行延迟 96 / 80、日志列 48–64）；等宽字体用于日志、端口、速率。
- 目标态（未实现）：数字使用 `tabular-nums`。当前未设置。

### 3.3 间距、尺寸、圆角

GPUI 间距为 Tailwind 刻度：`gap_1`=4、`gap_2`=8、`gap_3`=12、`gap_4`=16、`p_6`=24。

| Token | 值 | 实现 |
|---|---|---|
| 页面内边距 | 24 | 内容区 `p_6`（`ui/mod.rs`） |
| 卡片内边距 | 16 | `card()`：`p_4` |
| 卡片内部间距 | 12 | `card()`：`gap_3` |
| 卡片圆角 | 8 | `card()`：`rounded_lg` = `theme.radius_lg`（8） |
| 控件圆角 | 6 | `rounded_md` = `theme.radius`（6）；`rounded_sm` = 3；`rounded_full` 用于徽标 / 状态点 |
| 卡片样式 | 1px `--border` + 浅色模式 `shadow_xs`；深色无阴影 | `card()` |
| 按钮高度 | 32（gpui-kit `Medium` / `Large`） | `Small` = 24；`XSmall` = 20；应用内按钮几乎都用 `.compact()` |
| 工具按钮 | 26 / 28 | `button_group_item` h=26；`danger_icon_button` 28×28；连接页、节点页图标按钮 26×26 |
| 输入框高度 | 32 | 设置页行 `h(px(32))` |
| 侧栏 | 宽 220；项高 40；图标 18；项内 `px_3` + `gap_3`；macOS 顶部红绿灯占位 36；底部 `关于` 行高 36 | `ui/mod.rs` |
| 标题栏 | 高 54，下边框 1px | `header-bar` |
| 窗口 | 默认 1200×800；最小 960×640 | `main.rs` |
| 节点行 | `pl_4 pr_3 py_3`，圆角 8，选中左条 3px（上下各缩 7px）、禁用 `opacity .45` | `ui/nodes.rs` |
| 单选 / 复选 | 20×20；选中点 10 | `ui/nodes.rs` |
| 状态点 | 8×8 | `widgets::dot` |
| 订阅流量条 | 高 6 | `ui/subscriptions.rs` |
| 设置页 label 列 | 120；开关项宽 216、最小高 32 | `ui/settings.rs` |
| 弹窗宽度 | 440 / 720 | `ui/dialogs.rs` |
| 最小点击区 | 28×28 | 桌面端（目标） |

### 3.4 层级与动效

- 卡片：1px `--border`，**无阴影**。浮层（菜单、popover、toast）才用阴影。
- 动效 120–180ms `ease-out`；侧栏宽度 180ms。
- 必须尊重 `prefers-reduced-motion`。
- 超过 300ms 的等待要有可见反馈（spinner + 文字）。

---

## 4. 应用骨架（Shell）

```
┌──────────┬──────────────────────────────────────────────┐
│ ●●●      │  [当前节点 · 延迟]   [ 状态胶囊 ]   [↓ ↑ 速率] │ 标题栏 54px（drag region）
│ Xross    ├──────────────────────────────────────────────┤
│ nav…     │  page（bg-app，padding 24，卡片纵向堆叠）      │
│ 关于 v0.1│                                              │
└──────────┴──────────────────────────────────────────────┘
```

### 4.1 侧栏

- 展开宽 220，全高；macOS 用 vibrancy，内容区用实色。
- 项高 40，图标 18px，**统一图标集（gpui-kit `IconName`）**；选中 = `--accent-soft` 底 + accent 图标与文字。
- 分组（用分隔线，不加标题文字）：`[节点 订阅 路由 DNS 组网]` `[连接 测试 日志]` `[设置]`。`⚠K26`
- 底部：`关于` + 版本号。运行状态**不在这里重复显示**（标题栏胶囊已有）。
- 图标唯一性：`activity / pulse` 不得同时表示"连接"和"测速"。 `⚠K07`
  - 连接 → `arrow-left-right` 或 `waypoints`；测速 → `gauge` 或 `timer`；`zap` 留给"最快"。

### 4.2 收起（提案，未采用）

收起为 56px 图标栏。macOS 红绿灯约 70–76px 宽，会超出图标栏，**不要加宽图标栏**，也不要换成浮动顶栏 / 底栏。做法：顶部 54px 的"红绿灯区"横跨图标栏与内容区，图标栏从其下方开始，两种状态只变宽度、不变垂直位置。

```css
:root { --sb-w: 220px; --titlebar-h: 54px; --tl-inset: 0px; --ctrl-inset-right: 0px; }
[data-platform="macos"]   { --tl-inset: 76px; }            /* 实测后调整 */
[data-platform="macos"][data-fullscreen="true"] { --tl-inset: 0px; }
[data-platform="windows"] { --ctrl-inset-right: 138px; }   /* 优先取 windowControlsOverlay 实际值 */
.app[data-sb="rail"] { --sb-w: 56px; }
.app { display: grid; grid-template-columns: var(--sb-w) 1fr; transition: grid-template-columns .18s ease-out; }
```

- 全屏时 `--tl-inset` 归零，监听全屏事件。
- 图标栏每个图标必须有 tooltip + `aria-label`。
- 收起状态持久化；窗口宽度小于阈值时自动收起。
- 快捷键 `⌘B` / `Ctrl+B`。

### 4.3 标题栏（内容列，54px，整条是 drag region）

| 区域 | 内容 | 规则 |
|---|---|---|
| 左 | 当前节点（国旗、名称、延迟 / `未测速`）；第二行归属地 | 归属地未知或 core 停止时**隐藏第二行**，不显示"归属地未知" |
| 中 | 状态胶囊 | 见 §4.4 |
| 右 | `↓ ↑` 实时速率 + 第二行 `累计` | 停止时降为 `--text-2` |

- 左右 padding = `--page-pad`，与卡片边缘对齐（当前各差约 8px `⚠K09`）。
- 按钮、输入框设 no-drag；只有空白处是 drag。

### 4.4 状态胶囊（core 状态机）

```ts
type CoreState = 'stopped' | 'starting' | 'running' | 'stopping' | 'crashed';
```

| 状态 | 左侧 | 右侧操作 | 备注 |
|---|---|---|---|
| stopped | 灰点 `已停止` | `启动`（accent 实心） | 观测到 |
| starting | spinner `启动中…` | 禁用 | 位置不变，不抖 |
| running | 绿点 `运行中` | `停止`（ghost，次级文字色，hover 才变 danger） | 观测到；当前红字常驻 `⚠K25` |
| stopping | spinner `停止中…` | 禁用 | |
| crashed | 红点 `异常退出` | `重启`，旁有 `查看日志` | 开启"崩溃自动重启"时显示重试次数 |

- 启动 / 停止在**同一位置、同一宽度**切换。
- 胶囊不得与拖拽区冲突：胶囊整体 no-drag，周围留 ≥8px drag 空隙。

### 4.5 页面

- 背景 `--bg-app`；卡片纵向堆叠；**第一张卡片 = 页头卡片**（title + caption + 右上角动作）。
- 页头卡片高度由内容决定，不得出现大块空白（连接页当前有 `⚠K15`）。
- 页头动作区：`[次按钮…][主按钮]` 右对齐，从右向左依次降权；超过 3 个收进 `⋯`。
- 页面标题只出现一次（不要页头 + 卡片标题重复同名）。

---

## 5. 组件

每个组件：变体 / 状态 / 规则。状态缺一不可：default、hover、active、focus-visible、disabled、loading（如适用）。

### 5.1 Button

| 变体 | 外观 | 用于 |
|---|---|---|
| primary | `--ink` 实心 + 白字 | 页面主 CTA：添加节点、添加订阅、应用 |
| secondary | 1px 描边 + 透明底 | 全部更新、测速、导出 |
| ghost | 无描边，hover 出底色 | 低优先级（清空、重置、设为 final） |
| accent | `--accent` 实心 | 仅 `启动` |
| danger | 描边；hover / 确认态变 danger | 删除（见 §5.7） |
| icon | 32×32，必有 tooltip + `aria-label` | 工具栏、行内 |

- **ghost 必须有 hover 底色与 `cursor:pointer`**，否则不像按钮（`清空` / `重置` / `设为 final` 当前像纯文字 `⚠K13`）。
- **disabled**：`opacity: .45` + `cursor: not-allowed`，**不改变底色色相**（`全部断开` 当前禁用态仍带粉色 `⚠K14`）。
- **loading**：spinner 替换图标，宽度不变，禁止重复点击。
- 同一页头内最多出现 **两种** 按钮变体。日志页当前有三种 `⚠K13`。

### 5.2 选择类控件（一种语义一种控件）

| 场景 | 控件 |
|---|---|
| 单选，≤5 项，短文案 | **Segmented** |
| 单选，每项有说明 / 较大 | **Option card**（radio 语义） |
| 多选过滤 | **Chip**（toggle） |
| 列表中"当前使用"的一项 | **Radio** 行 |

- **单选一律不用 Chip**（连接页的 `全部/域名/IP/进程`、`实时/录制` 当前用了 Chip `⚠K11`）。
- 选中态**只有一种**，非颜色冗余（ring / 底色 / 左条）：

```css
/* Segmented */
.seg { display:inline-flex; padding:2px; background:var(--bg-app);
       border:1px solid var(--border); border-radius:var(--r-ctl); }
.seg [role=radio] { height:28px; padding:0 12px; border-radius:6px; color:var(--text-2); }
.seg [role=radio][aria-checked=true] {
  background:var(--surface); color:var(--accent);
  box-shadow: inset 0 0 0 1px var(--accent);
}
/* Option card */
.opt[aria-checked=true] { border-color:var(--accent); background:var(--accent-soft); }
```
- Option card **必须有一项处于选中**，否则要显式提供"自定义"项（DNS 预设当前无选中 `⚠K02`）。
- **模式开关与视图过滤不得并排同款**：`手动 / 自动`（改变代理行为）不得与 `平铺 / 订阅`、`顺序 / 延迟`（只改视图）使用相同外观并混在一排。`⚠K03`

### 5.3 Switch

- 36×20；on = `--accent`；off 轨道 `--switch-off`（≥3:1）。`⚠K05`
- 位置：**列表行内 → 行尾（trailing）**；**表单 → switch 在左、label 在右**。路由规则当前在行首 `⚠K12`。
- 表单开关**立即生效**；需要重启 core 的，行内显示 `需重启` 小标签，不弹窗。
- 影响安全的开关（`允许局域网连接`、`TUN`）必须有一行说明。 `⚠K18`
- 行内 switch 若含义不明（`启用` ≠ `选中`）必须有 tooltip，并让禁用行整体置灰。 `⚠K17`

### 5.4 列表行（节点 / 规则 / DNS 服务器 / 订阅）

```
[drag?] [radio?] [主信息]            [次信息: 延迟等] [行内动作…] [⋯] | [switch]
```

- **选中行**（列表中"当前使用"的行）：`--accent-soft` 底 + **贴行边**的 3px accent 左条 + radio 选中。**不要再加黑框 / 粗描边。** `⚠K10`

```css
.row[aria-selected=true] { background:var(--accent-soft); border-color:transparent; position:relative; }
.row[aria-selected=true]::before { content:""; position:absolute; inset:8px auto 8px 0;
  width:3px; border-radius:2px; background:var(--accent); }
.row .row-actions { opacity:0; transition:opacity .12s; }
.row:hover .row-actions, .row:focus-within .row-actions { opacity:1; }  /* focus-within 不能漏 */
```
- 行内次要动作（测速、复制、二维码）hover / focus 才显示，其余收进 `⋯`。拖拽手柄默认低透明度。
- **排序方式统一为拖拽手柄**，并在 `⋯` 里提供"上移 / 下移"作为键盘替代。不再用裸 `↑ ↓` 按钮。 `⚠K12`
- 手动排序与"按延迟排序"互斥：用户开始拖拽 → 自动回到"顺序"。
- 密度：提供 compact（48）与 default（64）；节点数 > 10 默认 compact，节点数 ≤ 3 隐藏搜索与排序。

### 5.5 Badge

- 形状：pill，浅底 + 深色文字（`--*-soft` + `--*`），**不用实心饱和底 + 白字**。
- 协议（VLESS / VMESS / …）：中性 badge（白底 + accent 文字，现状可保留）。传输层（WS / TCP / GRPC）用普通文字，且**不与协议重复显示**。
- 路由动作：`代理` = accent-soft；`直连` = **中性灰**；`阻止` = danger-soft。**不要用绿色表示直连。** `⚠K04`
- 不用 badge 展示实现细节（`ETag`、`userinfo`）。 `⚠K23`

### 5.6 Input

- 高 32；label 在左或上；校验信息在下方行内，不用弹窗。
- 宽度匹配内容：端口、分组名、网段**不要拉满整行**（`分组名称` 当前拉满 `⚠K20`）。
- **提交模型要显式**：开关 / 分段 = 立即生效；文本输入带校验 = 编辑后才出现 `应用`（dirty 才显示）。不要在一排立即生效的设置里常驻一个黑色 `应用`。 `⚠K18`
- 看起来可编辑的值必须像输入框；纯展示值不要像输入框（`fake-ip 网段` 当前两者都不像 `⚠K21`）。

### 5.7 破坏性操作

统一模式（当前有四种 `⚠K06`）：
1. 行内**不直接放**红色删除。放进 `⋯`，或用 icon 按钮（默认中性，hover 变 danger）。
2. 单项删除：**立即执行 + 6 秒"撤销" toast**。
3. 批量 / 不可逆（清空日志、全部断开、删除订阅连带节点）：确认对话框，说明影响范围。
4. 删除按钮**不得紧邻**主动作（订阅页 `更新` 旁的粉色 `删除` 不合格）。
5. 被引用的项（被 fake-ip 规则使用的 `fakeip` 服务器）：删除禁用 + tooltip 说明原因。

### 5.8 空状态

居中：图标 + 一句标题 + 一句说明 + **一个主动作**。卡片最小高 ≈160，不要左对齐一行灰字撑在大卡片里。

示例（core 未运行时的连接页）：图标 + `Core 未运行` + `启动后可查看实时连接` + `[启动]`。 `⚠K15`

### 5.9 Tooltip / ⓘ

- 所有仅图标控件必须有 tooltip；`ⓘ` 必须能打开说明（hover / focus 都可）。
- tooltip 延迟 400ms，不遮挡触发元素。

### 5.10 图表（实时流量）

- 零值和 <1% 峰值**不画柱**，基线用 1px `--border` hairline。 `⚠` 当前零值画成短横线像数据。
- 必须标注时间窗口（`近 60 秒`）、峰值定义、单位；图例用 `↓ 下行` / `↑ 上行`。
- 双系列不靠色相区分：下行实心、上行描边或线；颜色 `--data-down / --data-up`。 `⚠K24`
- 无数据时折叠，不留空白。节点页中图表可折叠，默认折叠。

```ts
const MIN_RATIO = 0.01;
const barH = (v: number, peak: number, H: number) => {
  const r = peak > 0 ? v / peak : 0;
  return r < MIN_RATIO ? 0 : Math.max(2, r * H);
};
```

### 5.11 Toast / Banner

- toast：操作结果、可撤销；6 秒，hover 暂停。
- banner：持久状态，如 `有未应用的变更  [重启 core]`。**仅在有变更时出现**，不要常驻"配置变更需要重启"。
- 切节点会重启 core（Xray）时，按钮行内显示 spinner，期间禁止再次点击；sing-box 走热切换则不提示。

### 5.12 进度条（订阅流量）

- 4px 圆角轨道；<80% accent，80–95% warning，≥95% danger，**同时显示百分比数字**。
- 到期日 ≤7 天用 warning 文字。

---

## 6. 页面模式

| 页面 | 结构 | 要点 |
|---|---|---|
| 节点 | 页头（标题、计数、`添加节点`）→ 工具栏 → 图表（可折叠）→ 节点列表 | 模式开关独立于视图过滤；搜索 / 排序在节点多时才出现；每行：协议 badge、`host:port`、延迟、`⋯`、switch |
| 订阅 | 页头 → 每个订阅一张卡片 | 卡片：名称 + 节点数、截断 URL、流量进度、到期、最后更新；动作 `更新` + `⋯`（含删除）+ switch |
| 路由 | 路由模板（Option card）→ 规则分组 → 规则列表 | 分组用 tab 行 + `+`；规则行：switch(行尾)、名称、匹配摘要、动作 badge、`⋯`；拖拽排序 |
| DNS | 策略（Option card + 开关）→ 服务器列表 → Hosts | 服务器列表中 `final` 用 radio；名称 / 类型 / 地址**不重复**；Hosts 用表格行 |
| 连接 | 页头 → 过滤栏 → 连接表 / 空状态 | 实时 / 录制用 Segmented；过滤类型用 Segmented；速率**不与标题栏重复**（或只留累计） |
| 日志 | 页头（动作：复制 / 导出 / 清空）→ 过滤栏 → 日志表 | 级别着色；按需"跟随最新"；日志目录可点击打开 / 复制 |
| 设置 | 多张卡片：外观、Core、网络与代理、启动与窗口、更新与备份 | 单列设置行（label + 一行说明 + 控件），不用三列开关网格 |

**未截图的页面（组网、测试）**：按本文组件实现；上线前补截图并过 §12。

连接页的两项过滤：`类型`（全部 / 域名 / IP / 进程）与搜索框同一行；`重置` 为 ghost 按钮。

设置页行对齐：label 与控件**垂直居中对齐**（当前 label 低约 5px `⚠K18`）；label 列宽固定 120。

---

## 7. 数据与格式

```ts
// 延迟分级（默认值，可调）；未测 / 超时必须单独表达
type Latency = { state: 'idle' } | { state: 'testing' } | { state: 'timeout' } | { state: 'ok'; ms: number };

const tier = (ms: number) => (ms < 200 ? 'good' : ms < 600 ? 'mid' : 'bad');

// idle    → "—"（--text-2）
// testing → spinner
// timeout → "超时" + 警告图标 + tooltip（不得显示 0 ms 或留空）
// ok      → "123 ms" + tier 色 + 与色无关的冗余（图标或文字）
```

- **"最快"标记**只在差距足够大时出现：`Δ ≥ max(30ms, 15%)`；否则不标。过期（>10 分钟）的延迟值调淡，tooltip 给出测速时间。
- 速率 / 流量用 IEC 单位：`B/s KiB/s MiB/s`，`B KiB MiB GiB`；同一页内单位风格一致。
- 时间：绝对日期 `yyyy-MM-dd`；相对时间用 `Intl.RelativeTimeFormat`：

```ts
new Intl.RelativeTimeFormat(locale, { numeric: 'auto' }).format(-3, 'minute') // "3 分钟前"
// 0 分钟内 → "刚刚"；禁止拼出 "0m 前"  ⚠K08
```
- **隐私模式**：开启后遮罩所有 IP / 域名 / 地址（节点行、标题栏第二行、连接表、日志）。按钮有 pressed 态（eye ↔ eye-off）。遮罩范围以"截图不泄露"为准。

---

## 8. 无障碍

- 文字对比 ≥4.5:1；UI 组件边界、图标 ≥3:1。**特别检查**：浅底上的语义色文字、白字 + 彩色实心 badge（当前约 2.7:1 / 3.9:1 `⚠K04`）、关闭态 switch（当前约 1.2:1 `⚠K05`）。
- **不得仅靠颜色**：延迟、状态点、↓↑ 数据色都要有文字 / 图标 / 形状冗余。
- 全部可操作元素可用键盘到达；`:focus-visible` 用 `--focus-ring`，**不得 `outline: none`**。
- 行内 hover 才出现的按钮在 `:focus-within` 也必须出现。
- Segmented = `role="radiogroup"` + 方向键切换；Switch = `role="switch"` + `aria-checked`；列表选中 = `aria-selected`。
- 只有图标的按钮：`aria-label` 必填。

---

## 9. 跨平台

| 项 | macOS | Windows | Linux |
|---|---|---|---|
| 窗口控件 | 左上红绿灯，`--tl-inset` | 右上，`--ctrl-inset-right` | 系统装饰，两者为 0 |
| 窗口材质 | vibrancy（侧栏） | Mica / 纯色 | 纯色 |
| 常驻入口术语 | 菜单栏 | 系统托盘 | 系统托盘 |
| 字体回退 | SF / PingFang | Segoe UI / Microsoft YaHei UI | system-ui |

- 平台独有选项（`窗口材质`、`菜单栏显示网速`）：不支持的平台**隐藏**，不要显示后禁用。
- 文案按平台取词：`最小化到托盘` → macOS 为 `最小化到菜单栏`。 `⚠K19`
- 窗口材质选项的值要映射到各平台实际能力，不能把 macOS 词汇（毛玻璃）直接用于 Windows。

---

## 10. 文案与 i18n

- 中文界面统一写 **Core**（不写 `core` 小写或 `内核` 混用）；协议名保持原大小写（VLESS、sing-box、Xray）。
- 按钮：动词开头，2–4 字（`添加节点`、`全部测速`）。破坏性按钮用具体动词（`删除订阅`），不用 `确定`。
- 说明文字面向用户目的，不写实现细节（`ETag / If-Modified-Since` 不出现在副标题）。 `⚠K23`
- 所有用户可见字符串走 i18n；单位、复数、相对时间用 `Intl`。
- 空值：`—`；错误信息 = 发生了什么 + 怎么办。

---

## 11. 待纠正清单（现状 → 目标）

位置为截图文件名。P0 = 误导 / 错误 / 明显不达标；P1 = 一致性与可用性；P2 = 待验证 / 补充。

| ID | 优 | 位置 | 现状问题 | 目标 |
|---|---|---|---|---|
| K01 | P0 | nodes | 忽略 | 忽略 |
| K02 | P0 | dns | 4 个 DNS 预设按钮**没有任何选中态** | Option card，必有选中；非预设时显示 `自定义` |
| K03 | P0 | nodes | `手动 / 自动` 与 `平铺 / 订阅`、`顺序 / 延迟` 同款并排；前者改变代理行为，后两者只是视图 | 模式开关单独放置并区分；`自动` 下 radio 禁用并标出 urltest 当前节点 |
| K04 | P0 | routing | `阻止` / `直连` 是实心红 / 绿 + 白字：对比约 3.9:1 / 2.7:1；绿色与"运行中 / 低延迟"语义冲突 | 浅底 + 深字；直连用中性灰（§5.5） |
| K05 | P0 | settings 等 | 关闭态 switch 轨道约 1.2:1 | `--switch-off` ≥3:1 |
| K06 | P0 | subscriptions / dns / routing / logs | 删除有 4 种样式：粉色实心 pill（紧邻 `更新`）、裸红 trash 图标常驻（含被引用的内置 `fakeip`）、`清空` 无样式、节点页放 `⋯` | §5.7 统一模式 |
| K07 | P0 | 全局 | pulse 图标同时是`连接`导航与`测速`动作 | §4.1 图标唯一 |
| K08 | P0 | subscriptions | `最后更新：0m 前` 拼接英文单位 | `刚刚` / `Intl.RelativeTimeFormat` |
| K09 | P1 | 全局 | 标题栏左 / 右内容与卡片边缘各差约 8px；节点页标题栏缺少节点 / 速率，背景疑似纯白（**待确认是否截图伪影**） | 共用 `--page-pad`；所有页一致 |
| K10 | P1 | nodes / dns / routing | 选中态至少 4 种：左条 + 底色、仅 1px 描边（DNS final）、描边 + 底色（模板）、两种 segmented | §5.2 / §5.4 各一种 |
| K11 | P1 | connections | 单选过滤用 Chip（`全部/域名/IP/进程`、`实时/录制`） | Segmented |
| K12 | P1 | routing vs nodes | 行内 switch 一处在行首、其余在行尾；排序一处拖拽、一处 `↑↓` 按钮 | switch 行尾；拖拽 + `⋯` 内上移 / 下移 |
| K13 | P1 | logs / connections / dns | 页头按钮样式不一：描边 + 图标、描边、无样式；`重置`、`设为 final` 像纯文字 | ghost 要有 hover 与指针；页头 ≤2 种变体 |
| K14 | P1 | connections | 禁用按钮几乎不可见，`全部断开` 禁用仍带粉色 | `opacity .45`，不改色相 |
| K15 | P1 | connections | 页头卡片内大块空白；空状态为一行左对齐灰字，占一整张大卡片 | §5.8 空状态，含 `启动` 动作 |
| K16 | P1 | connections vs titlebar | 速率与累计重复展示；这里 ↓↑ 为黑色，标题栏为蓝 / 紫 | 页内只留差异信息（活动连接数）；颜色一致 |
| K17 | P1 | nodes | 节点行约 71px，7 行占满窗口；行内 switch 含义不明且与 radio 并存；页头 `‹` 图标无标签（推测为折叠图表）；pulse / eye 无标签 | compact 行高；switch tooltip；`‹` 改 chevron-down/up + tooltip；图标带 tooltip |
| K18 | P1 | settings | label 与控件垂直错位约 5px；三列开关网格无说明；混合端口常驻黑色 `应用`；`通用` 卡片塞了外观 / Core / 网络 / 代理 / 启动 / 窗口 | 单列设置行 + 说明；dirty 才出现 `应用`；拆卡片（§6） |
| K19 | P1 | settings | `托盘`、`菜单栏`、`毛玻璃` 等平台词汇未按平台区分 | §9 |
| K20 | P1 | routing | `分组名称` 输入拉满整行且常驻；规则行有 3 个描边按钮 + 红 trash；长匹配表达式原样换行；`AsIs / IPIfNonMatch` 为 Xray 术语（**sing-box 下是否适用待确认**） | 分组 tab + `+`；动作进 `⋯`；匹配项折叠为 `+N`；按 core 条件显示 |
| K21 | P1 | dns | 第一行 `fakeip` 重复三次；类型 pill 过宽；`fake-ip 网段` 不像输入也不像纯文本；`设为 final` 无按钮外观 | 名称 / 类型 / 地址不重复；窄 pill；明确的输入框；radio |
| K22 | P1 | logs | 日志级别无颜色；mono 字号偏大；日志目录是纯文本；`清空` 无确认；无"跟随最新" | WARN 黄 / ERROR 红；12–13px；路径可点击；确认或撤销 |
| K23 | P1 | subscriptions | `ETag`、`userinfo` 标签与副标题中的 `If-Modified-Since` 属于实现细节 | 移入详情 / tooltip |
| K24 | P1 | 全局 | ↓ 蓝 `#2563eb` / ↑ 紫 `#7c3aed` 对色弱不友好，图表中相邻 | 增加冗余编码（实心 / 描边、文字）；若换色，优先蓝 + 橙并重新校对警告色冲突 |
| K25 | P1 | 全局 | 两套"主色"：ink 黑（CTA）与 teal（状态 + `启动`）；`停止` 红字常驻 | §3.1 规则；`停止` 默认次级文字色，hover 变红 |
| K26 | P2 | sidebar | 9 项扁平；无分组、无收起 | §4.1 / §4.2 |
| K27 | P2 | — | 未验证：暗色、`组网` / `测试` 页、hover / focus / 动效、错误态、0 节点、节点 >50 | 补截图并回填本文 |
| K28 | — | — | 已完成：§3 数值已对照源码校正 | — |

---

## 12. 交付检查（Agent 每次 UI 变更必过）

- [ ] 只引用 token；无裸 hex / 魔法数字。
- [ ] 复用了 §5 组件；新增样式已先提议。
- [ ] 状态齐全：default / hover / focus-visible / disabled / loading / empty / error。
- [ ] 同一语义没有出现第二种控件、图标、颜色（对照 §11）。
- [ ] 键盘可完成全部操作；焦点环可见；仅图标按钮有 `aria-label` + tooltip。
- [ ] 文字 ≥4.5:1、组件边界 ≥3:1；信息不只靠颜色。
- [ ] 浅色 + 暗色各一张截图；若涉及 macOS，再检查红绿灯区与全屏。
- [ ] Windows 布局（窗口控件在右）没有被 macOS 假设破坏。
- [ ] 破坏性操作可撤销或有确认；被引用项不可删。
- [ ] 所有字符串走 i18n；相对时间 / 单位走 `Intl`。
- [ ] 若修复了 §11 某项，在 PR 中写明 `Fixes K##`，并更新本文。
