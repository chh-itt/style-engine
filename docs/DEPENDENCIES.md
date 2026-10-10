# 依赖清单与版本策略

原则（2026-09 定）：选兼容的最新版本；一个 workspace 内每个 crate 只出现一个版本（One-Version Rule）；升级节奏由 vello 的 wgpu 锁定充当节拍器。版本核实日期：2026-09-11（数据源 crates.io API）。

## 决定性依赖

| Crate | 版本 | 层 | 角色 | 备注 |
|---|---|---|---|---|
| cssparser | 0.38.0 | L1 | CSS 词法/语法解析，错误恢复 | 2026-09-07 发布，MPL-2.0 |
| cssparser-sel（package=cssparser） | 0.37.0 | L1 选择器 | selectors 0.40 的配对解析器；0.38 起解析器改单生命周期，二者类型不兼容，重命名依赖隔离于 src/selector.rs 内部（选择器预lude 以源文本形式跨界）。**与 Cargo.toml/Cargo.lock 对齐核实（第五批①）：lock 中 cssparser 0.37.0 与 0.38.0 双版本共存为重命名依赖的预期形态，其余决定性依赖版本均与上表一致** | MPL-2.0 |
| precomputed-hash | 0.1.1 | L1 选择器 | selectors 0.40 要求的选择器词哈希；SelString 包装实现（FNV-1a） | Apache-2.0/MIT |
| selectors | 0.40.0 | L1 | 选择器解析/匹配/specificity | 需实现 SelectorImpl，MPL-2.0 |
| color | 0.3.3 | L1 | CSS Color 4 颜色模型（oklch/color-mix）| linebender 出品 |
| taffy | 0.14.0 | L2 | 布局引擎 | 2026-08-24 发布；calc 集成面 = 类型擦除指针 + 宿主回调（`CompactLength::calc(*const ())` / `traits.rs calc(val, basis)`，block.rs 处以 parent_size 为基调用）——接入路径存在（`resolve_calc_value` 公开 trait 方法，见下节复评修正），维持引擎侧结算式直通为工程性选择，记 B 级重估 |
| slotmap | 1.1.1 | 核心 | StyleTree 与 secondary map 存储 | |
| bitflags | 2.13.2 | 核心 | StateFlags | |
| smallvec | 1.x | L1 | 声明/子选择器内联存储 | 由 cargo update 定 patch |
| unicode-segmentation | 1.x | 核心 | text-transform capitalize 的 UAX#29 词界分词 | css-text-4 capitalize 要求 UAX#29 词界；纯 Rust、零传递依赖，MIT/Apache-2.0 |
| peniko | 0.6.1 | 核心(词汇表) | 画笔/颜色/图片类型 | 无 GPU 依赖，vello 同款，见 ADR-0001 |
| kurbo | (peniko 传递) | 核心(词汇表) | 几何词汇表（仅经 peniko 传递） | 阶段4 审计：本方 DisplayList 几何全部为 `f32` 字段，kurbo 类型不出现在公有面 |
| parley | 0.11.1 | L2 核心 | 文本 shaping/测量/行布局（测量内置，见 ADR-0006） | 传递引入 fontique |
| vello | 0.11.0 | sink | GPU 绘制执行器 | 2026-10-02 发布锁 wgpu 30；**P9-8（2026-10-08）已从 0.10 升级**，见"版本配对"节 |
| wgpu | **30.x** | sink | GPU 底座 | 30.0.1；⚠️ 见下"版本配对" |
| tiny-skia | 0.12.0 | tiny sink | CPU 2D 光栅（路径/渐变/Pattern/混合） | P10 引入；BSD-3-Clause（deny.toml 许可白名单已覆盖）；lowp 管线不支持 Pattern（自动走 HQ） |
| skrifa | 0.44.0 | tiny sink | 字体轮廓读取（Text 逐字形填充） | parley 0.11 传递同版本（Cargo.lock 单版本无分叉，read-fonts 0.41.0）；Apache-2.0/MIT |
| winit | =0.31.0-beta.3 | demo/harness | 窗口（仅示例与冒烟测试） | beta，精确锁版本 |
| image | 0.25.10 | 资源 | 图片解码（宿主喂字节，核心不做 IO） | |
| tracing | 0.1.x | 全部 | 警告/诊断通道 | ParseReport 同时进 tracing |
| bytemuck | 1.x | sink | vello 传递依赖（^1.25） | |

## 版本配对：wgpu 必须跟 vello 走（当前 30）

vello 0.11.0 的 wgpu 依赖为 `^30`（optional feature `wgpu`）。wgpu 大版本间 semver 不兼容：若 sink/宿主声明与 vello 不同的 wgpu 大版本，应用会同时编译两份 wgpu（或无法统一）。因此：

- sink 与宿主统一使用 vello 传递的 wgpu 30.x，sink crate 显式声明同版本（其测试代码直接使用 wgpu API；**公有 API 不暴露任何 wgpu 类型，无需 re-export**——阶段4 审计修正，此前的 `pub use wgpu` 计划未落地也不再需要）。
- **P9-8（2026-10-10）升级已执行**：vello 0.10→0.11 + wgpu 29→30（30.0.1）+ naga 29→30。实际 breaking 面与升级前核实一致（仅 wgpu/naga 升 30，vello Scene/Renderer/peniko/kurbo 使用面零变化）：我方代码共 7 处适配——vello sink 2 处（`RequestAdapterOptions` 新字段 `apply_limit_buckets: bool` 填 false=旧默认、`get_mapped_range` 改返回 `Result<BufferView, MapRangeError>` 补 expect/map_err）+ demo main 3 处（同上 adapter 字段、`SurfaceTexture::present()` → `Queue::present(tex)` 消费式、`SurfaceConfiguration` 新字段 `color_space: SurfaceColorSpace` 填 `Auto`=后端按格式选择复现历史行为）+ demo snapshot 示例 2 处（同款）。`SurfaceColorSpace::Auto` 为 #[default] 且文档明言复现历史行为——行为零漂移，conformance 像素腿（WARP）全绿佐证。
- 重估条件更新：追高 vello 0.12+/wgpu 31 时仍按"一次升级整条 linebender 链"规则执行。
- winit 0.31-beta 与 wgpu 的对接走 raw-window-handle，不受此影响。

## 版本策略

- 决定性依赖（上表全部直接依赖行；kurbo/bytemuck 仅传递）在 workspace 根 Cargo.toml 用 `[workspace.dependencies]` 统一声明，成员 crate 一律引用 workspace 版本。
- winit beta 是唯一精确锁（`=0.31.0-beta.3`）的依赖，且只出现在 demo/harness；0.31 stable 发布后立即替换。
- vello/parley/taffy 均 0.x：允许破坏性升级，但必须一次升级整条 linebender 链（vello+peniko+parley 同批），不允许混代。
- MSRV 以依赖最高者为准：workspace `rust-version = 1.90`（2026-10 CI msrv 实测上调——edition 2024 起点 1.85 被依赖链抬升：ordered-float 5.5.0 需 1.90、smol_str 0.3.6 需 1.89、vello 0.11 需 1.89/parley 0.11/fontique 0.11 链需 1.88、wgpu-types/naga 30.0.1 需 1.87、winit 0.31.0-beta.3 系需 1.86；msrv job 以 `dtolnay/rust-toolchain@1.90.0` 钉定验证。依赖再抬高时以实际失败为准上调）。
- 不承诺 no_std；L1（值与级联）保持 no_std+alloc 可达性，作为将来选项保留。

## taffy calc 直通（二期①已落地：引擎侧结算式直通；2026-09 复评修正上游结论）

调研结论（2026-09，**2026-09-11 复评修正**）：taffy 0.14 的 calc = 类型擦除指针 + 宿主回调（`CompactLength::calc(*const ())`，布局期以 parent_size 为基调用）。~~上游接入路径（自定义 LayoutPartialTree 包装覆盖 `resolve_calc_value`）经源码核查被阻断~~ **修正：该结论不成立**——taffy 0.14 `resolve_calc_value` 为公开 trait 方法（LayoutPartialTree 库侧缺省实现，可覆盖），官方示例 examples/custom_tree_owned_unsafe.rs 演示自定义树全流程；旧评估引用的「TaffyView」非 taffy 公共类型（评估引用有误）。接入路径实际存在，维持结算式直通的原因是工程性而非可行性：结算式已落地、perf 实测达标（conformance calc-width 转正 + 两级链收敛锁定），原生 calc 集成（指针所有权约定 + 自定义树搬运）收益不及重写风险，记 B 级重估清单。

**落地设计（二期①）**：引擎侧「结算式直通」，零 unsafe、零上游票——

- 映射期：含百分比 calc（`LengthPercentage::Calc` + `CalcNode::has_percent`）捕获为延迟条目（layout.rs `DeferredRaw`，thread_local 收集，`map_style` 每次调用即清空；px 部分照旧折叠供首遍布局）；v1 结算槽位 = width/height（flex-basis/min/max/margin/padding 维持 0 折算，记录偏差）。
- 布局期：每帧首遍布局后 `settle_calc`（engine.rs）以父节点内容盒（Layout.size − border − padding；taffy `content_size` 字段在 content_size 特性门下、workspace 未启用）为基准解析百分比，回写固定值并重算；循环至无变更，上限 3 遍——百分比基准恒为祖先派生 DAG，逐遍稳定一层，3 层内链路与浏览器单遍语义一致，更深链路记偏差待重估。
- 语义验证：conformance calc-width xfail 转正（.b = 780×50%+10 = 400px 与 Chromium 153 一致，0.5px 容差）；引擎锁定测试两级链收敛（110 → 65）。

## parley 分词数据调研（2026-09，complex-scripts 已启用）

背景：CJK 快照运行时警告 `ICU4X data error: No segmentation model for complex script: Chinese/Japanese`。本地取证（registry 源码：parley 0.11.1 / icu_segmenter 2.3.0 / icu_provider 2.3.1）结论：

- **已启用** `complex-scripts`（上游 [parley#621](https://github.com/linebender/parley/pull/621)，随 0.11.x 发布）：LineSegmenter/WordSegmenter 切换 `new_dictionary()`。cjdict 已随 icu_segmenter_data 2.3.0 baked（≈1.9MiB 数据段，无 feature 门控），警告消除、CJ 获词典级**词**分段、SEA（泰/老/柬/缅）获词典级行断。
- **警告来源是词分段非行分段**：行分段数据的 complex_property 为 SA，行规则将 CJ 映射为 ID，永不触达词典查询；词分段数据的 complex_property 含 CJ，遇 CJ run 即查询失败并告警（debug 构建下 icu4x 将 `log::warn!` 映射为 eprintln，release 无输出）。
- **CJK 行断行仍是 UAX #14**：icu_segmenter 2.x 行分段器有意不加载 CJ 词典（`line.rs` 注释：UAX 14 rules handle CJK characters）——这是上游设计取舍，非缺陷；我们的快照目验（表意文字边界折行）与之一致。
- **无 provider 注入口**：parley 硬编码 baked 数据（无 DataProvider 参数/re-export），`AnalysisDataSources` 为 pub(crate)。
- **禁则（kinsoku）上游受限**：parley 唯一断行钩子 `LineBreakOverrideFn` 对非 ASCII 直接返回 None（`break_overrides.rs:135-139`）——CJK 禁则无法经此实现。若将来需要「CJ 词典级行断行」或「禁则」，循 [parley#623](https://github.com/linebender/parley/issues/623)（已按 #621 关闭）的措辞向上游提 issue（"an API to choose the segmenter variant" 或 override 解除 ASCII 限制）。

## parley/fontique 度量查询缺口（第五批㉚）

- fontique 0.11 公共 API 不暴露字体度量（`FontInfo` 无 metrics 字段——仅 source/width/style/weight/axes/charmap_index；`Collection::family_id/family/query` 均不可达度量）。normal 行高的 Chromium 对齐值（第五批㉔）只能经布局探针（build+break 读首 run `RunMetrics`）取得。
- 缓解：TextSystem 探针缓存（主族+字号+字重+italic → normal 值），同键组合免探针遍（两遍法退单遍，`add_font` 时清空）；稳态摊销实测见 FEATURES ㉚（50 文本叶帧 0.757ms vs 首帧 3.905ms，单叶摊销 14.5µs）。
- 升级路径：引入 skrifa（parley 传递依赖，已在树内）直读 OS/2/hhea 表自算 typo 度量，可彻底免探针——但需与 parley RunMetrics 口径逐位对齐（fontique 内部取表路径），列为特性票不排期。

## vello 混合模式枚举缺口（P1-2，2026-10）

- css-compositing-1 定义 16 种标准 mix-blend-mode（peniko `Mix` 一一映射，vello sink 已原生接通）；css-compositing-2 的 plus-lighter/plus-darker **不在 `Mix` 枚举内**——vello sink 退 `Mix::Normal`（B 级偏差在案）。
- 缓解：soft sink 全 18 种模式原生像素合成（含 plus 族按预乘加法惯例），像素级验收路径不受影响；plus 族在 GPU 路径的缺口升级时自然闭合（vello/peniko 增补枚举或 sink 手工双 pass）。
- 上游观察点：peniko issue tracker 的 `Mix` 扩展讨论；升级 vello 0.11 代时复查。

## 字体资产与 LFS 评估（2026-09）

现状：仓库内字体 ≈19.2MB（DejaVu Regular/Bold ≈1.4MB + Noto Sans SC 可变字体 ≈17.8MB），随用例增长只会更多（conformance 需与浏览器强制同字体，ADR-0003）。

- **方案 A（推荐，建仓时执行）**：git-lfs 收 `*.ttf`/`*.otf`/`*.woff2`——仓库瘦身、CI 需 `git lfs install` + 缓存；LFS 配额是长期成本项，文本类 conformance 资产（golden PNG）同样适用此通道。
- **方案 B（备选）**：fonttools 对 demo 展示字体做子集化（可压至 ~1–2MB），完整字体置于 conformance 资产外置通道——缺点是子集与全量双制品会漂移，且子集化后的度量须与全量一致（shaping 依赖 cmap/gpos 完整性），不建议在 conformance 路径使用。
- **现阶段（建仓前）**：保持仓库内直存，不引入工具链成本；CI 化时再切换方案 A。
- 硬规则不变：字体从不 fork——一律上游原文件 + 许可证随目录归档（LICENSE-DejaVu.txt / LICENSE-NotoSansSC.txt）。

## 阶段4 依赖治理审计（2026-10）

- **公有词汇表 re-export**：核心 crate 公有面唯一第三方类型 = peniko 色彩 `AlphaColor<Srgb>`（PaintOp 变体字段 / ColorValue）。lib.rs 根 `pub use peniko::color::{AlphaColor, Srgb};` 使宿主**零直依 peniko**（版本由本 crate 锚定，杜绝双份 peniko）；`pub use smallvec;` 此前已有（同语义）。几何不经 kurbo（见上表）。
- **feature 门禁审计**：`--no-default-features` / `+layout` / `+text` / `--all-features` 四组合编译全过且零警告；发现并修复 layout-only 死代码盲区（`engine.rs abs_avail_width` 仅 text 测量路径消费 → 补 `#[cfg(feature = "text")]`）。核心 crate 重依赖（taffy/parley）确认经 feature 可选。
- **重复版本审计**（Cargo.lock 318 包）：16 组重复。cssparser 0.37/0.38 = selectors 0.40 配对重命名依赖的预期形态（已文档化）；其余（syn 2/3、thiserror 1/2、winnow 0.7/1.0、phf 0.13/0.14、hashbrown ×3、toml_datetime、rustc-hash、foldhash、miniz_oxide、jni-sys、redox_syscall）均为传递链正常形态，升级收敛时机跟随决定性依赖。
- **供应链门禁**：deny.toml（cargo-deny v2 schema）+ CI `cargo deny check`（ubuntu）；本地 run.ps1 工具未装则跳过（语义同 cargo-hack）。licenses 硬门（MIT/Apache-2.0/BSD/ISC/Unicode-3.0/MPL-2.0/Zlib/CC0——CC0 为 hexf-parse v0.2.1，color 链 hex 解析，公有领域指定）；advisories：漏洞/unsound 硬失败，yanked=warn；`RUSTSEC-2026-0192`（ttf-parser unmaintained）ignore——传入路径仅 Linux demo 链（winit 0.31-beta → winit-wayland → sctk-adwaita → ab_glyph → owned_ttf_parser），核心不含此链，重估条件=winit 0.31 stable 或 sctk-adwaita 换字体栈。
- **yank 处置**：yoke-derive 0.8.3 被 yank → `cargo update -p yoke-derive` 升 0.8.4（zerofrom/ICU4X 链，semver 兼容）。
