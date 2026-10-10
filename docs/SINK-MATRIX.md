# Sink 一致性矩阵（SINK-MATRIX）

> 块 A（soft/vello/tiny 三 sink parity）的门禁政策与偏差矩阵。特性级语义以
> [docs/FEATURES.md](FEATURES.md) 为权威；本文件回答「同一 DisplayList
> 经多个 sink 渲染，哪些行为要求一致、哪些是记录在案的近似」。

## 政策

1. **一致性基线**：soft（零依赖软光栅，确定性参照）、vello（GPU）与
   tiny（tiny-skia CPU，嵌入式目标）接收同一 `DisplayList`；布局/绘制
   语义（几何、层序、裁剪、混合、透明度）各 sink 必须一致——一致性由
   差分属性测试（`tests/differential.rs`：增量 ≡ 全量重放）与 Pixel
   Channel 三腿（`tests/pixel.rs` / `tests/pixel_vello.rs` /
   `tests/pixel_tiny.rs`，tiny 腿全平台必跑、无 GPU 探测跳过逻辑）锁定。
2. **禁止静默忽略**：parity 目标是「每一条样式影响都进 Sink 渲染」；
   soft/vello/tiny 源码中 `let _ =`、下划线前缀假绑定、`.ok()` 丢弃一律
   视为行为断层。门禁工具 `crates/style-engine-conformance/tools/check_sink_ignores.py`
   全量扫描三 crate 源码（CI gate job ubuntu 腿 + 本地均可运行）；确需
   忽略的合法场景在该工具 `ALLOWLIST` 登记（路径+片段双匹配，条目失效
   亦报错，防陈旧白名单掩盖回归）。
3. **近似分级**：B 级 = 视觉细节豁免但需文档（下列矩阵 + FEATURES.md
   各条）；C 级 = 性能/上游限制取舍可保留。A 级（正确性）不允许偏差。

## 偏差矩阵

| 通路 | soft | vello | tiny | 分级/备注 |
|---|---|---|---|---|
| FillRect / Gradient 基元 | 逐像素直绘 | vello Scene | tiny-skia Path/Shader | 一致（Pixel 通道三腿锁定） |
| 渐变停点均布 | 核心 `distribute_stop_positions`（P9-1a 共享单源） | 同（P9-1a 起删本地实现） | 同 | 一致；旧 soft 前向填充塌缩（中段无位停点并到前停位）已修 |
| 渐变色彩提示 | `apply_gradient_hints` 展开（Px/% 提示；em/rem/cq 丢弃） | 同 | 同 | 一致（P9-1a）；无上下文单位提示丢弃=B·豁免（与 em 停点同约定） |
| 渐变停点非 Absolute 色防御分支 | 不透明黑 | 不透明黑 | 不透明黑 | 一致（P9-1a 统一；旧 soft=透明黑、vello=不透明黑相反——引擎契约下不可达路径） |
| 阴影 blur | 真 3× 可分离盒模糊（σ=blur/2） | 多重同心圆环近似 | 真 3× 可分离盒模糊（复用 soft `blur_alpha_u8`，σ=blur·scale/2；ADR-0043 D2 单源） | soft/tiny A 级同源；vello 圆环近似 B（FEATURES「阴影」条） |
| Border dashed/dotted | 按边拆 FillRect 序列（P4 D1） | 同 soft（PaintOp 层已拆） | 同 | 一致；不等宽圆角弧起点 B |
| Text 对齐/spans | P7 收口：逐字符 span 归属 | parley VelloTextSystem | parley 排版 + skrifa 轮廓填充（YFlipPen 逐字形） | 一致；tiny 支持 font_features/variations/装饰线（含 wavy）/文本影；装饰线基样式单行近似 B |
| Text 折行 | 复用引擎测量（同源） | 复用引擎测量（同源） | 复用引擎测量（同源，parley） | 一致（折行在 L2，sink 零折行） |
| Image repeat/size | F3b 平铺 + space/round 精确化 | 同 | 同 | 一致（paint 层语义，sink 零分叉） |
| 变换 | 逆映射光栅化 | 自维护 xforms 栈 + 偏移共轭 | 同 vello（`effective()`=T(+off)∘top∘T(−off)；`draw_tr()`=effective∘world∘T(−origin)） | 一致；transform×自身滚动次序 C |
| clip-path / 滚动裁剪 | PushClip/ClipShape 直绘 | 同 | 同（组缓冲 + `apply_mask` DestIn 合成） | 一致（F3c） |
| 混合层（isolation/mix-blend） | 合成期混合 | vello push_layer 混合 | 组缓冲混合；PlusDarker 逐像素复用 `soft::blend_pixel`（tiny-skia 缺该模式） | 一致（P1-2） |
| conic 渐变起点采样 | start_deg 归一 | vello Pad 采样在 start>360° 时相位缺失 | `rem_euclid(360°)` 归一 + Repeat 采样 | tiny 修正 vello 缺陷；vello 端升级待议（上游 vello gradient 采样语义） |
| 径向椭圆渐变 | 标准椭圆语义（rx/ry 正向） | rx/ry 交换疑似上游缺陷 | `T(c)∘S(rx/ry)∘T(−c)` 标准语义 | tiny/soft 一致；vello 端待议（带 golden 证据后与上游对质） |

## tiny 合成不变量（P10 实证，tiny-skia 0.12 约束）

- **组遮罩尺寸**：`RasterPipelineBlitter::new` 要求 mask 尺寸 == 目标
  SubPixmap 尺寸，不匹配即 `log::warn!` + 静默空绘。tiny 的组合成一律
  先 `apply_mask`（DestIn，组缓冲与组遮罩同尺寸恒过）再无遮罩
  `fill_rect`；PlusDarker 手动像素路径按组局部坐标索引，天然规避。
- **Pattern 锚定**：`Pattern::new` 的 transform 语义 = 瓦片局部→填充
  坐标。identity 会把瓦片钉在目标 (0,0)（内容整体位移 −(ox,oy)）；所有
  组缓冲/阴影/BackdropFilter 合成必须 `from_translate(合成矩形左上)`。
- **AA 覆盖率边界**：路径边界落在整数 y 的像素行覆盖率为 0（行 y 覆盖
  区间 [y, y+1)）——golden 对比预算已吸收，tiny 内建测试断言须避开边界行。

## 变更协议

- 新增 sink 侧近似或豁免：先在 FEATURES.md 对应条目定级，再在本矩阵
  登记一行；需要静默忽略源码返回值时同步登记 `ALLOWLIST` 并附理由。
- 本矩阵与工具为单一事实源：`python crates/style-engine-conformance/
  tools/check_sink_ignores.py` 退出码 0 = 无未登记静默忽略、无陈旧白名单。
