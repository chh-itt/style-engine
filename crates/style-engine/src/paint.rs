//! Paint/display list (L3): layout + computed style → a sequence of paint primitives.
//!
//! Neutral interchange format (ADR-0001/0003): colors are resolved absolute sRGBA
//! (currentColor / light-dark terminate at this layer; linearization and blending are
//! handled by the vello sink, ADR-0002). Paint order = tree order (parent before
//! child; z-index ordering comes later). Within a node: shadows → background →
//! border → text.

use crate::computed::ComputedStyle;
use crate::css::property::{
    Attachment, BackgroundBox, BackgroundClip, BackgroundImage, BgSize, BorderImageOutsetComp,
    BorderImageRepeatKind, BorderImageSliceComp, BorderImageWidthComp, BorderStyle, ClipRadius,
    ClipShape, DeclValue, FontStyle, Gradient, LPorAuto, LineHeight, OutlineStyle, Overflow,
    Position2D, PositionComp, PropertyId, RepeatAxis, RepeatXY, TransformFn,
};
use crate::css::property::{FontFamilyList, TextAlign};
use crate::css::stylesheet::MediaEnv;
use crate::css::value::{ColorValue, LengthPercentage, ResolveCtx};
use crate::tree::{NodeId, StyleTree};
use peniko::color::{AlphaColor, Srgb};
use std::collections::HashMap;

/// F2 (ADR-0022 D4): text decoration paint unit (leaf-level; resolved at paint time).
#[derive(Debug, Clone, PartialEq)]
pub struct TextDecorationPaint {
    /// Line bit set (1=underline 2=overline 4=line-through).
    pub line: u8,
    /// Line style.
    pub style: crate::css::property::TextDecoStyleKind,
    /// Decoration color (absolute sRGBA).
    pub color: AlphaColor<Srgb>,
    /// Thickness in px (resolved from the declared LP; auto/from-font degrade to
    /// font_size/12).
    pub thickness_px: f32,
}

/// F2 (ADR-0022 D5): text shadow paint unit (list order = paint order, shadows
/// before glyphs).
#[derive(Debug, Clone, PartialEq)]
pub struct TextShadowPaint {
    /// Horizontal offset in px (positive = right).
    pub dx: f32,
    /// Vertical offset in px (positive = down).
    pub dy: f32,
    /// Blur radius in px (0 = sharp; the sink approximates with multiple
    /// concentric offset rings — same origin as the box-shadow precedent).
    pub blur: f32,
    /// Shadow color (absolute sRGBA).
    pub color: AlphaColor<Srgb>,
}

/// Hit-testing clip unit (P4 D4, ADR-0037): a rectangle (with per-corner radii) or a
/// polyline polygon; `inv` = inverse of the active affine at registration time
/// (same semantics as the sink-side Clip.inv) — a point is mapped back through
/// `inv` before testing the local shape, so clipping of transformed nodes is exact
/// in transformed geometry.
#[derive(Debug, Clone, PartialEq)]
pub enum HitClip {
    /// Rectangular clip (overflow PushClip / clip-path inset): [x, y, w, h] plus
    /// per-corner (horizontal, vertical) radii in px (order as in FillRect.radius;
    /// all zeros = square corners).
    Rect {
        /// Clip box [x, y, w, h] (viewport coordinates, px).
        rect: [f32; 4],
        /// Per-corner (horizontal, vertical) radii in px (order tl_h, tl_v, tr_h,
        /// tr_v, br_h, br_v, bl_h, bl_v).
        radius: [f32; 8],
        /// Inverse of the active affine at registration time ([a, b, c, d, e, f]).
        inv: [f32; 6],
    },
    /// Polyline polygon clip (clip-path circle/ellipse/polygon).
    Path {
        /// Vertices in viewport coordinates (boundary order, implicitly closed).
        points: Vec<[f32; 2]>,
        /// Fill rule: true = nonzero, false = evenodd.
        nonzero: bool,
        /// Inverse of the active affine at registration time.
        inv: [f32; 6],
    },
}

/// Hit geometry unit (F3a, ADR-0023; refined by P4 D4): collected in paint order,
/// later = closer to the top; border-box viewport coordinates plus a snapshot of
/// the active clip chain at collection time (exact rectangle/polyline shapes).
/// `mat` = active affine at collection time (transform nodes = composite matrix;
/// the hit point is mapped back into node-local space before testing the box —
/// deviation for unrotated boxes converges, ADR-0037 D4).
#[derive(Debug, Clone, PartialEq)]
pub struct HitRect {
    /// Owning node.
    pub node_id: NodeId,
    /// border-box x (viewport).
    pub x: f32,
    /// border-box y (viewport).
    pub y: f32,
    /// border-box width.
    pub w: f32,
    /// border-box height.
    pub h: f32,
    /// Ancestor clip chain (outer→inner; exact rectangle/polyline shapes plus
    /// their inverse matrices).
    pub clips: Vec<HitClip>,
    /// Active affine at collection time (composite of the node's and ancestors'
    /// transforms; identity = no transform).
    pub mat: [f32; 6],
}

/// Hit collector (F3a, ADR-0023): fed through `PaintCtx.hit` during painting.
/// `mat` = top of the active affine stack while walking (updated/restored by the
/// paint_node transform section).
pub struct HitCollector {
    /// Collection order = paint order (later = on top).
    pub rects: Vec<HitRect>,
    /// Active clip chain (subtree PushClip/PushClipPath pushes / PopClip pops).
    pub clips: Vec<HitClip>,
    /// Active affine (paint_node composes on transform nodes, restored after the
    /// subtree walk).
    /// Starts as identity (derive Default on arrays is all zeros — unusable).
    pub mat: [f32; 6],
}

impl Default for HitCollector {
    fn default() -> Self {
        Self {
            rects: Vec::new(),
            clips: Vec::new(),
            mat: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        }
    }
}

/// Hit result (F3a, ADR-0023).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HitTestHit {
    /// Hit node.
    pub node_id: NodeId,
}

/// F2 (ADR-0022 D4): leaf-level decoration resolution — emitted only when the
/// `line` bit set is nonzero; thickness = declared LP resolved via px(),
/// auto/from-font degrade to font_size/12 (approximation documented in
/// FEATURES.md/SINK-MATRIX.md).
fn text_decorations(style: &ComputedStyle, env: &MediaEnv) -> Vec<TextDecorationPaint> {
    let line = style.text_decoration_line();
    if line == 0 {
        return Vec::new();
    }
    let thickness_px = match style.text_decoration_thickness() {
        crate::css::property::TextDecoThickness::Length(lp) => px(&lp, style, env),
        _ => style.font_size_px() / 12.0,
    };
    vec![TextDecorationPaint {
        line,
        style: style.text_decoration_style(),
        color: resolve_color(&style.text_decoration_color(), style, env),
        thickness_px,
    }]
}

/// F2 (ADR-0022 D5): leaf-level shadow resolution — declared list → paint units
/// (LP→px, color resolution, blur defaults to 0, color defaults to currentColor).
fn text_shadows(style: &ComputedStyle, env: &MediaEnv) -> Vec<TextShadowPaint> {
    style
        .text_shadows()
        .iter()
        .map(|s| TextShadowPaint {
            dx: px(&s.dx, style, env),
            dy: px(&s.dy, style, env),
            blur: s.blur.as_ref().map(|b| px(b, style, env)).unwrap_or(0.0),
            color: resolve_color(
                s.color
                    .as_ref()
                    .unwrap_or(&crate::css::value::ColorValue::CurrentColor),
                style,
                env,
            ),
        })
        .collect()
}

/// Paint-domain filter effect (P2, ADR-0031 D3): the L3-resolved form of `FilterFn` —
/// length components (blur/drop-shadow radius, offsets) are converted to px against
/// the node style, currentcolor is resolved to a concrete color (same convention as
/// Shadow/TextShadowPaint: the DisplayList is self-contained and holds no style
/// references). Numeric components match declaration-time clamping.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum FilterEffect {
    /// blur(`<length>`): blur radius in px (the sink converts σ = radius/2,
    /// css-filters-1 §3).
    Blur(f32),
    /// brightness(`<number-percentage>`): linear multiplier.
    Brightness(f32),
    /// contrast(`<number-percentage>`): affine contrast.
    Contrast(f32),
    /// grayscale(`<number-percentage>`).
    Grayscale(f32),
    /// sepia(`<number-percentage>`).
    Sepia(f32),
    /// saturate(`<number-percentage>`).
    Saturate(f32),
    /// invert(`<number-percentage>`).
    Invert(f32),
    /// opacity(`<number-percentage>`): alpha scaling.
    Opacity(f32),
    /// hue-rotate(`<angle>`): degrees.
    HueRotate(f32),
    /// drop-shadow: offset/blur radius in px + resolved color.
    DropShadow {
        /// x offset in px.
        dx: f32,
        /// y offset in px.
        dy: f32,
        /// Blur radius in px (0 = hard edge).
        blur: f32,
        /// Shadow color (currentcolor already resolved).
        color: AlphaColor<Srgb>,
    },
}

/// `FilterFn` declaration chain → paint-domain effect chain (px/color resolution;
/// unknown variants are skipped — non_exhaustive forward compatibility).
fn resolve_filter_effects(
    fns: &[crate::css::property::FilterFn],
    style: &ComputedStyle,
    env: &MediaEnv,
) -> Vec<FilterEffect> {
    fns.iter()
        .map(|f| match f {
            crate::css::property::FilterFn::Blur(len) => FilterEffect::Blur(px(len, style, env)),
            crate::css::property::FilterFn::Brightness(v) => FilterEffect::Brightness(*v),
            crate::css::property::FilterFn::Contrast(v) => FilterEffect::Contrast(*v),
            crate::css::property::FilterFn::Grayscale(v) => FilterEffect::Grayscale(*v),
            crate::css::property::FilterFn::Sepia(v) => FilterEffect::Sepia(*v),
            crate::css::property::FilterFn::Saturate(v) => FilterEffect::Saturate(*v),
            crate::css::property::FilterFn::Invert(v) => FilterEffect::Invert(*v),
            crate::css::property::FilterFn::Opacity(v) => FilterEffect::Opacity(*v),
            crate::css::property::FilterFn::HueRotate(v) => FilterEffect::HueRotate(*v),
            crate::css::property::FilterFn::DropShadow {
                dx,
                dy,
                blur,
                color,
            } => FilterEffect::DropShadow {
                dx: px(dx, style, env),
                dy: px(dy, style, env),
                blur: px(blur, style, env),
                color: resolve_color(color, style, env),
            },
            // 同 crate 匹配暂不可达；跨版本新增变体时兜底恒等（前向兼容）
            #[allow(unreachable_patterns)]
            _ => FilterEffect::Brightness(1.0),
        })
        .collect()
}

/// A single paint primitive. Coordinates are relative to the viewport (pre-scroll);
/// sizes are border-box.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum PaintOp {
    /// Solid-color rectangle (background).
    FillRect {
        /// Box left edge x (viewport coordinates, px, border-box).
        x: f32,
        /// Box top edge y (viewport coordinates, px, border-box).
        y: f32,
        /// Box width in px (border-box).
        width: f32,
        /// Box height in px (border-box).
        height: f32,
        /// Per-corner (horizontal, vertical) radii in px (batch 5 ⑪ elliptical
        /// corners; order tl.x tl.y tr.x tr.y br.x br.y bl.x bl.y).
        radius: [f32; 8],
        /// Fill color (absolute sRGBA).
        color: AlphaColor<Srgb>,
    },
    /// Gradient background (linear/radial; CSS semantics).
    Gradient {
        /// Box left edge x (viewport coordinates, px, border-box).
        x: f32,
        /// Box top edge y (viewport coordinates, px, border-box).
        y: f32,
        /// Box width in px (border-box).
        width: f32,
        /// Box height in px (border-box).
        height: f32,
        /// Per-corner (horizontal, vertical) radii in px (order as in
        /// FillRect.radius).
        radius: [f32; 8],
        /// Gradient parameters (CSS linear-gradient/radial-gradient; the repeating
        /// flag lives in Gradient.repeating, P1-3 tiling semantics resolved by the
        /// sink).
        gradient: Gradient,
        /// Radial geometry (T4c): center and radii resolved to absolute px against
        /// the box (None for linear gradients).
        radial: Option<RadialGeom>,
        /// Conic geometry (C3): center in absolute px + start angle in radians
        /// (None for non-conic).
        conic: Option<ConicGeom>,
        /// Linear geometry (F3d, ADR-0026): absolute gradient-line endpoints
        /// (None for non-linear).
        linear: Option<LinearGeom>,
    },
    /// Shadow (batch 5 ⑩: blur = sink multi-ring approximation; inset = inverted
    /// fill inside the box).
    Shadow {
        /// Box left edge x (viewport coordinates, px, border-box).
        x: f32,
        /// Box top edge y (viewport coordinates, px, border-box).
        y: f32,
        /// Box width in px (border-box).
        width: f32,
        /// Box height in px (border-box).
        height: f32,
        /// Per-corner (horizontal, vertical) radii in px (order as in
        /// FillRect.radius).
        radius: [f32; 8],
        /// Shadow color (box-shadow `<color>`, absolute sRGBA).
        color: AlphaColor<Srgb>,
        /// Horizontal offset in px (box-shadow `<offset-x>`, positive = right).
        offset_x: f32,
        /// Vertical offset in px (box-shadow `<offset-y>`, positive = down).
        offset_y: f32,
        /// Blur radius in px (box-shadow `<blur-radius>`).
        blur: f32,
        /// Spread in px (outward / inward).
        spread: f32,
        /// Inset shadow (inverted fill inside the box).
        inset: bool,
    },
    /// Background image (batch 5 ⑨): host pre-decoded RGBA (zero side effects —
    /// the engine never fetches URLs, references are registered via add_image);
    /// source size and pixels travel with the op (the DisplayList is
    /// self-contained).
    Image {
        /// Draw box left edge x (viewport coordinates, px, border-box).
        x: f32,
        /// Draw box top edge y (viewport coordinates, px, border-box).
        y: f32,
        /// Draw box width in px (border-box).
        width: f32,
        /// Draw box height in px (border-box).
        height: f32,
        /// Per-corner (horizontal, vertical) radii (batch 5 ⑪ order) — the sink
        /// uses this to decide whether to clip.
        radius: [f32; 8],
        /// Source image width in px (intrinsic sizing).
        source_w: u32,
        /// Source image height in px (intrinsic sizing).
        source_h: u32,
        /// Source sub-region left edge in px (F3d 9-slice; 0 = full image).
        src_x: f32,
        /// Source sub-region top edge in px (0 = full image).
        src_y: f32,
        /// Source sub-region width in px (full image = source_w).
        src_w: f32,
        /// Source sub-region height in px (full image = source_h).
        src_h: f32,
        /// Pre-decoded RGBA pixels (host-registered).
        pixels: ImageRes,
    },
    /// Border (four independent sides: top/right/bottom/left; sides with style
    /// none or width 0 are skipped by the sink).
    Border {
        /// Box left edge x (viewport coordinates, px, border-box).
        x: f32,
        /// Box top edge y (viewport coordinates, px, border-box).
        y: f32,
        /// Box width in px (border-box).
        width: f32,
        /// Box height in px (border-box).
        height: f32,
        /// Per-corner (horizontal, vertical) radii in px (order as in
        /// FillRect.radius).
        radius: [f32; 8],
        /// Four border sides in [top, right, bottom, left] order.
        sides: [BorderSide; 4],
    },
    /// Text (T5 converts to glyph runs; spans are T5c rich-text overrides, may be
    /// empty).
    Text {
        /// Text origin x (viewport coordinates, px, top-left of the content box).
        x: f32,
        /// Text origin y (viewport coordinates, px, top-left of the content box).
        y: f32,
        /// UTF-8 text to typeset and draw (full text of the leaf node).
        text: String,
        /// Base text color (color, absolute sRGBA; spans may override).
        color: AlphaColor<Srgb>,
        /// Span override styles (T5c): resolved at paint time; empty = no rich text.
        spans: Vec<TextSpanPaint>,
        /// Font size in px (font-size).
        font_size: f32,
        /// Font family list (font-family, in fallback order).
        font_family: FontFamilyList,
        /// Font weight (numeric font-weight, 400 = normal).
        font_weight: f32,
        /// Whether italic (font-style: italic).
        italic: bool,
        /// Line-wrapping constraint (T5c-2): measurement and painting share the
        /// same max_advance so wrapping is consistent; None = unbounded.
        max_advance: Option<f32>,
        /// Line height (inventory fix): Some = absolute px; None = normal
        /// (typesetter default font metrics).
        line_height: Option<f32>,
        /// Letter spacing in px (0 = default). Span-level line height/letter
        /// spacing are known approximations (only the base style applies).
        letter_spacing: f32,
        /// Inline alignment (batch 5 ⑳): does not change measurement (wrapping is
        /// independent of both box width and alignment); consumed by the sink as
        /// align after typesetting; Start = typesetter default.
        text_align: TextAlign,
        /// C2 (ADR-0016): intra-word line-breaking strength (the sink re-typesets
        /// with the same parameter — the measurement/painting line-breaking
        /// consistency contract, same channel as max_advance).
        word_break: crate::css::property::WordBreakKind,
        /// C2 (ADR-0016): breaking of overflowing long words (the sink re-typesets
        /// with the same parameter).
        overflow_wrap: crate::css::property::OverflowWrapKind,
        /// F2 (ADR-0022 D4): leaf-level text decorations (nonempty only when the
        /// line bit set is nonzero; color/thickness already resolved to px at
        /// paint time).
        decorations: Vec<TextDecorationPaint>,
        /// F2 (ADR-0022 D5): text shadows (comma-list order; the sink draws
        /// shadows before glyphs).
        shadows: Vec<TextShadowPaint>,
        /// F3d (ADR-0026 D5): font-stretch percentage (100 = normal; 100 is not
        /// pushed = sink default). Base-style level; span level is a known
        /// approximation (line height/letter spacing precedent).
        font_stretch: f32,
        /// F3d (ADR-0026 D5): word spacing in px (None = normal = sink default 0).
        word_spacing: Option<f32>,
        /// F3d (ADR-0026 D5): effective OpenType feature pairs
        /// (font-feature-settings ∪ font-variant-caps-derived titl/unic; empty =
        /// sink defaults).
        font_features: Vec<([u8; 4], u16)>,
        /// F3d (ADR-0026 D5): variation axis pairs (font-variation-settings;
        /// empty = sink defaults).
        font_variations: Vec<([u8; 4], f32)>,
    },
    /// Begin clip layer (overflow other than visible).
    PushClip {
        /// Clip box left edge x (viewport coordinates, px, border-box).
        x: f32,
        /// Clip box top edge y (viewport coordinates, px, border-box).
        y: f32,
        /// Clip box width in px (border-box).
        width: f32,
        /// Clip box height in px (border-box).
        height: f32,
        /// Per-corner (horizontal, vertical) radii in px (order as in
        /// FillRect.radius).
        radius: [f32; 8],
    },
    /// Begin polygon clip layer (F3c, ADR-0025: non-inset clip-path shapes).
    /// Vertex sequence in viewport coordinates (≥3; circles/ellipses are
    /// approximated at paint time by a 64-segment polyline, Tier B, documented in
    /// FEATURES.md/SINK-MATRIX.md).
    PushClipPath {
        /// Vertex [x, y] px sequence in viewport coordinates (order = polygon
        /// boundary order, implicitly closed).
        points: Vec<[f32; 2]>,
        /// Fill rule: true = nonzero (default for circle/ellipse/polygon),
        /// false = evenodd (polygon(evenodd, …)).
        nonzero: bool,
    },
    /// End clip layer (matches the nearest PushClip).
    PopClip,
    /// Begin opacity layer (opacity < 1, ADR-0008): the whole node subtree is
    /// composited with alpha.
    PushOpacity {
        /// Layer opacity (0.0–1.0, CSS opacity).
        alpha: f32,
        /// Affected node box left edge x (viewport coordinates, px, border-box).
        x: f32,
        /// Affected node box top edge y (viewport coordinates, px, border-box).
        y: f32,
        /// Affected node box width in px (border-box).
        width: f32,
        /// Affected node box height in px (border-box).
        height: f32,
    },
    /// End opacity layer (matches the nearest PushOpacity).
    PopOpacity,
    /// Begin blend layer (P1-2, css-compositing-1): wraps the whole node subtree
    /// when mix-blend-mode ≠ normal or isolation: isolate — layer content is
    /// composited with the backdrop using `mode` (Normal = a pure isolation-group
    /// boundary). The blend layer must be outermost (inside the opacity layer):
    /// compositing order = blend(backdrop, opacity(subtree)).
    PushBlend {
        /// Blend mode (16 standard modes + plus-lighter/darker).
        mode: crate::css::property::BlendMode,
        /// Affected node box left edge x (viewport coordinates, px, border-box).
        x: f32,
        /// Affected node box top edge y (viewport coordinates, px, border-box).
        y: f32,
        /// Affected node box width in px (border-box).
        width: f32,
        /// Affected node box height in px (border-box).
        height: f32,
    },
    /// End blend layer (matches the nearest PushBlend).
    PopBlend,
    /// Begin filter layer (P2, ADR-0031 D3, css-filters-1): wraps the whole node
    /// subtree when filter ≠ none — layer content is rendered offscreen, then
    /// processed in order through the `filters` function chain and composited
    /// (chain order = semantic order). Stacks inside opacity (compositing order =
    /// opacity(filter(subtree)), css-filters-1 §3) and outside blend/clip/
    /// transform (clip clips the filter output).
    PushFilter {
        /// Filter effect chain (paint-domain resolved values, declaration order;
        /// an empty chain is never emitted).
        filters: Vec<FilterEffect>,
        /// Affected node box left edge x (viewport coordinates, px, border-box).
        x: f32,
        /// Affected node box top edge y (viewport coordinates, px, border-box).
        y: f32,
        /// Affected node box width in px (border-box).
        width: f32,
        /// Affected node box height in px (border-box).
        height: f32,
    },
    /// End filter layer (matches the nearest PushFilter).
    PopFilter,
    /// Backdrop filter (P2, ADR-0031 D3, css-filters-2): an immediate op (no
    /// layer pair) — applies the `filters` function chain to canvas content
    /// already painted behind the node's border-box area (v1 approximation of the
    /// sampled surface = current canvas region; stacking-context refinement is
    /// Tier B, documented in FEATURES.md/SINK-MATRIX.md). Emitted before the
    /// node's own content and outside effect layer pairs (operates on the main
    /// canvas).
    BackdropFilter {
        /// Filter effect chain (paint-domain resolved values, declaration order;
        /// an empty chain is never emitted).
        filters: Vec<FilterEffect>,
        /// Affected node box left edge x (viewport coordinates, px, border-box).
        x: f32,
        /// Affected node box top edge y (viewport coordinates, px, border-box).
        y: f32,
        /// Affected node box width in px (border-box).
        width: f32,
        /// Affected node box height in px (border-box).
        height: f32,
    },
    /// Begin 2D affine transform layer (ADR-0009): all drawing of this node's
    /// subtree goes through the matrix; layout boxes keep untransformed
    /// coordinates (taffy cannot see transforms).
    PushTransform {
        /// [a, b, c, d, e, f]: x' = a·x + c·y + e, y' = b·x + d·y + f.
        affine: [f32; 6],
    },
    /// End transform layer (matches the nearest PushTransform).
    PopTransform,
    /// Begin scroll-offset layer.
    PushScroll {
        /// Horizontal scroll offset in px (equivalent to scrollLeft; subtree
        /// content translates with it).
        dx: f32,
        /// Vertical scroll offset in px (equivalent to scrollTop; subtree content
        /// translates with it).
        dy: f32,
    },
    /// End scroll-offset layer (matches the nearest PushScroll).
    PopScroll,
}

/// Single border side (T4b: width/style/color already resolved to absolute values
/// at the paint layer).
#[derive(Debug, Clone, PartialEq)]
pub struct BorderSide {
    /// Side width in px (border-width, already resolved to an absolute value).
    pub width: f32,
    /// Border style (border-style; none or width 0 is skipped by the sink).
    pub style: BorderStyle,
    /// Border color (border-color, absolute sRGBA).
    pub color: AlphaColor<Srgb>,
}

/// Resolved radial geometry (absolute px; ellipses get rx/ry separately, equal for
/// circles).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadialGeom {
    /// Center x (px, box coordinates).
    pub cx: f32,
    /// Center y (px, box coordinates).
    pub cy: f32,
    /// Horizontal radius in px.
    pub rx: f32,
    /// Vertical radius in px.
    pub ry: f32,
}

/// Resolved conic geometry (C3, css-images-3; ADR-0017). Center in absolute px;
/// start angle in radians, measured from the positive X axis, clockwise (direct
/// alignment with peniko SweepGradientPosition semantics — CSS 0deg = 12 o'clock
/// is shifted by (deg−90°)·π/180).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConicGeom {
    /// Center x (px, box coordinates).
    pub cx: f32,
    /// Center y (px, box coordinates).
    pub cy: f32,
    /// Start angle (radians, measured from the positive X axis, clockwise).
    pub start: f32,
}

/// Resolved linear gradient geometry (F3d, ADR-0026): both CSS gradient-line
/// endpoints resolved to absolute px against the paint box (css-images-3 §3.2;
/// completes the radial/conic absolute-geometry precedent — 9-slice regions must
/// share one absolute gradient). Resolved at the paint layer; the sink consumes
/// it preferentially, a default None = box-derived fallback (existing semantics
/// unchanged).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinearGeom {
    /// Gradient-line start (x, y) (px, box coordinates; the stop-0 end).
    pub start: [f32; 2],
    /// Gradient-line end (x, y) (px, box coordinates; the last-stop end).
    pub end: [f32; 2],
}

/// Rich-text span (T5c): override styles resolved at paint time; [start, end) are
/// text byte offsets.
#[derive(Debug, Clone, PartialEq)]
pub struct TextSpanPaint {
    /// Span start byte offset (inclusive).
    pub start: u32,
    /// Span end byte offset (exclusive).
    pub end: u32,
    /// Text color (color, absolute sRGBA).
    pub color: AlphaColor<Srgb>,
    /// Font size in px (font-size).
    pub font_size: f32,
    /// Font weight (numeric font-weight, 400 = normal).
    pub font_weight: f32,
    /// Whether italic (font-style: italic).
    pub italic: bool,
    /// Font family list (font-family, in fallback order).
    pub font_family: crate::css::property::FontFamilyList,
    /// Span letter spacing in px (computed letter-spacing value; 0 = none).
    pub letter_spacing: f32,
    /// Span line height in px (computed line-height value; None = normal, falls
    /// back to the op-level line height).
    pub line_height: Option<f32>,
}

/// The paint list for one frame.
#[derive(Debug, Clone, Default, PartialEq)]
#[must_use = "dropping the paint list leaves the frame unrendered"]
pub struct DisplayList {
    /// Primitive sequence in tree order.
    pub ops: Vec<PaintOp>,
    /// Generation number of the corresponding frame.
    pub generation: u64,
}

/// Host-registered background image (batch 5 ⑨): pre-decoded RGBA (the engine
/// never fetches URLs or decodes bitmap formats — zero side effects); the
/// DisplayList carries the pixels self-contained. `rgba` is held as
/// `Arc<dyn AsRef<[u8]>>` (same shape as a peniko Blob, avoiding an unsize
/// conversion).
pub struct ImageRes {
    /// Image width in px (intrinsic sizing).
    pub width: u32,
    /// Image height in px (intrinsic sizing).
    pub height: u32,
    /// Pre-decoded RGBA bytes (4 bytes per pixel, sRGB).
    pub rgba: std::sync::Arc<dyn std::convert::AsRef<[u8]> + Send + Sync>,
}

impl std::fmt::Debug for ImageRes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ImageRes({}x{}, bytes={})",
            self.width,
            self.height,
            self.rgba.as_ref().as_ref().len()
        )
    }
}

impl Clone for ImageRes {
    fn clone(&self) -> Self {
        Self {
            width: self.width,
            height: self.height,
            rgba: std::sync::Arc::clone(&self.rgba),
        }
    }
}

impl PartialEq for ImageRes {
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && std::sync::Arc::ptr_eq(&self.rgba, &other.rgba)
    }
}

/// Multicol column rule band (phase 3 ⑤c): geometry settled by settle_column_rules,
/// coordinates relative to the multicol container's border-box origin (the paint
/// layer adds the container origin); the engine rebuilds it fully each frame
/// (column balancing geometry drifts with content — no steady-state cache).
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnRuleSeg {
    /// Segment left edge x (px, multicol container border-box coordinates).
    pub x: f32,
    /// Segment top edge y (px, multicol container border-box coordinates).
    pub y: f32,
    /// Segment width in px.
    pub width: f32,
    /// Segment height in px.
    pub height: f32,
    /// Rule color (column-rule-color, absolute sRGBA).
    pub color: AlphaColor<Srgb>,
}

/// Paint input context (tree mirror + layout + scroll + environment).
pub struct PaintCtx<'a> {
    /// Style tree mirror (provides node structure and leaf text).
    pub tree: &'a StyleTree,
    /// Node → computed style.
    pub styles: &'a HashMap<NodeId, ComputedStyle>,
    /// Node → layout box (x, y, w, h) (viewport coordinates, px, border-box).
    pub layout: &'a HashMap<NodeId, (f32, f32, f32, f32)>,
    /// Node → scroll offset (dx, dy) (px, equivalent to scrollLeft/scrollTop).
    pub scroll: &'a HashMap<NodeId, (f32, f32)>,
    /// Media environment (input for media query evaluation).
    pub env: &'a MediaEnv,
    /// Span-level styles (T5c): already cascaded against the node's base style.
    pub spans: &'a HashMap<NodeId, Vec<(u32, u32, ComputedStyle)>>,
    /// Line-wrapping constraints used for text-leaf measurement (T5c-2): absent =
    /// unbounded / host-measured.
    pub wrap_widths: &'a HashMap<NodeId, Option<f32>>,
    /// F2 (ADR-0022 D2): text truncation override (ellipsis/line-clamp) —
    /// produced by apply_text_truncation; consumed as the Text op's text
    /// replacement.
    pub text_overrides: &'a HashMap<NodeId, String>,
    /// Hit collection channel (F3a, ADR-0023; None = no collection, zero cost).
    pub hit: Option<&'a std::cell::RefCell<HitCollector>>,
    /// Background image registry (batch 5 ⑨): url() reference → host pre-decoded
    /// RGBA.
    pub images: &'a HashMap<String, ImageRes>,
    /// Multicol column rule bands (phase 3 ⑤c): NodeId → segment list (rebuilt by
    /// the engine each frame).
    pub column_rules: &'a HashMap<NodeId, Vec<ColumnRuleSeg>>,
    /// P9-3 (css-lists-3 §3.1, ADR-0041): list-item host → text marker
    /// (marker node, advance width in px). The marker pseudo-node is hidden from
    /// taffy — the paint layer synthesizes the marker text op at the left edge of
    /// the host's first line of content, and the host's text op is shifted by the
    /// advance width (inside semantics; outside≈inside is a Tier B exemption).
    pub markers: &'a HashMap<NodeId, (NodeId, f32)>,
}

/// Builds the paint list (tree-order walk; layout supplies a border-box per node).
pub fn build_display_list(
    ctx: &PaintCtx<'_>,
    root: NodeId,
    generation: u64,
    out: &mut DisplayList,
) {
    out.ops.clear();
    out.generation = generation;
    paint_node(ctx, root, out);
}

/// ADR-0010: appends one subtree's paint primitives to an existing DisplayList
/// (does not clear or change generation) — the frame appends root by root along
/// root_order (the super-root has no style and cannot be a walk start; user roots
/// each have style/layout entries).
pub(crate) fn append_display_list(ctx: &PaintCtx<'_>, root: NodeId, out: &mut DisplayList) {
    paint_node(ctx, root, out);
}

/// Linear gradient geometry resolution (F3d, ADR-0026): CSS angle (0deg = 12
/// o'clock, clockwise) → gradient-line endpoints (css-images-3 §3.2: direction
/// d=(sin θ, −cos θ) with screen y pointing down, line length L=|w·sin θ|+|h·cos θ|,
/// endpoints = center±(L/2)·d). The to-corner form is gracefully rejected at parse
/// time (documented in FEATURES.md/SINK-MATRIX.md); only the angle form is needed
/// here.
fn resolve_linear(angle_deg: f32, x: f32, y: f32, w: f32, h: f32) -> LinearGeom {
    let th = angle_deg.to_radians();
    let (sn, cs) = th.sin_cos();
    let half = (w * sn.abs() + h * cs.abs()) * 0.5;
    let (cx, cy) = (x + w * 0.5, y + h * 0.5);
    LinearGeom {
        start: [cx - sn * half, cy + cs * half],
        end: [cx + sn * half, cy - cs * half],
    }
}

/// border-image nine-slice emission (F3d, ADR-0026; css-backgrounds-3 §6.3-6.6):
/// when source ≠ none, resolves slice/band widths/outset and emits nine-region
/// primitives (4 stretched corners + 4 stretch/repeat/round/space edges + the
/// fill center), one op per region (ADR-0001 neutrality, zero logic duplication
/// in the sink). Gradient sources share one full-box absolute geometry across the
/// nine regions (linear/radial/conic) = exact slicing; bitmap sources are cropped
/// via the Image source sub-region fields (vello = affine sub-region + clip
/// layer, soft = sampling offset). outset only shifts the paint domain (ink
/// overflow, not part of scrollable/hit). Returns true = the Border op has been
/// replaced; false = source missing or URL unregistered (the caller falls back
/// to the border — the zero-side-effect contract).
///
/// Tier B, documented in FEATURES.md/SINK-MATRIX.md: gradient sources have no
/// intrinsic slice size → every tiling mode degrades to stretch; border radii do
/// not clip the 9-slice (browsers clip the border image along the radii; regions
/// here are square-cornered).
#[allow(clippy::too_many_arguments)] // 九宫格几何直传（结构体化=调用噪声）
fn paint_border_image(
    ctx: &PaintCtx<'_>,
    style: &ComputedStyle,
    env: &MediaEnv,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    sides: &[BorderSide; 4],
    out: &mut DisplayList,
) -> bool {
    // —— 源终结（一次；enum 局部项，区域发射函数引用）——
    enum BiPaint {
        /// Bitmap source (host-registered RGBA).
        Image(ImageRes),
        /// Gradient source: stops resolved to absolute colors; geometry resolved
        /// against the full box (shared by the nine regions).
        Gradient {
            gradient: Gradient,
            linear: Option<LinearGeom>,
            radial: Option<RadialGeom>,
            conic: Option<ConicGeom>,
        },
    }
    let paint = match style.border_image_source() {
        BackgroundImage::None => return false,
        BackgroundImage::Url(reference) => match ctx.images.get(&reference) {
            Some(img) => BiPaint::Image(img.clone()),
            None => {
                tracing::warn!(
                    target: "style_engine::css",
                    reference = reference.as_str(),
                    "border-image url not registered; fallback to border"
                );
                return false;
            }
        },
        BackgroundImage::Gradient(g) => {
            let gradient = Gradient {
                kind: g.kind.clone(),
                repeating: g.repeating,
                stops: g
                    .stops
                    .iter()
                    .map(|s| crate::css::property::ColorStop {
                        color: ColorValue::Absolute(resolve_color(&s.color, style, env)),
                        position: s.position.clone(),
                    })
                    .collect(),
                hints: g.hints.clone(),
            };
            let linear = match &g.kind {
                crate::css::property::GradientKind::Linear(angle) => {
                    Some(resolve_linear(angle.0, x, y, w, h))
                }
                _ => None,
            };
            let radial = match &g.kind {
                crate::css::property::GradientKind::Radial(spec) => {
                    Some(resolve_radial(spec, x, y, w, h, style, env))
                }
                _ => None,
            };
            let conic = match &g.kind {
                crate::css::property::GradientKind::Conic(spec) => {
                    Some(resolve_conic(spec, x, y, w, h, style, env))
                }
                _ => None,
            };
            BiPaint::Gradient {
                gradient,
                linear,
                radial,
                conic,
            }
        }
    };
    let (sw, sh) = match &paint {
        BiPaint::Image(img) => (img.width as f32, img.height as f32),
        BiPaint::Gradient { .. } => (w, h), // 渐变源 area = 边框盒（ADR-0026 number 语义）
    };
    if sw <= 0.0 || sh <= 0.0 || w <= 0.0 || h <= 0.0 {
        return true; // 源已取代边框；退化盒无可绘区域
    }

    // —— 切片（源空间 px；TRBL 序）——
    let slice = style.border_image_slice();
    let fill = slice.fill;
    let slice_px = |c: BorderImageSliceComp, dim: f32| -> f32 {
        match c {
            BorderImageSliceComp::Number(v) => v, // 光栅=源像素；渐变=area px
            BorderImageSliceComp::Percentage(p) => p * dim / 100.0,
        }
    };
    let (mut st, mut sr, mut sb, mut sl) = (
        slice_px(slice.slices[0], sh),
        slice_px(slice.slices[1], sw),
        slice_px(slice.slices[2], sh),
        slice_px(slice.slices[3], sw),
    );
    // 对边切片和溢出源 → 等比缩小（css-backgrounds-3 §6.3 f 规则）。
    let f = (sw / (sl + sr)).min(sh / (st + sb));
    if f < 1.0 {
        st *= f;
        sr *= f;
        sb *= f;
        sl *= f;
    }

    // —— 带宽（绘制域 px；TRBL 序）——
    let width = style.border_image_width();
    let bw = [
        sides[0].width,
        sides[1].width,
        sides[2].width,
        sides[3].width,
    ];
    let width_px = |c: &BorderImageWidthComp, i: usize| -> f32 {
        match c {
            BorderImageWidthComp::Length(lp) => px(lp, style, env).max(0.0),
            BorderImageWidthComp::Number(n) => (n * bw[i]).max(0.0),
            // auto：位图 = 切片自身尺寸；渐变无内在尺寸 = 对应边 border-width。
            BorderImageWidthComp::Auto => match &paint {
                BiPaint::Image(_) => [st, sr, sb, sl][i],
                BiPaint::Gradient { .. } => bw[i],
            },
        }
    };
    let (mut wt, mut wr, mut wb, mut wl) = (
        width_px(&width.comps[0], 0),
        width_px(&width.comps[1], 1),
        width_px(&width.comps[2], 2),
        width_px(&width.comps[3], 3),
    );
    // 带宽和溢出边框盒 → 等比缩小（css-backgrounds-3 U+38 规则）。
    let fw = (w / (wl + wr)).min(h / (wt + wb));
    if fw < 1.0 {
        wt *= fw;
        wr *= fw;
        wb *= fw;
        wl *= fw;
    }

    // —— 外扩（ink overflow：仅位移绘制域；TRBL 序）——
    let outset = style.border_image_outset();
    let out_px = |c: &BorderImageOutsetComp, i: usize| -> f32 {
        match c {
            BorderImageOutsetComp::Length(lp) => px(lp, style, env).max(0.0),
            BorderImageOutsetComp::Number(n) => (n * bw[i]).max(0.0),
        }
    };
    let (ot, orr, ob, ol) = (
        out_px(&outset.comps[0], 0),
        out_px(&outset.comps[1], 1),
        out_px(&outset.comps[2], 2),
        out_px(&outset.comps[3], 3),
    );
    let (bx, by, bwid, bhei) = (x - ol, y - ot, w + ol + orr, h + ot + ob);

    // —— 九区域发射 ——
    #[allow(clippy::too_many_arguments)] // 区域几何直传
    fn emit_region(
        out: &mut DisplayList,
        paint: &BiPaint,
        rx: f32,
        ry: f32,
        rw: f32,
        rh: f32,
        sx: f32,
        sy: f32,
        swd: f32,
        shd: f32,
    ) {
        if rw <= 0.0 || rh <= 0.0 || swd <= 0.0 || shd <= 0.0 {
            return;
        }
        match paint {
            BiPaint::Image(img) => out.ops.push(PaintOp::Image {
                x: rx,
                y: ry,
                width: rw,
                height: rh,
                radius: [0.0; 8],
                source_w: img.width,
                source_h: img.height,
                src_x: sx,
                src_y: sy,
                src_w: swd,
                src_h: shd,
                pixels: img.clone(),
            }),
            BiPaint::Gradient {
                gradient,
                linear,
                radial,
                conic,
            } => out.ops.push(PaintOp::Gradient {
                x: rx,
                y: ry,
                width: rw,
                height: rh,
                radius: [0.0; 8],
                gradient: gradient.clone(),
                radial: *radial,
                conic: *conic,
                linear: *linear,
            }),
        }
    }

    /// Per-axis tile segments (css-backgrounds-3 §2.4/§5.5 per-axis rules):
    /// returns a sequence of (offset along the axis, segment length), offsets
    /// relative to the region origin. stretch → a single segment [0, len];
    /// repeat → original-size tiles laid out in order, last tile truncated;
    /// round → n = max(1, round(len/tile)) segments evenly spaced (segment length
    /// len/n, whole tiles scaled); space → n = floor(len/tile): n == 0 (less than
    /// one tile) → one stretched segment, n == 1 → one original-size segment at
    /// the start, otherwise first/last tiles flush to the edges with the gaps
    /// spread evenly. tile ≤ 0 (gradient source has no intrinsic slice size) → a
    /// single stretched segment (Tier B fallback).
    fn bi_axis_segments(mode: BorderImageRepeatKind, len: f32, tile: f32) -> Vec<(f32, f32)> {
        if len <= 0.0 || tile <= 0.0 {
            return vec![(0.0, len.max(0.0))];
        }
        match mode {
            BorderImageRepeatKind::Stretch => vec![(0.0, len)],
            BorderImageRepeatKind::Repeat => {
                let mut segs = Vec::new();
                let mut p = 0.0;
                while p < len {
                    segs.push((p, tile.min(len - p)));
                    p += tile;
                }
                segs
            }
            BorderImageRepeatKind::Round => {
                let n = ((len / tile).round() as usize).max(1);
                let tw = len / n as f32;
                (0..n).map(|i| (tw * i as f32, tw)).collect()
            }
            BorderImageRepeatKind::Space => {
                let n = (len / tile) as usize;
                if n == 0 {
                    vec![(0.0, len)]
                } else if n == 1 {
                    vec![(0.0, tile)]
                } else {
                    let gap = (len - tile * n as f32) / (n - 1) as f32;
                    (0..n).map(|i| ((tile + gap) * i as f32, tile)).collect()
                }
            }
        }
    }

    /// Center (fill) region tiling emission (css-backgrounds-3 §5.5: repeat
    /// applies to "the sides and the middle part"): tile positions are computed
    /// along x by mode_x and along y by mode_y and combined as a Cartesian
    /// product; when either axis repeats, the region is clipped by PushClip as a
    /// safety net.
    #[allow(clippy::too_many_arguments)] // 平铺几何+双轴模式直传
    fn emit_tiled_region(
        out: &mut DisplayList,
        paint: &BiPaint,
        ox: f32,
        oy: f32,
        len_x: f32,
        len_y: f32,
        mode_x: BorderImageRepeatKind,
        tile_x: f32,
        mode_y: BorderImageRepeatKind,
        tile_y: f32,
        sx: f32,
        sy: f32,
        swd: f32,
        shd: f32,
    ) {
        if len_x <= 0.0 || len_y <= 0.0 || swd <= 0.0 || shd <= 0.0 {
            return;
        }
        let segs_x = bi_axis_segments(mode_x, len_x, tile_x);
        let segs_y = bi_axis_segments(mode_y, len_y, tile_y);
        let clipped = (tile_x > 0.0 && matches!(mode_x, BorderImageRepeatKind::Repeat))
            || (tile_y > 0.0 && matches!(mode_y, BorderImageRepeatKind::Repeat));
        if clipped {
            out.ops.push(PaintOp::PushClip {
                x: ox,
                y: oy,
                width: len_x,
                height: len_y,
                radius: [0.0; 8],
            });
        }
        for (py, ph) in segs_y {
            for (px, pw) in &segs_x {
                emit_region(out, paint, ox + px, oy + py, *pw, ph, sx, sy, swd, shd);
            }
        }
        if clipped {
            out.ops.push(PaintOp::PopClip);
        }
    }

    /// Edge region tiling emission (css-backgrounds-3 §5.5 sides): emits per-axis
    /// tile positions from bi_axis_segments; with repeat, the region is clipped by
    /// PushClip as a safety net. `tile` = source tile size along the axis (0 =
    /// gradient has no intrinsic slice size → stretch, Tier B).
    #[allow(clippy::too_many_arguments)] // 平铺几何+模式直传
    fn emit_tiled_edge(
        out: &mut DisplayList,
        paint: &BiPaint,
        origin: f32,
        cross_pos: f32,
        len: f32,
        thick: f32,
        horizontal: bool,
        mode: BorderImageRepeatKind,
        tile: f32,
        sx: f32,
        sy: f32,
        swd: f32,
        shd: f32,
    ) {
        if len <= 0.0 || thick <= 0.0 || swd <= 0.0 || shd <= 0.0 {
            return;
        }
        let clipped = tile > 0.0 && matches!(mode, BorderImageRepeatKind::Repeat);
        if clipped {
            out.ops.push(PaintOp::PushClip {
                x: if horizontal { origin } else { cross_pos },
                y: if horizontal { cross_pos } else { origin },
                width: if horizontal { len } else { thick },
                height: if horizontal { thick } else { len },
                radius: [0.0; 8],
            });
        }
        for (p, seg_len) in bi_axis_segments(mode, len, tile) {
            if horizontal {
                emit_region(
                    out,
                    paint,
                    origin + p,
                    cross_pos,
                    seg_len,
                    thick,
                    sx,
                    sy,
                    swd,
                    shd,
                );
            } else {
                emit_region(
                    out,
                    paint,
                    cross_pos,
                    origin + p,
                    thick,
                    seg_len,
                    sx,
                    sy,
                    swd,
                    shd,
                );
            }
        }
        if clipped {
            out.ops.push(PaintOp::PopClip);
        }
    }

    let rep = style.border_image_repeat();
    // 四角（恒拉伸；dest 带宽 × src 切片）。
    emit_region(out, &paint, bx, by, wl, wt, 0.0, 0.0, sl, st);
    emit_region(
        out,
        &paint,
        bx + bwid - wr,
        by,
        wr,
        wt,
        sw - sr,
        0.0,
        sr,
        st,
    );
    emit_region(
        out,
        &paint,
        bx + bwid - wr,
        by + bhei - wb,
        wr,
        wb,
        sw - sr,
        sh - sb,
        sr,
        sb,
    );
    emit_region(
        out,
        &paint,
        bx,
        by + bhei - wb,
        wl,
        wb,
        0.0,
        sh - sb,
        sl,
        sb,
    );
    // 四边（沿轴平铺模式：上/下 = rep.x，左/右 = rep.y）。tile = 源片沿轴尺寸
    //（位图源；渐变源 0 → stretch）。重复轴源窗口 = 切片间的中段。
    let (mid_w, mid_h) = (sw - sl - sr, sh - st - sb);
    let tile_for = |src_span: f32| -> f32 {
        match &paint {
            BiPaint::Image(_) => src_span,
            BiPaint::Gradient { .. } => 0.0,
        }
    };
    // 上边：沿 x、rep.x。
    emit_tiled_edge(
        out,
        &paint,
        bx + wl,
        by,
        (bwid - wl - wr).max(0.0),
        wt,
        true,
        rep.x,
        tile_for(mid_w),
        sl,
        0.0,
        mid_w,
        st,
    );
    // 下边。
    emit_tiled_edge(
        out,
        &paint,
        bx + wl,
        by + bhei - wb,
        (bwid - wl - wr).max(0.0),
        wb,
        true,
        rep.x,
        tile_for(mid_w),
        sl,
        sh - sb,
        mid_w,
        sb,
    );
    // 左边：沿 y、rep.y。
    emit_tiled_edge(
        out,
        &paint,
        by + wt,
        bx,
        (bhei - wt - wb).max(0.0),
        wl,
        false,
        rep.y,
        tile_for(mid_h),
        0.0,
        st,
        sl,
        mid_h,
    );
    // 右边。
    emit_tiled_edge(
        out,
        &paint,
        by + wt,
        bx + bwid - wr,
        (bhei - wt - wb).max(0.0),
        wr,
        false,
        rep.y,
        tile_for(mid_h),
        sw - sr,
        st,
        sr,
        mid_h,
    );
    // 中心（fill 才绘制；css-backgrounds-3 §5.5：repeat 作用于「the sides
    // and the middle part」——按 rep.x/rep.y 逐轴平铺、两轴片位取笛卡尔积；
    // 渐变源 tile=0 → 两轴单片拉伸回退）。
    if fill {
        emit_tiled_region(
            out,
            &paint,
            bx + wl,
            by + wt,
            (bwid - wl - wr).max(0.0),
            (bhei - wt - wb).max(0.0),
            rep.x,
            tile_for(mid_w),
            rep.y,
            tile_for(mid_h),
            sl,
            st,
            mid_w,
            mid_h,
        );
    }
    true
}

/// Radial geometry resolution (T4c): semantic values → absolute center/radii (px).
/// Circle formula per the CSS spec: circle farthest-corner = distance to the
/// farthest corner; ellipse farthest-corner = fx·√2, fy·√2 (fx/fy = distance from
/// the center to the farthest edge).
fn resolve_radial(
    spec: &crate::css::property::RadialSpec,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    style: &ComputedStyle,
    env: &MediaEnv,
) -> RadialGeom {
    let ctx = ResolveCtx {
        em: style.font_size_px(),
        rem: env.rem,
        viewport_w: env.viewport_w,
        viewport_h: env.viewport_h,
        ..ResolveCtx::base(style.font_size_px(), 16.0, env.viewport_w, env.viewport_h)
    };
    let cx = spec.position.0.resolve(&ctx, w).unwrap_or(w * 0.5);
    let cy = spec.position.1.resolve(&ctx, h).unwrap_or(h * 0.5);
    let (fx, fx_min) = (cx.max(w - cx), cx.min(w - cx));
    let (fy, fy_min) = (cy.max(h - cy), cy.min(h - cy));
    let sqrt2 = std::f32::consts::SQRT_2;
    use crate::css::property::{RadialShape, RadialSize};
    let circle = matches!(spec.shape, RadialShape::Circle);
    let (rx, ry) = match &spec.size {
        RadialSize::Explicit { rx, ry } => {
            let r = rx.resolve(&ctx, w).unwrap_or(0.0);
            let r2 = ry.as_ref().and_then(|v| v.resolve(&ctx, h)).unwrap_or(r);
            (r, r2)
        }
        RadialSize::ClosestSide => {
            if circle {
                (fx_min.min(fy_min), fx_min.min(fy_min))
            } else {
                (fx_min, fy_min)
            }
        }
        RadialSize::FarthestSide => {
            if circle {
                (fx.max(fy), fx.max(fy))
            } else {
                (fx, fy)
            }
        }
        RadialSize::ClosestCorner => {
            if circle {
                let d = (fx_min * fx_min + fy_min * fy_min).sqrt();
                (d, d)
            } else {
                (fx_min * sqrt2, fy_min * sqrt2)
            }
        }
        RadialSize::FarthestCorner => {
            if circle {
                let d = (fx * fx + fy * fy).sqrt();
                (d, d)
            } else {
                (fx * sqrt2, fy * sqrt2)
            }
        }
    };
    RadialGeom {
        cx: x + cx,
        cy: y + cy,
        rx: rx.max(0.0),
        ry: ry.max(0.0),
    }
}

/// Conic geometry resolution (C3, ADR-0017): semantic values → absolute center +
/// start angle. Angle mapping (D1): CSS 0deg = 12 o'clock clockwise →
/// peniko/ConicGeom 0 = positive X axis clockwise, hence
/// start = (from_deg − 90°)·π/180.
fn resolve_conic(
    spec: &crate::css::property::ConicSpec,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    style: &ComputedStyle,
    env: &MediaEnv,
) -> ConicGeom {
    let ctx = ResolveCtx {
        em: style.font_size_px(),
        rem: env.rem,
        viewport_w: env.viewport_w,
        viewport_h: env.viewport_h,
        ..ResolveCtx::base(style.font_size_px(), 16.0, env.viewport_w, env.viewport_h)
    };
    let cx = spec.position.0.resolve(&ctx, w).unwrap_or(w * 0.5);
    let cy = spec.position.1.resolve(&ctx, h).unwrap_or(h * 0.5);
    ConicGeom {
        cx: x + cx,
        cy: y + cy,
        start: (spec.from.0 - 90.0).to_radians(),
    }
}

fn paint_node(ctx: &PaintCtx<'_>, id: NodeId, out: &mut DisplayList) {
    let tree = ctx.tree;
    let styles = ctx.styles;
    let layout = ctx.layout;
    let scroll = ctx.scroll;
    let env = ctx.env;
    // P9-3（ADR-0041）：::marker 伪节点自身不绘制——taffy 隐藏但零尺寸
    // 布局条目仍存在；其文本已由宿主 paint 层合成（markers 表），此处
    // 跳过防双重发射。
    if tree.node(id).pseudo == Some(crate::tree::PseudoWhich::Marker) {
        return;
    }
    let Some(style) = styles.get(&id) else {
        return;
    };
    let Some(&(x, y, w, h)) = layout.get(&id) else {
        return;
    };
    let radius = resolve_radius(style, env);

    // backdrop-filter ≠ none（P2，ADR-0031 D3）：即时 op 先于一切层对与
    // 自身内容——作用于主画布（背后已绘制内容），区域 = 节点 border-box
    // （未变换视口坐标）。v1 采样面 = 当前画布（stacking context 边界
    // 细化 B 级在案）；效果由 sink 实现（soft 原生 / vello T2）。
    if let Some(DeclValue::Filters(bfs)) = style.get(PropertyId::BackdropFilter)
        && !bfs.is_empty()
        && w > 0.0
        && h > 0.0
    {
        out.ops.push(PaintOp::BackdropFilter {
            filters: resolve_filter_effects(bfs, style, env),
            x,
            y,
            width: w,
            height: h,
        });
    }

    // transform ≠ none（ADR-0009）：L3 绘制期仿射终结——translate 百分比基 =
    // 自身 border-box，transform-origin 默认 50% 50%（T(o)·M·T(−o)）；层栈序
    // transform → clip → blend → opacity → filter（transform 最外、filter 最内；
    // P2 修订：css-filters-1 §3 合成序 opacity(filter(子树))）。布局盒保持
    // 未变换坐标（taffy 不可见 transform）；绘制期终结见 ADR-0009 双时机契约。
    let transformed = style.has_transform();
    let node_affine = if transformed {
        Some(resolve_transform_affine(style, x, y, w, h, env))
    } else {
        None
    };
    if transformed {
        out.ops.push(PaintOp::PushTransform {
            affine: node_affine.unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
        });
        // P4 D4（ADR-0037）：命中收集的活跃仿射复合（子树走查继承，
        // 尾部 PopTransform 段恢复）。
        if let Some(hc) = ctx.hit {
            let mut h = hc.borrow_mut();
            h.mat = mul_affine(
                &h.mat,
                &node_affine.unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
            );
        }
    }

    // F3c（ADR-0025）：clip-path 裁剪层——元素自身绘制（阴影/背景/替换图/
    // 文本）与子树全部包入；栈位 transform 内、opacity 外（ADR-0009 层序
    // transform → clip）；overflow 裁剪在其内 push（LIFO：后 push 先 pop）。
    // inset → 复用 PushClip 逐角圆角精确能力；circle/ellipse 64 段折线 /
    // polygon 顶点直传 → PushClipPath；Other（url()/path() 收容）不裁剪。
    // 命中链：形状 AABB 登记（折线 AABB 近似，B 级在案）——登记先于命中
    // 收集 = 元素自身命中亦受自身 clip-path 约束（浏览器一致）。
    let insets = resolve_insets(style, env);
    let clip_emit = match style.clip_path() {
        ClipShape::None => ClipEmit::None,
        shape => clip_path_emit(&shape, x, y, w, h, &insets, style, env),
    };
    match &clip_emit {
        ClipEmit::None => {}
        ClipEmit::Rect { rect, radius } => {
            out.ops.push(PaintOp::PushClip {
                x: rect.0,
                y: rect.1,
                width: rect.2,
                height: rect.3,
                radius: *radius,
            });
            if let Some(hc) = ctx.hit {
                let inv = invert_affine(&hc.borrow().mat).unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
                hc.borrow_mut().clips.push(HitClip::Rect {
                    rect: [rect.0, rect.1, rect.2, rect.3],
                    radius: *radius,
                    inv,
                });
            }
        }
        ClipEmit::Path { points, nonzero } => {
            out.ops.push(PaintOp::PushClipPath {
                points: points.clone(),
                nonzero: *nonzero,
            });
            if let Some(hc) = ctx.hit {
                let inv = invert_affine(&hc.borrow().mat).unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
                hc.borrow_mut().clips.push(HitClip::Path {
                    points: points.clone(),
                    nonzero: *nonzero,
                    inv,
                });
            }
        }
    }

    // F3a（ADR-0023）：命中几何收集（border-box 视口系+活跃 clip 链
    // 快照；visibility/display:none 不入表=paint 期已跳过天然正确；
    // pointer-events: none 排除=A1 语义消费；变换节点=未旋盒 B 级在案；
    // F3c：置于 clip-path push 之后 = 自身命中含自身裁剪链）。
    if let Some(hc) = ctx.hit {
        let pointer_none = matches!(
            style.get(PropertyId::PointerEvents),
            Some(DeclValue::PointerEvents(
                crate::css::property::PointerEventsKind::None
            ))
        );
        if !pointer_none {
            let clips = hc.borrow().clips.clone();
            let mat = hc.borrow().mat;
            hc.borrow_mut().rects.push(HitRect {
                node_id: id,
                x,
                y,
                w,
                h,
                clips,
                mat,
            });
        }
    }

    // 0) 混合/隔离层（P1-2，css-compositing-1）：mix-blend-mode ≠ normal 或
    //    isolation: isolate → PushBlend/PopBlend 包住整节点（背景/边框/
    //    文本/子树）。混合层必须最外（先于 opacity push）：合成序 =
    //    blend(背后画布, opacity(子树))；isolation 复用 Normal 混合层对
    //    =隔离组边界（子树内混合模式不越界，css-compositing-1 §5.1）。
    //    覆盖区域=节点 border-box（bbox 外绘制如外阴影不参与混合——B 级
    //    同 opacity 约定）。
    let blend = style.mix_blend();
    let blend_isolated = (blend != crate::css::property::BlendMode::Normal
        || style.has_isolation())
        && w > 0.0
        && h > 0.0;
    if blend_isolated {
        out.ops.push(PaintOp::PushBlend {
            mode: blend,
            x,
            y,
            width: w,
            height: h,
        });
    }

    // 0b) opacity < 1（ADR-0008）：整节点（背景/边框/文本/子树）包 alpha 合成层；
    //    opacity 触发 stacking context，节点进 Pos 带（见子树分带）。
    let opacity = match style.get(PropertyId::Opacity) {
        Some(DeclValue::Number(n)) => (*n).clamp(0.0, 1.0),
        _ => 1.0,
    };
    let faded = opacity < 1.0 && w > 0.0 && h > 0.0;
    if faded {
        out.ops.push(PaintOp::PushOpacity {
            alpha: opacity,
            x,
            y,
            width: w,
            height: h,
        });
    }

    // 0c) filter ≠ none（P2，ADR-0031 D3）：整节点（背景/边框/文本/子树）
    //     包滤镜层——离屏渲染经函数链处理再合成。栈位 opacity 之内
    //     （css-filters-1 §3：opacity 应用于 filter 输出）→ push 在
    //     PushOpacity 之后（LIFO：PopFilter 先于 PopOpacity）；节点进 Pos 带
    //     （SC 谓词 has_filter 已覆盖）。
    let filtered = match style.get(PropertyId::Filter) {
        Some(DeclValue::Filters(f)) if !f.is_empty() && w > 0.0 && h > 0.0 => {
            out.ops.push(PaintOp::PushFilter {
                filters: resolve_filter_effects(f, style, env),
                x,
                y,
                width: w,
                height: h,
            });
            true
        }
        _ => false,
    };

    // 1) 外阴影（CSS 绘制顺序：先于背景）
    if let Some(DeclValue::BoxShadows(shadows)) = style.get(PropertyId::BoxShadow) {
        for sh in shadows.iter().filter(|s| !s.inset) {
            let color = resolve_color(&sh.color, style, env);
            if color.components[3] <= 0.0 {
                continue;
            }
            out.ops.push(PaintOp::Shadow {
                x,
                y,
                width: w,
                height: h,
                radius,
                color,
                offset_x: px(&sh.offset_x, style, env),
                offset_y: px(&sh.offset_y, style, env),
                blur: px(&sh.blur, style, env),
                spread: px(&sh.spread, style, env),
                inset: false,
            });
        }
    }

    // 2) 背景（F3b，ADR-0024）：色底（border-box 绘制，元素圆角）+
    // 多层图——首层最上 → 逆序发射；层几何 = origin 定位区 / clip 绘制区 /
    // size 解析 / position 语义 / repeat 平铺；fixed = 视口锚定，
    // clip:text 降级 border-box + warn（B 级，在案）。
    {
        let bg = resolve_color(&style.background_color(), style, env);
        if bg.components[3] > 0.0 && w > 0.0 && h > 0.0 {
            out.ops.push(PaintOp::FillRect {
                x,
                y,
                width: w,
                height: h,
                radius,
                color: bg,
            });
        }
    }
    let layers = style.background_layers();
    // insets 已在 clip-path 段解析（F3c 提前；背景定位区/绘制区复用）。
    for layer in layers.iter().rev() {
        // 定位区（origin 盒；fixed = 视口锚定）。
        let area = match layer.attachment {
            Attachment::Fixed => (0.0, 0.0, env.viewport_w, env.viewport_h),
            _ => background_box_rect(layer.origin, x, y, w, h, &insets),
        };
        // 绘制区（clip 盒；text → border-box 降级 + warn，B 级）。
        let clip_kind = match layer.clip {
            BackgroundClip::Box(b) => b,
            BackgroundClip::Text => {
                tracing::warn!(
                    target: "style_engine::css",
                    "background-clip: text not supported in paint; fallback border-box"
                );
                BackgroundBox::BorderBox
            }
        };
        let clip = background_box_rect(clip_kind, x, y, w, h, &insets);
        let clip_radius = if matches!(clip_kind, BackgroundBox::BorderBox) {
            radius
        } else {
            [0.0; 8]
        };
        // 层图内在尺寸（gradient = 定位区；url = 注册图像；未注册告警跳过）。
        let (img_w, img_h, gradient, img) = match &layer.image {
            BackgroundImage::None => continue,
            BackgroundImage::Gradient(g) => (area.2, area.3, Some(g), None),
            BackgroundImage::Url(reference) => match ctx.images.get(reference) {
                Some(img) => (img.width as f32, img.height as f32, None, Some(img)),
                None => {
                    tracing::warn!(
                        target: "style_engine::css",
                        reference = reference.as_str(),
                        "background-image url not registered; skipped"
                    );
                    continue;
                }
            },
        };
        let geom = match resolve_layer_geom(
            &layer.position,
            &layer.size,
            &layer.repeat,
            area,
            img_w,
            img_h,
            style,
            env,
        ) {
            Some(g) => g,
            None => continue,
        };
        let tiles_x = tile_axis_positions(
            geom.axis_x,
            geom.dx,
            geom.dw,
            clip.0,
            clip.2,
            area.0,
            area.2,
        );
        let tiles_y = tile_axis_positions(
            geom.axis_y,
            geom.dy,
            geom.dh,
            clip.1,
            clip.3,
            area.1,
            area.3,
        );
        out.ops.push(PaintOp::PushClip {
            x: clip.0,
            y: clip.1,
            width: clip.2,
            height: clip.3,
            radius: clip_radius,
        });
        for &(ty, th) in &tiles_y {
            for &(tx, tw) in &tiles_x {
                if let Some(g) = gradient {
                    let resolved = Gradient {
                        kind: g.kind.clone(),
                        repeating: g.repeating,
                        stops: g
                            .stops
                            .iter()
                            .map(|s| crate::css::property::ColorStop {
                                color: ColorValue::Absolute(resolve_color(&s.color, style, env)),
                                position: s.position.clone(),
                            })
                            .collect(),
                        hints: g.hints.clone(),
                    };
                    let radial = match &g.kind {
                        crate::css::property::GradientKind::Radial(spec) => {
                            Some(resolve_radial(spec, tx, ty, tw, th, style, env))
                        }
                        _ => None,
                    };
                    let conic = match &g.kind {
                        crate::css::property::GradientKind::Conic(spec) => {
                            Some(resolve_conic(spec, tx, ty, tw, th, style, env))
                        }
                        _ => None,
                    };
                    // 线性几何（F3d，ADR-0026）：渐变线按 tile 盒解析为绝对
                    // 端点（radial/conic 同款 paint 层终结；sink 优先消费）。
                    let linear = match &g.kind {
                        crate::css::property::GradientKind::Linear(angle) => {
                            Some(resolve_linear(angle.0, tx, ty, tw, th))
                        }
                        _ => None,
                    };
                    out.ops.push(PaintOp::Gradient {
                        x: tx,
                        y: ty,
                        width: tw,
                        height: th,
                        radius: [0.0; 8],
                        gradient: resolved,
                        radial,
                        conic,
                        linear,
                    });
                } else if let Some(img) = img {
                    out.ops.push(PaintOp::Image {
                        x: tx,
                        y: ty,
                        width: tw,
                        height: th,
                        radius: [0.0; 8],
                        source_w: img.width,
                        source_h: img.height,
                        src_x: 0.0,
                        src_y: 0.0,
                        src_w: img.width as f32,
                        src_h: img.height as f32,
                        pixels: img.clone(),
                    });
                }
            }
        }
        out.ops.push(PaintOp::PopClip);
    }

    // 2a) 元素替换内容图像（C3，css-images-3；ADR-0017）：StyleNode.image →
    // 注册表解析 → object-fit/object-position 适配内容盒（与背景通道语义
    // 完全解耦；未注册引用告警跳过——零副作用契约）。
    if let Some(reference) = tree.node(id).image.as_ref() {
        match ctx.images.get(reference) {
            Some(img) => {
                // 内容盒 = border-box − border − padding（替换内容区）。
                let bl = style
                    .len(PropertyId::BorderLeftWidth)
                    .map(|lp| px(lp, style, env))
                    .unwrap_or(0.0);
                let br = style
                    .len(PropertyId::BorderRightWidth)
                    .map(|lp| px(lp, style, env))
                    .unwrap_or(0.0);
                let bt = style
                    .len(PropertyId::BorderTopWidth)
                    .map(|lp| px(lp, style, env))
                    .unwrap_or(0.0);
                let bb = style
                    .len(PropertyId::BorderBottomWidth)
                    .map(|lp| px(lp, style, env))
                    .unwrap_or(0.0);
                let pl = style
                    .len(PropertyId::PaddingLeft)
                    .map(|lp| px(lp, style, env))
                    .unwrap_or(0.0);
                let pr = style
                    .len(PropertyId::PaddingRight)
                    .map(|lp| px(lp, style, env))
                    .unwrap_or(0.0);
                let pt = style
                    .len(PropertyId::PaddingTop)
                    .map(|lp| px(lp, style, env))
                    .unwrap_or(0.0);
                let pb = style
                    .len(PropertyId::PaddingBottom)
                    .map(|lp| px(lp, style, env))
                    .unwrap_or(0.0);
                let (bx, by, bw, bh) = (
                    x + bl + pl,
                    y + bt + pt,
                    (w - bl - br - pl - pr).max(0.0),
                    (h - bt - bb - pt - pb).max(0.0),
                );
                // fit 数学（ADR-0017 D4）：s = 源装入盒的适配比例。
                let (sw, sh) = (img.width as f32, img.height as f32);
                let (fw, fh) = match style.object_fit() {
                    crate::css::property::ObjectFitKind::Fill => (bw, bh),
                    crate::css::property::ObjectFitKind::Contain => {
                        let s = (bw / sw).min(bh / sh);
                        (sw * s, sh * s)
                    }
                    crate::css::property::ObjectFitKind::Cover => {
                        let s = (bw / sw).max(bh / sh);
                        (sw * s, sh * s)
                    }
                    crate::css::property::ObjectFitKind::None => (sw, sh),
                    crate::css::property::ObjectFitKind::ScaleDown => {
                        let s = ((bw / sw).min(bh / sh)).min(1.0);
                        (sw * s, sh * s)
                    }
                };
                // object-position：offset = pct·(盒 − 拟合)（负基合法=溢出反向对齐）。
                let (posx, posy) = style.object_position();
                let rc = ResolveCtx {
                    em: style.font_size_px(),
                    rem: env.rem,
                    viewport_w: env.viewport_w,
                    viewport_h: env.viewport_h,
                    ..ResolveCtx::base(style.font_size_px(), 16.0, env.viewport_w, env.viewport_h)
                };
                let ox = posx.resolve(&rc, bw - fw).unwrap_or(0.0);
                let oy = posy.resolve(&rc, bh - fh).unwrap_or(0.0);
                let (dx, dy) = (bx + ox, by + oy);
                let clipped = radius.iter().any(|r| *r > 0.0)
                    || dx < bx
                    || dy < by
                    || dx + fw > bx + bw
                    || dy + fh > by + bh;
                if clipped {
                    out.ops.push(PaintOp::PushClip {
                        x: bx,
                        y: by,
                        width: bw,
                        height: bh,
                        radius,
                    });
                }
                out.ops.push(PaintOp::Image {
                    x: dx,
                    y: dy,
                    width: fw,
                    height: fh,
                    radius: [0.0; 8],
                    source_w: img.width,
                    source_h: img.height,
                    src_x: 0.0,
                    src_y: 0.0,
                    src_w: img.width as f32,
                    src_h: img.height as f32,
                    pixels: img.clone(),
                });
                if clipped {
                    out.ops.push(PaintOp::PopClip);
                }
            }
            None => {
                tracing::warn!(
                    target: "style_engine::css",
                    reference = reference.as_str(),
                    "element image not registered; skipped"
                );
            }
        }
    }

    // 2b) 内阴影（第五批⑩：CSS 绘制序=背景之上、边框之下）
    if let Some(DeclValue::BoxShadows(shadows)) = style.get(PropertyId::BoxShadow) {
        for sh in shadows.iter().filter(|s| s.inset) {
            let color = resolve_color(&sh.color, style, env);
            if color.components[3] <= 0.0 {
                continue;
            }
            out.ops.push(PaintOp::Shadow {
                x,
                y,
                width: w,
                height: h,
                radius,
                color,
                offset_x: px(&sh.offset_x, style, env),
                offset_y: px(&sh.offset_y, style, env),
                blur: px(&sh.blur, style, env),
                spread: px(&sh.spread, style, env),
                inset: true,
            });
        }
    }

    // 3) 边框：四边独立读取（top/right/bottom/left）
    let side = |w_id: PropertyId, s_id: PropertyId, c_id: PropertyId| -> BorderSide {
        let width = match style.get(w_id) {
            Some(DeclValue::BorderWidth(Some(lp))) => px(lp, style, env),
            _ => 0.0,
        };
        let bstyle = match style.get(s_id) {
            Some(DeclValue::BorderStyle(s)) => *s,
            _ => BorderStyle::None,
        };
        let color = match style.get(c_id) {
            Some(DeclValue::Color(c)) => resolve_color(c, style, env),
            _ => AlphaColor::new([0.0, 0.0, 0.0, 1.0]),
        };
        BorderSide {
            width,
            style: bstyle,
            color,
        }
    };
    let sides = [
        side(
            PropertyId::BorderTopWidth,
            PropertyId::BorderTopStyle,
            PropertyId::BorderTopColor,
        ),
        side(
            PropertyId::BorderRightWidth,
            PropertyId::BorderRightStyle,
            PropertyId::BorderRightColor,
        ),
        side(
            PropertyId::BorderBottomWidth,
            PropertyId::BorderBottomStyle,
            PropertyId::BorderBottomColor,
        ),
        side(
            PropertyId::BorderLeftWidth,
            PropertyId::BorderLeftStyle,
            PropertyId::BorderLeftColor,
        ),
    ];
    // border-image（F3d，ADR-0026）：source ≠ none 时按 css-backgrounds-3
    // 在 Border op 位取代边框（9-slice 九区域）；源不可用（URL 未注册）→
    // warn 回退边框（零副作用契约，背景图同款）。
    if !paint_border_image(ctx, style, env, x, y, w, h, &sides, out) {
        // P4 D1（ADR-0037）：dashed/dotted 拆段/圆角退 Solid 判定收口。
        emit_borders(x, y, w, h, radius, sides, out);
    }

    // 3a-2) outline（A2）：不占布局的装饰描边（ink overflow，css-ui-4）——
    // 复用 Border 基元以外扩矩形承载：描边带 = [border-box+offset,
    // border-box+offset+width]，Border op 在外扩 (offset+width) 的矩形上画
    // 带宽 width 的边框条即精确落位；radius 随外扩量增长（圆角圆心不变
    // 近似，v1 锁定）。outline 不入滚动量程（ink overflow 非 scrollable
    // overflow）；style auto = 宿主 focus ring 语义位，绘制按 Solid 近似
    //（B 级，FEATURES.md 在案）。绘制序：边框之后、列规与内容之前。
    let o_width = match style.get(PropertyId::OutlineWidth) {
        Some(DeclValue::BorderWidth(Some(lp))) => px(lp, style, env),
        _ => 0.0,
    };
    let o_style = match style.get(PropertyId::OutlineStyle) {
        Some(DeclValue::OutlineStyle(s)) => match s {
            OutlineStyle::None => None,
            // 花式线型（double/groove/ridge/inset/outset）按 Solid 近似
            // （B 级在案；BorderStyle 实际仅 None/Hidden/Solid/Dashed/
            // Dotted 五变体）。auto = 宿主 focus ring 语义位（css-ui-4）：
            // 引擎不注入 UA 默认 outline，绘制按 Solid——宿主以 outline-
            // width/offset/color 显式声明时即获得精确渲染（P4 D5，ADR-0037）。
            OutlineStyle::Auto
            | OutlineStyle::Solid
            | OutlineStyle::Double
            | OutlineStyle::Groove
            | OutlineStyle::Ridge
            | OutlineStyle::Inset
            | OutlineStyle::Outset => Some(BorderStyle::Solid),
            OutlineStyle::Dashed => Some(BorderStyle::Dashed),
            OutlineStyle::Dotted => Some(BorderStyle::Dotted),
        },
        _ => None,
    };
    if let Some(o_style) = o_style
        && o_width > 0.0
    {
        let o_color = match style.get(PropertyId::OutlineColor) {
            Some(DeclValue::Color(c)) => resolve_color(c, style, env),
            _ => resolve_color(&ColorValue::CurrentColor, style, env),
        };
        let offset = match style.get(PropertyId::OutlineOffset) {
            Some(DeclValue::Len(lp)) => px(lp, style, env),
            _ => 0.0,
        };
        let d = offset + o_width;
        // 直角盒外扩依旧直角（圆心不变的 +d 增长只对真圆角有意义）——
        // 零 radius 传零数组，保 emit_borders 拆段路径可达。
        let o_radius = if radius == [0.0; 8] {
            [0.0; 8]
        } else {
            let mut grown = radius;
            for r in &mut grown {
                *r = (*r + d).max(0.0);
            }
            grown
        };
        // P4 D1：outline 虚线与边框同通道（外扩矩形无圆角时精确拆段）。
        emit_borders(
            x - d,
            y - d,
            w + 2.0 * d,
            h + 2.0 * d,
            o_radius,
            std::array::from_fn(|_| BorderSide {
                width: o_width,
                style: o_style,
                color: o_color,
            }),
            out,
        );
    }

    // 3b) 多列列规（三期⑤c）：settle_column_rules 结算的条带，坐标相对
    // 容器 border-box 原点。装饰绘制序：内容之下（子件前）、边框之上——
    // 列规只落在列间 gap 中，不与内容重叠（css-multicol）。
    if let Some(segs) = ctx.column_rules.get(&id) {
        for seg in segs {
            out.ops.push(PaintOp::FillRect {
                x: x + seg.x,
                y: y + seg.y,
                width: seg.width,
                height: seg.height,
                radius: [0.0; 8],
                color: seg.color,
            });
        }
    }

    // 4) 文本（叶内容；原点 = 内容盒左上）
    if let Some(text) = tree.node(id).text.as_ref()
        && !text.is_empty()
    {
        let pad_l = style
            .len(PropertyId::PaddingLeft)
            .map(|lp| px(lp, style, env))
            .unwrap_or(0.0);
        let pad_t = style
            .len(PropertyId::PaddingTop)
            .map(|lp| px(lp, style, env))
            .unwrap_or(0.0);
        // P9-3（css-lists-3 §3.1，ADR-0041）：宿主首行内容左缘合成
        // marker 文本 op（树序先于宿主文本）；样式/文本/度量取 marker
        // 节点自身（::marker 作者规则经 originating element 级联）。
        // 无折行（单行标记；max_advance None）、无 span、无命中收集
        //（标记非交互目标）。suffix 尾空格经 white-space:pre 计入测量
        // 宽，与宿主文本间距由前进宽承载。
        if let Some(&(mid, _adv)) = ctx.markers.get(&id)
            && let Some(mcs) = styles.get(&mid)
            && let Some(mtext) = tree.node(mid).text.as_ref()
            && !mtext.is_empty()
        {
            out.ops.push(PaintOp::Text {
                x: x + pad_l,
                y: y + pad_t,
                text: mtext.clone(),
                color: resolve_color(&mcs.color(), mcs, env),
                spans: Vec::new(),
                font_size: mcs.font_size_px(),
                font_family: mcs.font_family().clone(),
                font_weight: mcs.font_weight(),
                italic: mcs.font_style() == FontStyle::Italic,
                max_advance: None,
                line_height: mcs.resolved_line_height_px(env),
                letter_spacing: mcs.resolved_letter_spacing_px(env),
                text_align: mcs.text_align(),
                word_break: mcs.word_break(),
                overflow_wrap: mcs.overflow_wrap(),
                decorations: text_decorations(mcs, env),
                shadows: text_shadows(mcs, env),
                font_stretch: mcs.font_stretch(),
                word_spacing: mcs.resolved_word_spacing_px(env),
                font_features: mcs.effective_font_features(),
                font_variations: mcs.font_variations(),
            });
        }
        // inside 语义：宿主文本自 marker 右缘起排（前进宽偏移；折行宽
        // 已在引擎 leaf_wrap_width 同步收缩）。
        let marker_adv = ctx.markers.get(&id).map(|(_, a)| *a).unwrap_or(0.0);
        let color = resolve_color(&style.color(), style, env);
        // T5c：span 绘制期样式终结（与基样式同一条解析路径）；C2（ADR-0016）：
        // text-transform 分段变换——op.text 携带变换后文本 + span 偏移经
        // old→new 字节映射重写（sink 零改动）；全表 none → 直通。
        let raw_spans = ctx.spans.get(&id);
        let span_refs: Vec<(u32, u32, &ComputedStyle)> = raw_spans
            .map(|list| list.iter().map(|(s, e, cs)| (*s, *e, cs)).collect())
            .unwrap_or_default();
        let has_tt = crate::text_transform::needs_transform(style, &span_refs);
        let t = if has_tt {
            let segs = crate::text_transform::segments(text, style, &span_refs);
            Some(crate::text_transform::transform(text, &segs))
        } else {
            None
        };
        // F3d（ADR-0026 D5）：font-variant-caps 小型大写合成（先 text-transform
        // 后合成 = CSS 顺序，输入为变换后文本）。截断 override 旁路一致
        //（override 为原始前缀、transform 已旁路，caps 同）。
        let synth: Option<crate::text_transform::CapsSynthesis> =
            if ctx.text_overrides.contains_key(&id) {
                None
            } else if crate::text_transform::needs_caps_synth(style, &span_refs) {
                let src: &str = match &t {
                    Some(tt) => tt.text.as_str(),
                    None => text.as_str(),
                };
                Some(crate::text_transform::synth_caps(src, style, &span_refs))
            } else {
                None
            };
        let map_start = |s: u32| -> u32 {
            let mut v = match &t {
                Some(tt) => tt.map(s as usize) as u32,
                None => s,
            };
            if let Some(sy) = &synth {
                v = sy.map(v as usize) as u32;
            }
            v
        };
        let map_end = |e: u32| -> u32 {
            let mut v = match &t {
                Some(tt) => {
                    if (e as usize) >= text.len() {
                        tt.text.len() as u32
                    } else {
                        tt.map(e as usize) as u32
                    }
                }
                None => e,
            };
            if let Some(sy) = &synth {
                v = sy.map(v as usize) as u32;
            }
            v
        };
        // F2（ADR-0022 D2）：text-overflow/line-clamp 截断 override——
        // 生成于 engine.apply_text_truncation（测量宽就绪后），此处替换
        // 绘制文本（盒几何不变）。
        let op_text = match ctx.text_overrides.get(&id) {
            Some(t2) => t2.clone(),
            None => match &synth {
                Some(sy) => sy.text.clone(),
                None => match &t {
                    Some(tt) => tt.text.clone(),
                    None => text.clone(),
                },
            },
        };
        // 截断 override 存在时 span 字节区间按前缀长度过滤+钳位（越界
        // span 丢弃；前缀内 end 钳至前缀尾；"…" 字形吃基色）。偏差：override
        // 为原始文本前缀，text-transform 对截断叶旁路（ADR-0022 在案）。
        let trunc_prefix = if ctx.text_overrides.contains_key(&id) {
            Some(op_text.len().saturating_sub(3)) // "…" UTF-8 3 字节
        } else {
            None
        };
        let mut spans: Vec<TextSpanPaint> = span_refs
            .iter()
            .filter_map(|(start, end, scs)| {
                if let Some(p) = trunc_prefix
                    && (*start as usize) >= p
                {
                    return None;
                }
                let e = map_end(*end);
                let e = match trunc_prefix {
                    Some(p) => (e as usize).min(p) as u32,
                    None => e,
                };
                Some(TextSpanPaint {
                    start: map_start(*start),
                    end: e,
                    color: resolve_color(&scs.color(), scs, env),
                    font_size: scs.font_size_px(),
                    font_weight: scs.font_weight(),
                    italic: scs.font_style() == FontStyle::Italic,
                    font_family: scs.font_family().clone(),
                    // P9-5（ADR-0042）：span 级行高/字距终结值——测量
                    // （text.rs build_layout）与 sink 同规则消费。
                    letter_spacing: scs.resolved_letter_spacing_px(env),
                    line_height: if scs.line_height() == &LineHeight::Normal {
                        None
                    } else {
                        scs.resolved_line_height_px(env)
                    },
                })
            })
            .collect();
        // F3d（ADR-0026 D5）：caps 合成区间以 0.8× 字号作为独立 span 追加
        //（新文本坐标 = op_text 坐标；样式 = 区间首字符原文域覆盖 span，
        // 无覆盖 = 基样式）。override 旁路时 synth=None 无追加。
        if let Some(sy) = &synth {
            for (s, e, orig) in &sy.scaled {
                let cover = span_refs
                    .iter()
                    .find(|(ps, pe, _)| {
                        (*ps as usize) <= *orig && *orig < (*pe as usize).min(text.len())
                    })
                    .map(|(_, _, scs)| *scs);
                let scs = cover.unwrap_or(style);
                spans.push(TextSpanPaint {
                    start: *s,
                    end: (*e as usize).min(op_text.len()) as u32,
                    color: resolve_color(&scs.color(), scs, env),
                    font_size: scs.font_size_px() * crate::text_transform::SYNTH_CAPS_SCALE,
                    font_weight: scs.font_weight(),
                    italic: scs.font_style() == FontStyle::Italic,
                    font_family: scs.font_family().clone(),
                    // caps 合成区间：测量侧仅推字号覆盖（F3d），行高/字距
                    // 均按覆盖 span 的计算值承载（无覆盖 = 基样式）。
                    letter_spacing: scs.resolved_letter_spacing_px(env),
                    line_height: if scs.line_height() == &LineHeight::Normal {
                        None
                    } else {
                        scs.resolved_line_height_px(env)
                    },
                });
            }
        }
        out.ops.push(PaintOp::Text {
            x: x + pad_l + marker_adv,
            y: y + pad_t,
            text: op_text,
            color,
            spans,
            font_size: style.font_size_px(),
            font_family: style.font_family().clone(),
            font_weight: style.font_weight(),
            italic: style.font_style() == FontStyle::Italic,
            max_advance: ctx.wrap_widths.get(&id).copied().flatten(),
            line_height: style.resolved_line_height_px(env),
            letter_spacing: style.resolved_letter_spacing_px(env),
            text_align: style.text_align(),
            word_break: style.word_break(),
            overflow_wrap: style.overflow_wrap(),
            decorations: text_decorations(style, env),
            shadows: text_shadows(style, env),
            font_stretch: style.font_stretch(),
            word_spacing: style.resolved_word_spacing_px(env),
            font_features: style.effective_font_features(),
            font_variations: style.font_variations(),
        });
    }

    // 5) 裁剪 + 滚动 + 子节点
    let clip = matches!(
        style.overflow_x(),
        Overflow::Hidden | Overflow::Clip | Overflow::Scroll
    ) || matches!(
        style.overflow_y(),
        Overflow::Hidden | Overflow::Clip | Overflow::Scroll
    );
    if clip {
        out.ops.push(PaintOp::PushClip {
            x,
            y,
            width: w,
            height: h,
            radius,
        });
        // F3a（ADR-0023）：子树裁剪 → 命中 clip 链登记（PopClip 同步弹出；
        // P4 D4：矩形+圆角+活跃仿射逆精确承载）。
        if let Some(hc) = ctx.hit {
            let inv = invert_affine(&hc.borrow().mat).unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
            hc.borrow_mut().clips.push(HitClip::Rect {
                rect: [x, y, w, h],
                radius,
                inv,
            });
        }
    }
    let mut scrolled = false;
    if let Some(&(dx, dy)) = scroll.get(&id)
        && (dx != 0.0 || dy != 0.0)
    {
        out.ops.push(PaintOp::PushScroll { dx, dy });
        scrolled = true;
    }
    // 7) 子树（ADR-0008，CSS 2.1 Appendix E 简化三带）：
    //    Neg：positioned 且负数字 z（z 升序、等值树序）——先于 in-flow；
    //    Flow：in-flow 非定位、非 SC 触发（树序，含文本叶）；
    //    Pos：positioned（auto/0 树序在前、正 z 升序在后）+ 非定位 SC 触发者
    //    （opacity<1 或 transform ≠ none，键 0 树序——ADR-0008/0009）
    //    第五批㉑：flex/grid 子项的显式 z-index（≠ auto）无需 position——
    //    与定位元素同等参与三带（CSS：flex/grid item 的 z-index ≠ auto
    //    还创建 stacking context，按 ADR-0008 三带即可表达）
    let is_flex_or_grid = matches!(
        style.get(PropertyId::Display),
        Some(DeclValue::Display(crate::css::property::Display::Flex))
            | Some(DeclValue::Display(crate::css::property::Display::Grid))
    );
    let mut neg: Vec<(f32, usize, NodeId)> = Vec::new();
    let mut flow: Vec<NodeId> = Vec::new();
    let mut pos: Vec<(f32, usize, NodeId)> = Vec::new();
    for (idx, c) in tree.children(id).iter().enumerate() {
        let Some(cstyle) = styles.get(c) else {
            flow.push(*c);
            continue;
        };
        let z = match cstyle.get(PropertyId::ZIndex) {
            Some(DeclValue::ZIndex(Some(n))) => Some(*n),
            _ => None,
        };
        let positioned = matches!(
            cstyle.get(PropertyId::Position),
            Some(DeclValue::Position(p)) if !matches!(p, crate::css::property::Position::Static)
        ) || (is_flex_or_grid && z.is_some());
        let faded = matches!(
            cstyle.get(PropertyId::Opacity),
            Some(DeclValue::Number(n)) if *n < 1.0
        );
        // SC 触发（ADR-0008 全集 / ADR-0009）：transform ≠ none、
        // filter/clip-path ≠ none（第四批④）或 will-change 含触发属性 /
        // isolation: isolate / mix-blend-mode ≠ normal（第五批㉒触发；
        // P1-2 起二者经 PushBlend/PopBlend 层对携带效果）/
        // backdrop-filter ≠ none（F4，ADR-0028，css-filters-2 同 filter
        // 语义）——其余仅触发、不产生 PaintOp；非定位
        // 触发者进 Pos 带键 0
        let sc = cstyle.has_transform()
            || cstyle.has_filter()
            || cstyle.has_clip_path()
            || cstyle.has_will_change_sc()
            || cstyle.has_isolation()
            || cstyle.has_mix_blend()
            || cstyle.has_backdrop_filter();
        match (positioned, sc, z, faded) {
            (true, _, Some(n), _) if n < 0.0 => neg.push((n, idx, *c)),
            (true, _, _, _) | (_, true, _, _) | (_, _, _, true) => {
                pos.push((z.unwrap_or(0.0).max(0.0), idx, *c))
            }
            _ => flow.push(*c),
        }
    }
    neg.sort_by(|a, b| {
        (a.0, a.1)
            .partial_cmp(&(b.0, b.1))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    pos.sort_by(|a, b| {
        (a.0, a.1)
            .partial_cmp(&(b.0, b.1))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (_, _, c) in neg {
        paint_node(ctx, c, out);
    }
    for c in flow {
        paint_node(ctx, c, out);
    }
    for (_, _, c) in pos {
        paint_node(ctx, c, out);
    }
    if scrolled {
        out.ops.push(PaintOp::PopScroll);
    }
    if clip {
        out.ops.push(PaintOp::PopClip);
        // F3a（ADR-0023）：子树裁剪结束 → clip 链弹出。
        if let Some(hc) = ctx.hit {
            hc.borrow_mut().clips.pop();
        }
    }
    // F3c（ADR-0025）：clip-path 裁剪层结束（对应开头 PushClip/PushClipPath；
    // 恒 PopClip——sink 端单一裁剪栈弹出最近 push 的矩形/多边形）。
    if !matches!(clip_emit, ClipEmit::None) {
        out.ops.push(PaintOp::PopClip);
        if let Some(hc) = ctx.hit {
            hc.borrow_mut().clips.pop();
        }
    }
    if filtered {
        out.ops.push(PaintOp::PopFilter);
    }
    if faded {
        out.ops.push(PaintOp::PopOpacity);
    }
    if blend_isolated {
        out.ops.push(PaintOp::PopBlend);
    }
    if transformed {
        out.ops.push(PaintOp::PopTransform);
        // P4 D4：活跃仿射恢复（子树走查毕；复合的逆 = 逐因子逆乘回）。
        if let Some(hc) = ctx.hit {
            let inv = node_affine.and_then(|a| invert_affine(&a));
            if let Some(inv) = inv {
                let mut h = hc.borrow_mut();
                h.mat = mul_affine(&h.mat, &inv);
            }
        }
    }
}

/// 2D affine composition [a, b, c, d, e, f] (column-vector convention:
/// x' = a·x + c·y + e). Returns m∘n (n applied first, then m): the block product
/// M = M·N. Phase 3 ⑥: reused by scroll-range settling.
pub(crate) fn mul_affine(m: &[f32; 6], n: &[f32; 6]) -> [f32; 6] {
    [
        m[0] * n[0] + m[2] * n[1],
        m[1] * n[0] + m[3] * n[1],
        m[0] * n[2] + m[2] * n[3],
        m[1] * n[2] + m[3] * n[3],
        m[0] * n[4] + m[2] * n[5] + m[4],
        m[1] * n[4] + m[3] * n[5] + m[5],
    ]
}

/// Affine inverse (P4 D4, ADR-0037): det = a·d − b·c; singular (non-invertible)
/// → None. Same semantics as `Mat::invert` on the soft side (core hit-test
/// replication).
pub(crate) fn invert_affine(m: &[f32; 6]) -> Option<[f32; 6]> {
    let det = m[0] * m[3] - m[1] * m[2];
    if det.abs() < 1e-12 {
        return None;
    }
    let ia = m[3] / det;
    let ib = -m[1] / det;
    let ic = -m[2] / det;
    let id = m[0] / det;
    let ie = -(ia * m[4] + ic * m[5]);
    let if_ = -(ib * m[4] + id * m[5]);
    Some([ia, ib, ic, id, ie, if_])
}

/// Affine application ([a, b, c, d, e, f], x' = a·x + c·y + e).
pub(crate) fn apply_affine(m: &[f32; 6], x: f32, y: f32) -> (f32, f32) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

/// Rounded-rect point containment (P4 D4; same formula as soft `src_inside`):
/// half-open edges plus per-corner elliptical quadrant tests after normalization
/// (radius order tl_h, tl_v, tr_h, tr_v, br_h, br_v, bl_h, bl_v).
fn rounded_rect_contains(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: &[f32; 8]) -> bool {
    if !(px >= x && px < x + w && py >= y && py < y + h) {
        return false;
    }
    let corners = [
        (
            r[0],
            r[1],
            x + r[0],
            y + r[1],
            px < x + r[0] && py < y + r[1],
        ), // tl
        (
            r[2],
            r[3],
            x + w - r[2],
            y + r[3],
            px >= x + w - r[2] && py < y + r[3],
        ), // tr
        (
            r[4],
            r[5],
            x + w - r[4],
            y + h - r[5],
            px >= x + w - r[4] && py >= y + h - r[5],
        ), // br
        (
            r[6],
            r[7],
            x + r[6],
            y + h - r[7],
            px < x + r[6] && py >= y + h - r[7],
        ), // bl
    ];
    for (rx, ry, cx, cy, in_square) in corners {
        if in_square && rx > 0.0 && ry > 0.0 {
            let nx = (px - cx) / rx;
            let ny = (py - cy) / ry;
            return nx * nx + ny * ny <= 1.0;
        }
    }
    true
}

/// Polyline polygon point containment (P4 D4; same formula as soft
/// `poly_inside`): nonzero = winding number ≠ 0, evenodd = ray-crossing parity;
/// half-open edge rule yi ≤ py < yj counts only upward crossings.
fn poly_contains(px: f32, py: f32, pts: &[[f32; 2]], nonzero: bool) -> bool {
    let n = pts.len();
    if n < 3 {
        return false;
    }
    let mut winding = 0i32;
    let mut crossed = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = (pts[i][0], pts[i][1]);
        let (xj, yj) = (pts[j][0], pts[j][1]);
        let side = (xj - xi) * (py - yi) - (px - xi) * (yj - yi);
        if yi <= py {
            if yj > py && side > 0.0 {
                winding += 1;
                crossed = !crossed;
            }
        } else if yj <= py && side < 0.0 {
            winding -= 1;
            crossed = !crossed;
        }
        j = i;
    }
    if nonzero { winding != 0 } else { crossed }
}

/// Hit-clip containment test (P4 D4): the query point (viewport coordinates) is
/// mapped to the local frame through the inverse of the affine active when the
/// clip was registered, then tested against the rectangle (with rounded corners)
/// or the polyline (nonzero/evenodd).
pub(crate) fn hit_clip_contains(c: &HitClip, x: f32, y: f32) -> bool {
    let (lx, ly) = match c {
        HitClip::Rect { inv, .. } => apply_affine(inv, x, y),
        HitClip::Path { inv, .. } => apply_affine(inv, x, y),
    };
    match c {
        HitClip::Rect { rect, radius, .. } => {
            rounded_rect_contains(lx, ly, rect[0], rect[1], rect[2], rect[3], radius)
        }
        HitClip::Path {
            points, nonzero, ..
        } => poly_contains(lx, ly, points, *nonzero),
    }
}

/// Paint-time affine resolution (ADR-0009): the function list is multiplied
/// together in written order (rightmost applied first); translate percentages are
/// based on the element's own border-box width/height; the result is finally
/// wrapped in the transform-origin (default 50% 50%): A = T(o)·M·T(−o). rotate is
/// clockwise (y-down screen coordinates). Op coordinates are viewport-based (box
/// top-left at (x, y)), origin = (x, y) + the percentage-based point inside the
/// box — phase 2 ⑦ fix: the old implementation omitted the box offset, so
/// transformed boxes rotated around the wrong center / shifted wholesale.
/// Phase 3 ⑥: scroll-range settling reuses the same resolution (pub(crate)).
pub(crate) fn resolve_transform_affine(
    style: &ComputedStyle,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    env: &MediaEnv,
) -> [f32; 6] {
    let ctx = crate::css::value::ResolveCtx {
        em: style.font_size_px(),
        rem: env.rem,
        viewport_w: env.viewport_w,
        viewport_h: env.viewport_h,
        ..crate::css::value::ResolveCtx::base(
            style.font_size_px(),
            16.0,
            env.viewport_w,
            env.viewport_h,
        )
    };
    let mut m: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    for f in style.transform() {
        let t = match f {
            TransformFn::Translate(tx, ty) => [
                1.0,
                0.0,
                0.0,
                1.0,
                tx.resolve(&ctx, w).unwrap_or(0.0),
                ty.resolve(&ctx, h).unwrap_or(0.0),
            ],
            TransformFn::Scale(sx, sy) => [*sx, 0.0, 0.0, *sy, 0.0, 0.0],
            TransformFn::Rotate(deg) => {
                let (s, c) = deg.to_radians().sin_cos();
                [c, s, -s, c, 0.0, 0.0]
            }
            TransformFn::Skew(ax, ay) => [
                1.0,
                ay.to_radians().tan(),
                ax.to_radians().tan(),
                1.0,
                0.0,
                0.0,
            ],
            TransformFn::Matrix(a, b, c, d, e, f) => [*a, *b, *c, *d, *e, *f],
        };
        m = mul_affine(&m, &t);
    }
    // transform-origin（第五批⑬）：解析入库并消费于此——A = T(o)·M·T(−o)；
    // 百分比基 = 自身 border-box（与 CSS 一致）；缺席回退 50% 50%
    let (ox, oy) = match style.get(PropertyId::TransformOrigin) {
        Some(DeclValue::TransformOrigin(rx, ry)) => (
            x + rx.resolve(&ctx, w).unwrap_or(w * 0.5),
            y + ry.resolve(&ctx, h).unwrap_or(h * 0.5),
        ),
        _ => (x + w * 0.5, y + h * 0.5),
    };
    let pre = [1.0, 0.0, 0.0, 1.0, ox, oy];
    let post = [1.0, 0.0, 0.0, 1.0, -ox, -oy];
    mul_affine(&mul_affine(&pre, &m), &post)
}

/// Resolves currentColor / light-dark to an absolute sRGBA.
pub(crate) fn resolve_color(
    cv: &ColorValue,
    style: &ComputedStyle,
    env: &MediaEnv,
) -> AlphaColor<Srgb> {
    match (*cv).pick_scheme(env.dark) {
        ColorValue::Absolute(c) => c,
        ColorValue::CurrentColor => match style.color() {
            ColorValue::Absolute(c) => c,
            _ => AlphaColor::new([0.0, 0.0, 0.0, 1.0]),
        },
        ColorValue::LightDark(..) => AlphaColor::new([0.0, 0.0, 0.0, 1.0]), // pick_scheme 已消化
    }
}

/// F3b (ADR-0024): border/padding inset in px on all four sides.
struct BoxInsets {
    /// Left.
    l: f32,
    /// Right.
    r: f32,
    /// Top.
    t: f32,
    /// Bottom.
    b: f32,
}

/// Resolves border and padding widths in px on all four sides (missing
/// declaration = 0).
fn resolve_insets(style: &ComputedStyle, env: &MediaEnv) -> (BoxInsets, BoxInsets) {
    let side =
        |id: PropertyId| -> f32 { style.len(id).map(|lp| px(lp, style, env)).unwrap_or(0.0) };
    (
        BoxInsets {
            l: side(PropertyId::BorderLeftWidth),
            r: side(PropertyId::BorderRightWidth),
            t: side(PropertyId::BorderTopWidth),
            b: side(PropertyId::BorderBottomWidth),
        },
        BoxInsets {
            l: side(PropertyId::PaddingLeft),
            r: side(PropertyId::PaddingRight),
            t: side(PropertyId::PaddingTop),
            b: side(PropertyId::PaddingBottom),
        },
    )
}

/// Background box keyword → viewport rectangle (F3b ADR-0024: origin = positioning
/// area / clip = painting area).
fn background_box_rect(
    kind: BackgroundBox,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    insets: &(BoxInsets, BoxInsets),
) -> (f32, f32, f32, f32) {
    let (bd, pd) = insets;
    match kind {
        BackgroundBox::BorderBox => (x, y, w, h),
        BackgroundBox::PaddingBox => (
            x + bd.l,
            y + bd.t,
            (w - bd.l - bd.r).max(0.0),
            (h - bd.t - bd.b).max(0.0),
        ),
        BackgroundBox::ContentBox => (
            x + bd.l + pd.l,
            y + bd.t + pd.t,
            (w - bd.l - bd.r - pd.l - pd.r).max(0.0),
            (h - bd.t - bd.b - pd.t - pd.b).max(0.0),
        ),
    }
}

/// LP semantic component: Percent → fraction × semantic basis (positioning =
/// area−img; sizing = area); other units → px() (em/rem/vw resolved absolutely).
fn lp_semantic(lp: &LengthPercentage, basis: f32, style: &ComputedStyle, env: &MediaEnv) -> f32 {
    match lp {
        LengthPercentage::Percent(v) => v * basis,
        other => px(other, style, env),
    }
}

/// Background tiling axis semantics (P4 D2, ADR-0037: exact round/space,
/// replacing the retired repeat approximation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TileAxis {
    /// No tiling (single tile).
    None,
    /// Tile (window-aligned stepping, size unchanged).
    Repeat,
    /// Evenly spaced gaps (whole tiles in the positioning area + uniform gap;
    /// 0/1 tiles → single tile in the positioning area).
    Space,
    /// Whole-tile stretching (positioning area divided into n =
    /// round(len/size).max(1) segments; position is inoperative).
    Round,
}

/// Paint geometry for one background layer (F3b ADR-0024; P4 D2 axis semantics).
struct LayerGeom {
    /// First tile origin x.
    dx: f32,
    /// First tile origin y.
    dy: f32,
    /// Paint width.
    dw: f32,
    /// Paint height.
    dh: f32,
    /// Horizontal axis tiling semantics.
    axis_x: TileAxis,
    /// Vertical axis tiling semantics.
    axis_y: TileAxis,
}

/// Per-layer geometry resolution (css-backgrounds-3 §3.9 sizing / §3.10
/// positioning / §3.4 tiling; P4 D2: round/space axis semantics pass through
/// directly, tile positions are settled by tile_axis_positions).
#[allow(clippy::too_many_arguments)]
fn resolve_layer_geom(
    pos: &Position2D,
    size: &BgSize,
    repeat: &RepeatXY,
    area: (f32, f32, f32, f32),
    img_w: f32,
    img_h: f32,
    style: &ComputedStyle,
    env: &MediaEnv,
) -> Option<LayerGeom> {
    let (ax, ay, aw, ah) = area;
    if aw <= 0.0 || ah <= 0.0 {
        return None;
    }
    let (iw, ih) = if img_w > 0.0 && img_h > 0.0 {
        (img_w, img_h)
    } else {
        (aw, ah)
    };
    let (dw, dh) = match size {
        BgSize::Auto => (iw, ih),
        BgSize::Cover => {
            let s = (aw / iw).max(ah / ih);
            (iw * s, ih * s)
        }
        BgSize::Contain => {
            let s = (aw / iw).min(ah / ih);
            (iw * s, ih * s)
        }
        BgSize::Explicit { w: bw, h: bh } => {
            let wv = match bw {
                LPorAuto::LP(lp) => lp_semantic(lp, aw, style, env),
                LPorAuto::Auto => iw,
            };
            let hv = match bh {
                LPorAuto::LP(lp) => lp_semantic(lp, ah, style, env),
                LPorAuto::Auto => {
                    if matches!(bw, LPorAuto::LP(_)) && iw > 0.0 {
                        // 宽显式 + 高 auto：保持内在比例。
                        ih * (wv / iw)
                    } else {
                        ih
                    }
                }
            };
            (wv, hv)
        }
    };
    if dw <= 0.0 || dh <= 0.0 {
        return None;
    }
    // 定位：pos = pct(base)·(area−img) + len(base) + pct(off)·(area−img) +
    // len(off)（right/bottom 偏移已解析期取负归一）。
    let comp = |c: &PositionComp, space: f32| -> f32 {
        let base = lp_semantic(&c.base, space, style, env);
        let off = c
            .offset
            .as_ref()
            .map(|o| lp_semantic(o, space, style, env))
            .unwrap_or(0.0);
        base + off
    };
    let dx = ax + comp(&pos.x, aw - dw);
    let dy = ay + comp(&pos.y, ah - dh);
    Some(LayerGeom {
        dx,
        dy,
        dw,
        dh,
        axis_x: tile_axis(repeat.x),
        axis_y: tile_axis(repeat.y),
    })
}

/// RepeatAxis → TileAxis mapping (P4 D2).
fn tile_axis(r: RepeatAxis) -> TileAxis {
    match r {
        RepeatAxis::NoRepeat => TileAxis::None,
        RepeatAxis::Repeat => TileAxis::Repeat,
        RepeatAxis::Space => TileAxis::Space,
        RepeatAxis::Round => TileAxis::Round,
    }
}

/// Tile position enumeration (P4 D2, ADR-0037): returns a sequence of (tile
/// origin, tile size).
/// - None: single tile (position applies; origin/size passed through);
/// - Repeat: window-aligned stepping over [win, win+len) (size unchanged);
/// - Space: the positioning area [area, area+area_len) fits n = floor(area_len/
///   size) tiles with positive gaps spread evenly; n ≤ 1 (cannot fit two tiles)
///   → single tile placed by position (origin applies — css-backgrounds-3 §2.4);
///   otherwise the first tile anchors at the positioning-area start and the step
///   = size + gap (position is inoperative — css-backgrounds-3 §2.4);
/// - Round: the positioning area is divided into n = round(area_len/size).max(1)
///   tiles (ts = area_len/n), window-aligned stepping fills it (position is
///   inoperative); size ≤ 0 / area ≤ 0 defensively degrades to a single tile.
fn tile_axis_positions(
    axis: TileAxis,
    origin: f32,
    size: f32,
    win: f32,
    win_len: f32,
    area_origin: f32,
    area_len: f32,
) -> Vec<(f32, f32)> {
    if axis == TileAxis::None || size <= 0.0 {
        return vec![(origin, size)];
    }
    if axis == TileAxis::Space {
        if area_len <= 0.0 {
            return vec![(origin, size)];
        }
        let n = (area_len / size) as usize;
        if n <= 1 {
            // 容不下两片 → 单片按 background-position 定位（css-backgrounds-3
            // §2.4 space：「only one image is placed, and background-position
            // determines its position in this axis」）。
            return vec![(origin, size)];
        }
        let gap = (area_len - size * n as f32) / (n as f32 - 1.0);
        let step = size + gap;
        let end = area_origin + area_len;
        let mut out = Vec::with_capacity(n);
        let mut p = area_origin;
        while p < end && out.len() < n {
            out.push((p, size));
            p += step;
        }
        return out;
    }
    // Repeat / Round：窗口对齐步进；Round 先整数片拉伸（锚定定位区）。
    let (ts, anchor) = if axis == TileAxis::Round {
        if area_len <= 0.0 {
            return vec![(origin, size)];
        }
        let n = (area_len / size).round().max(1.0);
        (area_len / n, area_origin)
    } else {
        (size, origin)
    };
    if ts <= 0.0 || win_len <= 0.0 {
        return vec![(anchor, ts)];
    }
    let mut out = Vec::new();
    // Round 网格 = n 片恰好铺满定位区（不出界延伸）；Repeat 窗口对齐
    // 步进（边缘半片合法）。
    let (mut p, end) = if axis == TileAxis::Round {
        (anchor, area_origin + area_len)
    } else {
        (anchor - ((anchor - win) / ts).ceil() * ts, win + win_len)
    };
    while p < end {
        out.push((p, ts));
        p += ts;
    }
    out
}

/// Border emission (P4 D1, ADR-0037): dashed/dotted without rounded corners →
/// DisplayList-level segmentation (css-backgrounds-3 §7.1: dash segments are 2×
/// border-width long with 1× border-width gaps, the first segment aligns with
/// the line start, a final partial segment is not drawn; dotted circles have
/// diameter = border-width, center spacing 2× border-width, the first dot's
/// circle covers the line start); dashed/dotted with rounded corners → the whole
/// frame degrades to a single Solid op (arc dashed/dotted strokes are Tier B,
/// documented in FEATURES.md/SINK-MATRIX.md); pure Solid/None/Hidden → the
/// original Border op path. The outline channel shares this (the outset
/// rectangle plus Dashed/Dotted mapping benefit automatically).
pub(crate) fn emit_borders(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: [f32; 8],
    sides: [BorderSide; 4],
    out: &mut DisplayList,
) {
    let fancy = |s: &BorderSide| {
        s.width > 0.0 && matches!(s.style, BorderStyle::Dashed | BorderStyle::Dotted)
    };
    let visible = sides
        .iter()
        .any(|s| s.style != BorderStyle::None && s.width > 0.0);
    if !visible {
        return;
    }
    let any_fancy = sides.iter().any(fancy);
    if !any_fancy {
        out.ops.push(PaintOp::Border {
            x,
            y,
            width: w,
            height: h,
            radius,
            sides,
        });
        return;
    }
    let square = radius == [0.0; 8];
    if !square {
        // 弧上虚线段需要弧长参数化（B 级）：整框退 Solid 一次成带。
        let mut solid = sides;
        for s in &mut solid {
            if matches!(s.style, BorderStyle::Dashed | BorderStyle::Dotted) {
                s.style = BorderStyle::Solid;
            }
        }
        out.ops.push(PaintOp::Border {
            x,
            y,
            width: w,
            height: h,
            radius,
            sides: solid,
        });
        return;
    }
    // 直角框：四边带布局与 Border op 一致（top 全宽、right 纵带、
    // bottom 全宽、left 纵带），逐边拆段。
    emit_side_band(x, y, w, &sides[0], true, out);
    emit_side_band(x + w - sides[1].width, y, h, &sides[1], false, out);
    emit_side_band(x, y + h - sides[2].width, w, &sides[2], true, out);
    emit_side_band(x, y, h, &sides[3], false, out);
}

/// Single side-band painting (P4 D1): `horizontal` = length `len` along the x
/// axis, thickness = border width; Solid = whole band / Dashed = 2t-t segments /
/// Dotted = dots of diameter t with 2t center spacing.
fn emit_side_band(
    x0: f32,
    y0: f32,
    len: f32,
    side: &BorderSide,
    horizontal: bool,
    out: &mut DisplayList,
) {
    let t = side.width;
    if t <= 0.0 {
        return;
    }
    let rect = |off: f32, seg: f32, out: &mut DisplayList| {
        let (rx, ry, rw, rh) = if horizontal {
            (x0 + off, y0, seg, t)
        } else {
            (x0, y0 + off, t, seg)
        };
        out.ops.push(PaintOp::FillRect {
            x: rx,
            y: ry,
            width: rw,
            height: rh,
            radius: [0.0; 8],
            color: side.color,
        });
    };
    match side.style {
        BorderStyle::None => {}
        BorderStyle::Solid => rect(0.0, len, out),
        BorderStyle::Dashed => {
            // 段 2t、间隔 t（步进 3t）、首段对齐起点、末段不足不画。
            let (seg, step) = (2.0 * t, 3.0 * t);
            let mut p = 0.0;
            while p + seg <= len {
                rect(p, seg, out);
                p += step;
            }
        }
        BorderStyle::Dotted => {
            // 圆点直径 t、中心间距 2t（点间空隙 t）、首点圆覆盖起点。
            let (r, step) = (t / 2.0, 2.0 * t);
            let mut c = r;
            while c + r <= len {
                rect(c - r, t, out);
                c += step;
            }
        }
    }
}

/// clip-path emission shape (F3c, ADR-0025).
enum ClipEmit {
    /// No clipping (none / tolerant acceptance of Other).
    None,
    /// Rectangle clip (inset: reuses PushClip's exact per-corner radius
    /// capability).
    Rect {
        /// (x, y, w, h) px (after reference-box insetting; width/height clamped
        /// to 0).
        rect: (f32, f32, f32, f32),
        /// Per-corner (horizontal, vertical) radius px (same order as
        /// FillRect.radius; after the §5.5 reduction).
        radius: [f32; 8],
    },
    /// Polygon clip (circle/ellipse 64-segment polyline / polygon vertices
    /// passed through).
    Path {
        /// Vertices in viewport coordinates.
        points: Vec<[f32; 2]>,
        /// Fill rule (true = nonzero).
        nonzero: bool,
    },
}

/// Circle/ellipse polyline segment lower/upper bounds and target chord length
/// (P4 D6, ADR-0037: radius-adaptive seg = clamp(ceil(2π·r/3), 16, 256) — chord
/// ≤3px is visually smooth while keeping small circles from being over-tessellated).
const CLIP_POLY_SEGMENTS_MIN: usize = 16;
const CLIP_POLY_SEGMENTS_MAX: usize = 256;
const CLIP_POLY_CHORD_TARGET: f32 = 3.0;

/// Inscribed polyline vertices of a circle/ellipse (starting angle 0,
/// counterclockwise; css-shapes-1 §3). Segment count adapts to the semi-major
/// axis (D6); r→0 defensively degrades to a small segment count.
fn ellipse_points(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<[f32; 2]> {
    let r = rx.max(ry).max(0.5);
    let seg = ((std::f32::consts::TAU * r / CLIP_POLY_CHORD_TARGET).ceil() as usize)
        .clamp(CLIP_POLY_SEGMENTS_MIN, CLIP_POLY_SEGMENTS_MAX);
    (0..seg)
        .map(|i| {
            let a = (i as f32) * std::f32::consts::TAU / seg as f32;
            [cx + rx * a.cos(), cy + ry * a.sin()]
        })
        .collect()
}

/// clip-path shape → viewport-coordinate emission shape (css-shapes-1 §3 +
/// css-masking-1 §5, F3c, ADR-0025). Percentage semantics: inset margins are
/// based on reference-box width on the left/right and height on the top/bottom;
/// radius horizontal components use width, vertical components use height
/// (isomorphic to the css-backgrounds-3 §5.5 reduction); position follows
/// background-position point semantics (right/bottom normalized to negative
/// offsets at parse time); circle/ellipse corner keywords = Euclidean distance
/// from center to corner (circle) / per-axis distance (ellipse); polygon
/// vertices are x-based on width, y-based on height.
#[allow(clippy::too_many_arguments)]
fn clip_path_emit(
    shape: &ClipShape,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    insets: &(BoxInsets, BoxInsets),
    style: &ComputedStyle,
    env: &MediaEnv,
) -> ClipEmit {
    match shape {
        ClipShape::None | ClipShape::Other => ClipEmit::None,
        ClipShape::Inset {
            insets: ins,
            radius,
            reference,
        } => {
            let (rx, ry, rw, rh) = background_box_rect(*reference, x, y, w, h, insets);
            // 边距：[0]=上 [1]=右 [2]=下 [3]=左；左右 % 基准宽、上下 % 基准高
            let top = lp_semantic(&ins[0], rh, style, env);
            let right = lp_semantic(&ins[1], rw, style, env);
            let bottom = lp_semantic(&ins[2], rh, style, env);
            let left = lp_semantic(&ins[3], rw, style, env);
            let rect = (
                rx + left,
                ry + top,
                (rw - left - right).max(0.0),
                (rh - top - bottom).max(0.0),
            );
            let radius = match radius {
                Some((hs, vs)) => {
                    let h4: [f32; 4] =
                        std::array::from_fn(|i| lp_semantic(&hs[i], rect.2, style, env));
                    let v4: [f32; 4] =
                        std::array::from_fn(|i| lp_semantic(&vs[i], rect.3, style, env));
                    // css-backgrounds-3 §5.5 收束：f = min(边长/相邻半径和) ≤ 1
                    let mut f = 1.0f32;
                    for (sum, edge) in [
                        (h4[0] + h4[1], rect.2),
                        (v4[1] + v4[2], rect.3),
                        (h4[2] + h4[3], rect.2),
                        (v4[3] + v4[0], rect.3),
                    ] {
                        if sum > edge && sum > 0.0 {
                            f = f.min(edge / sum);
                        }
                    }
                    let mut out = [0.0f32; 8];
                    for i in 0..4 {
                        out[i * 2] = h4[i] * f;
                        out[i * 2 + 1] = v4[i] * f;
                    }
                    out
                }
                None => [0.0; 8],
            };
            ClipEmit::Rect { rect, radius }
        }
        ClipShape::Circle {
            radius,
            at,
            reference,
        } => {
            let (rx, ry, rw, rh) = background_box_rect(*reference, x, y, w, h, insets);
            let comp = |c: &PositionComp, space: f32| -> f32 {
                lp_semantic(&c.base, space, style, env)
                    + c.offset
                        .as_ref()
                        .map(|o| lp_semantic(o, space, style, env))
                        .unwrap_or(0.0)
            };
            let cx = rx + comp(&at.x, rw);
            let cy = ry + comp(&at.y, rh);
            // 圆心到四缘距离（side 关键字）与到四角欧氏距离（corner 关键字）
            let sides = [cx - rx, rx + rw - cx, cy - ry, ry + rh - cy];
            let corners = [
                (cx - rx).hypot(cy - ry),
                (rx + rw - cx).hypot(cy - ry),
                (rx + rw - cx).hypot(ry + rh - cy),
                (cx - rx).hypot(ry + rh - cy),
            ];
            let r = match radius {
                // css-shapes-1 §3.2.1：circle 百分比半径基准 = √(w²+h²)/√2
                ClipRadius::Length(lp) => {
                    lp_semantic(lp, rw.hypot(rh) / std::f32::consts::SQRT_2, style, env)
                }
                ClipRadius::ClosestSide => sides.iter().cloned().fold(f32::INFINITY, f32::min),
                ClipRadius::FarthestSide => sides.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
                ClipRadius::ClosestCorner => corners.iter().cloned().fold(f32::INFINITY, f32::min),
                ClipRadius::FarthestCorner => {
                    corners.iter().cloned().fold(f32::NEG_INFINITY, f32::max)
                }
            };
            if r <= 0.0 {
                // 零/负半径 = 零面积裁剪区（全隐藏）：3 重合点空路径
                return ClipEmit::Path {
                    points: vec![[cx, cy]; 3],
                    nonzero: true,
                };
            }
            ClipEmit::Path {
                points: ellipse_points(cx, cy, r, r),
                nonzero: true,
            }
        }
        ClipShape::Ellipse {
            rx: erx,
            ry: ery,
            at,
            reference,
        } => {
            let (rx, ry, rw, rh) = background_box_rect(*reference, x, y, w, h, insets);
            let comp = |c: &PositionComp, space: f32| -> f32 {
                lp_semantic(&c.base, space, style, env)
                    + c.offset
                        .as_ref()
                        .map(|o| lp_semantic(o, space, style, env))
                        .unwrap_or(0.0)
            };
            let cx = rx + comp(&at.x, rw);
            let cy = ry + comp(&at.y, rh);
            let sx = [cx - rx, rx + rw - cx];
            let sy = [cy - ry, ry + rh - cy];
            // 椭圆 corner：逐轴到角距离（css-shapes-1 §3.2.1）
            let cxs = [cx - rx, rx + rw - cx];
            let cys = [cy - ry, ry + rh - cy];
            let rrx = match erx {
                ClipRadius::Length(lp) => lp_semantic(lp, rw, style, env),
                ClipRadius::ClosestSide => sx.iter().cloned().fold(f32::INFINITY, f32::min),
                ClipRadius::FarthestSide => sx.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
                ClipRadius::ClosestCorner => cxs.iter().cloned().fold(f32::INFINITY, f32::min),
                ClipRadius::FarthestCorner => cxs.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
            };
            let rry = match ery {
                ClipRadius::Length(lp) => lp_semantic(lp, rh, style, env),
                ClipRadius::ClosestSide => sy.iter().cloned().fold(f32::INFINITY, f32::min),
                ClipRadius::FarthestSide => sy.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
                ClipRadius::ClosestCorner => cys.iter().cloned().fold(f32::INFINITY, f32::min),
                ClipRadius::FarthestCorner => cys.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
            };
            if rrx <= 0.0 || rry <= 0.0 {
                return ClipEmit::Path {
                    points: vec![[cx, cy]; 3],
                    nonzero: true,
                };
            }
            ClipEmit::Path {
                points: ellipse_points(cx, cy, rrx, rry),
                nonzero: true,
            }
        }
        ClipShape::Polygon {
            points,
            nonzero,
            reference,
        } => {
            let (rx, ry, rw, rh) = background_box_rect(*reference, x, y, w, h, insets);
            ClipEmit::Path {
                points: points
                    .iter()
                    .map(|(vx, vy)| {
                        [
                            rx + lp_semantic(vx, rw, style, env),
                            ry + lp_semantic(vy, rh, style, env),
                        ]
                    })
                    .collect(),
                nonzero: *nonzero,
            }
        }
    }
}

fn px(lp: &LengthPercentage, style: &ComputedStyle, env: &MediaEnv) -> f32 {
    lp.resolve(
        &ResolveCtx {
            em: style.font_size_px(),
            rem: env.rem,
            viewport_w: env.viewport_w,
            viewport_h: env.viewport_h,
            ..ResolveCtx::base(style.font_size_px(), 16.0, env.viewport_w, env.viewport_h)
        },
        0.0,
    )
    .unwrap_or(0.0)
}

fn resolve_radius(style: &ComputedStyle, env: &MediaEnv) -> [f32; 8] {
    // 第五批⑪椭圆圆角：每角 (横, 纵)——tl.x tl.y tr.x tr.y br.x br.y
    // bl.x bl.y；Len 旧值按圆形角处理
    let per: [[f32; 2]; 4] = [
        PropertyId::BorderTopLeftRadius,
        PropertyId::BorderTopRightRadius,
        PropertyId::BorderBottomRightRadius,
        PropertyId::BorderBottomLeftRadius,
    ]
    .map(|pid| match style.get(pid) {
        Some(DeclValue::Radius(h, v)) => [px(h, style, env), px(v, style, env)],
        Some(DeclValue::Len(lp)) => {
            let r = px(lp, style, env);
            [r, r]
        }
        _ => [0.0, 0.0],
    });
    let mut out = [0.0f32; 8];
    for (i, pair) in per.iter().enumerate() {
        out[i * 2] = pair[0];
        out[i * 2 + 1] = pair[1];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computed::compute_node;
    use crate::css::stylesheet::parse_stylesheet;
    use crate::tree::{StyleNode, StyleTree};

    fn setup(inline: &str, text: Option<&str>) -> (StyleTree, NodeId, ComputedStyle) {
        let mut tree = StyleTree::new();
        let root = tree.root();
        let child = tree.insert_child(
            root,
            StyleNode {
                name: Some("div".into()),
                text: text.map(String::from),
                declarations: crate::css::decl::parse_inline_declarations(inline).0,
                ..Default::default()
            },
        );
        let sheet = parse_stylesheet("");
        let style = compute_node(&tree, child, &sheet, &MediaEnv::default(), None);
        (tree, child, style)
    }

    fn run(
        tree: &StyleTree,
        id: NodeId,
        style: ComputedStyle,
        scroll: &HashMap<NodeId, (f32, f32)>,
    ) -> DisplayList {
        let mut styles = HashMap::new();
        styles.insert(id, style);
        let mut layout = HashMap::new();
        layout.insert(id, (10.0, 20.0, 100.0, 50.0));
        let mut out = DisplayList::default();
        let images: HashMap<String, ImageRes> = HashMap::new();
        let ctx = PaintCtx {
            tree,
            styles: &styles,
            layout: &layout,
            scroll,
            env: &MediaEnv::default(),
            spans: &HashMap::new(),
            wrap_widths: &HashMap::new(),
            text_overrides: &HashMap::new(),
            hit: None,
            images: &images,
            column_rules: &HashMap::new(),
            markers: &HashMap::new(),
        };
        build_display_list(&ctx, id, 1, &mut out);
        out
    }

    #[test]
    fn elliptical_radius_pairs_resolved() {
        // 第五批⑪：resolve_radius 产出每角 (横, 纵)——tl.x tl.y tr.x tr.y
        // br.x br.y bl.x bl.y（斜杠简写横/纵分组独立解析）
        let (_, _, cs) = setup("border-radius: 10px 20px / 5px 8px", None);
        let env = MediaEnv {
            viewport_w: 1280.0,
            viewport_h: 800.0,
            dark: false,
            reduced_motion: false,
            ..Default::default()
        };
        assert_eq!(
            resolve_radius(&cs, &env),
            [10.0, 5.0, 20.0, 8.0, 10.0, 5.0, 20.0, 8.0]
        );
    }

    #[test]
    fn radial_ellipse_geometry_calibrated() {
        // 第五批⑫校准：径向几何全组与 css-images-3 一致——ellipse
        // farthest-corner（默认）= fx·√2 / fy·√2（过最远角的规范唯一解）、
        // closest/farthest-side=边距本身、closest-corner=最小边距·√2；
        // circle 全组=对应距离标量（角=欧氏、边=min/max）。
        use crate::css::property::{RadialShape as RS, RadialSize as RZ, RadialSpec};
        let (_, _, cs) = setup("", None);
        let env = MediaEnv {
            viewport_w: 1280.0,
            viewport_h: 800.0,
            dark: false,
            reduced_motion: false,
            ..Default::default()
        };
        let geom = |shape: RS, size: RZ, px: f32, py: f32| {
            resolve_radial(
                &RadialSpec {
                    shape,
                    size,
                    position: (
                        LengthPercentage::Percent(px / 100.0),
                        LengthPercentage::Percent(py / 100.0),
                    ),
                },
                10.0,
                20.0,
                100.0,
                50.0,
                &cs,
                &env,
            )
        };
        let near = |a: f32, b: f32| (a - b).abs() < 0.01;
        // 居中：fx=50 fy=25（min=max）
        let g = geom(RS::Ellipse, RZ::FarthestCorner, 50.0, 50.0);
        assert!(
            near(g.rx, 50.0 * std::f32::consts::SQRT_2)
                && near(g.ry, 25.0 * std::f32::consts::SQRT_2)
        );
        let g = geom(RS::Circle, RZ::FarthestCorner, 50.0, 50.0);
        assert!(near(g.rx, (50.0f32 * 50.0 + 25.0 * 25.0).sqrt()) && near(g.ry, g.rx));
        // 偏心 10% 20%：fx=90 fy=40 min=(10,10)
        let g = geom(RS::Ellipse, RZ::FarthestCorner, 10.0, 20.0);
        assert!(
            near(g.rx, 90.0 * std::f32::consts::SQRT_2)
                && near(g.ry, 40.0 * std::f32::consts::SQRT_2)
        );
        let g = geom(RS::Ellipse, RZ::ClosestCorner, 10.0, 20.0);
        assert!(
            near(g.rx, 10.0 * std::f32::consts::SQRT_2)
                && near(g.ry, 10.0 * std::f32::consts::SQRT_2)
        );
        let g = geom(RS::Circle, RZ::ClosestCorner, 10.0, 20.0);
        assert!(near(g.rx, (200.0f32).sqrt()));
        let g = geom(RS::Ellipse, RZ::ClosestSide, 10.0, 20.0);
        assert!(near(g.rx, 10.0) && near(g.ry, 10.0));
        let g = geom(RS::Ellipse, RZ::FarthestSide, 10.0, 20.0);
        assert!(near(g.rx, 90.0) && near(g.ry, 40.0));
        let g = geom(RS::Circle, RZ::ClosestSide, 10.0, 20.0);
        assert!(near(g.rx, 10.0) && near(g.ry, 10.0));
        let g = geom(RS::Circle, RZ::FarthestSide, 10.0, 20.0);
        assert!(near(g.rx, 90.0) && near(g.ry, 90.0));
        // 绝对定位锚：cx/cy 加盒子原点 (10, 20)
        let g = geom(RS::Ellipse, RZ::ClosestSide, 10.0, 20.0);
        assert!(near(g.cx, 20.0) && near(g.cy, 30.0));
    }

    #[test]
    fn flex_grid_item_z_index_orders() {
        // 第五批㉑：flex/grid 子项的显式 z-index 无需 position——与定位
        // 元素同等参与三带排序（z=5 的 b 排到流带 a/c 之后）
        let mut tree = StyleTree::new();
        let root = tree.root();
        let mk = |extra: &str| StyleNode {
            name: Some("div".into()),
            declarations: crate::css::decl::parse_inline_declarations(&format!(
                "background-color: #000001; {extra}"
            ))
            .0,
            ..Default::default()
        };
        let a = tree.insert_child(root, mk(""));
        let b = tree.insert_child(root, mk("z-index: 5"));
        let c = tree.insert_child(root, mk(""));
        // 父容器 = flex（子项判定基于父 display）
        tree.node_mut(root).declarations =
            crate::css::decl::parse_inline_declarations("display: flex").0;
        let sheet = parse_stylesheet("");
        let env = MediaEnv::default();
        let mut styles = HashMap::new();
        for id in [root, a, b, c] {
            styles.insert(id, compute_node(&tree, id, &sheet, &env, None));
        }
        let mut layout = HashMap::new();
        layout.insert(root, (0.0, 0.0, 310.0, 100.0));
        layout.insert(a, (10.0, 0.0, 100.0, 100.0));
        layout.insert(b, (110.0, 0.0, 100.0, 100.0));
        layout.insert(c, (210.0, 0.0, 100.0, 100.0));
        let mut out = DisplayList::default();
        let images: HashMap<String, ImageRes> = HashMap::new();
        let ctx = PaintCtx {
            tree: &tree,
            styles: &styles,
            layout: &layout,
            scroll: &HashMap::new(),
            env: &env,
            spans: &HashMap::new(),
            wrap_widths: &HashMap::new(),
            text_overrides: &HashMap::new(),
            hit: None,
            images: &images,
            column_rules: &HashMap::new(),
            markers: &HashMap::new(),
        };
        build_display_list(&ctx, root, 1, &mut out);
        // 期望顺序：a(x=10) → c(x=210)（流带）→ b(x=110)（flex 子项 z=5 进
        // Pos 带最后）；无 ㉑ 修复时 b 按树序落在 a/c 之间
        let xs: Vec<f32> = out
            .ops
            .iter()
            .filter_map(|op| match op {
                PaintOp::FillRect { x, .. } => Some(*x),
                _ => None,
            })
            .collect();
        assert_eq!(xs, vec![10.0, 210.0, 110.0], "{xs:?}");
    }

    #[test]
    fn z_index_orders_siblings() {
        let mut tree = StyleTree::new();
        let root = tree.root();
        let mk = |z: &str| StyleNode {
            name: Some("div".into()),
            declarations: crate::css::decl::parse_inline_declarations(&format!(
                "background-color: #000001; position: relative; z-index: {z}"
            ))
            .0,
            ..Default::default()
        };
        let a = tree.insert_child(root, mk("2"));
        let b = tree.insert_child(root, mk("0"));
        let c = tree.insert_child(root, mk("1"));
        let sheet = parse_stylesheet("");
        let env = MediaEnv::default();
        let mut styles = HashMap::new();
        for id in [root, a, b, c] {
            styles.insert(id, compute_node(&tree, id, &sheet, &env, None));
        }
        let mut layout = HashMap::new();
        layout.insert(root, (0.0, 0.0, 310.0, 100.0));
        layout.insert(a, (10.0, 0.0, 100.0, 100.0));
        layout.insert(b, (110.0, 0.0, 100.0, 100.0));
        layout.insert(c, (210.0, 0.0, 100.0, 100.0));
        let mut out = DisplayList::default();
        let images: HashMap<String, ImageRes> = HashMap::new();
        let ctx = PaintCtx {
            tree: &tree,
            styles: &styles,
            layout: &layout,
            scroll: &HashMap::new(),
            env: &env,
            spans: &HashMap::new(),
            wrap_widths: &HashMap::new(),
            text_overrides: &HashMap::new(),
            hit: None,
            images: &images,
            column_rules: &HashMap::new(),
            markers: &HashMap::new(),
        };
        build_display_list(&ctx, root, 1, &mut out);
        // 期望顺序：b(z0,x=110) → c(z1,x=210) → a(z2,x=10)；根无背景不产生 FillRect
        let xs: Vec<f32> = out
            .ops
            .iter()
            .filter_map(|op| match op {
                PaintOp::FillRect { x, .. } => Some(*x),
                _ => None,
            })
            .collect();
        assert_eq!(xs, vec![110.0, 210.0, 10.0]);
    }

    #[test]
    fn appendix_e_band_order() {
        // ADR-0008 三带（CSS 2.1 Appendix E 简化）：
        // Neg(z-1) → Flow(树序) → Pos(auto 树序 → faded → 正 z 升序)。
        // DOM 序：n1 负 z、n2 flow、n3 定位 auto、n4 z2、n5 半透明。
        let mut tree = StyleTree::new();
        let root = tree.root();
        let mk = |inline: &str| StyleNode {
            name: Some("div".into()),
            declarations: crate::css::decl::parse_inline_declarations(inline).0,
            ..Default::default()
        };
        let c1 = tree.insert_child(
            root,
            mk("background-color: #000001; position: relative; z-index: -1"),
        );
        let c2 = tree.insert_child(root, mk("background-color: #000002"));
        let c3 = tree.insert_child(
            root,
            mk("background-color: #000003; position: relative; z-index: auto"),
        );
        let c4 = tree.insert_child(
            root,
            mk("background-color: #000004; position: relative; z-index: 2"),
        );
        let c5 = tree.insert_child(root, mk("background-color: #000005; opacity: 0.5"));
        let sheet = parse_stylesheet("");
        let env = MediaEnv::default();
        let mut styles = HashMap::new();
        for id in [root, c1, c2, c3, c4, c5] {
            styles.insert(id, compute_node(&tree, id, &sheet, &env, None));
        }
        let mut layout = HashMap::new();
        layout.insert(root, (0.0, 0.0, 510.0, 100.0));
        for (i, id) in [c1, c2, c3, c4, c5].iter().enumerate() {
            layout.insert(*id, (10.0 + i as f32 * 100.0, 0.0, 100.0, 100.0));
        }
        let mut out = DisplayList::default();
        let images: HashMap<String, ImageRes> = HashMap::new();
        let ctx = PaintCtx {
            tree: &tree,
            styles: &styles,
            layout: &layout,
            scroll: &HashMap::new(),
            env: &env,
            spans: &HashMap::new(),
            wrap_widths: &HashMap::new(),
            text_overrides: &HashMap::new(),
            hit: None,
            images: &images,
            column_rules: &HashMap::new(),
            markers: &HashMap::new(),
        };
        build_display_list(&ctx, root, 1, &mut out);
        // 期望：n1(Neg) → n2(Flow) → n3(auto) → n5(faded) → n4(z2)
        let ids: Vec<u8> = out
            .ops
            .iter()
            .filter_map(|op| match op {
                PaintOp::FillRect { color, .. } => Some((color.components[2] * 255.0) as u8),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec![1, 2, 3, 5, 4]);
    }

    #[test]
    fn opacity_layer_pairing() {
        // opacity < 1：PushOpacity/PopOpacity 包住整节点绘制；alpha clamp 到 [0,1]
        let (tree, id, style) = setup("background-color: #101010; opacity: 0.5", None);
        let out = run(&tree, id, style, &HashMap::new());
        assert!(matches!(
            out.ops.first(),
            Some(PaintOp::PushOpacity { alpha, .. }) if (alpha - 0.5).abs() < 1e-6
        ));
        assert!(matches!(out.ops.get(1), Some(PaintOp::FillRect { .. })));
        assert!(matches!(out.ops.last(), Some(PaintOp::PopOpacity)));
        // opacity: 2 → clamp 为 1 → 不包层
        let (tree2, id2, style2) = setup("background-color: #101010; opacity: 2", None);
        let out2 = run(&tree2, id2, style2, &HashMap::new());
        assert!(matches!(out2.ops.first(), Some(PaintOp::FillRect { .. })));
        assert!(!matches!(out2.ops.last(), Some(PaintOp::PopOpacity)));
    }

    #[test]
    fn filter_layer_triple_nesting_lifo() {
        // P2（ADR-0031 D3）：blend+opacity+filter 同节点 → 层序
        // transform→clip→blend→opacity→filter（合成序
        // opacity(filter(子树))，css-filters-1 §3），收尾严格 LIFO：
        // PopFilter→PopOpacity→PopBlend。
        let (tree, id, style) = setup(
            "background-color: #101010; opacity: 0.5; mix-blend-mode: multiply; filter: invert(1) blur(2px)",
            None,
        );
        let out = run(&tree, id, style, &HashMap::new());
        let kinds: Vec<&str> = out
            .ops
            .iter()
            .map(|op| match op {
                PaintOp::PushBlend { .. } => "push_blend",
                PaintOp::PushOpacity { .. } => "push_opacity",
                PaintOp::PushFilter { .. } => "push_filter",
                PaintOp::PopFilter => "pop_filter",
                PaintOp::PopOpacity => "pop_opacity",
                PaintOp::PopBlend => "pop_blend",
                _ => "content",
            })
            .collect();
        let idx = |k: &str| kinds.iter().position(|x| *x == k).expect(k);
        assert!(idx("push_blend") < idx("push_opacity"));
        assert!(idx("push_opacity") < idx("push_filter"));
        // filter 层对内含内容 fill
        assert!(idx("push_filter") < idx("content"));
        assert!(idx("content") < idx("pop_filter"));
        // 收尾 LIFO
        assert!(idx("pop_filter") < idx("pop_opacity"));
        assert!(idx("pop_opacity") < idx("pop_blend"));
        // 滤镜链解析序保留：invert(1) → Blur(2px)
        let (tree2, id2, style2) = setup("filter: invert(1) blur(2px)", None);
        let out2 = run(&tree2, id2, style2, &HashMap::new());
        let mut saw_invert = false;
        for op in &out2.ops {
            if let PaintOp::PushFilter { filters, .. } = op {
                assert_eq!(filters.len(), 2);
                assert_eq!(filters[0], FilterEffect::Invert(1.0));
                assert_eq!(filters[1], FilterEffect::Blur(2.0));
                saw_invert = true;
            }
        }
        assert!(saw_invert, "PushFilter 应携带解析序滤镜链");
        // filter: none（空链）→ 不发射层对
        let (tree3, id3, style3) = setup("background-color: #101010; filter: none", None);
        let out3 = run(&tree3, id3, style3, &HashMap::new());
        assert!(
            out3.ops
                .iter()
                .all(|op| !matches!(op, PaintOp::PushFilter { .. }))
        );
    }

    #[test]
    fn repeating_gradient_flag_flows_to_display_list() {
        // P1-3：repeating-* 前缀标记（css::Gradient.repeating）经背景 tile
        // resolved 副本流入 DisplayList——各 sink 据此取模平铺。
        let (tree, id, style) = setup(
            "background-image: repeating-linear-gradient(90deg, red 0px, blue 20px)",
            None,
        );
        let out = run(&tree, id, style, &HashMap::new());
        let g = out
            .ops
            .iter()
            .find_map(|op| match op {
                PaintOp::Gradient { gradient, .. } => Some(gradient),
                _ => None,
            })
            .expect("应有渐变 op");
        assert!(g.repeating, "repeating 标记应随 op 流入显示列表");
        assert_eq!(g.stops.len(), 2);
        // 同文法无前缀 → false
        let (tree, id, style) = setup(
            "background-image: linear-gradient(90deg, red 0px, blue 20px)",
            None,
        );
        let out = run(&tree, id, style, &HashMap::new());
        let g = out
            .ops
            .iter()
            .find_map(|op| match op {
                PaintOp::Gradient { gradient, .. } => Some(gradient),
                _ => None,
            })
            .expect("应有渐变 op");
        assert!(!g.repeating);
    }

    #[test]
    fn radial_gradient_geometry() {
        // 显式半径 + 关键字圆心：circle 20px at left top → (0,0,r=20)
        let (tree, id, style) = setup(
            "background-image: radial-gradient(circle 20px at left top, red, blue)",
            None,
        );
        let out = run(&tree, id, style, &HashMap::new());
        // F3b（ADR-0024）：多层发射 → 渐变层包 PushClip/PopClip，按 op 找
        let radial = out.ops.iter().find_map(|op| match op {
            PaintOp::Gradient {
                radial: Some(g), ..
            } => Some(*g),
            _ => None,
        });
        let g = radial.expect("radial geometry");
        // 盒原点 (10,20)（run 脚手架）+ 圆心 left/top (0,0) + r=20
        assert_eq!((g.cx, g.cy, g.rx, g.ry), (10.0, 20.0, 20.0, 20.0));
        // 默认（无前导段）：盒心 + ellipse farthest-corner → r = 50√2
        let (tree, id, style) = setup("background-image: radial-gradient(red, blue)", None);
        let out = run(&tree, id, style, &HashMap::new());
        let radial = out.ops.iter().find_map(|op| match op {
            PaintOp::Gradient {
                radial: Some(g), ..
            } => Some(*g),
            _ => None,
        });
        let g = radial.expect("radial geometry");
        // 盒 100×50 @ (10,20)：盒心 (60,45)；fx=50→rx=50√2，fy=25→ry=25√2
        let rx = 50.0 * std::f32::consts::SQRT_2;
        let ry = 25.0 * std::f32::consts::SQRT_2;
        assert_eq!((g.cx, g.cy), (60.0, 45.0));
        assert!((g.rx - rx).abs() < 0.01 && (g.ry - ry).abs() < 0.01);
        // 偏心 circle farthest-corner（第四批⑤锁公式）：at 25% 25% →
        // 圆心 (35, 32.5)；fx = max(25, 75) = 75、fy = max(12.5, 37.5) = 37.5，
        // r = √(75² + 37.5²) ≈ 83.8526（最远角 = 右下）
        let (tree, id, style) = setup(
            "background-image: radial-gradient(circle farthest-corner at 25% 25%, red, blue)",
            None,
        );
        let out = run(&tree, id, style, &HashMap::new());
        let radial = out.ops.iter().find_map(|op| match op {
            PaintOp::Gradient {
                radial: Some(g), ..
            } => Some(*g),
            _ => None,
        });
        let g = radial.expect("radial geometry");
        let r = (75.0f32 * 75.0 + 37.5 * 37.5).sqrt();
        assert_eq!((g.cx, g.cy), (35.0, 32.5));
        assert!((g.rx - r).abs() < 0.01 && (g.ry - r).abs() < 0.01);
    }

    #[test]
    fn background_then_border() {
        let (tree, id, style) = setup(
            "background-color: red; border-style: solid; border-top-width: 2px; border-top-color: #0000ff",
            None,
        );
        let out = run(&tree, id, style, &HashMap::new());
        assert_eq!(out.ops.len(), 2);
        match &out.ops[0] {
            PaintOp::FillRect {
                x,
                y,
                width,
                height,
                color,
                ..
            } => {
                assert_eq!((*x, *y, *width, *height), (10.0, 20.0, 100.0, 50.0));
                assert_eq!(color.components, [1.0, 0.0, 0.0, 1.0]);
            }
            other => panic!("{other:?}"),
        }
        match &out.ops[1] {
            PaintOp::Border { sides, .. } => {
                let s = &sides[0]; // top
                assert_eq!(s.width, 2.0);
                assert_eq!(s.style, BorderStyle::Solid);
                assert_eq!(s.color.components, [0.0, 0.0, 1.0, 1.0]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn transparent_bg_only_text() {
        let (tree, id, style) = setup("", Some("hi"));
        let out = run(&tree, id, style, &HashMap::new());
        assert_eq!(out.ops.len(), 1);
        match &out.ops[0] {
            PaintOp::Text {
                text,
                font_size,
                color,
                ..
            } => {
                assert_eq!(text, "hi");
                assert_eq!(*font_size, 16.0);
                assert_eq!(color.components, [0.0, 0.0, 0.0, 1.0]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn clip_and_scroll_pairing() {
        let (tree, id, style) = setup("overflow: hidden", None);
        let mut scroll = HashMap::new();
        scroll.insert(id, (0.0, 10.0));
        let out = run(&tree, id, style, &scroll);
        assert_eq!(out.ops.len(), 4);
        assert!(matches!(out.ops[0], PaintOp::PushClip { .. }));
        assert!(matches!(out.ops[1], PaintOp::PushScroll { dy: 10.0, .. }));
        assert!(matches!(out.ops[2], PaintOp::PopScroll));
        assert!(matches!(out.ops[3], PaintOp::PopClip));
    }

    #[test]
    fn currentcolor_border_resolves() {
        let (tree, id, style) = setup(
            "color: #00ff00; border-style: solid; border-top-width: 1px",
            None,
        );
        let out = run(&tree, id, style, &HashMap::new());
        assert_eq!(out.ops.len(), 1); // 透明背景跳过
        match &out.ops[0] {
            PaintOp::Border { sides, .. } => {
                assert_eq!(sides[0].color.components, [0.0, 1.0, 0.0, 1.0])
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn shadow_op() {
        let (tree, id, style) = setup("box-shadow: 0px 4px 8px rgba(0, 0, 0, 0.5)", None);
        let out = run(&tree, id, style, &HashMap::new());
        assert_eq!(out.ops.len(), 1);
        match &out.ops[0] {
            PaintOp::Shadow {
                offset_x,
                offset_y,
                blur,
                ..
            } => {
                assert_eq!((*offset_x, *offset_y, *blur), (0.0, 4.0, 8.0));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn shadow_inset_and_spread_op() {
        // 第五批⑩：inset 标志与 spread 进入 Shadow op
        let (tree, id, style) = setup("box-shadow: inset 0px 4px 8px 2px rgba(0, 0, 0, 0.5)", None);
        let out = run(&tree, id, style, &HashMap::new());
        match &out.ops[0] {
            PaintOp::Shadow {
                blur,
                spread,
                inset,
                ..
            } => {
                assert_eq!((*blur, *spread, *inset), (8.0, 2.0, true));
            }
            other => panic!("{other:?}"),
        }
        let (tree, id, style) = setup("box-shadow: 0px 4px 8px 2px rgba(0, 0, 0, 0.5)", None);
        let out = run(&tree, id, style, &HashMap::new());
        match &out.ops[0] {
            PaintOp::Shadow { spread, inset, .. } => {
                assert_eq!((*spread, *inset), (2.0, false));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn background_image_op() {
        // 第五批⑨：url() → 注册表解析 → Image op（源尺寸/像素自足）；
        // 未注册引用 → 告警跳过（无 op）
        let (tree, id, style) = setup("background-image: url(res://hero)", None);
        let mut images: HashMap<String, ImageRes> = HashMap::new();
        images.insert(
            "res://hero".to_string(),
            ImageRes {
                width: 2,
                height: 2,
                rgba: std::sync::Arc::new(vec![255u8; 16]),
            },
        );
        let mut styles = HashMap::new();
        styles.insert(id, style);
        let mut layout = HashMap::new();
        layout.insert(id, (10.0, 20.0, 100.0, 50.0));
        let mut out = DisplayList::default();
        let ctx = PaintCtx {
            tree: &tree,
            styles: &styles,
            layout: &layout,
            scroll: &HashMap::new(),
            env: &MediaEnv::default(),
            spans: &HashMap::new(),
            wrap_widths: &HashMap::new(),
            text_overrides: &HashMap::new(),
            hit: None,
            images: &images,
            column_rules: &HashMap::new(),
            markers: &HashMap::new(),
        };
        build_display_list(&ctx, id, 1, &mut out);
        // F3b（ADR-0024）：多层发射 → 图层包 PushClip/PopClip，按 op 找
        let img = out.ops.iter().find_map(|op| match op {
            PaintOp::Image {
                x,
                y,
                width,
                height,
                source_w,
                source_h,
                pixels,
                ..
            } => Some((*x, *y, *width, *height, *source_w, *source_h, pixels.width)),
            _ => None,
        });
        let (x, y, width, height, source_w, source_h, pixels_width) = img.expect("image op");
        // F3b（ADR-0024）：size:auto → 固有尺寸 2×2 + repeat 平铺（旧单层
        // 实现拉伸至盒尺寸，行为精化为规范语义）。
        assert_eq!((x, y, width, height), (10.0, 20.0, 2.0, 2.0));
        assert_eq!((source_w, source_h), (2, 2));
        assert_eq!(pixels_width, 2);
        // repeat 平铺覆盖定位区：100/2 × 50/2 = 1250 块
        let tiles = out
            .ops
            .iter()
            .filter(|op| matches!(op, PaintOp::Image { .. }))
            .count();
        assert_eq!(tiles, 1250);
        // 未注册引用 → 无 Image op（背景纯色臂接管→透明；多层壳余 PushClip/PopClip）
        let (tree, id, style) = setup("background-image: url(res://missing)", None);
        let out = run(&tree, id, style, &HashMap::new());
        assert!(
            !out.ops.iter().any(|op| matches!(op, PaintOp::Image { .. })),
            "{:?}",
            out.ops
        );
    }

    #[test]
    fn background_layers_paint_first_layer_on_top() {
        // F3b（ADR-0024）：多层背景首层最上。发射序=反层序（末层先发），
        // 每层 PushClip/PopClip 包裹；色底 FillRect 恒发于最底。
        let (tree, id, style) = setup(
            "background-image: linear-gradient(to bottom, red, blue), \
             radial-gradient(circle, red, blue); background-color: #101010",
            None,
        );
        let out = run(&tree, id, style, &HashMap::new());
        assert!(
            matches!(&out.ops[0], PaintOp::FillRect { .. }),
            "{:?}",
            out.ops
        );
        // 末层（radial）先发
        assert!(matches!(&out.ops[1], PaintOp::PushClip { .. }));
        match &out.ops[2] {
            PaintOp::Gradient {
                gradient, radial, ..
            } => {
                assert!(matches!(
                    gradient.kind,
                    crate::css::property::GradientKind::Radial(_)
                ));
                assert!(radial.is_some());
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(&out.ops[3], PaintOp::PopClip));
        // 首层（linear）后发 = 最上
        assert!(matches!(&out.ops[4], PaintOp::PushClip { .. }));
        match &out.ops[5] {
            PaintOp::Gradient { gradient, .. } => {
                assert!(matches!(
                    gradient.kind,
                    crate::css::property::GradientKind::Linear(_)
                ))
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(&out.ops[6], PaintOp::PopClip));
        assert_eq!(out.ops.len(), 7, "{:?}", out.ops);
    }

    #[test]
    fn column_rule_strips_paint_relative_to_container_origin() {
        // 三期⑤c：settle_column_rules 条带（相对容器 border-box 原点）→
        // FillRect 视口系 = 容器原点 + 条带偏移；radius 全零、纯色直传。
        let (tree, id, style) = setup("column-rule: 4px solid red", None);
        let mut styles = HashMap::new();
        styles.insert(id, style);
        let mut layout = HashMap::new();
        layout.insert(id, (10.0, 20.0, 400.0, 90.0));
        let mut rules: HashMap<NodeId, Vec<ColumnRuleSeg>> = HashMap::new();
        rules.insert(
            id,
            vec![ColumnRuleSeg {
                x: 198.0,
                y: 0.0,
                width: 4.0,
                height: 90.0,
                color: AlphaColor::new([1.0, 0.0, 0.0, 1.0]),
            }],
        );
        let mut out = DisplayList::default();
        let ctx = PaintCtx {
            tree: &tree,
            styles: &styles,
            layout: &layout,
            scroll: &HashMap::new(),
            env: &MediaEnv::default(),
            spans: &HashMap::new(),
            wrap_widths: &HashMap::new(),
            text_overrides: &HashMap::new(),
            hit: None,
            images: &HashMap::new(),
            column_rules: &rules,
            markers: &HashMap::new(),
        };
        build_display_list(&ctx, id, 1, &mut out);
        match &out.ops[0] {
            PaintOp::FillRect {
                x,
                y,
                width,
                height,
                radius,
                color,
            } => {
                assert_eq!((*x, *y, *width, *height), (208.0, 20.0, 4.0, 90.0));
                assert_eq!(*radius, [0.0; 8]);
                assert_eq!(color.components, [1.0, 0.0, 0.0, 1.0]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn gradient_op() {
        let (tree, id, style) = setup(
            "background-image: linear-gradient(to bottom, red, blue)",
            None,
        );
        let out = run(&tree, id, style, &HashMap::new());
        // F3b（ADR-0024）：多层发射 → 渐变层包 PushClip/PopClip
        assert_eq!(out.ops.len(), 3, "{:?}", out.ops);
        assert!(matches!(&out.ops[0], PaintOp::PushClip { .. }));
        assert!(matches!(&out.ops[2], PaintOp::PopClip));
        match &out.ops[1] {
            PaintOp::Gradient { gradient, .. } => assert_eq!(gradient.stops.len(), 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn transform_origin_includes_box_offset() {
        // 二期⑦回归：run 默认盒 (10,20,100,50)——origin=盒中心 (60,45)，
        // rotate(180) → e=2·60=120、f=2·45=90（旧实现漏加盒偏移得 (w,h)=(100,50)，
        // offset 盒绕错中心旋转、子树整体错位/消失，由 transform-pixel 用例暴露）。
        let (tree, id, style) = setup("transform: rotate(180deg)", None);
        let out = run(&tree, id, style, &HashMap::new());
        assert_eq!(out.ops.len(), 2, "{:?}", out.ops);
        match &out.ops[0] {
            PaintOp::PushTransform { affine } => {
                let want = [-1.0, 0.0, 0.0, -1.0, 120.0, 90.0];
                for (g, e) in affine.iter().zip(want.iter()) {
                    assert!((g - e).abs() < 1e-4, "affine {affine:?} want {want:?}");
                }
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn clip_path_inset_emits_clip_rect() {
        // F3c（ADR-0025）：inset → PushClip 矩形（参考盒内缩；§5.5 收束后
        // 圆角），包住自身背景与子树；末尾 PopClip 对偶
        let (tree, id, style) = setup("clip-path: inset(5px); background-color: red", None);
        let out = run(&tree, id, style, &HashMap::new());
        assert_eq!(out.ops.len(), 3, "{:?}", out.ops);
        match &out.ops[0] {
            PaintOp::PushClip {
                x,
                y,
                width,
                height,
                radius,
            } => {
                assert_eq!((*x, *y, *width, *height), (15.0, 25.0, 90.0, 40.0));
                assert_eq!(*radius, [0.0; 8]);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(&out.ops[1], PaintOp::FillRect { .. }));
        assert!(matches!(&out.ops[2], PaintOp::PopClip));
    }

    #[test]
    fn clip_path_circle_segments_adaptive() {
        // F3c + P4 D6（ADR-0037）：circle → PushClipPath 折线（起点 0°、
        // 逆时针、首点 = 右极点）；段数随长半轴自适应
        // clamp(ceil(2πr/3), 16, 256)——r=20→42、r=2→16（下限）、
        // r=200→256（上限）。
        let (tree, id, style) = setup("clip-path: circle(20px at 50% 50%)", None);
        let out = run(&tree, id, style, &HashMap::new());
        assert_eq!(out.ops.len(), 2, "{:?}", out.ops);
        match &out.ops[0] {
            PaintOp::PushClipPath { points, nonzero } => {
                assert_eq!(points.len(), 42, "r=20 → ceil(2π·20/3)=42");
                assert!(*nonzero, "圆 = nonzero");
                // 盒 (10,20,100,50) → 心 (60,45)；首点 = 右极点 (80,45)
                assert!(
                    (points[0][0] - 80.0).abs() < 1e-4 && (points[0][1] - 45.0).abs() < 1e-4,
                    "首点 {:?}",
                    points[0]
                );
                for p in points {
                    let d = ((p[0] - 60.0).powi(2) + (p[1] - 45.0).powi(2)).sqrt();
                    assert!((d - 20.0).abs() < 1e-2, "段点 {p:?} 半径偏差 {d}");
                }
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(&out.ops[1], PaintOp::PopClip));
        // 段数下限：小圆 r=2 → ceil(2π·2/3)=5 → clamp 16。
        let (tree, id, style) = setup("clip-path: circle(2px at 50% 50%)", None);
        let out = run(&tree, id, style, &HashMap::new());
        match &out.ops[0] {
            PaintOp::PushClipPath { points, .. } => {
                assert_eq!(points.len(), 16, "小圆触及段数下限");
            }
            other => panic!("{other:?}"),
        }
        // 段数上限：大圆 r=200 → ceil(2π·200/3)=419 → clamp 256。
        let (tree, id, style) = setup("clip-path: circle(200px at 50% 50%)", None);
        let out = run(&tree, id, style, &HashMap::new());
        match &out.ops[0] {
            PaintOp::PushClipPath { points, .. } => {
                assert_eq!(points.len(), 256, "大圆触及段数上限");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn clip_path_polygon_points_and_fill_rules() {
        // F3c：polygon 顶点直传（% 基准参考盒）+ fill-rule 语义位随 op 传递
        let (tree, id, style) = setup("clip-path: polygon(0% 0%, 100% 0%, 50% 100%)", None);
        let out = run(&tree, id, style, &HashMap::new());
        match &out.ops[0] {
            PaintOp::PushClipPath { points, nonzero } => {
                assert_eq!(points.len(), 3);
                assert!(*nonzero);
                assert_eq!(points[0], [10.0, 20.0]);
                assert_eq!(points[1], [110.0, 20.0]);
                assert_eq!(points[2], [60.0, 70.0]);
            }
            other => panic!("{other:?}"),
        }
        let (tree, id, style) = setup(
            "clip-path: polygon(evenodd, 0% 0%, 100% 0%, 50% 100%)",
            None,
        );
        let out = run(&tree, id, style, &HashMap::new());
        match &out.ops[0] {
            PaintOp::PushClipPath { nonzero, .. } => assert!(!nonzero, "evenodd 语义位"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn clip_path_reference_box_offset() {
        // F3c：geometry-box 偏移——content-box 参考系 = border-box 平移
        // padding（盒 (10,20,100,50) + padding 10 → 内容盒 (20,30,80,30)）
        let (tree, id, style) = setup(
            "padding: 10px; clip-path: circle(5px at 0% 0%) content-box",
            None,
        );
        let out = run(&tree, id, style, &HashMap::new());
        match &out.ops[0] {
            PaintOp::PushClipPath { points, .. } => {
                assert!((points[0][0] - 25.0).abs() < 1e-4, "首点 {:?}", points[0]);
                assert!((points[0][1] - 30.0).abs() < 1e-4);
            }
            other => panic!("{other:?}"),
        }
        let (tree, id, style) = setup(
            "padding: 10px; clip-path: circle(5px at 0% 0%) border-box",
            None,
        );
        let out = run(&tree, id, style, &HashMap::new());
        match &out.ops[0] {
            PaintOp::PushClipPath { points, .. } => {
                assert!((points[0][0] - 15.0).abs() < 1e-4, "首点 {:?}", points[0]);
                assert!((points[0][1] - 20.0).abs() < 1e-4);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn clip_path_url_and_none_emit_nothing() {
        // F3c：Other（url/path 收容）与 none 均不产生裁剪 op
        for css in [
            "clip-path: url(#c)",
            "clip-path: none",
            "clip-path: path(\"M0 0 L1 1 Z\")",
        ] {
            let (tree, id, style) = setup(css, None);
            let out = run(&tree, id, style, &HashMap::new());
            assert!(
                out.ops.iter().all(|op| !matches!(
                    op,
                    PaintOp::PushClip { .. } | PaintOp::PushClipPath { .. } | PaintOp::PopClip
                )),
                "{css} 不应产生裁剪 op: {:?}",
                out.ops
            );
        }
    }

    #[test]
    fn clip_path_composes_with_overflow_order() {
        // F3c：clip-path 层在 overflow 裁剪之外（LIFO：overflow 后 push 在
        // 内层）；收尾双 PopClip 依序弹出
        let (tree, id, style) = setup("clip-path: circle(10px); overflow: hidden", None);
        let out = run(&tree, id, style, &HashMap::new());
        assert_eq!(out.ops.len(), 4, "{:?}", out.ops);
        assert!(matches!(
            &out.ops[0],
            PaintOp::PushClipPath { points, .. } if points.len() == 21
        ));
        match &out.ops[1] {
            PaintOp::PushClip {
                x,
                y,
                width,
                height,
                radius,
            } => {
                assert_eq!((*x, *y, *width, *height), (10.0, 20.0, 100.0, 50.0));
                assert_eq!(*radius, [0.0; 8]);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(&out.ops[2], PaintOp::PopClip));
        assert!(matches!(&out.ops[3], PaintOp::PopClip));
    }

    #[test]
    fn hit_rect_carries_transform_mat() {
        // P4 D4：变换节点的 HitRect.mat = 登记时活跃仿射（rotate90 ≠ 恒等）。
        let (tree, id, style) = setup("transform: rotate(90deg)", None);
        let mut styles = HashMap::new();
        styles.insert(id, style);
        let mut layout = HashMap::new();
        layout.insert(id, (100.0, 0.0, 50.0, 100.0));
        let cell = std::cell::RefCell::new(HitCollector::default());
        let ctx = PaintCtx {
            tree: &tree,
            styles: &styles,
            layout: &layout,
            scroll: &HashMap::new(),
            env: &MediaEnv::default(),
            spans: &HashMap::new(),
            wrap_widths: &HashMap::new(),
            text_overrides: &HashMap::new(),
            hit: Some(&cell),
            images: &HashMap::new(),
            column_rules: &HashMap::new(),
            markers: &HashMap::new(),
        };
        let mut out = DisplayList::default();
        build_display_list(&ctx, id, 1, &mut out);
        let rects = cell.into_inner().rects;
        assert_eq!(rects.len(), 1);
        // rotate(90deg) 绕盒心 (125,50)：[0,1,−1,0,175,−75]（cos90≈0、
        // sin90≈1；T(o)·M·T(−o) 平移分量）。
        let m = rects[0].mat;
        assert!(
            m[0].abs() < 1e-5 && (m[1] - 1.0).abs() < 1e-5,
            "a/b 应 0/1：{m:?}"
        );
        assert!(
            (m[2] + 1.0).abs() < 1e-5 && m[3].abs() < 1e-5,
            "c/d 应 −1/0：{m:?}"
        );
        assert!(
            (m[4] - 175.0).abs() < 1e-3 && (m[5] + 75.0).abs() < 1e-3,
            "平移：{m:?}"
        );
    }

    // ===== P4（ADR-0037）：D1 拆段 / D2 round/space / D6 自适应段数 =====

    /// Collects the FillRect sequence from a DisplayList.
    fn fill_rects(out: &DisplayList) -> Vec<(f32, f32, f32, f32)> {
        out.ops
            .iter()
            .filter_map(|op| match op {
                PaintOp::FillRect {
                    x,
                    y,
                    width,
                    height,
                    ..
                } => Some((*x, *y, *width, *height)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn dashed_border_segments_display_list() {
        // P4 D1：border-top dashed 4px 直角框 → 8 段 FillRect（段 8px、
        // 间隔 4px、首段对齐、末段不足不画），无 Border op。
        let (tree, id, style) = setup("border-top: 4px dashed black", None);
        let out = run(&tree, id, style, &HashMap::new());
        assert!(
            !out.ops
                .iter()
                .any(|op| matches!(op, PaintOp::Border { .. })),
            "dashed 不再发 Border op"
        );
        let segs: Vec<_> = fill_rects(&out)
            .into_iter()
            .filter(|&(_, y, _, h)| y == 20.0 && h == 4.0)
            .collect();
        assert_eq!(segs.len(), 8, "段起点 0,12,…,84（{segs:?}）");
        for (k, &(x, y, w, h)) in segs.iter().enumerate() {
            assert_eq!((x, y, w, h), (10.0 + 12.0 * k as f32, 20.0, 8.0, 4.0));
        }
    }

    #[test]
    fn dotted_border_circles_display_list() {
        // P4 D1：dotted 4px → 圆点直径 4、中心距 8、首点覆盖起点：13 点。
        let (tree, id, style) = setup("border-top: 4px dotted black", None);
        let out = run(&tree, id, style, &HashMap::new());
        let segs: Vec<_> = fill_rects(&out)
            .into_iter()
            .filter(|&(_, y, w, h)| y == 20.0 && h == 4.0 && w == 4.0)
            .collect();
        assert_eq!(segs.len(), 13, "中心 12,20,…,108 → 覆盖 [10,110]");
        assert_eq!(segs[0].0, 10.0, "首点圆左缘 = 线起点");
        let last = segs.last().unwrap();
        assert!((last.0 + 4.0 - 110.0).abs() < 1e-4, "末点圆右缘 ≤ 线终点");
    }

    #[test]
    fn dashed_border_radius_falls_back_to_solid() {
        // P4 D1：dashed + 圆角 → 弧上虚线 B 级，整框退 Solid 单 Border op。
        let (tree, id, style) = setup("border: 4px dashed black; border-radius: 10px", None);
        let out = run(&tree, id, style, &HashMap::new());
        let borders: Vec<_> = out
            .ops
            .iter()
            .filter_map(|op| match op {
                PaintOp::Border { sides, .. } => Some(sides.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(borders.len(), 1);
        assert!(
            borders[0].iter().all(|s| s.style == BorderStyle::Solid),
            "dashed 全部退 Solid"
        );
        assert!(fill_rects(&out).is_empty(), "无拆段 FillRect");
    }

    #[test]
    fn outline_dashed_segments_benefit() {
        // P4 D1：outline 通道共用拆段——外扩 3px 的 dashed 3px outline
        // → 顶带 12 段（len=106、段 6、步进 9）。
        let (tree, id, style) = setup("outline: 3px dashed black", None);
        let out = run(&tree, id, style, &HashMap::new());
        let top_segs: Vec<_> = fill_rects(&out)
            .into_iter()
            .filter(|&(_, y, _, h)| y == 17.0 && h == 3.0)
            .collect();
        assert_eq!(top_segs.len(), 12, "p=0,9,…,99（{top_segs:?}）");
        assert_eq!(top_segs[0], (7.0, 17.0, 6.0, 3.0));
    }

    #[test]
    fn tile_axis_space_round_positions() {
        // P4 D2：tile_axis_positions 直测。
        use TileAxis::{None as TA, Repeat, Round, Space};
        // Space：定位区 100、tile 30 → 3 片 gap 5。
        assert_eq!(
            tile_axis_positions(Space, 5.0, 30.0, 10.0, 100.0, 0.0, 100.0),
            vec![(0.0, 30.0), (35.0, 30.0), (70.0, 30.0)]
        );
        // Space 单片：n=1 容不下两片 → 单片按 background-position 定位
        //（css-backgrounds-3 §2.4：only one image is placed, and
        // background-position determines its position in this axis）。
        assert_eq!(
            tile_axis_positions(Space, 5.0, 60.0, 10.0, 100.0, 0.0, 100.0),
            vec![(5.0, 60.0)]
        );
        // Round：n=round(100/30)=3 → ts=100/3，锚定定位区、铺满窗口。
        let rp = tile_axis_positions(Round, 5.0, 30.0, 10.0, 100.0, 0.0, 100.0);
        assert_eq!(rp.len(), 3);
        assert_eq!(rp[0].0, 0.0);
        assert!((rp[0].1 - 100.0 / 3.0).abs() < 1e-4);
        assert!(
            (rp[2].0 + rp[2].1 - 100.0).abs() < 1e-4,
            "末片右缘 = 定位区终点"
        );
        // Repeat：窗口对齐步进（origin=5 → 首片 −25）。
        assert_eq!(
            tile_axis_positions(Repeat, 5.0, 30.0, 0.0, 100.0, 0.0, 100.0)
                .iter()
                .map(|&(p, _)| p)
                .collect::<Vec<_>>(),
            vec![-25.0, 5.0, 35.0, 65.0, 95.0]
        );
        // None：单 tile 原样。
        assert_eq!(
            tile_axis_positions(TA, 5.0, 30.0, 0.0, 100.0, 0.0, 100.0),
            vec![(5.0, 30.0)]
        );
    }

    #[test]
    fn background_round_stretches_tiles() {
        // P4 D2 端到端：background-size 30px + repeat round → 3 片各宽
        // 100/3（定位失效锚定定位区）。
        let (tree, id, style) = setup(
            "background-image: linear-gradient(red, blue);
             background-repeat: round;
             background-size: 30px 30px;",
            None,
        );
        let out = run(&tree, id, style, &HashMap::new());
        // 双轴 round：x 3 片各宽 100/3；y 2 片各高 25（round(50/30)=2）。
        let grads: Vec<(f32, f32, f32, f32)> = out
            .ops
            .iter()
            .filter_map(|op| match op {
                PaintOp::Gradient {
                    x,
                    y,
                    width,
                    height,
                    ..
                } => Some((*x, *y, *width, *height)),
                _ => None,
            })
            .collect();
        assert_eq!(grads.len(), 6, "{grads:?}");
        for &(x, _, w, _) in &grads {
            assert!((w - 100.0 / 3.0).abs() < 1e-3);
            let k = ((x - 10.0) / (100.0 / 3.0)).round();
            assert!((x - (10.0 + k * 100.0 / 3.0)).abs() < 1e-3, "{grads:?}");
        }
        let ys: Vec<f32> = grads.iter().map(|&(_, y, _, _)| y).collect();
        assert!(ys.contains(&20.0) && ys.contains(&45.0), "{grads:?}");
    }

    #[test]
    fn background_space_even_gaps() {
        // P4 D2 端到端：repeat space → 3 片各宽 30、gap 5。
        let (tree, id, style) = setup(
            "background-image: linear-gradient(red, blue);
             background-repeat: space;
             background-size: 30px 30px;",
            None,
        );
        let out = run(&tree, id, style, &HashMap::new());
        let grads: Vec<f32> = out
            .ops
            .iter()
            .filter_map(|op| match op {
                PaintOp::Gradient { x, width, .. } if *width == 30.0 => Some(*x),
                _ => None,
            })
            .collect();
        assert_eq!(grads, vec![10.0, 45.0, 80.0], "{grads:?}");
    }

    // ===== F3d（ADR-0026）：border-image 九片 + 渐变绝对几何 =====

    /// `run` variant: injects a host-registered bitmap (used to exercise
    /// border-image source resolution).
    fn run_with_images(
        tree: &StyleTree,
        id: NodeId,
        style: ComputedStyle,
        images: &HashMap<String, ImageRes>,
    ) -> DisplayList {
        let mut styles = HashMap::new();
        styles.insert(id, style);
        let mut layout = HashMap::new();
        layout.insert(id, (10.0, 20.0, 100.0, 50.0));
        let mut out = DisplayList::default();
        let ctx = PaintCtx {
            tree,
            styles: &styles,
            layout: &layout,
            scroll: &HashMap::new(),
            env: &MediaEnv::default(),
            spans: &HashMap::new(),
            wrap_widths: &HashMap::new(),
            text_overrides: &HashMap::new(),
            hit: None,
            images,
            column_rules: &HashMap::new(),
            markers: &HashMap::new(),
        };
        build_display_list(&ctx, id, 1, &mut out);
        out
    }

    fn rgba_image(w: u32, h: u32) -> ImageRes {
        ImageRes {
            width: w,
            height: h,
            rgba: std::sync::Arc::new(vec![128u8; (w * h * 4) as usize]),
        }
    }

    #[test]
    fn border_image_nine_regions_replace_border() {
        // 位图源：Border op 被九区域 Image op 取代（fill 中心 + 拉伸边）；
        // 角/边/中心 src 子域窗与 dest 区域按 css-backgrounds-3 §6.3-6.6 校准
        let (tree, id, cs) = setup(
            "border: 5px solid black; border-image-source: url(t.png); \
             border-image-slice: 10 fill; border-image-width: 10px",
            None,
        );
        let mut images = HashMap::new();
        images.insert("t.png".to_string(), rgba_image(30, 30));
        let out = run_with_images(&tree, id, cs, &images);
        let imgs: Vec<&PaintOp> = out
            .ops
            .iter()
            .filter(|op| matches!(op, PaintOp::Image { .. }))
            .collect();
        assert_eq!(imgs.len(), 9, "{:?}", out.ops);
        assert!(
            !out.ops
                .iter()
                .any(|op| matches!(op, PaintOp::Border { .. })),
            "border-image 应取代 Border op: {:?}",
            out.ops
        );
        // 左上角：dest (10,20,10,10) src (0,0,10,10)
        match imgs[0] {
            PaintOp::Image {
                x,
                y,
                width,
                height,
                src_x,
                src_y,
                src_w,
                src_h,
                source_w,
                source_h,
                ..
            } => {
                assert_eq!((*x, *y, *width, *height), (10.0, 20.0, 10.0, 10.0));
                assert_eq!((*src_x, *src_y, *src_w, *src_h), (0.0, 0.0, 10.0, 10.0));
                assert_eq!((*source_w, *source_h), (30, 30));
            }
            other => panic!("{other:?}"),
        }
        // 右下角：dest (100,60,10,10) src (20,20,10,10)
        match imgs[2] {
            PaintOp::Image {
                x,
                y,
                width,
                height,
                src_x,
                src_y,
                src_w,
                src_h,
                ..
            } => {
                assert_eq!((*x, *y, *width, *height), (100.0, 60.0, 10.0, 10.0));
                assert_eq!((*src_x, *src_y, *src_w, *src_h), (20.0, 20.0, 10.0, 10.0));
            }
            other => panic!("{other:?}"),
        }
        // 上边（stretch）：dest (20,20,80,10) src (10,0,10,10)
        match imgs[4] {
            PaintOp::Image {
                x,
                y,
                width,
                height,
                src_x,
                src_y,
                src_w,
                src_h,
                ..
            } => {
                assert_eq!((*x, *y, *width, *height), (20.0, 20.0, 80.0, 10.0));
                assert_eq!((*src_x, *src_y, *src_w, *src_h), (10.0, 0.0, 10.0, 10.0));
            }
            other => panic!("{other:?}"),
        }
        // 右边（stretch，沿 y）：dest (100,30,10,30) src (20,10,10,10)
        match imgs[7] {
            PaintOp::Image {
                x,
                y,
                width,
                height,
                src_x,
                src_y,
                src_w,
                src_h,
                ..
            } => {
                assert_eq!((*x, *y, *width, *height), (100.0, 30.0, 10.0, 30.0));
                assert_eq!((*src_x, *src_y, *src_w, *src_h), (20.0, 10.0, 10.0, 10.0));
            }
            other => panic!("{other:?}"),
        }
        // 中心（fill）：dest (20,30,80,30) src (10,10,10,10)
        match imgs[8] {
            PaintOp::Image {
                x,
                y,
                width,
                height,
                src_x,
                src_y,
                src_w,
                src_h,
                ..
            } => {
                assert_eq!((*x, *y, *width, *height), (20.0, 30.0, 80.0, 30.0));
                assert_eq!((*src_x, *src_y, *src_w, *src_h), (10.0, 10.0, 10.0, 10.0));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn border_image_repeat_tiles_with_clip() {
        // repeat 模式：源片尺寸（10px）顺排 80px 上边 → 8 整片 + 区域
        // PushClip；角仍单片；fill 中心按 rep 逐轴平铺（8×3 = 24 片）
        let (tree, id, cs) = setup(
            "border: 5px solid black; border-image-source: url(t.png); \
             border-image-slice: 10 fill; border-image-width: 10px; \
             border-image-repeat: repeat",
            None,
        );
        let mut images = HashMap::new();
        images.insert("t.png".to_string(), rgba_image(30, 30));
        let out = run_with_images(&tree, id, cs, &images);
        let img_count = out
            .ops
            .iter()
            .filter(|op| matches!(op, PaintOp::Image { .. }))
            .count();
        // 4 角 + 上 8 + 下 8 + 左 3（30/10） + 右 3 + 中心 8×3 = 50
        assert_eq!(img_count, 50, "{:?}", out.ops.len());
        assert!(
            out.ops
                .iter()
                .any(|op| matches!(op, PaintOp::PushClip { .. }))
        );
    }

    #[test]
    fn border_image_missing_url_falls_back_to_border() {
        // URL 未注册：回退 Border op、零 Image op（零副作用契约）
        let (tree, id, cs) = setup(
            "border: 5px solid black; border-image-source: url(missing.png)",
            None,
        );
        let out = run_with_images(&tree, id, cs, &HashMap::new());
        assert!(
            out.ops
                .iter()
                .any(|op| matches!(op, PaintOp::Border { .. }))
        );
        assert!(!out.ops.iter().any(|op| matches!(op, PaintOp::Image { .. })));
    }

    #[test]
    fn border_image_gradient_nine_regions_share_full_box_line() {
        // 渐变源：九区域共享全盒渐变线（90deg → 水平线 y=45，x 10→110），
        // 区域 = 切片精确（linear 绝对几何完成切片语义）；f32 sin_cos 非精确
        // → 几何断言取 1e-2 容差
        let near = |a: f32, b: f32| (a - b).abs() < 1e-2;
        let (tree, id, cs) = setup(
            "border: 5px solid black; \
             border-image-source: linear-gradient(90deg, red, blue); \
             border-image-slice: 10 fill; border-image-width: 10px",
            None,
        );
        let out = run_with_images(&tree, id, cs, &HashMap::new());
        let grads: Vec<Option<LinearGeom>> = out
            .ops
            .iter()
            .filter_map(|op| match op {
                PaintOp::Gradient { linear, .. } => Some(*linear),
                _ => None,
            })
            .collect();
        assert_eq!(grads.len(), 9, "{:?}", out.ops);
        for (i, g) in grads.iter().enumerate() {
            let g = g.expect("渐变源九区域必须携带 linear 几何");
            assert!(
                near(g.start[0], 10.0) && near(g.start[1], 45.0),
                "region {i} start {:?}",
                g.start
            );
            assert!(
                near(g.end[0], 110.0) && near(g.end[1], 45.0),
                "region {i} end {:?}",
                g.end
            );
        }
        assert!(
            !out.ops
                .iter()
                .any(|op| matches!(op, PaintOp::Border { .. })),
            "{:?}",
            out.ops
        );
    }

    #[test]
    fn linear_background_emits_absolute_geometry() {
        // 普通背景线性渐变同发绝对几何（与 radial/conic 对齐）；
        // 90deg 盒 (10,20,100,50)：线 y=45、x 10→110（1e-2 容差）
        let near = |a: f32, b: f32| (a - b).abs() < 1e-2;
        let (tree, id, cs) = setup("background: linear-gradient(90deg, red, blue)", None);
        let out = run_with_images(&tree, id, cs, &HashMap::new());
        let mut found = 0;
        for op in &out.ops {
            if let PaintOp::Gradient { x, y, linear, .. } = op {
                found += 1;
                assert_eq!((*x, *y), (10.0, 20.0));
                let g = linear.expect("背景线性渐变必须携带绝对几何");
                assert!(near(g.start[0], 10.0) && near(g.start[1], 45.0), "{g:?}");
                assert!(near(g.end[0], 110.0) && near(g.end[1], 45.0), "{g:?}");
            }
        }
        assert_eq!(found, 1, "{:?}", out.ops);
    }

    #[cfg(feature = "serde")]
    // paint_dump 模块随 serde 门控（默认 feature 集下编译不过的既有耦合，P1-2 顺手修正）
    #[test]
    fn paint_dump_roundtrips_blend_layer() {
        // P1-2：PushBlend{mode}/PopBlend 无损往返（kebab-case 模式名）
        let mut list = DisplayList::default();
        list.ops.push(PaintOp::PushBlend {
            mode: crate::css::property::BlendMode::ColorDodge,
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 4.0,
        });
        list.ops.push(PaintOp::PopBlend);
        let dump = list.to_dump();
        let json = serde_json::to_string(&dump).expect("json");
        assert!(json.contains("\"push_blend\""));
        assert!(json.contains("\"color-dodge\""));
        let rebuilt = serde_json::from_str::<crate::paint_dump::DisplayListDump>(&json)
            .expect("parse")
            .to_display_list();
        assert_eq!(rebuilt.ops.len(), 2);
        match &rebuilt.ops[0] {
            PaintOp::PushBlend {
                mode,
                x,
                y,
                width,
                height,
            } => {
                assert_eq!(*mode, crate::css::property::BlendMode::ColorDodge);
                assert_eq!((*x, *y, *width, *height), (1.0, 2.0, 3.0, 4.0));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(&rebuilt.ops[1], PaintOp::PopBlend));
    }

    #[cfg(feature = "serde")] // paint_dump 模块随 serde 门控
    #[test]
    fn paint_dump_roundtrips_image_src_window_and_linear_geom() {
        // serde 镜像：Image src 子域 + Gradient linear 绝对几何无损往返
        let mut list = DisplayList::default();
        let img = rgba_image(30, 30);
        list.ops.push(PaintOp::Image {
            x: 1.0,
            y: 2.0,
            width: 10.0,
            height: 20.0,
            radius: [0.0; 8],
            source_w: 30,
            source_h: 30,
            src_x: 10.0,
            src_y: 5.0,
            src_w: 10.0,
            src_h: 10.0,
            pixels: img.clone(),
        });
        list.ops.push(PaintOp::Gradient {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 50.0,
            radius: [0.0; 8],
            gradient: crate::css::property::Gradient {
                kind: crate::css::property::GradientKind::Linear(crate::css::value::Angle(90.0)),
                repeating: false,
                stops: vec![],
                hints: vec![],
            },
            radial: None,
            conic: None,
            linear: Some(LinearGeom {
                start: [10.0, 45.0],
                end: [110.0, 45.0],
            }),
        });
        let dump = list.to_dump();
        let json = serde_json::to_string(&dump).expect("json");
        assert!(json.contains("\"src_x\""));
        assert!(json.contains("\"linear\""));
        let rebuilt = serde_json::from_str::<crate::paint_dump::DisplayListDump>(&json)
            .expect("parse")
            .to_display_list();
        assert_eq!(rebuilt.ops.len(), 2);
        match &rebuilt.ops[0] {
            PaintOp::Image {
                src_x,
                src_y,
                src_w,
                src_h,
                source_w,
                source_h,
                ..
            } => {
                assert_eq!((*src_x, *src_y, *src_w, *src_h), (10.0, 5.0, 10.0, 10.0));
                assert_eq!((*source_w, *source_h), (30, 30));
            }
            other => panic!("{other:?}"),
        }
        match &rebuilt.ops[1] {
            PaintOp::Gradient { linear, .. } => {
                assert_eq!(
                    *linear,
                    Some(LinearGeom {
                        start: [10.0, 45.0],
                        end: [110.0, 45.0],
                    })
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[cfg(feature = "serde")] // paint_dump 模块随 serde 门控
    #[test]
    fn paint_dump_roundtrips_repeating_gradient() {
        // P1-3：Gradient.repeating serde 往返；旧 dump（无该字段）经
        // #[serde(default)] 兼容回落 false。
        let mut list = DisplayList::default();
        list.ops.push(PaintOp::Gradient {
            x: 0.0,
            y: 0.0,
            width: 40.0,
            height: 40.0,
            radius: [0.0; 8],
            gradient: crate::css::property::Gradient {
                kind: crate::css::property::GradientKind::Linear(crate::css::value::Angle(90.0)),
                repeating: true,
                stops: vec![],
                hints: vec![],
            },
            radial: None,
            conic: None,
            linear: Some(LinearGeom {
                start: [0.0, 0.0],
                end: [40.0, 0.0],
            }),
        });
        let dump = list.to_dump();
        let json = serde_json::to_string(&dump).expect("json");
        assert!(json.contains("\"repeating\":true"));
        let rebuilt = serde_json::from_str::<crate::paint_dump::DisplayListDump>(&json)
            .expect("parse")
            .to_display_list();
        match &rebuilt.ops[0] {
            PaintOp::Gradient { gradient, .. } => assert!(gradient.repeating),
            other => panic!("{other:?}"),
        }
        // 旧 dump 兼容：抹去 repeating 字段（serde_json::Value 手术）→ default false
        let mut v: serde_json::Value = serde_json::from_str(&json).expect("value");
        v["ops"][0]["gradient"]
            .as_object_mut()
            .expect("gradient obj")
            .remove("repeating");
        let old = serde_json::from_value::<crate::paint_dump::DisplayListDump>(v)
            .expect("parse")
            .to_display_list();
        match &old.ops[0] {
            PaintOp::Gradient { gradient, .. } => assert!(!gradient.repeating),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn text_op_carries_font_deep_fields() {
        // F3d（ADR-0026 D5）：深化字体四字段进入 PaintOp::Text（基样式级）；
        // font-variant-caps: titling-caps 派生 'titl' 特性并入生效表
        let (tree, id, style) = setup(
            "font-stretch: 125%; word-spacing: 4px; font-feature-settings: \"kern\" 0; font-variation-settings: \"wght\" 350; font-variant-caps: titling-caps",
            Some("hi"),
        );
        let out = run(&tree, id, style, &HashMap::new());
        match out.ops.iter().find(|op| matches!(op, PaintOp::Text { .. })) {
            Some(PaintOp::Text {
                font_stretch,
                word_spacing,
                font_features,
                font_variations,
                ..
            }) => {
                assert_eq!(*font_stretch, 125.0);
                assert_eq!(*word_spacing, Some(4.0));
                assert!(font_features.contains(&(*b"kern", 0)), "{font_features:?}");
                assert!(font_features.contains(&(*b"titl", 1)), "{font_features:?}");
                assert!(
                    font_variations.contains(&(*b"wght", 350.0)),
                    "{font_variations:?}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn small_caps_text_emits_scaled_span() {
        // F3d：small-caps 合成——小写→大写 + 0.8 缩放 span；'B' 原大写
        // 不缩放（Small 模式）；op 文本为合成后文本
        let (tree, id, style) = setup("font-size: 20px; font-variant-caps: small-caps", Some("aB"));
        let out = run(&tree, id, style, &HashMap::new());
        match &out.ops[0] {
            PaintOp::Text { text, spans, .. } => {
                assert_eq!(text, "AB");
                let s = spans
                    .iter()
                    .find(|s| (s.font_size - 16.0).abs() < 1e-3)
                    .expect("scaled span");
                assert_eq!((s.start, s.end), (0, 1), "{spans:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[cfg(feature = "serde")] // paint_dump 模块随 serde 门控
    #[test]
    fn paint_dump_roundtrips_text_font_fields() {
        // serde 镜像：Text 深化字体四字段无损往返
        let mut list = DisplayList::default();
        let family = || {
            FontFamilyList(
                std::iter::once(crate::css::property::FamilyName::Named("serif".into())).collect(),
            )
        };
        list.ops.push(PaintOp::Text {
            x: 1.0,
            y: 2.0,
            text: "Aa".into(),
            color: AlphaColor::<Srgb>::new([0.0, 0.0, 0.0, 1.0]),
            spans: vec![TextSpanPaint {
                start: 0,
                end: 2,
                color: AlphaColor::<Srgb>::new([0.1, 0.2, 0.3, 1.0]),
                font_size: 16.0,
                font_weight: 700.0,
                italic: true,
                font_family: family(),
                letter_spacing: 1.5,
                line_height: Some(24.0),
            }],
            font_size: 16.0,
            font_family: family(),
            font_weight: 400.0,
            italic: false,
            max_advance: Some(80.0),
            line_height: Some(20.0),
            letter_spacing: 0.5,
            text_align: TextAlign::Center,
            word_break: crate::css::property::WordBreakKind::Normal,
            overflow_wrap: crate::css::property::OverflowWrapKind::Normal,
            decorations: vec![],
            shadows: vec![],
            font_stretch: 125.0,
            word_spacing: Some(4.0),
            font_features: vec![(*b"kern", 0), (*b"titl", 1)],
            font_variations: vec![(*b"wght", 350.0)],
        });
        let dump = list.to_dump();
        let json = serde_json::to_string(&dump).expect("json");
        assert!(json.contains("\"font_stretch\":125.0"));
        assert!(json.contains("\"font_features\""));
        let rebuilt = serde_json::from_str::<crate::paint_dump::DisplayListDump>(&json)
            .expect("parse")
            .to_display_list();
        assert_eq!(rebuilt.ops.len(), 1);
        match &rebuilt.ops[0] {
            PaintOp::Text {
                font_stretch,
                word_spacing,
                font_features,
                font_variations,
                spans,
                ..
            } => {
                assert_eq!(*font_stretch, 125.0);
                assert_eq!(*word_spacing, Some(4.0));
                assert_eq!(font_features, &vec![(*b"kern", 0), (*b"titl", 1)]);
                assert_eq!(font_variations, &vec![(*b"wght", 350.0)]);
                assert_eq!(spans.len(), 1);
            }
            other => panic!("{other:?}"),
        }
    }
}
