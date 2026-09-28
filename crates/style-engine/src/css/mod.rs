//! CSS 语法层：真实 CSS 文本 → 规则/声明/值的内部表示（ADR-0004）。
//!
//! 容错语义（ADR-0004/CONTEXT.md）：任何内容层错误（未知声明、无效值、
//! 未知 at-rule）不中断解析，记录进 `ParseReport` 后跳过；契约错误不在
//! 此层出现。

pub mod value;

pub use value::{Angle, ColorValue, LengthPercentage, ResolveCtx};
