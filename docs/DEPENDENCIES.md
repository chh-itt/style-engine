# 依赖清单与版本策略

原则（2026-09 定）：选兼容的最新版本；一个 workspace 内每个 crate 只出现一个版本（One-Version Rule）；升级节奏由 vello 的 wgpu 锁定充当节拍器。版本核实日期：2026-09-11（数据源 crates.io API）。

## 决定性依赖

| Crate | 版本 | 层 | 角色 | 备注 |
|---|---|---|---|---|
| cssparser | 0.38.0 | L1 | CSS 词法/语法解析，错误恢复 | 2026-09-07 发布，MPL-2.0 |
| cssparser-sel（package=cssparser） | 0.37.0 | L1 选择器 | selectors 0.40 的配对解析器；0.38 起解析器改单生命周期，二者类型不兼容，重命名依赖隔离于 src/selector.rs 内部（选择器预lude 以源文本形式跨界） | MPL-2.0 |
| precomputed-hash | 0.1.1 | L1 选择器 | selectors 0.40 要求的选择器词哈希；SelString 包装实现（FNV-1a） | Apache-2.0/MIT |
| selectors | 0.40.0 | L1 | 选择器解析/匹配/specificity | 需实现 SelectorImpl，MPL-2.0 |
| color | 0.3.3 | L1 | CSS Color 4 颜色模型（oklch/color-mix）| linebender 出品 |
| taffy | 0.14.0 | L2 | 布局引擎 | 2026-08-24 发布；calc 集成面 = 类型擦除指针 + 宿主回调（`CompactLength::calc(*const ())` / `traits.rs calc(val, basis)`，block.rs 处以 parent_size 为基调用）——接入需指针所有权约定，待 calc 正式特性票 |
| slotmap | 1.1.1 | 核心 | StyleTree 与 secondary map 存储 | |
| bitflags | 2.13.2 | 核心 | StateFlags | |
| smallvec | 1.x | L1 | 声明/子选择器内联存储 | 由 cargo update 定 patch |
| peniko | 0.6.1 | 核心(词汇表) | 画笔/颜色/图片类型 | 无 GPU 依赖，vello 同款，见 ADR-0001 |
| kurbo | (peniko 传递) | 核心(词汇表) | DisplayList 几何类型 | |
| parley | 0.11.1 | L2 核心 | 文本 shaping/测量/行布局（测量内置，见 ADR-0006） | 传递引入 fontique |
| vello | 0.10.0 | sink | GPU 绘制执行器 | 2026-08-14 发布 |
| wgpu | **29.x** | sink | GPU 底座 | ⚠️ 见下"版本配对" |
| winit | =0.31.0-beta.3 | demo/harness | 窗口（仅示例与冒烟测试） | beta，精确锁版本 |
| image | 0.25.10 | 资源 | 图片解码（宿主喂字节，核心不做 IO） | |
| tracing | 0.1.x | 全部 | 警告/诊断通道 | ParseReport 同时进 tracing |
| bytemuck | 1.x | sink | vello 传递依赖（^1.25） | |

## 版本配对：wgpu 必须跟 vello 走（29，不是 30）

vello 0.10.0 的 wgpu 依赖为 `^29.0.3`（optional feature `wgpu`）。wgpu 29 与 30 是 semver 不兼容的大版本：若我们直接声明 wgpu 30.0.1，应用会同时编译两份 wgpu（或无法与 vello 统一）。因此：

- sink 与宿主统一使用 vello 传递的 wgpu 29.x，sink crate 显式声明同版本并 `pub use wgpu;`。
- 这符合"选**兼容的**最新版本"原则：29.x 就是当前兼容的最新。等 vello 升级支持 wgpu 30（linebender 节奏通常数周内跟上）后整体升级。
- winit 0.31-beta 与 wgpu 的对接走 raw-window-handle，不受此影响。

## 版本策略

- 决定性依赖（上表前 12 行）在 workspace 根 Cargo.toml 用 `[workspace.dependencies]` 统一声明，成员 crate 一律引用 workspace 版本。
- winit beta 是唯一精确锁（`=0.31.0-beta.3`）的依赖，且只出现在 demo/harness；0.31 stable 发布后立即替换。
- vello/parley/taffy 均 0.x：允许破坏性升级，但必须一次升级整条 linebender 链（vello+peniko+parley 同批），不允许混代。
- MSRV 以依赖最高者为准（当前这一代 linebender/wgpu 通常要求 Rust 1.85+，edition 2024），CI 用 cargo-hack 验证后在 README 定值。
- 不承诺 no_std；L1（值与级联）保持 no_std+alloc 可达性，作为将来选项保留。

## taffy calc 直通（预留特性票，未排期）

调研结论（2026-09）：taffy 0.14 的 calc = 类型擦除指针 + 宿主回调（`CompactLength::calc(*const ())` / `traits.rs fn calc(&self, val: *const (), basis: f32) -> f32`，布局期以 parent_size 为基调用，见 block.rs 各 resolve 点）。接入决策：

- **独立特性票**，不搭车其他变更。实现必须引入 `unsafe`（指针所有权）、生命周期契约（calc 表达式必须活得比 TaffyTree 长——引擎侧由引擎池拥有、结构变更时回收），并走**独立的 unsafe review**（单独 PR，不与功能混提）。
- **前置条件**：Numeric Channel（conformance harness 的数值对比通道）先就绪——calc 的正确性只能靠数值级验证（百分比基、嵌套 calc、边界 clamp），Pixel Channel 无法区分 0.5px 级语义差。
- 替代现状：百分比基按 0 扁平化（FEATURES 布局映射注记）保留至直通落地。

## parley 分词数据调研（2026-09，complex-scripts 已启用）

背景：CJK 快照运行时警告 `ICU4X data error: No segmentation model for complex script: Chinese/Japanese`。本地取证（registry 源码：parley 0.11.1 / icu_segmenter 2.3.0 / icu_provider 2.3.1）结论：

- **已启用** `complex-scripts`（上游 [parley#621](https://github.com/linebender/parley/pull/621)，随 0.11.x 发布）：LineSegmenter/WordSegmenter 切换 `new_dictionary()`。cjdict 已随 icu_segmenter_data 2.3.0 baked（≈1.9MiB 数据段，无 feature 门控），警告消除、CJ 获词典级**词**分段、SEA（泰/老/柬/缅）获词典级行断。
- **警告来源是词分段非行分段**：行分段数据的 complex_property 为 SA，行规则将 CJ 映射为 ID，永不触达词典查询；词分段数据的 complex_property 含 CJ，遇 CJ run 即查询失败并告警（debug 构建下 icu4x 将 `log::warn!` 映射为 eprintln，release 无输出）。
- **CJK 行断行仍是 UAX #14**：icu_segmenter 2.x 行分段器有意不加载 CJ 词典（`line.rs` 注释：UAX 14 rules handle CJK characters）——这是上游设计取舍，非缺陷；我们的快照目验（表意文字边界折行）与之一致。
- **无 provider 注入口**：parley 硬编码 baked 数据（无 DataProvider 参数/re-export），`AnalysisDataSources` 为 pub(crate)。
- **禁则（kinsoku）上游受限**：parley 唯一断行钩子 `LineBreakOverrideFn` 对非 ASCII 直接返回 None（`break_overrides.rs:135-139`）——CJK 禁则无法经此实现。若将来需要「CJ 词典级行断行」或「禁则」，循 [parley#623](https://github.com/linebender/parley/issues/623)（已按 #621 关闭）的措辞向上游提 issue（"an API to choose the segmenter variant" 或 override 解除 ASCII 限制）。

## 字体资产与 LFS 评估（2026-09）

现状：仓库内字体 ≈19.2MB（DejaVu Regular/Bold ≈1.4MB + Noto Sans SC 可变字体 ≈17.8MB），随用例增长只会更多（conformance 需与浏览器强制同字体，ADR-0003）。

- **方案 A（推荐，建仓时执行）**：git-lfs 收 `*.ttf`/`*.otf`/`*.woff2`——仓库瘦身、CI 需 `git lfs install` + 缓存；LFS 配额是长期成本项，文本类 conformance 资产（golden PNG）同样适用此通道。
- **方案 B（备选）**：fonttools 对 demo 展示字体做子集化（可压至 ~1–2MB），完整字体置于 conformance 资产外置通道——缺点是子集与全量双制品会漂移，且子集化后的度量须与全量一致（shaping 依赖 cmap/gpos 完整性），不建议在 conformance 路径使用。
- **现阶段（建仓前）**：保持仓库内直存，不引入工具链成本；CI 化时再切换方案 A。
- 硬规则不变：字体从不 fork——一律上游原文件 + 许可证随目录归档（LICENSE-DejaVu.txt / LICENSE-NotoSansSC.txt）。
