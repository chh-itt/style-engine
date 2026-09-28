# transform 几何模型：绘制期仿射 + 双时机包含块

日期：2026-09。状态：Accepted（实现排期见 FEATURES/backlog；本 ADR 定契约）。

## Context

transform 是 ADR-0008 触发条件全集的第一个未实现项，但它横跨三层语义，必须先分清各自的层级与时机再动手：

1. **几何变换本身**——元素视觉上被平移/旋转/缩放/错切；
2. **containing block 副作用**——transform ≠ none 的元素成为 absolute/fixed 后代的包含块（CSS Transforms §3）；
3. **stacking context 触发**——transform ≠ none 创建 SC（ADR-0008 全集之一）。

三者常被混为一谈；ADR-0008 初版称 containing block 副作用与 SC 触发「同点实现」，本 ADR 予以修正。

## Decision

### 1. 几何变换 = L3 绘制期（布局盒保持未变换坐标）

- taffy 永远看不到 transform：布局树、Frame 盒、滚动量程全部使用**未变换**几何（CSS Transforms：transform 不影响布局）。
- DisplayList 新增 `PushTransform{affine}` / `PopTransform` 层对；sink 用 vello `push_transform(Affine)` + `pop_transform`（纯矩阵，无 layer 分配，与 PushOpacity 的 push_layer 不同）。
- 仿射在 **paint 收集期终结**：paint_node 已知盒的 border-box 尺寸，故 translate 百分比（相对自身 border-box）与 transform-origin（默认 50% 50%）在此解析为绝对 Affine：`A = T(origin) · M · T(−origin)`（M = 函数列表按书写顺序连乘，CSS 语义 A(B(C(p)))）。级联层只保存原始声明值，不做百分比终结。
- 命中测试与滚动边界是宿主关切：宿主拿到的是未变换盒；需要命中变换后内容时自行求逆映射。v1 的 `Frame.scrollable` 并集不补偿变换后的溢出（近似记录，与「绝对定位后代并入为近似」同级）。
- **范围 v1 = 2D 仿射**：`translate/rotate/scale/skew/matrix` 及其 2D 变体。3D 函数（matrix3d/translate3d/rotate3d/scale3d/perspective）解析层拒绝（warn + 声明丢弃，CSS 宽容路径）——vello 0.10 是纯 2D 仿射，CSS 3D 属 L4 领域，重估条件 = 上游 3D 能力或扁平化投影需求成立。

### 2. containing block 副作用 = L2 布局期（restyle 解析，cb walk 消费）

- transform ≠ none 的元素**在 restyle 期**物化为 ComputedStyle 标记（`has_transform_cb` 语义位）。
- 消费点是 cb 演走：`abs_avail_width` 的「最近 positioned 祖先」谓词扩为「positioned **或** has_transform_cb」；将来 fixed 落地（T2）时同一标记服务 fixed 的 cb 解析。
- 该效果**改变布局结果**（absolute 后代的可用宽、锚定几何随之变化），属 L2 职责——这就是它与 SC 触发的本质区别。

### 3. SC 触发 = L3 paint 收集期（单点判定不变量）

- paint_node 的 SC 判定单点扩入 `transform ≠ none`；层栈顺序（外→内）：**PushTransform → clip → filter → PushOpacity**——变换建立局部坐标系，其余包装都发生在该坐标系内。
- 三带模型（Neg/Flow/Pos）与 SC 原子性不受影响：非定位 SC 触发者键 0 树序（transform 触发的非定位元素进 Pos 带键 0）。

### 4. 「同点实现」修正（对 ADR-0008 的勘误）

两者共享的是**判定谓词**（transform ≠ none，单一来源 `has_transform(cs)`），但**时机不同**：

| 语义 | 层级 | 时机 | 消费者 |
|---|---|---|---|
| 几何变换 | L3 | paint 收集期 | PushTransform/PopTransform + sink |
| containing block | L2 | restyle 期（标记物化） | abs_avail_width / 将来 fixed cb walk |
| SC 触发 | L3 | paint 收集期 | paint_node 单点判定 + 三带归位 |

谓词单点、时机双点——ADR-0008 的触发条件全集表中 transform 行的「同点实现」表述以本表为准。

## Consequences

- L1/L2 无新属性依赖：transform 解析进级联（值 = 函数列表）即完成 L1；L2 只加一个语义位；L3 承担全部几何。
- 滚动量程、命中测试的变换补偿是显式近似（记录于 FEATURES），不静默。
- 3D 拒绝路径与 CSS 宽容语义一致（warn 后继续），不破坏样式表其余部分。
- 实现票（backlog「transform 实现」）的验收用例应进入 Numeric Channel：布局面（transform-CB 改变 absolute 后代几何）0.5px 可验；绘制面走 Pixel Channel（旋转后的像素对比属 Class 3 几何，零容忍）。
