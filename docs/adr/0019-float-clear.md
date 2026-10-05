# ADR-0019：float / clear 浮动结算（E4）

状态：已接受（E4 批实现期）
日期：2026（实现批次记录）

## 背景与问题

31 项缺口清单之 float/clear 盘点确认真缺口（全 src 树零实现，仅 text.rs :107
一处 Rust 类型注释词命中）。taffy 0.14.0（Cargo.lock :2326 坐实）实现范围 =
flexbox/grid/block，**无原生 CSS float 支持**（Style 无 float 字段）。

浮动语义（CSS 2 §9.5 / css-position-3）：浮动盒出流；后续行内/块内容环绕
（行盒与块可用宽度缩短避让浮盒）；`clear` 钳位后续盒 y ≥ 相关浮盒底缘。
A体B度定位：浮动是**布局语义**非绘制语义——仅影响盒几何，不进 paint。

## 决策

### E1 属性面（property.rs）

- `FloatKind`（None 缺省 / Left / Right）+ `Clear`（None 缺省 / Left / Right /
  Both）两新枚举（`#[non_exhaustive]` + Default）。
- PropertyId +2 槽：属性尾部追加（实现期对 property.rs ALL 表与动画描述符
  表核验精确槽位；计划 Float/Clear=132/133，7 动画描述符顺延 134-140，
  SLOT_COUNT 139→**141**）；解析（ident 臂）+ initial + 访问器
  `ComputedStyle::float()` / `ComputedStyle::clear()`。
- 0.x 破坏性：PropertyId/DeclValue +2 变体（SLOT_COUNT 141）、ComputedStyle
  +2 访问器（纯增量）。

### E2 结算模型（engine.rs `settle_floats`）

- 引擎侧结算 pass：`fn settle_floats(&mut self, viewport: (f32, f32))`。
- **插位 = frame() 收敛环内 `settle_columns`（:1404）之后、文本换行重排
  （T5c-2 remeasure，:1410）之前**——浮动缩短后续内容包含块宽 → 折行宽
  依赖结算值（与 settle_calc/tables/columns 同理同块内）。
- 语义四则：
  1. **出流**：float≠none 的盒自正常流移除——后续兄弟布局不再为其预留
     空间（结算期按收集序重排几何）；
  2. **堆叠**：同向浮盒沿主轴排布（右浮右缘对齐容器内容右缘），空间不足
     下移（y = 前浮盒底缘）；
  3. **环绕**：浮盒之后兄弟内容的可用宽度 = 容器内容宽 − 浮盒侵占区间
     （左浮→左缘侵占、右浮→右缘侵占）；
  4. **clear 钳位**：clear:left/right/both 的盒 y ≥ 左/右/任向最后浮盒底缘。
- 0.x 诚实边界（首版范围收敛）：环绕作用于**兄弟级后续内容**；嵌套浮动
  （浮盒内部再浮）按子树局部结算；BFC 建立元素（overflow≠visible 等）为
  浮动隔离边界。残余偏差逐条记 FEATURES 偏差段，迭代有据。

### E3 锁测试（tests/css_float.rs）

盒几何锁（A体B度=Chromium getBoundingClientRect 对齐维度）：
左浮+后续块环绕缩宽 / clear:both 下移 / 右浮右缘堆叠 / 空间不足浮盒下移 /
float:none 全透回归。

## 否决

- taffy 树内原生 float 仿真（taffy 无 float 原语）。
- paint 层仿真（浮动是布局语义非绘制语义）。
- 完整 BFC 语义全集首版即达（先兄弟级环绕 + clear 钳位，嵌套/BFC 边界
  按需迭代）。

## 0.x 破坏性清单

1. PropertyId +Float/Clear、DeclValue +2 变体（SLOT_COUNT 139→141，
   动画描述符顺延；实现期对表核验）。
2. ComputedStyle +`float()` / `clear()` 访问器（纯增量）。
