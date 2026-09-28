# 滚动容器：所有权与量程

滚动是引擎里第一处「宿主输入直接进入绘制结果」的连续量。按 ADR-0005 的所有权原则定案：滚动偏移由宿主拥有并推进，引擎只识别滚动容器、计算量程、在绘制时平移——crate 内没有任何滚动状态机（无惯性、无夹紧动画、无命中测试回弹），从而保住 frame() 的幂等不变量（同输入同输出）。

## 决策

- **偏移所有权 = 宿主**：`set_scroll_offset(key, x, y)`（未知 key 报 `ContractError::UnknownNode`）按节点推进偏移；结构性变更（remove/set_children 清理）同步丢弃失效偏移（与 transition/clock 的处理同型）。引擎不夹紧——量程由 `Frame.scrollable`（key → 各轴最大滚动量，非滚动轴为 0）上报，clamp 与回弹动画是宿主的事。
- **识别**：overflow-x/y ∈ {auto, scroll}（两轴独立判定）→ 滚动容器；hidden/clip = 仅裁剪、不可滚动、无量程上报；visible 无裁剪。解析层把 `auto` 归一为 `Overflow::Scroll`（引擎无从判定"是否溢出"，auto 与 scroll 的量程/裁剪行为本就相同，差异属宿主交互层）。
- **可滚动外延**（量程计算）：padding box 自身与全部后代 border box 的并集，相对 padding box 原点的最大超出。近似：绝对定位后代一并并入（按 CSS 应归其包含块链，随定位模型细化）；文本叶以自身盒计入。
- **滚动条 = overlay、宿主所有**：引擎永远不为滚动条预留布局空间（区别于桌面 classic scrollbar）；宿主可用 `LayoutEntry` 自绘。sticky 同理后置：契约 = 后布局位移 pass（读宿主偏移，不改 taffy 结果），实现停泊。
- **绘制**：滚动容器 PushClip（padding box + radius）→ PushScroll{dx,dy} 子树平移 → PopScroll → PopClip。平移只发生在 DisplayList 消费端与 paint 生成端，不改 taffy 布局——偏移变更不触发重排，宿主下一帧拿到的布局盒不变，仅 paint 与 clip 生效。

## Considered Options

- 引擎内建滚动状态（自动夹紧/动量）：被否——破坏幂等与零副作用；滚动策略（iOS 式回弹、平滑滚动、scroll-snap）应属宿主。
- 滚动条占布局空间（classic）：被否——vello 应用多为 overlay 视觉；classic 需要第二套盒模型分支，等真实宿主需求出现再议。
- 量程放 PaintOp（绘制时顺带算）：被否——量程是布局事实，应在 Frame 与布局盒同级上报，绘制无关。

## Consequences

- `Frame` 新增 `scrollable: HashMap<K, (f32, f32)>`；测试覆盖量程计算（padding/border 计入）、hidden 不上报、偏移平移进 DisplayList。
- 偏移变更的成本 = 一次 paint 重建（无 restyle/无 taffy 重排）——宿主滚动循环可以 120fps 跑。
- 滚动容器嵌套：偏移各自独立（按节点键），裁剪层随 paint 树天然嵌套。
