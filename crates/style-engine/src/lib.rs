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
//! 设计文档见仓库根目录 `CONTEXT.md` 与 `docs/adr/`。

#![forbid(unsafe_code)]

pub mod error;

pub use error::{ContractError, ParseReport};
