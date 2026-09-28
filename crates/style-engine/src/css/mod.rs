//! CSS 语法层：真实 CSS 文本 → 规则/声明/值的内部表示（ADR-0004）。
//!
//! 容错语义（ADR-0004/CONTEXT.md）：任何内容层错误（未知声明、无效值、
//! 未知 at-rule）不中断解析，记录进 `ParseReport` 后跳过；契约错误不在
//! 此层出现。

pub mod decl;
pub mod property;
pub mod stylesheet;
pub mod value;

pub use decl::{DeclSource, Declaration, DeclarationBlock};
pub use property::{
    Align, BackgroundImage, BorderStyle, BoxShadow, BoxShadowList, ColorStop, DeclValue, Display,
    FamilyName, FlexDirection, FlexWrap, FontFamilyList, Gradient, GradientKind, GridAutoFlowKind,
    GridTemplate, LineHeight, Overflow, Position, PropertyId, TextAlign, TrackSize, WhiteSpace,
};
pub use stylesheet::{ColorScheme, MediaEnv, MediaFeature, MediaQuery, Rule, Stylesheet};
pub use value::{Angle, ColorValue, LengthPercentage, ResolveCtx};
