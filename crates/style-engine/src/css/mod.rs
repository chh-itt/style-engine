//! CSS syntax layer: real CSS text → internal representation of rules,
//! declarations, and values (ADR-0004).
//!
//! Fault-tolerance semantics (ADR-0004/CONTEXT.md): any content-level error
//! (unknown declaration, invalid value, unknown at-rule) does not abort
//! parsing — it is recorded in `ParseReport` and skipped; contract errors
//! never surface at this layer.

pub mod decl;
pub(crate) mod fontprobe;
pub mod property;
pub mod property_rule;
pub mod stylesheet;
pub mod value;

/// The full set of CSS-wide keywords (css-cascade): a descriptor or registered
/// property value containing one is invalid. Single source shared by
/// counter_style.rs and property_rule.rs (eliminates the drift surface of
/// duplicate declarations).
pub(crate) const CSS_WIDE_KEYWORDS: [&str; 5] =
    ["initial", "inherit", "unset", "revert", "revert-layer"];

pub use decl::{DeclSource, Declaration, DeclarationBlock};
pub use property::{
    Align, BackgroundImage, BorderStyle, BoxShadow, BoxShadowList, ColorStop, DeclValue, Display,
    FamilyName, FlexDirection, FlexWrap, FontFamilyList, Gradient, GradientKind, GridAutoFlowKind,
    GridTemplate, LineHeight, Overflow, Position, PropertyId, TextAlign, TrackSize, WhiteSpace,
};
pub use stylesheet::{ColorScheme, MediaEnv, MediaFeature, MediaQuery, Rule, Stylesheet};
pub use value::{Angle, ColorValue, LengthPercentage, ResolveCtx};
