//! `style-engine` — a framework-agnostic CSS semantic styling layer.
//!
//! It solves one problem: native GUIs want CSS-level styling power without
//! dragging in a full browser engine. This crate owns the CSS "style algebra"
//! and "layout" semantics, and compiles the "paint" semantics into a neutral
//! `DisplayList` (an ordered sequence of paint commands) — rendering is up to
//! the host, which can plug in any backend; the reference implementation
//! lives in `style-engine-vello`.
//!
//! # Layering (ADR-0001)
//!
//! - **L1 style algebra**: real CSS text parsing (cssparser), selector
//!   matching (selectors), cascade/inheritance/custom properties, producing
//!   `ComputedStyle`.
//! - **L2 layout** (feature = "layout"): ComputedStyle is mapped onto taffy,
//!   text leaves are measured by the built-in text stack (parley),
//!   producing `LayoutTree`.
//! - **L3 paint**: layout results are compiled into a `DisplayList`, stored
//!   contiguously in per-node segments; wgpu/vello backends live only in the
//!   separate sink crate.
//!
//! # Neutrality contract (ADR-0001)
//!
//! The crate has zero side effects: clocks, windows, input, fonts, and image
//! bytes are all pushed in by the host; frame computation is pure (same
//! input, same output, idempotent). Errors are dual-tracked: CSS content
//! errors are handled with the spec's fault-tolerant semantics and recorded
//! in [`ParseReport`]; host contract violations surface as
//! [`ContractError`].
//!
//! # Quick start
//!
//! ```rust
//! use style_engine::{StyleEngine, StyleNode};
//!
//! # fn main() -> Result<(), style_engine::ContractError> {
//! let mut engine = StyleEngine::new();
//! assert!(engine.set_stylesheet(".btn { background-color: #3366cc; }").is_clean());
//!
//! // Host tree mirror: root 1 → button 2 (K is the host's own Copy + Eq + Hash key).
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
//! # API freeze policy (C3)
//!
//! - All public enums are `#[non_exhaustive]` (the variant set is the
//!   evolution surface; host `match`es must carry a wildcard arm); structs
//!   are decided by the host-construction surface — host-constructible types
//!   such as [`StyleNode`] remain exhaustive.
//! - `#![deny(missing_docs)]`: a public item without docs fails to compile.
//! - Threading promise: `StyleEngine`/`Frame`/`ComputedStyle`/`DisplayList`
//!   are all `Send + Sync` (locked in by static assertions).
//! - `DisplayList` serialization: behind the `serde` feature (off by
//!   default) the `to_dump()`/`to_display_list()` typed projections are
//!   provided (`paint_dump` module); serde is an optional dependency only —
//!   the default build has a zero-dependency surface.
//!
//! Design docs live in the repository root: `CONTEXT.md` and `docs/adr/`.
//!
//! # Dependency policy (C4)
//!
//! - **Public vocabulary**: the only third-party types on the public surface
//!   are peniko's color types (re-exported below). Hosts consuming
//!   [`DisplayList`]/[`ComputedStyle`] color values do **not** need to depend
//!   on peniko themselves — the version is pinned by this crate, ruling out
//!   duplicate peniko versions. DisplayList geometry is entirely plain `f32`
//!   fields; kurbo types never appear on the public surface (only peniko,
//!   transitively).
//! - **Heavy dependency isolation**: GPU/wgpu/winit exist only in the sink
//!   crate (`style-engine-vello`) and the demo; the core crate's
//!   taffy/parley are optional behind the `layout`/`text` features, and with
//!   `--no-default-features` the core compiles only CSS parsing, cascade,
//!   and paint.
//! - **No infrastructure in the core**: serde exists only as an optional
//!   feature (see the serde decision under API freeze policy); diagnostics
//!   go uniformly through `tracing` (the only observability dependency).

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

/// Built-in UA-origin stylesheet constants (P5, ADR-0033 D3): not loaded by
/// default; hosts opt in explicitly.
pub mod builtins;
pub mod cascade;
pub mod computed;
pub mod css;
/// Debug tooling (F3e, ADR-0027): host-side human-readable views (DisplayList
/// tree / layout tree / geometry boxes; the ComputedStyle view lives in
/// `ComputedStyle::debug_dump`). Pure projection with zero new state; it never
/// participates in cascade, settlement, or paint paths.
pub mod debug;
pub mod error;
pub mod paint;
#[cfg(feature = "serde")]
pub mod paint_dump;
pub mod selector;
pub mod tree;

#[cfg(feature = "text")]
pub mod text;
/// text-transform mapping (C2; not cfg-gated — shared by the measurement and
/// paint paths).
pub mod text_transform;

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
