//! `style-engine` — 框架无关的 CSS 语义样式层。
//!
//! 它解决一个问题：原生 GUI 想要 CSS 级别的样式表达力，却不想拖进一个
//! 完整浏览器引擎。本 crate 承包 CSS 的"样式代数"与"布局"语义，并把
//! "绘制"语义编译成中立的 `DisplayList`（绘制指令的有序序列）——渲染
//! 由宿主接入任意后端，参考实现在 `style-engine-vello`。
//!
//! # 分层（ADR-0001）
//!
//! - **L1 样式代数**：真实 CSS 文本解析（cssparser）、选择器匹配
//!   （selectors）、级联/继承/custom properties，产出 `ComputedStyle`。
//! - **L2 布局**（feature = "layout"）：ComputedStyle 映射到 taffy，
//!   文本叶子由内置文本栈（parley）测量，产出 `LayoutTree`。
//! - **L3 绘制**：布局结果编译为 `DisplayList`，按节点分段连续存放；
//!   wgpu/vello 后端只存在于独立的 sink crate。
//!
//! # 中立性契约（ADR-0001）
//!
//! crate 零副作用：时钟、窗口、输入、字体与图片字节全部由宿主推入；
//! 帧计算是纯的（同输入同输出、幂等）。错误双轨：CSS 内容错误按规范
//! 容错并记录在 [`ParseReport`]；宿主违约以 [`ContractError`] 上浮。
//!
//! # 快速上手
//!
//! ```rust
//! use style_engine::{StyleEngine, StyleNode};
//!
//! # fn main() -> Result<(), style_engine::ContractError> {
//! let mut engine = StyleEngine::new();
//! assert!(engine.set_stylesheet(".btn { background-color: #3366cc; }").is_clean());
//!
//! // 宿主树镜像：根 1 → 按钮 2（K 取宿主自己的 Copy + Eq + Hash 键）。
//! engine.insert(None, 1, StyleNode::default())?;
//! let mut btn = StyleNode::default();
//! btn.name = Some("button".into());
//! btn.classes = ["btn"].iter().map(|s| s.to_string()).collect();
//! engine.insert(Some(1), 2, btn)?;
//!
//! let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
//! let hit = frame.find(2).expect("node laid out");
//! assert!(hit.width > 0.0);
//! # Ok(())
//! # }
//! ```
//!
//! # API 冻结策略（C3）
//!
//! - 公共枚举全量 `#[non_exhaustive]`（变体集=演进面，宿主 `match` 必带
//!   通配臂）；结构体按宿主构造面决策——[`StyleNode`](crate::tree::StyleNode)
//!   等宿主可构造类型保持穷举。
//! - `#![deny(missing_docs)]`：公共项无文档即编译失败。
//! - 线程承诺：`StyleEngine`/`Frame`/`ComputedStyle`/`DisplayList` 均
//!   `Send + Sync`（静态断言锁定）。
//! - `DisplayList` 序列化：v1 **不提供** serde 实现（不引入 serde 依赖；
//!   `PaintOp` 为中立公共枚举，宿主可自行编写转换）。重估条件=出现跨进程
//!   合成或录制回放需求。
//!
//! 设计文档见仓库根目录 `CONTEXT.md` 与 `docs/adr/`。
//!
//! # 依赖策略（C4）
//!
//! - **词汇表公有**：公有面唯一的第三方类型是 peniko 的色彩类型（下方
//!   re-export）。宿主消费 [`DisplayList`]/[`ComputedStyle`] 色值**无需**
//!   自行依赖 peniko——版本由本 crate 锚定，杜绝双份 peniko。DisplayList
//!   几何全部是 `f32` 字段，kurbo 类型不出现在公有面（仅 peniko 传递）。
//! - **重依赖隔离**：GPU/wgpu/winit 只存在于 sink crate（`style-engine-vello`）
//!   与 demo；核心 crate 的 taffy/parley 经 `layout`/`text` feature 可选，
//!   `--no-default-features` 下核心仅剩 CSS 解析/级联/绘制编译。
//! - **基础设施不进核心**：serde 等宿主侧设施不引入（见 API 冻结策略的
//!   serde 决策）；诊断统一走 `tracing`（唯一观测依赖）。

// unsafe 策略：全 crate 禁止（deny），唯一豁免点 = engine.rs 的
// SendSyncTaffy（taffy 0.14 CompactLength nan-boxing 非 Send/Sync 的
// 包装，见该类型 SAFETY 注释）——其余任何 unsafe 直接编译失败。
#![deny(unsafe_code)]
// 阶段3 API 冻结：公共项文档强制（C3 契约——新增 pub 项必须带文档）。
#![deny(missing_docs)]
// 阶段3 API 冻结：公共枚举全量 #[non_exhaustive]（变体集=演进面，宿主
// match 必须带通配臂；结构体按宿主构造面决策——StyleNode 等宿主可构造
// 类型保持穷举）。

#[cfg(feature = "layout")]
pub mod engine;
#[cfg(feature = "layout")]
pub mod layout;

pub mod cascade;
pub mod computed;
pub mod css;
pub mod error;
pub mod paint;
pub mod selector;
pub mod tree;

#[cfg(feature = "text")]
pub mod text;

pub use cascade::{Candidate, CascadeOutput, CustomCandidate, MatchedRule, Origin};
pub use computed::{ComputedStyle, compute_node};
#[cfg(feature = "layout")]
pub use engine::{Frame, LayoutEntry, StyleEngine};

pub use error::{ContractError, ParseReport};
pub use paint::{DisplayList, PaintOp};
#[cfg(feature = "text")]
pub use text::TextSystem;
pub use tree::StyleNode;

// 公共再导出：下游（如 style-engine-soft 零依赖测试）构造 FontFamilyList
// 等属性值需要 SmallVec 容器——复用本 crate 锁定版本，避免版本漂移。
pub use smallvec;

// 公共词汇表（C4 依赖策略）：公有面唯一的第三方类型——peniko 色彩。
// PaintOp/ComputedStyle 的色值签名即 `AlphaColor<Srgb>`；re-export 使宿主
// 零直依 peniko，版本由本 crate 锚定（见 crate 文档「依赖策略（C4）」）。
pub use peniko::color::{AlphaColor, Srgb};
