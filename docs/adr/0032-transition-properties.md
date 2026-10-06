# ADR-0032：CSS Transitions（transition-* 全量）

日期：2026-02（P1 批 / G1，goal-c293e543）
状态：已接受（随实施补记落地修正）

## 背景

动画系统已有 @keyframes 全量采样（第五批⑰），但 transition-*
四长手 + 简写缺失——宿主无法获得「样式变更平滑过渡」语义。
依据：css-transitions-1/2（reconciliation 三则）、css-easing-1
（steps/贝塞尔）。

## 决策

### D1：属性面（5 新槽位）

- `transition-property` → `TransitionProperty(TransitionTarget 列表)`：
  `None | All | Ident(String)`（custom-ident 合法保留——未知名不产生
  过渡但不报错，css-transitions-1 §2.1）。
- `transition-duration` / `transition-delay` → `TransitionTime(Vec<f32>)`：
  `<time>#`，duration 拒负、delay 允负（快进）。
- `transition-timing-function` → `TransitionTiming(Vec<TimingFn>)`：复用
  animation 文法（TimingFn 共享；steps/贝塞尔/cubic）。
- `transition-behavior` → `TransitionBehavior { Normal, AllowDiscrete }`。
- 简写 `transition`：`<single-transition>#`——首 time=duration、次
  time=delay、easing 复用、ident 落 property（`none`/`all` 关键字）、
  `allow-discrete` 落 behavior；顺序自由、逗号多组；三 time/负 duration
  报错整条忽略（ParseReport）。
- 槽位布局：164..168 五新槽 + 动画描述符后移 → SLOT_COUNT 176
  （P1a 实施记录）。

### D2：reconciliation（restyle 提交点，styles.insert 之前）

判定序（css-transitions-2 §3，规格精化在案）：

1. 快路径：无活动过渡 && duration 全零 → return（稳态零写入）。
2. before-change 值 = styles 表旧条目；**无旧值（首帧）→ 不启动**
   （CSS 语义：元素首次获得样式无 before-change，不过渡）。
3. 候选展开：None 跳过；All = ALL 槽表剔除描述符；Ident = 属性名路由。
4. 每候选槽：③ 新值==活动过渡 to → 保持运行（容器收敛环 pass2 保护，
   不重启时钟）→ ① 新级联==生效值 → 取消 → ② combined duration
   （duration+delay）≤0 → 取消/不启动 → ④ 重定向（from=当前插值中间
   值）→ ⑤ 可插值探针（lerp_decl 中点；离散对需 allow-discrete）→
   ⑥ 动画覆盖槽抑制启动（偏差：动画层高于过渡层，启动后首帧即被
   覆写，徒留隐形计时器；已运行过渡不受影响）。
- `transition-*` / `animation-*` 描述符槽自身不可过渡（文档未定义
  其动画性，自指无意义）。

### D3：采样挂点（frame()）

- 顺序：restyle（reconciliation）→ **sample_transitions** →
  apply_animations（CSS 层叠：animation 层高于 transition 层，同槽
  动画值随后覆写过渡值）。
- 延迟段写 from（负 delay 直接落进行中段）；进度 ≥1 写 to 并移除
  条目（终值保持）；进行中按缓动求值 lerp_decl；不可插值对
  （allow-discrete 启动的离散过渡）按 50% 翻转。
- `start_time` = 触发 restyle 的帧时间（引擎唯一可观测语义——宿主
  在 frame(now) 时才看到变更，变更提交时刻即该帧）。
- 过渡表空 = 零写入（稳态铁律：无活动过渡时 frame() 与既有实现
  逐位一致）。

### D4：steps 语义修正（实施期发现的引擎 bug）

- `TimingFn::Steps(n, jump_end)` 原实现分支写反（jump_end 走了
  ceil = jump-start）。修正为 css-easing-1 语义：
  jump-end（默认）= 阶跃在段尾 = `⌊p·n⌋/n`（p=1 端点恰为终值）；
  jump-start = `⌈p·n⌉/n`（p=0 端点恰为初值）。

### D5：动画结束恢复底层值（实施期补齐的动画缺口）

- 原 apply_animations「已结束且无填充 → 不覆写」≠「恢复底层」——
  styles 表残留最后动画采样值。补 `anim_underlying` 副本机制：
  - 首见动画：快照关键帧槽位当前值（覆写前 = 级联/底层值）；
  - 条目存在：槽值 ≠ 上帧写入值 → 外部重算（restyle 重建 cs）→
    刷新快照（语义锚定最新级联）；槽集随轨道对齐（换动画名自愈）；
  - 已结束无填充 → 写回快照并移除条目；forwards/both 已结束 →
    副本移除（终值固定）。
  - 卫生：节点移除 / 伪节点死亡 / 全清三处同步清理。

### D6：测试驱动契约（锁定 19 件）

before-change 语义要求测试分三帧驱动：**基线帧**（建立 styles 表）→
**变更帧**（set_declarations + 立即 frame(0.0) = 启动帧，读 from）→
**推进帧**（time > start 采样中间值/终值）。锁定测试 19 件
（tests/transition.rs）：解析 5（shorthand/长手/容错/none/all/behavior）、
驱动 7（启动/中点/终值/负 delay/延迟保持/重定向/取消）、none/all、
steps 采样点、离散跳变 vs allow-discrete、动画层覆盖、稳态幂等、
文本路径 color 过渡。

## 否决项

- 全局 transition 事件回调（transitionrun/start/end）——宿主无订阅
  需求，YAGNI（可观测性走 tracing span）。
- interrupted transition 的 canceled-with-fill 语义（css-transitions-2
  完整事件模型）——v1 采样模型足够。
- transition 与 @starting-style 组合——上游 spec 尚在演进。

## 落地记录

- P1a 代理实施核心（槽位/解析/reconciliation/采样/19 锁测试）；
  上下文耗尽由主会话接管收尾：steps 分支对调（D4）、
  anim_underlying 机制（D5）、测试驱动契约修正（D6）。
- 全量 **545** 测试绿（518→545；transition 19/19 + soft 像素差分/
  serde 往返/doctest 全绿）。
