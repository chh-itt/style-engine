# ADR-0033：UA 起源样式表与内置缺省呈现（set_ua_stylesheet / DEFAULT_UA_SHEET）

日期：2026-02（P5 批，goal-c293e543）
状态：已接受（落地记录见文末，随实施补记）

## 背景

级联阶梯（cascade.rs:38 `Origin`）已含 `UserAgent` 层序
（`Default < UserAgent < User < Author < Inline`，important 反转），
但引擎侧没有 UA 起源样式表挂点：`set_user_stylesheet`（User 层）之后
UA 层永远为空。HTML 文档的缺省呈现（h1 字号、p 边距、b 加粗、
i 倾斜）只能由宿主逐元素硬编码。

零副作用契约（README/CONTEXT）：引擎中立、状态与资源宿主推送——
UA 表属于「文档语义」而非「引擎状态」，以显式 API 注入与契约一致：
宿主选择是否装载内置表、装载哪份表（HTML/自定义文档方言）。

## 事实基线（实施前核查）

- `computed.rs:368`：`P::Display` 初始值 = **`Display::Block`**
  （引擎级缺省呈现=块流；CSS 规范初始为 inline。这是「无 UA 表」
  模式下的刻意捷径，未标 B 级——UA 表落地后重新审视）。
- `css/property.rs:2811`：parse_display 支持
  block|flow-root|inline|inline-block（+ table 系值）。
- `engine.rs:4514` `set_user_stylesheet` / `:4525` clear / `:268`
  `user_sheet` 字段 = User 层挂点模板（parse → rebuild_registered_props
  → rebuild_font_faces → dirty_style → dirty_struct）。
- `cascade.rs`：收集序与层序已备，UA 层序号 (1,6) 就位，仅缺收集源。

## 决策

### D1：挂点——镜像 User 层模式

- `engine.ua_sheet: Option<Stylesheet>`（引擎字段区 :268 旁）；
- `pub fn set_ua_stylesheet(&mut self, css: &str)`：parse_stylesheet +
  rebuild_registered_props + rebuild_font_faces + dirty_style +
  dirty_struct（与 set_user_stylesheet 同链）；
- `pub fn clear_ua_stylesheet(&mut self)`；
- Epoch 语义不变：UA 表注入=样式表版本单调递增（invalidation 正确）。

### D2：级联——收集进既有 UserAgent 层

- 级联收集序：Default → **UA** → User → Author → Inline；
- `@property` / `@font-face` 合并序：UA 表注册先于 user 表
  （CSS-cascade-5 起源序）；
- important 反转自动生效（既有 Origin::rank 表 (1,6) 已含）。

### D3：内置缺省表——`style_engine::builtins::DEFAULT_UA_SHEET`

- 新模块 `pub mod builtins`，常量 `pub const DEFAULT_UA_SHEET: &str`
  （html5 rendering 建议最小集 v1）：
  - 块级元素清单（address/article/aside/blockquote/body/div/dl/dd/dt/
    fieldset/figcaption/figure/footer/form/h1-h6/header/hgroup/hr/main/
    nav/ol/p/pre/section/table/ul）→ `display: block`
    （与引擎初始 Block 冗余但无害；装载自定义方言表时保持语义完整）；
  - `h1..h6` 字号（2em/1.5em/1.17em/1em/0.83em/0.67em）+ bold +
    上下 margin（0.67em×2 … html5 表）；
  - `p/blockquote/figure/figure/figcaption/pre/dd/ol/ul/dl` 边距组；
  - `b/strong` → `font-weight: bolder`；`i/em/cite/var/dfn` →
    `font-style: italic`；`u` → underline；`s/strike/del` →
    line-through；`small` → `font-size: smaller`
    （相对关键字由既有相对字号机制结算）；
  - `center` → `text-align: center`；`pre` → `white-space: pre` +
    等宽族；
  - **不做**（v1 边界）：list marker 生成（`::marker`/content 计数器
    依赖，随 counters 批后续）、hr 3D 边框（按 solid 平直画或排除）、
    表格 UA 边框塌陷细节、`dir`/`lang` 派生。
- 装载方式：**默认不装载**。宿主显式
  `engine.set_ua_stylesheet(style_engine::builtins::DEFAULT_UA_SHEET)`；
  demo/harness 用例装载（演示中立定位：表是数据，不是行为）。

### D4：Display 初始值保持 Block（不做 breaking 对齐）

- CSS 规范 display 初始=inline；引擎初始=Block。改初始值会破坏全部
  既有布局测试与宿主预期（未匹配元素全部转行内）。
- 决策：初始值不动；「引擎缺省呈现=块流」作为引擎级 B 级在案
  （FEATURES 注记），HTML 语义呈现由 UA 表提供。中立定位下
  「缺省呈现是宿主策略」——引擎只保证机制，不预置文档观点。

## 测试要点（实施时落锁）

- UA 层级联序：UA < User < Author（同 specific 竞争胜者）；
  important 反转（UA important 强于 Author important）；
- set_ua_stylesheet 后 h1 字号/边距生效；clear 后回引擎缺省；
- @font-face/@property 在 UA 表内注册生效且先于 user 表；
- DEFAULT_UA_SHEET 解析零错误（ParseReport 全 clean）；
- 不装载时引擎行为与现状逐位一致（回归锁）。

## 否决项

- 引擎默认装载内置表（违背中立定位；demo 显式装载）。
- Display 初始值改 inline（breaking 无收益；呈现策略归宿主）。
- UA 表支持 `@import`（样式表引入保持单文件；与 user 表同规）。

## 落地记录

（P5 实施后补记。2026-02，commit 见 IMPLEMENTATION-LOG P5 段。）

### 实施要点

- **挂点链**：`engine.ua_sheet` 字段（engine.rs，`user_sheet` 旁）+
  `set_ua_stylesheet`/`clear_ua_stylesheet`（clear_user_stylesheet 后），
  镜像链 parse → rebuild_registered_props → rebuild_font_faces →
  dirty_style + dirty_struct。API 返回 `()`（与 user 表同型——解析报告
  经 `ParseReport` 不外抛，装载失败以样式缺失表现；零错误锁由
  `ua_builtin_sheet_applies_and_clears` 间接承载）。
- **收集序**：`cascade_declarations`/`cascade_channel` 尾参
  `ua_sheet: Option<&Stylesheet>`；UA 段插 User 前；
  `compute_node_in` 转传；::selection/::placeholder 两通道同挂。
- **合并序**：`rebuild_registered_props`/`rebuild_font_faces` 合并序
  UA → user → 主表 → 附加表（文档注释同步）；materialize_pseudos
  `any_pseudo` 判定含 ua_sheet。
- **builtins**：`pub mod builtins` + `DEFAULT_UA_SHEET`（块级清单、
  h1–h6 字号+bold+margin、p/blockquote/figure/ul/ol/dl/menu margin、
  b/strong=bold、i/em/cite/var/dfn=italic、u/ins=underline、
  s/strike/del=line-through、small/big=绝对关键字、center、
  pre/code/kbd/samp/tt 等宽+pre=white-space:pre）。

### 实施偏差（对决策原文）

- 【B】`b/strong = font-weight: bold`（非 `bolder`）——bolder 相对
  关键字未实现，UA 表先物化为绝对值；
- 【B】`small/big = font-size: small/large`（绝对关键字物化，非
  smaller/larger）——相对关键字字号未实现；
- 【B】UA 表未启用引擎 既有相对字号机制外的派生（dir/lang 同不做）；
- Display 初始值保持 Block（D4 原案）。
- 重要：important 反转链锁实测确认——`ua_origin_ladder_and_
  important_inversion` 断言 **UA-important 压过 Author-important**
  （css-cascade-5 origin+importance 反转：UA !i(6) > Author !i(4)；
  Default !i 最强(7)）。首版测试期望 Author !i 胜出，读 rank 表后
  修正断言。

### 锁测试（+2）

- `engine.rs::ua_origin_ladder_and_important_inversion`：UA < Author、
  UA-important > Author 普通、Author-important 不改写 UA-important；
- `engine.rs::ua_builtin_sheet_applies_and_clears`：未装载=初始 16px、
  装载后 h1=32px（2em）、clear 复位；DEFAULT_UA_SHEET 经装载链
  零错误验证。
