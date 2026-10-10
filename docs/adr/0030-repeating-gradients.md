# ADR-0030: repeating 渐变三族与显式停点归一（含 P1-0 line_len 语义）

日期: 2026（P1-3 批 commit a06e1bb，8 文件，全量 506=499+7 测试；P1-0 停点归一同批前后落地，无独立 commit 号，见 IMPLEMENTATION-LOG 提交序列）
状态: 已接受
注记: **追溯补记（自 CHANGELOG/日志重构）**——本 ADR 与 ADR-0029 在 IMPLEMENTATION-LOG「追加批次：rem 基准修复（P0 批）+ P1-0～P1-3（ADR-0029/0030 时代，goal-e1e87e71）」中被编号引用，但 docs/adr/ 此前无对应文件；本文由 CHANGELOG.md 阶段7「P1-3」「P1-0」条与 docs/IMPLEMENTATION-LOG.md 对应段落重构。

## 背景

1. repeating-linear-gradient() / repeating-radial-gradient() / repeating-conic-gradient() 三族此前未支持（分派器按函数名拒收）。
2. P1-0 同期发现 vello sink `distribute_stops` 以 basis=0 解析显式停点——Percent 全部塌缩为 0、px 停点未按渐变线长归一（50px 与 100% 均按 0）；两缺陷同属「停点位置语义」管线，一并归档本 ADR。

## 决策

- **D1 解析复用**：分派器按函数名 strip `repeating-` 前缀复用既有三解析器（内层文法逐一相同）；`css::Gradient.repeating: bool` 随 BackgroundImage→`PaintOp::Gradient` 流入双 sink。
- **D2 平铺语义（css-images-3）**：周期=首末停点跨距，停点模式沿渐变轴无限平铺；显式逆序停点按 §4.5.2 抬升后首末重合→周期 0→透明黑（source-over 无操作）；全缺省停点→周期=全长，退化为非 repeating。
- **D3 vello 终结**：画刷几何收缩为「一个周期」（linear=单位向量缩段 / radial=`new_two_point` 两圆承载 r0 首停相位 / sweep=起终角弧段）+ stops 平移归一 + `Extend::Repeat`。径向必须 new_two_point——r0=0 + 平移 stops 会丢首停相位。
- **D4 soft 终结**：采样 u = first + (t − first).rem_euclid(period) 回停点序列插值；t 不夹取（CSS 无限平铺语义）。
- **D5 停点归一（P1-0）**：`distribute_stops` 签名加 `line_len: f32`——px→v/line_len 夹 [0,1]、%（存储即分数）夹 [0,1]、其余单位（em/rem/cq）→NaN→自动均布（B 级豁免在案）；NaN 填充后按 css-images-3 §4.5.2 对逆序停点单调夹取（后停<前停抬至前停）。几何臂 line_len 供给：linear=渐变线长（geom 与回落公式两臂）、radial=ry.max(0.5)（与画刷实际用半径一致）、conic=1.0（sweep 停点=角分数全周）。soft sink stop_positions 补同款单调夹取（双 sink 一致性契约）。解析器不做夹取——用值期语义归 sink。

## 边界与已知偏差

- em/rem/cq 显式停点退化为自动均布（B 级豁免在案）。
- 周期 0 = 透明黑（source-over 无操作）是语义行为，非错误路径。
- `GradientDump.repeating` 以 `#[serde(default)]` 保持旧 dump 反序列化兼容。
- P1-0 教训在案：conic 臂显式 1.0 与初始值重复触发 dead-assignment 警告（clippy -D warnings 会挂）；vello 测试 use 需 `vello::kurbo::Point`（kurbo 非独立 crate 名）、`ColorStops` 不可 `.iter()`（索引访问）。

## 后果（锁定测试与文档同步）

- 锁定：解析三族 flag/kind；repeating 标记流入 DisplayList；serde 往返 + 旧 dump 回落；vello 段收缩与周期 0 纯 fn 两件；soft 像素锁两件（40px 盒周期 20px：x=0/5/10 手算 (249,0,6)/(185,0,70)/(121,0,134)、x 与 x+20 同色；周期 0 画布不变）；`gradient_stop_positions_normalize_and_monotonic`（200px 线：0%/50px/缺省/100% → 0.0/0.25/0.625/1.0；逆序 60%+40% → [0.6,0.6]）。
- 文档同步：docs/FEATURES.md T0 repeating 条（P1-3 批登记）、T4 渐变条 stop 位置注记更新（P1-0 修）；CHANGELOG 阶段7 P1-0/P1-3 条；无新依赖（C4 纪律）。

## 否决项

- **解析期展开平铺停点序列**——停点数量随盒子长度无界增长，且丢失「单周期」结构信息，双 sink 各自重建成本高；改为流 `repeating: bool` + sink 侧周期化。
- **解析期夹取停点**——违背既定「解析不夹取、用值期归 sink」契约（P1-0 锚点）；夹取/归一/单调化统一在用值期 sink 侧执行（vello 与 soft 同款）。
- **vello 径向沿用 r0=0 + 平移 stops**——实测丢失首停相位（平移后 stops 以 r0=0 起算，first 相位消失）；改 `new_two_point` 两圆承载 r0 相位为硬契约。
