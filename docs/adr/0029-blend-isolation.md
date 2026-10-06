# ADR-0029: mix-blend-mode / isolation 实混合层

日期: 2026（P1-2 批，commit bf6ce2c，10 文件，全量 499=494+5 测试）
状态: 已接受
注记: **追溯补记（自 CHANGELOG/日志重构）**——本 ADR 与 ADR-0030 在 IMPLEMENTATION-LOG「追加批次：rem 基准修复（P0 批）+ P1-0～P1-3（ADR-0029/0030 时代，goal-e1e87e71）」中被编号引用，但 docs/adr/ 此前无对应文件；本文由 docs/CHANGELOG.md 阶段7「P1-2」条与 docs/IMPLEMENTATION-LOG.md 对应段落重构。

## 背景

1. mix-blend-mode 与 isolation 在第五批㉒仅以「SC 触发位」存在：`DeclValue::Effect(bool)` 存在性语义位驱动 stacking context 带序（ADR-0008），不产生任何绘制效果；`isolation: isolate` 存在「隔离组内混合穿透祖先画布」缺口。
2. css-compositing-1/2 要求 mix-blend-mode 全 18 值（16 标准模式含 normal + plus-lighter/plus-darker）有真实混合语义，isolation 形成隔离组边界。
3. 同期 filter/backdrop-filter 效果本体因 vello 0.10 无滤镜管线保持 T2 暂缓（存在位先行的既有先例），混合层为本体可落地的首批效果之一。

## 决策

- **D1 数据模型**：新公共枚举 `BlendMode`（css-compositing-1 16 标准模式含 normal + plus-lighter/plus-darker = 全 18 值）；`DeclValue::BlendMode` 替换旧 `Effect(bool)` 存在位——模式值需随声明进入级联与绘制链，布尔位无法承载。
- **D2 层对与层序契约**：`PaintOp::PushBlend{mode,x,y,width,height}/PopBlend` 层对，bbox=border-box 同 PushOpacity。混合层必须最外——先 PushOpacity push、后 PopOpacity pop，合成序=blend(背后画布, opacity(子树))：子树先整体按 alpha 合成，再与已绘制底以声明的混合模式合成。
- **D3 isolation 语义**：`isolation: isolate` ≡ `PushBlend{Normal}`——Normal 模式层对即隔离组边界，子树内混合不越界，修复旧「isolate 内混合穿透祖先画布」缺口。
- **D4 双 sink 终结**：
  - soft sink：全 18 值原生像素合成——快照底 + 清区累积 + pop 按模式全式合成 Co=αs(1−αb)Cs+αs·αb·B+(1−αs)αbCb；非可分离模式按 W3C Lum/Sat 定义；plus 族按预乘加法惯例。
  - vello sink：peniko `Mix` 枚举 16 标准模式一一映射；plus-lighter/plus-darker 不在 peniko Mix 枚举（上游缺口，登记 docs/DEPENDENCIES.md）→ 退 `Mix::Normal`，B 级偏差在案。

## 边界与已知偏差

- vello 16/18：plus 族两值在 vello 侧退 Normal（B 级）；soft sink 全 18/18 原生。上游补齐 peniko Mix 后应回补直映。
- filter/backdrop-filter 效果本体仍 T2 暂缓（vello 无滤镜管线）；`Effect(bool)` 存在位机制仅对这两族保留。
- 混合范围以 stacking context 三带（ADR-0008）原子性为准：层对覆盖「子树 vs 其后已绘制底」，与 CSS「与 stacking context 内背后内容混合」语义一致；跨 SC 的混合归属由 SC 边界表达。
- 顺修（非本 ADR 主题）：paint_dump 三测试无条件引用 `crate::paint_dump`（`#[cfg(feature="serde")]` 模块）致默认 feature 下 `cargo test -p style-engine --lib` 编译失败（HEAD 既有问题）——补 serde 门后默认 214 / --all-features 217 绿。

## 后果（锁定测试与文档同步）

- 锁定：SC 层对发射与发射序（blend 包 opacity）、解析语义重基线（isolation/mix-blend 各 +2 op）、soft 像素锁 ×10（可分离/加法/非可分离/嵌套 opacity）、BlendMode serde 往返（kebab-case 模式名）。
- 文档同步：docs/FEATURES.md T0 新增混合层条（本次追溯补建）、T2 条改写（filter/backdrop-filter 保持暂缓）、docs/DEPENDENCIES.md vello Mix 枚举缺口节；CHANGELOG 阶段7 P1-2 条（commit bf6ce2c）。

## 否决项

- **维持 `Effect(bool)` 存在位**——无法承载混合模式值，且使 isolation 与 mix-blend-mode 语义混同；替换为 `DeclValue::BlendMode`。
- **blend 层对内嵌于 opacity（先 blend 后 opacity）**——合成序错误（会变成 opacity(blend(底, 子树))），与 CSS 背景混合语义不符；定为「混合层最外」契约。
- **vello 侧软件模拟 plus 族**——越出 vello sink「直映上游画刷」契约，复杂度与维护面不成比例；以 B 级退化 + 上游缺口登记（DEPENDENCIES.md）替代。
