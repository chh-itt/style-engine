# Sink 一致性矩阵（SINK-MATRIX）

> 块 A（soft/vello parity）的门禁政策与偏差矩阵。特性级语义以
> [docs/FEATURES.md](FEATURES.md) 为权威；本文件回答「同一 DisplayList
> 经两个 sink 渲染，哪些行为要求一致、哪些是记录在案的近似」。

## 政策

1. **一致性基线**：soft（零依赖软光栅，确定性参照）与 vello（GPU）接收
   同一 `DisplayList`；布局/绘制语义（几何、层序、裁剪、混合、透明度）
   两 sink 必须一致——一致性由差分属性测试
   （`tests/differential.rs`：增量 ≡ 全量重放）与 Pixel Channel 双腿
   （`tests/pixel.rs` / `tests/pixel_vello.rs`）锁定。
2. **禁止静默忽略**：parity 目标是「每一条样式影响都进 Sink 渲染」；
   soft/vello 源码中 `let _ =`、下划线前缀假绑定、`.ok()` 丢弃一律视为
   行为断层。门禁工具 `crates/style-engine-conformance/tools/check_sink_ignores.py`
   全量扫描两 crate 源码（CI gate job ubuntu 腿 + 本地均可运行）；确需
   忽略的合法场景在该工具 `ALLOWLIST` 登记（路径+片段双匹配，条目失效
   亦报错，防陈旧白名单掩盖回归）。
3. **近似分级**：B 级 = 视觉细节豁免但需文档（下列矩阵 + FEATURES.md
   各条）；C 级 = 性能/上游限制取舍可保留。A 级（正确性）不允许偏差。

## 偏差矩阵

| 通路 | soft | vello | 分级/备注 |
|---|---|---|---|
| FillRect / Gradient 基元 | 逐像素直绘 | vello Scene | 一致（Pixel 通道双腿锁定） |
| 阴影 blur | 真 3× 可分离盒模糊（σ=blur/2） | 多重同心圆环近似 | B（两条路径均文档化，FEATURES「阴影」条） |
| Border dashed/dotted | 按边拆 FillRect 序列（P4 D1） | 同 soft（PaintOp 层已拆） | 一致；不等宽圆角弧起点 B |
| Text 对齐/spans | P7 收口：逐字符 span 归属 | parley VelloTextSystem | 一致；装饰线基样式单行近似 B |
| Text 折行 | 复用引擎测量（同源） | 复用引擎测量（同源） | 一致（折行在 L2，sink 零折行） |
| Image repeat/size | F3b 平铺 + space/round 精确化 | 同 | 一致（paint 层语义，sink 零分叉） |
| 变换 | 逆映射光栅化 | 自维护 xforms 栈 + 偏移共轭 | 一致；transform×自身滚动次序 C |
| clip-path / 滚动裁剪 | PushClip/ClipShape 直绘 | 同 | 一致（F3c） |
| 混合层（isolation/mix-blend） | 合成期混合 | vello push_layer 混合 | 一致（P1-2） |

## 变更协议

- 新增 sink 侧近似或豁免：先在 FEATURES.md 对应条目定级，再在本矩阵
  登记一行；需要静默忽略源码返回值时同步登记 `ALLOWLIST` 并附理由。
- 本矩阵与工具为单一事实源：`python crates/style-engine-conformance/
  tools/check_sink_ignores.py` 退出码 0 = 无未登记静默忽略、无陈旧白名单。
