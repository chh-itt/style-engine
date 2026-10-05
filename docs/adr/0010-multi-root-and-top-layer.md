# ADR-0010：多根引擎与 top-layer（ICB 超根方案）

日期：实施期 A3。状态：已接受。

## 背景

引擎原为单根：`StyleEngine::insert(None, key, node)` 至多一次
（重复报 `ContractError::RootExists`），根占用 `StyleTree` 的 arena
根槽。缺口清单第 3 项要求多根（弹窗/浮层）与 top-layer 绘制序。

## 决策

1. **超根（synthetic super-root）**：`StyleTree::new()` 的 arena 根槽
   变为合成超根——永不绑定宿主 key、样式输入恒为默认空。用户根经
   `insert_child(super_root)` 挂载，可以有任意多个。
2. **根分类**：首个用户根 = 文档根（`root_key`，现有语义逐位保留，
   ICB 内 block 流）；后续用户根 = overlay 根（插入序记于
   `overlay_roots`）。
3. **top-layer**：`set_top_layer(key, on)` 把 overlay 根在「普通
   overlay」与「top 层」两个名单间移动；有效绘制序 =
   文档根 → 非 top overlay（插入序）→ top 层根（进层序）。top 层根
   与文档根互为兄弟子树，故**不被文档 overflow 裁剪**、不共享文档
   层叠上下文（与 CSS top layer 一致）。引擎每帧
   `sync_root_order()`：把超根的树子序与 taffy 子序重排为有效绘制
   序，此后所有既有走查（绘制/命中/滚动量程/文本/级联）零改动地
   遵序。
4. **布局锚定**：overlay 根的 taffy 样式在样式同步期特判——
   position Absolute + top/left 0 + right/bottom auto：相对 ICB
   （视口）锚定、auto 尺寸 fit-content（弹窗语义）；文档根保持
   block 子（宽度默认填满视口，与现状一致）。taffy 镜像结构
   viewport → 超根不变：每个用户根都是超根之子，根盒 margin 相对
   ICB 生效的既有语义自动成立。
5. **级联根**：超根样式恒空 → 用户根继承=初始值，天然是级联根；
   `:root` 重定义为「父=超根的用户根」（匹配全部用户根）。

## 兼容性

- 单根宿主：绘制/布局/级联逐位不变（超根为空直通层）。
- `insert(None)` 的 `RootExists` 不再发生——契约放宽（允许更多输
  入），非破坏；`BREAKING-POLICY` 0.x 在案。
- `ContractError` 新增 `NotOverlayRoot`（对文档根调用
  `set_top_layer(key, true)`）——`#[non_exhaustive]` 加变体，minor。

## 后果

- `StyleTree::is_root` 语义变更（超根不再是 `:root`）——唯一消费者
  为 `:root` 伪类，回归由锁测试覆盖。
- 根移除语义：文档根移除=全清（含全部 overlay）；overlay 移除=
  子树摘除（文档不受影响）。
