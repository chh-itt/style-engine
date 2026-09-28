# 层叠模型：stacking context 与 opacity

paint 层的绘制顺序从「兄弟按 z 稳定排序」（T4d）升级为 CSS 2.1 Appendix E 的简化三带模型，并把 opacity 纳入为第一个 stacking context 触发者。此前若先单独实现 opacity，半透明节点会被当作普通 in-flow 节点参与树序混合，层叠结果仍是错的——所以二者必须一次做对。

## 决策

- **SC 触发条件**（本引擎当前支持的子集）：根节点；positioned 且 z-index 为数字（`ZIndex(Some)`）；opacity < 1（paint 层 clamp 到 [0,1] 后判定）。transform/filter/isolation 触发者随对应属性落地时加入。
- **三带绘制序**（一个 stacking context 内部）：① 自身背景/边框/文本 → ② Neg 带：负数字 z 的 positioned 子树（z 升序、等值树序）→ ③ Flow 带：in-flow 非定位、非 SC 触发内容（树序，含文本叶）→ ④ Pos 带：positioned（auto/0 键 0 按树序在前、正 z 升序在后）与非定位但 opacity<1 的 SC 触发者（键 0 树序）。
- **SC 原子性**：SC 节点的子树经 `paint_node` 递归整体绘制，天然作为一个单元参与父级带排序；SC 内部继续按三带递归。
- **opacity**：`PaintOp::PushOpacity{alpha,x,y,w,h}/PopOpacity` 层对包住整节点绘制（背景边框文本子树全部在内）；sink 侧用 vello `push_layer` 的 alpha 参数（与裁剪层同机制）。`z-index: auto` 已在解析层物化为 `ZIndex(None)`，与缺席等价——「有值且为 Some」即是将来判定"此定位元素创建 SC"的依据。
- 与 T4d 的行为差异：positioned z-index:auto/0 现在绘制在 in-flow 内容**之后**（Appendix E step 6），即使 DOM 序在前；负/正 z 相对顺序与旧排序一致。

## Considered Options

- 继续单一排序列表（T4d 模型）：被否——无法表达 in-flow 与 auto/0 定位的带序差，opacity 无处安放。
- 完整 Appendix E（行内级、float 级独立带）：被否——float 未实现、文本叶即块级叶，七步中三步并带等价且可测试。

## Consequences

- `PaintOp` 新增 `PushOpacity/PopOpacity`（成对不变量与 PushClip/PopClip 同）；paint 测试断言带序（appendix_e_band_order）与层对（opacity_layer_pairing）。
- 后续 transform/filter 落地时只需扩展 SC 触发判定与 paint 层包装，带模型不变。
- flex/grid 子项 z-index（无 position）仍不生效，属已知残余偏差。
