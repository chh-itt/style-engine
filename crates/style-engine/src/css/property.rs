//! Property inventory and declaration parsing: the T0 property set
//! (FEATURES.md), with grammars reused per value family.
//!
//! PropertyId is the stable vocabulary between declaration blocks and
//! ComputedStyle; the `PropertyId` → CSS property name mapping is one-to-one
//! (parsing is case-insensitive; names are normalized to lowercase here).

use crate::css::value::{
    Angle, ColorValue, LengthPercentage, ValResult, parse_color_value, parse_length_percentage,
    parse_number,
};
use crate::{AlphaColor, Srgb};
use cssparser::{Parser, Token, match_ignore_ascii_case};
use smallvec::SmallVec;

/// T0 properties (the FEATURES.md syntax-layer inventory).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum PropertyId {
    // 布局：显示与定位
    /// display — display type.
    Display,
    /// position — positioning scheme.
    Position,
    /// top — top offset (length-percentage|auto).
    Top,
    /// right — right offset (length-percentage|auto).
    Right,
    /// bottom — bottom offset (length-percentage|auto).
    Bottom,
    /// left — left offset (length-percentage|auto).
    Left,
    /// z-index — stacking order (auto|`<number>`).
    ZIndex,
    // 动画（第五批⑰）：描述符属性——不可动画、不参与插值，仅驱动
    // @keyframes 采样
    /// animation-name — keyframe name (none → None).
    AnimationName,
    /// animation-duration — duration of one cycle (seconds).
    AnimationDuration,
    /// animation-delay — start delay (seconds; negative values are valid).
    AnimationDelay,
    /// animation-iteration-count — iteration count (infinite → ∞).
    AnimationIterationCount,
    /// animation-timing-function — easing function.
    AnimationTimingFunction,
    /// animation-direction — playback direction.
    AnimationDirection,
    /// animation-fill-mode — fill mode outside the animation.
    AnimationFillMode,
    // 布局：盒子
    /// width — width (length-percentage|auto).
    Width,
    /// height — height (length-percentage|auto).
    Height,
    /// min-width — minimum width (length-percentage|auto).
    MinWidth,
    /// min-height — minimum height (length-percentage|auto).
    MinHeight,
    /// max-width — maximum width (length-percentage|auto).
    MaxWidth,
    /// max-height — maximum height (length-percentage|auto).
    MaxHeight,
    /// aspect-ratio — aspect ratio (auto|`<ratio>`).
    AspectRatio,
    // 布局：盒间距
    /// margin-top — top margin (length-percentage).
    MarginTop,
    /// margin-right — right margin.
    MarginRight,
    /// margin-bottom — bottom margin.
    MarginBottom,
    /// margin-left — left margin.
    MarginLeft,
    /// padding-top — top padding (length-percentage).
    PaddingTop,
    /// padding-right — right padding.
    PaddingRight,
    /// padding-bottom — bottom padding.
    PaddingBottom,
    /// padding-left — left padding.
    PaddingLeft,
    /// gap — row/column gap (single length-percentage; shorthand).
    Gap,
    /// row-gap — row gap.
    RowGap,
    /// column-gap — column gap.
    ColumnGap,
    // 布局：flex
    /// flex-direction — main-axis direction.
    FlexDirection,
    /// flex-wrap — wrapping mode.
    FlexWrap,
    /// flex-grow — factor for growing into leftover space (`<number>`).
    FlexGrow,
    /// flex-shrink — shrink factor under overflow (`<number>`).
    FlexShrink,
    /// flex-basis — main-axis base size (length-percentage|auto).
    FlexBasis,
    /// justify-content — main-axis alignment.
    JustifyContent,
    /// align-items — cross-axis alignment (default for children).
    AlignItems,
    /// align-self — cross-axis alignment (per-child override).
    AlignSelf,
    /// align-content — cross-axis alignment of multiple lines/tracks.
    AlignContent,
    // 布局：grid
    /// grid-template-columns — explicit column tracks.
    GridTemplateColumns,
    /// grid-template-rows — explicit row tracks.
    GridTemplateRows,
    /// grid-auto-flow — auto-placement direction (row|column).
    GridAutoFlow,
    /// grid-auto-rows — implicit row tracks.
    GridAutoRows,
    /// grid-auto-columns — implicit column tracks.
    GridAutoColumns,
    /// grid-template-areas (E5, ADR-0020): area template (rows of quoted
    /// strings × whitespace-separated columns; `.` = empty cell;
    /// rectangularity + per-name rectangularity are validated — violation =
    /// invalid declaration).
    GridTemplateAreas,
    /// grid-row-start (E5, ADR-0020): row placement start line.
    GridRowStart,
    /// grid-row-end (E5, ADR-0020): row placement end line.
    GridRowEnd,
    /// grid-column-start (E5, ADR-0020): column placement start line.
    GridColumnStart,
    /// grid-column-end (E5, ADR-0020): column placement end line.
    GridColumnEnd,
    /// text-overflow (F2, ADR-0022 D2).
    TextOverflow,
    /// -webkit-line-clamp (F2, ADR-0022 D3).
    WebkitLineClamp,
    /// text-decoration-line (F2, ADR-0022 D4).
    TextDecorationLine,
    /// text-decoration-style (F2, ADR-0022 D4).
    TextDecorationStyle,
    /// text-decoration-color (F2, ADR-0022 D4).
    TextDecorationColor,
    /// text-decoration-thickness (F2, ADR-0022 D4).
    TextDecorationThickness,
    /// text-shadow (F2, ADR-0022 D5).
    TextShadow,
    /// Phase 2 ③ multi-column: explicit column count (auto|`<integer≥1>`);
    /// with no count, a column-width declaration alone requests multi-column
    /// (the count is settled at layout time).
    ColumnCount,
    /// Phase 2 ③ multi-column: ideal column width (auto|`<length>`) — in
    /// width mode the column count is
    /// n = max(1, ⌊(content width+gap)/(ideal width+gap)⌋).
    ColumnWidth,
    /// Phase 3 ⑤a: css-break line-breaking control (avoid|auto) — in v1 all
    /// blocks are unbreakable, so avoid is the default semantics; parsed and
    /// stored for conformance alignment and future fragmentation support.
    BreakInside,
    /// Phase 3 ⑤b: multi-column span (none|all) — all cuts the child off the
    /// column flow; the segments before and after are balanced independently
    /// (row wrapping model).
    ColumnSpan,
    /// Phase 3 ⑤c: multi-column rule's three longhands. style reuses the
    /// BorderStyle value family (none/hidden draw nothing; v1 only draws
    /// solid for real, dashed/dotted approximate solid — Tier B deviation);
    /// width keywords are materialized to fixed values (absent = medium);
    /// color reuses the Color value family (initial currentcolor, not
    /// inherited in v1).
    ColumnRuleWidth,
    /// column-rule-style — column rule style (reuses the border-style keyword
    /// family).
    ColumnRuleStyle,
    /// column-rule-color — column rule color (initial currentcolor, not
    /// inherited in v1).
    ColumnRuleColor,
    // 绘制
    /// background-color — background color.
    BackgroundColor,
    /// background-image — background image (none|url()|gradient).
    BackgroundImage,
    /// background-repeat — tiling style (F3b, ADR-0024).
    BackgroundRepeat,
    /// background-attachment — background attachment (F3b, ADR-0024).
    BackgroundAttachment,
    /// background-position — background position (F3b, ADR-0024).
    BackgroundPosition,
    /// background-size — background size (F3b, ADR-0024).
    BackgroundSize,
    /// background-origin — positioning area box (F3b, ADR-0024).
    BackgroundOrigin,
    /// background-clip — painting area box (F3b, ADR-0024).
    BackgroundClip,
    /// border-top-left-radius — top-left corner radius (length-percentage{1,2}).
    BorderTopLeftRadius,
    /// border-top-right-radius — top-right corner radius.
    BorderTopRightRadius,
    /// border-bottom-right-radius — bottom-right corner radius.
    BorderBottomRightRadius,
    /// border-bottom-left-radius — bottom-left corner radius.
    BorderBottomLeftRadius,
    /// border-top-width — top border width (none|thin|medium|thick|`<length>`).
    BorderTopWidth,
    /// border-right-width — right border width.
    BorderRightWidth,
    /// border-bottom-width — bottom border width.
    BorderBottomWidth,
    /// border-left-width — left border width.
    BorderLeftWidth,
    /// border-top-style — top border style.
    BorderTopStyle,
    /// border-right-style — right border style.
    BorderRightStyle,
    /// border-bottom-style — bottom border style.
    BorderBottomStyle,
    /// border-left-style — left border style.
    BorderLeftStyle,
    /// border-top-color — top border color.
    BorderTopColor,
    /// border-right-color — right border color.
    BorderRightColor,
    /// border-bottom-color — bottom border color.
    BorderBottomColor,
    /// border-left-color — left border color.
    BorderLeftColor,
    /// box-shadow — shadow list (comma-separated; inset supported).
    BoxShadow,
    /// opacity — opacity (`<number>`; 1 = fully opaque).
    Opacity,
    /// overflow-x — horizontal overflow handling.
    OverflowX,
    /// overflow-y — vertical overflow handling.
    OverflowY,
    /// box-sizing — box sizing basis (content-box|border-box).
    BoxSizing,
    /// transform (ADR-0009 v1: list of 2D affine functions; 3D functions are
    /// rejected at parse time).
    Transform,
    /// filter (batch 4 ④: parsed only as a presence semantic bit that
    /// triggers SC; no filter effects are performed).
    Filter,
    /// clip-path — clip shape (batch 4 ④ SC bit; upgraded in F3c to shape
    /// parsing and paint-time clipping, ADR-0025).
    ClipPath,
    /// will-change (batch 5 ㉒, full SC-trigger set: triggers when the list
    /// contains any property whose non-initial value creates an SC; purely a
    /// semantic bit, no hint-driven optimization implemented).
    WillChange,
    /// isolation (batch 5 ㉒: `isolate` triggers SC).
    Isolation,
    /// mix-blend-mode (batch 5 ㉒: any non-normal value triggers SC; the
    /// blend effect itself is out of scope).
    MixBlendMode,
    /// transform-origin (batch 5 ⑬: consumed at paint time as the origin
    /// transforms act around, 2D subset).
    TransformOrigin,
    // 文本
    /// color — foreground text color.
    Color,
    /// font-family — font family list (comma-separated).
    FontFamily,
    /// font-size — font size (length-percentage or absolute size keyword).
    FontSize,
    /// font-weight — font weight (normal=400, bold=700, or `<number>`).
    FontWeight,
    /// font-style — font style (normal|italic).
    FontStyle,
    /// line-height — line height (normal|`<number>`|`<length-percentage>`).
    LineHeight,
    /// text-align — horizontal text alignment.
    TextAlign,
    /// white-space — whitespace and line breaking handling.
    WhiteSpace,
    /// letter-spacing — letter spacing (length-percentage; normal → 0).
    LetterSpacing,
    // 容器查询（阶段2③）
    /// container-type (normal|size|inline-size) — size/inline-size makes the
    /// node a queryable container (the engine records the content box size
    /// for @container evaluation; v1 does not enforce size containment, a
    /// Tier B deviation documented in FEATURES.md).
    ContainerType,
    /// container-name (none|custom-ident#) — named container queries match by
    /// name from the nearest ancestor outward.
    ContainerName,
    // 行为提示属性（A1，docs/BEHAVIOR-HINT-PROPS.md）：宿主可读、零绘制
    // 语义——引擎解析入库+级联继承，消费端是宿主（光标/选择/命中/输入
    // 插示器/控件着色），不产生任何 PaintOp、不参与布局。
    /// cursor (CSS UI 4 keyword subset) — host mouse pointer shape.
    Cursor,
    /// user-select (CSS UI 4) — text selectability semantics (**not
    /// inherited**).
    UserSelect,
    /// pointer-events — hit-testing pass-through semantics.
    PointerEvents,
    /// caret-color — text insertion caret color (auto|color).
    CaretColor,
    /// accent-color — widget accent color (auto|color).
    AccentColor,
    // outline（A2）：不占布局的装饰描边（ink overflow，css-ui-4）——
    // 绘制复用 Border 基元（外扩矩形承载），布局零映射。
    /// outline-width (thin|medium|thick|`<length>`).
    OutlineWidth,
    /// outline-style (none|solid|dashed|dotted|double|groove|ridge|inset|outset|auto).
    OutlineStyle,
    /// outline-color (`<color>`; initial currentcolor).
    OutlineColor,
    /// outline-offset (`<length>`, may be negative; how far the stroke band
    /// is outset).
    OutlineOffset,
    // 逻辑属性（A8，css-logical-1）：独立槽位 + computed 期按元素
    // direction 与映射物理槽按级联序键定夺（规范正确：物理/逻辑同池
    // 比先后）。纵向书写模式不支持（在案 FEATURES.md）。
    /// direction (ltr|rtl, inherited) — the basis for logical→physical
    /// mapping.
    Direction,
    /// unicode-bidi (inherited) — text stack hint (readable as a computed
    /// value).
    UnicodeBidi,
    /// margin-inline-start
    MarginInlineStart,
    /// margin-inline-end
    MarginInlineEnd,
    /// margin-block-start
    MarginBlockStart,
    /// margin-block-end
    MarginBlockEnd,
    /// padding-inline-start
    PaddingInlineStart,
    /// padding-inline-end
    PaddingInlineEnd,
    /// padding-block-start
    PaddingBlockStart,
    /// padding-block-end
    PaddingBlockEnd,
    /// inset-inline-start
    InsetInlineStart,
    /// inset-inline-end
    InsetInlineEnd,
    /// inset-block-start
    InsetBlockStart,
    /// inset-block-end
    InsetBlockEnd,
    /// border-inline-start-width
    BorderInlineStartWidth,
    /// border-inline-end-width
    BorderInlineEndWidth,
    /// border-block-start-width
    BorderBlockStartWidth,
    /// border-block-end-width
    BorderBlockEndWidth,
    /// border-inline-start-style
    BorderInlineStartStyle,
    /// border-inline-end-style
    BorderInlineEndStyle,
    /// border-block-start-style
    BorderBlockStartStyle,
    /// border-block-end-style
    BorderBlockEndStyle,
    /// border-inline-start-color
    BorderInlineStartColor,
    /// border-inline-end-color
    BorderInlineEndColor,
    /// border-block-start-color
    BorderBlockStartColor,
    /// border-block-end-color
    BorderBlockEndColor,
    /// border-start-start-radius
    BorderStartStartRadius,
    /// border-start-end-radius
    BorderStartEndRadius,
    /// border-end-start-radius
    BorderEndStartRadius,
    /// border-end-end-radius
    BorderEndEndRadius,
    /// content (C1, css-content-3): generated content for pseudo-elements.
    Content,
    /// text-transform (C2, css-text-3): text case/fullwidth transformation.
    TextTransform,
    /// overflow-wrap (C2, css-text-3): line breaking of overflowing long words.
    OverflowWrap,
    /// word-break (C2, css-text-3): intra-word line-breaking strength.
    WordBreak,
    /// object-fit (C3, css-images-3): replaced content fitting mode.
    ObjectFit,
    /// object-position (C3, css-images-3): alignment of replaced content
    /// within its box.
    ObjectPosition,
    /// float (E4, css-position-3 / ADR-0019): floats out of flow.
    Float,
    /// clear (E4, css-position-3 / ADR-0019): clamps against floats.
    Clear,
    // F3d（ADR-0026）：border-image 全集五长手
    /// border-image-source — border image source (none|url()|gradient;
    /// reuses the background image single-layer value family).
    BorderImageSource,
    /// border-image-slice — four slice lines into the source
    /// (number/percentage) + fill.
    BorderImageSlice,
    /// border-image-width — widths of the painting area's four edges
    /// (length|number|auto).
    BorderImageWidth,
    /// border-image-outset — outset of the painting area's four edges
    /// (length|number).
    BorderImageOutset,
    /// border-image-repeat — tiling style of the edge/middle regions (x/y
    /// axes).
    BorderImageRepeat,
    // F3d（ADR-0026）：字体深化五属性
    /// font-stretch — width axis (percent normalized to 50..=200,
    /// normal=100).
    FontStretch,
    /// word-spacing — word spacing (length-percentage; normal → 0).
    WordSpacing,
    /// font-feature-settings — OpenType feature list.
    FontFeatures,
    /// font-variation-settings — variable font axis list.
    FontVariations,
    /// font-variant-caps — capital-form variant (small-caps family
    /// synthesized, ADR-0026 D3).
    FontVariantCaps,
    /// hyphens — hyphenation mode (F4, ADR-0028: property layer; the
    /// hyphenation effect is bounded by upstream segmenter boundaries, all
    /// three values behave the same in v1, documented as Tier B).
    Hyphens,
    /// backdrop-filter — backdrop filter presence (F4, ADR-0028: non-none
    /// triggers SC; the effect itself is T2, following the filter batch 4 ④
    /// precedent).
    BackdropFilter,
    /// transition-property — list of transitionable property names (G1,
    /// ADR-0032).
    TransitionProperty,
    /// transition-duration — transition duration list, seconds (G1,
    /// ADR-0032).
    TransitionDuration,
    /// transition-timing-function — transition easing list (G1, ADR-0032).
    TransitionTimingFunction,
    /// transition-delay — transition delay list, seconds, may be negative
    /// (G1, ADR-0032).
    TransitionDelay,
    /// transition-behavior — transition policy for discrete properties (G1,
    /// ADR-0032).
    TransitionBehavior,
    /// vertical-align — vertical alignment of inline participants (P3,
    /// ADR-0034 D3).
    VerticalAlign,
    /// counter-reset — counter creation/assignment (P5, ADR-0036 D2). Not
    /// inherited.
    CounterReset,
    /// counter-increment — counter increments (P5, ADR-0036 D2). Not
    /// inherited.
    CounterIncrement,
    /// quotes — quote pair table (P5, ADR-0036 D3). Inherited.
    Quotes,
    /// list-style-type — list item marker style (P9-3, css-lists-3 §3.4).
    /// Inherited. Initial disc.
    ListStyleType,
    /// list-style-position — marker box position inside|outside (P9-3, §3.5).
    /// Inherited. Initial outside (the engine always renders as inside = a
    /// Tier B approximation, ADR-0041).
    ListStylePosition,
    /// list-style-image — marker image (P9-3, §3.3). Inherited. Initial none.
    ListStyleImage,
}

impl PropertyId {
    /// The full inventory (linear scan table for from_css_name).
    pub const ALL: &'static [PropertyId] = &[
        Self::Display,
        Self::Position,
        Self::Top,
        Self::Right,
        Self::Bottom,
        Self::Left,
        Self::ZIndex,
        Self::Width,
        Self::Height,
        Self::MinWidth,
        Self::MinHeight,
        Self::MaxWidth,
        Self::MaxHeight,
        Self::AspectRatio,
        Self::MarginTop,
        Self::MarginRight,
        Self::MarginBottom,
        Self::MarginLeft,
        Self::PaddingTop,
        Self::PaddingRight,
        Self::PaddingBottom,
        Self::PaddingLeft,
        Self::Gap,
        Self::RowGap,
        Self::ColumnGap,
        Self::FlexDirection,
        Self::FlexWrap,
        Self::FlexGrow,
        Self::FlexShrink,
        Self::FlexBasis,
        Self::JustifyContent,
        Self::AlignItems,
        Self::AlignSelf,
        Self::AlignContent,
        Self::GridTemplateColumns,
        Self::GridTemplateRows,
        Self::GridAutoFlow,
        Self::GridAutoRows,
        Self::GridAutoColumns,
        Self::ColumnCount,
        Self::ColumnWidth,
        Self::BreakInside,
        Self::ColumnSpan,
        Self::ColumnRuleWidth,
        Self::ColumnRuleStyle,
        Self::ColumnRuleColor,
        Self::BackgroundColor,
        Self::BackgroundImage,
        Self::BorderTopLeftRadius,
        Self::BorderTopRightRadius,
        Self::BorderBottomRightRadius,
        Self::BorderBottomLeftRadius,
        Self::BorderTopWidth,
        Self::BorderRightWidth,
        Self::BorderBottomWidth,
        Self::BorderLeftWidth,
        Self::BorderTopStyle,
        Self::BorderRightStyle,
        Self::BorderBottomStyle,
        Self::BorderLeftStyle,
        Self::BorderTopColor,
        Self::BorderRightColor,
        Self::BorderBottomColor,
        Self::BorderLeftColor,
        Self::BoxShadow,
        Self::Opacity,
        Self::OverflowX,
        Self::OverflowY,
        Self::BoxSizing,
        Self::Transform,
        Self::Filter,
        Self::ClipPath,
        Self::WillChange,
        Self::Isolation,
        Self::MixBlendMode,
        Self::TransformOrigin,
        Self::Color,
        Self::FontFamily,
        Self::FontSize,
        Self::FontWeight,
        Self::FontStyle,
        Self::LineHeight,
        Self::TextAlign,
        Self::WhiteSpace,
        Self::LetterSpacing,
        Self::ContainerType,
        Self::ContainerName,
        Self::Cursor,
        Self::UserSelect,
        Self::PointerEvents,
        Self::CaretColor,
        Self::AccentColor,
        Self::OutlineWidth,
        Self::OutlineStyle,
        Self::OutlineColor,
        Self::OutlineOffset,
        Self::Direction,
        Self::UnicodeBidi,
        Self::MarginInlineStart,
        Self::MarginInlineEnd,
        Self::MarginBlockStart,
        Self::MarginBlockEnd,
        Self::PaddingInlineStart,
        Self::PaddingInlineEnd,
        Self::PaddingBlockStart,
        Self::PaddingBlockEnd,
        Self::InsetInlineStart,
        Self::InsetInlineEnd,
        Self::InsetBlockStart,
        Self::InsetBlockEnd,
        Self::BorderInlineStartWidth,
        Self::BorderInlineEndWidth,
        Self::BorderBlockStartWidth,
        Self::BorderBlockEndWidth,
        Self::BorderInlineStartStyle,
        Self::BorderInlineEndStyle,
        Self::BorderBlockStartStyle,
        Self::BorderBlockEndStyle,
        Self::BorderInlineStartColor,
        Self::BorderInlineEndColor,
        Self::BorderBlockStartColor,
        Self::BorderBlockEndColor,
        Self::BorderStartStartRadius,
        Self::BorderStartEndRadius,
        Self::BorderEndStartRadius,
        Self::BorderEndEndRadius,
        Self::Content,
        Self::TextTransform,
        Self::OverflowWrap,
        Self::WordBreak,
        Self::ObjectFit,
        Self::ObjectPosition,
        Self::Float,
        Self::Clear,
        Self::GridTemplateAreas,
        Self::GridRowStart,
        Self::GridRowEnd,
        Self::GridColumnStart,
        Self::GridColumnEnd,
        Self::TextOverflow,
        Self::WebkitLineClamp,
        Self::TextDecorationLine,
        Self::TextDecorationStyle,
        Self::TextDecorationColor,
        Self::TextDecorationThickness,
        Self::TextShadow,
        Self::BackgroundRepeat,
        Self::BackgroundAttachment,
        Self::BackgroundPosition,
        Self::BackgroundSize,
        Self::BackgroundOrigin,
        Self::BackgroundClip,
        // F3d（ADR-0026）：border-image 五长手 + 字体深化五属性（ALL 尾
        // 追加，动画描述符槽整体后移 ×10）
        Self::BorderImageSource,
        Self::BorderImageSlice,
        Self::BorderImageWidth,
        Self::BorderImageOutset,
        Self::BorderImageRepeat,
        Self::FontStretch,
        Self::WordSpacing,
        Self::FontFeatures,
        Self::FontVariations,
        Self::FontVariantCaps,
        // F4（ADR-0028）：hyphens + backdrop-filter（ALL 尾追加；动画描述
        // 符槽整体后移 ×2 至 164..171，slot_alignment 不变量=ALL 序连续）
        Self::Hyphens,
        Self::BackdropFilter,
        // G1（ADR-0032）：transition 五长手（ALL 尾追加；动画描述符槽整体
        // 后移 ×5 至 169..176，slot_alignment 不变量=ALL 序连续）
        Self::TransitionProperty,
        Self::TransitionDuration,
        Self::TransitionTimingFunction,
        Self::TransitionDelay,
        Self::TransitionBehavior,
        // P3（ADR-0034）：vertical-align（ALL 尾追加；动画描述符槽整体
        // 后移 ×1 至 170..177，slot_alignment 不变量=ALL 序连续）
        Self::VerticalAlign,
        // P5（ADR-0036）：counter-reset/counter-increment/quotes（ALL 尾
        // 追加；动画描述符槽整体后移 ×3 至 173..180）
        Self::CounterReset,
        Self::CounterIncrement,
        Self::Quotes,
        // P9-3（css-lists-3）：list-style 三长手（ALL 尾追加；动画描述符
        // 槽整体后移 ×3 至 176..183，slot_alignment 不变量=ALL 序连续）
        Self::ListStyleType,
        Self::ListStylePosition,
        Self::ListStyleImage,
    ];

    /// Total number of slot-storage slots: all 176 entries of ALL (0..139 in
    /// original order, the 7 F2 text additions, 6 F3b background additions,
    /// 5 F3d border-image additions, 5 F3d font additions, the two F4
    /// properties, the five G1 transition longhands, P3 vertical-align, the
    /// three P5 counter properties, and the three P9-3 list properties, all
    /// explicitly numbered in slot()) plus 7 animation descriptor slots (not
    /// in ALL).
    pub const SLOT_COUNT: usize = 183;

    /// Slot-storage index (used by ComputedStyle's `Vec<Option<DeclValue>>`).
    /// 0..139 = ALL in original order; 139..146 = F2 text additions (ALL tail
    /// members); 146..152 = F3b background additions; 152..164 = F3d
    /// border-image + font additions + the two F4 properties; 164..169 = the
    /// five G1 transition longhands (ALL tail members); 169 = P3
    /// vertical-align (ALL tail member); 170..173 = the three P5 counter
    /// properties (ALL tail members); 173..176 = the three P9-3 list
    /// properties (ALL tail members); 176..183 = animation descriptors (not
    /// in ALL).
    /// clip-path reuses the original ALL slot 71 (batch 4 ④ placeholder,
    /// upgraded in place by F3c, ADR-0025).
    /// The `slot_alignment` test locks this
    /// table's consistency with ALL — when adding a variant you must extend
    /// both this match and SLOT_COUNT.
    pub fn slot(self) -> usize {
        match self {
            Self::Display => 0,
            Self::Position => 1,
            Self::Top => 2,
            Self::Right => 3,
            Self::Bottom => 4,
            Self::Left => 5,
            Self::ZIndex => 6,
            Self::Width => 7,
            Self::Height => 8,
            Self::MinWidth => 9,
            Self::MinHeight => 10,
            Self::MaxWidth => 11,
            Self::MaxHeight => 12,
            Self::AspectRatio => 13,
            Self::MarginTop => 14,
            Self::MarginRight => 15,
            Self::MarginBottom => 16,
            Self::MarginLeft => 17,
            Self::PaddingTop => 18,
            Self::PaddingRight => 19,
            Self::PaddingBottom => 20,
            Self::PaddingLeft => 21,
            Self::Gap => 22,
            Self::RowGap => 23,
            Self::ColumnGap => 24,
            Self::FlexDirection => 25,
            Self::FlexWrap => 26,
            Self::FlexGrow => 27,
            Self::FlexShrink => 28,
            Self::FlexBasis => 29,
            Self::JustifyContent => 30,
            Self::AlignItems => 31,
            Self::AlignSelf => 32,
            Self::AlignContent => 33,
            Self::GridTemplateColumns => 34,
            Self::GridTemplateRows => 35,
            Self::GridAutoFlow => 36,
            Self::GridAutoRows => 37,
            Self::GridAutoColumns => 38,
            Self::ColumnCount => 39,
            Self::ColumnWidth => 40,
            Self::BreakInside => 41,
            Self::ColumnSpan => 42,
            Self::ColumnRuleWidth => 43,
            Self::ColumnRuleStyle => 44,
            Self::ColumnRuleColor => 45,
            Self::BackgroundColor => 46,
            Self::BackgroundImage => 47,
            Self::BorderTopLeftRadius => 48,
            Self::BorderTopRightRadius => 49,
            Self::BorderBottomRightRadius => 50,
            Self::BorderBottomLeftRadius => 51,
            Self::BorderTopWidth => 52,
            Self::BorderRightWidth => 53,
            Self::BorderBottomWidth => 54,
            Self::BorderLeftWidth => 55,
            Self::BorderTopStyle => 56,
            Self::BorderRightStyle => 57,
            Self::BorderBottomStyle => 58,
            Self::BorderLeftStyle => 59,
            Self::BorderTopColor => 60,
            Self::BorderRightColor => 61,
            Self::BorderBottomColor => 62,
            Self::BorderLeftColor => 63,
            Self::BoxShadow => 64,
            Self::Opacity => 65,
            Self::OverflowX => 66,
            Self::OverflowY => 67,
            Self::BoxSizing => 68,
            Self::Transform => 69,
            Self::Filter => 70,
            Self::ClipPath => 71,
            Self::WillChange => 72,
            Self::Isolation => 73,
            Self::MixBlendMode => 74,
            Self::TransformOrigin => 75,
            Self::Color => 76,
            Self::FontFamily => 77,
            Self::FontSize => 78,
            Self::FontWeight => 79,
            Self::FontStyle => 80,
            Self::LineHeight => 81,
            Self::TextAlign => 82,
            Self::WhiteSpace => 83,
            Self::LetterSpacing => 84,
            Self::ContainerType => 85,
            Self::ContainerName => 86,
            // 行为提示属性（A1）：宿主可读非绘制通道
            Self::Cursor => 87,
            Self::UserSelect => 88,
            Self::PointerEvents => 89,
            Self::CaretColor => 90,
            Self::AccentColor => 91,
            // outline（A2）：不占布局的装饰描边（ink overflow）
            Self::OutlineWidth => 92,
            Self::OutlineStyle => 93,
            Self::OutlineColor => 94,
            Self::OutlineOffset => 95, // 逻辑属性（A8）：96..125 = css-logical-1 与 direction/unicode-bidi
            Self::Direction => 96,
            Self::UnicodeBidi => 97,
            Self::MarginInlineStart => 98,
            Self::MarginInlineEnd => 99,
            Self::MarginBlockStart => 100,
            Self::MarginBlockEnd => 101,
            Self::PaddingInlineStart => 102,
            Self::PaddingInlineEnd => 103,
            Self::PaddingBlockStart => 104,
            Self::PaddingBlockEnd => 105,
            Self::InsetInlineStart => 106,
            Self::InsetInlineEnd => 107,
            Self::InsetBlockStart => 108,
            Self::InsetBlockEnd => 109,
            Self::BorderInlineStartWidth => 110,
            Self::BorderInlineEndWidth => 111,
            Self::BorderBlockStartWidth => 112,
            Self::BorderBlockEndWidth => 113,
            Self::BorderInlineStartStyle => 114,
            Self::BorderInlineEndStyle => 115,
            Self::BorderBlockStartStyle => 116,
            Self::BorderBlockEndStyle => 117,
            Self::BorderInlineStartColor => 118,
            Self::BorderInlineEndColor => 119,
            Self::BorderBlockStartColor => 120,
            Self::BorderBlockEndColor => 121,
            Self::BorderStartStartRadius => 122,
            Self::BorderStartEndRadius => 123,
            Self::BorderEndStartRadius => 124,
            Self::BorderEndEndRadius => 125,
            // content（C1）：ALL 末位
            Self::Content => 126,
            // C2 文本三属性
            Self::TextTransform => 127,
            Self::OverflowWrap => 128,
            Self::WordBreak => 129,
            // C3 替换内容两属性
            Self::ObjectFit => 130,
            Self::ObjectPosition => 131,
            // E4 浮动两属性（ADR-0019）
            Self::Float => 132,
            Self::Clear => 133,
            // E5 grid-template-areas 与放置四长手（ADR-0020）
            Self::GridTemplateAreas => 134,
            Self::GridRowStart => 135,
            Self::GridRowEnd => 136,
            Self::GridColumnStart => 137,
            Self::GridColumnEnd => 138,
            // F2（ADR-0022 D2/D3/D4）：text-overflow/line-clamp/decoration
            // 槽（ALL 尾后）。
            Self::TextOverflow => 139,
            Self::WebkitLineClamp => 140,
            Self::TextDecorationLine => 141,
            Self::TextDecorationStyle => 142,
            Self::TextDecorationColor => 143,
            Self::TextDecorationThickness => 144,
            Self::TextShadow => 145,
            // F3b（ADR-0024）：background 全集六长手（=ALL 尾位，
            // slot==ALL 位序；描述符让位其后）。clip-path 原位 71 不动。
            Self::BackgroundRepeat => 146,
            Self::BackgroundAttachment => 147,
            Self::BackgroundPosition => 148,
            Self::BackgroundSize => 149,
            Self::BackgroundOrigin => 150,
            Self::BackgroundClip => 151,
            // F3d（ADR-0026）：border-image 五长手 + 字体深化五属性
            //（=ALL 尾位，slot==ALL 位序；动画描述符让位其后 ×10）。
            Self::BorderImageSource => 152,
            Self::BorderImageSlice => 153,
            Self::BorderImageWidth => 154,
            Self::BorderImageOutset => 155,
            Self::BorderImageRepeat => 156,
            Self::FontStretch => 157,
            Self::WordSpacing => 158,
            Self::FontFeatures => 159,
            Self::FontVariations => 160,
            Self::FontVariantCaps => 161,
            // G1（ADR-0032）：transition 五长手（=ALL 尾位，slot==ALL 位序；
            // 动画描述符让位其后 ×5）
            Self::TransitionProperty => 164,
            Self::TransitionDuration => 165,
            Self::TransitionTimingFunction => 166,
            Self::TransitionDelay => 167,
            Self::TransitionBehavior => 168,
            // P3（ADR-0034）：vertical-align（ALL 尾位；描述符让位 ×1）
            Self::VerticalAlign => 169,
            // P5（ADR-0036）：counter-reset/counter-increment/quotes
            // （ALL 尾位；描述符让位 ×3）
            Self::CounterReset => 170,
            Self::CounterIncrement => 171,
            Self::Quotes => 172,
            // P9-3（css-lists-3）：list-style 三长手（ALL 尾位；描述符
            // 让位 ×3）
            Self::ListStyleType => 173,
            Self::ListStylePosition => 174,
            Self::ListStyleImage => 175,
            // 动画描述符（非 ALL 成员；声明/采样时落槽；P3 追加后整体
            // 后移 ×1，P5 再 ×3，P9-3 再 ×3）
            Self::AnimationName => 176,
            Self::AnimationDuration => 177,
            Self::AnimationDelay => 178,
            Self::AnimationIterationCount => 179,
            Self::AnimationTimingFunction => 180,
            Self::AnimationDirection => 181,
            Self::AnimationFillMode => 182,
            Self::Hyphens => 162,
            Self::BackdropFilter => 163,
        }
    }

    /// CSS property name (lowercase). Shared by parsing and diagnostics.
    pub fn css_name(self) -> &'static str {
        match self {
            Self::Display => "display",
            Self::Position => "position",
            Self::Top => "top",
            Self::Right => "right",
            Self::Bottom => "bottom",
            Self::Left => "left",
            Self::ZIndex => "z-index",
            Self::AnimationName => "animation-name",
            Self::AnimationDuration => "animation-duration",
            Self::AnimationDelay => "animation-delay",
            Self::AnimationIterationCount => "animation-iteration-count",
            Self::AnimationTimingFunction => "animation-timing-function",
            Self::AnimationDirection => "animation-direction",
            Self::TextOverflow => "text-overflow",
            Self::WebkitLineClamp => "-webkit-line-clamp",
            Self::TextDecorationLine => "text-decoration-line",
            Self::TextDecorationStyle => "text-decoration-style",
            Self::TextDecorationColor => "text-decoration-color",
            Self::TextDecorationThickness => "text-decoration-thickness",
            Self::TextShadow => "text-shadow",
            Self::AnimationFillMode => "animation-fill-mode",
            Self::Hyphens => "hyphens",
            Self::BackdropFilter => "backdrop-filter",
            Self::TransitionProperty => "transition-property",
            Self::TransitionDuration => "transition-duration",
            Self::TransitionTimingFunction => "transition-timing-function",
            Self::VerticalAlign => "vertical-align",
            Self::CounterReset => "counter-reset",
            Self::CounterIncrement => "counter-increment",
            Self::Quotes => "quotes",
            Self::ListStyleType => "list-style-type",
            Self::ListStylePosition => "list-style-position",
            Self::ListStyleImage => "list-style-image",
            Self::TransitionDelay => "transition-delay",
            Self::TransitionBehavior => "transition-behavior",
            Self::Width => "width",
            Self::Height => "height",
            Self::MinWidth => "min-width",
            Self::MinHeight => "min-height",
            Self::MaxWidth => "max-width",
            Self::MaxHeight => "max-height",
            Self::AspectRatio => "aspect-ratio",
            Self::MarginTop => "margin-top",
            Self::MarginRight => "margin-right",
            Self::MarginBottom => "margin-bottom",
            Self::MarginLeft => "margin-left",
            Self::PaddingTop => "padding-top",
            Self::PaddingRight => "padding-right",
            Self::PaddingBottom => "padding-bottom",
            Self::PaddingLeft => "padding-left",
            Self::Gap => "gap",
            Self::RowGap => "row-gap",
            Self::ColumnGap => "column-gap",
            Self::ColumnCount => "column-count",
            Self::ColumnWidth => "column-width",
            Self::BreakInside => "break-inside",
            Self::ColumnSpan => "column-span",
            Self::ColumnRuleWidth => "column-rule-width",
            Self::ColumnRuleStyle => "column-rule-style",
            Self::ColumnRuleColor => "column-rule-color",
            Self::FlexDirection => "flex-direction",
            Self::FlexWrap => "flex-wrap",
            Self::FlexGrow => "flex-grow",
            Self::FlexShrink => "flex-shrink",
            Self::FlexBasis => "flex-basis",
            Self::JustifyContent => "justify-content",
            Self::AlignItems => "align-items",
            Self::AlignSelf => "align-self",
            Self::AlignContent => "align-content",
            Self::GridTemplateColumns => "grid-template-columns",
            Self::GridTemplateRows => "grid-template-rows",
            Self::GridAutoFlow => "grid-auto-flow",
            Self::GridAutoRows => "grid-auto-rows",
            Self::GridAutoColumns => "grid-auto-columns",
            Self::BackgroundColor => "background-color",
            Self::BackgroundImage => "background-image",
            Self::BackgroundRepeat => "background-repeat",
            Self::BackgroundAttachment => "background-attachment",
            Self::BackgroundPosition => "background-position",
            Self::BackgroundSize => "background-size",
            Self::BackgroundOrigin => "background-origin",
            Self::BackgroundClip => "background-clip",
            Self::BorderImageSource => "border-image-source",
            Self::BorderImageSlice => "border-image-slice",
            Self::BorderImageWidth => "border-image-width",
            Self::BorderImageOutset => "border-image-outset",
            Self::BorderImageRepeat => "border-image-repeat",
            Self::FontStretch => "font-stretch",
            Self::WordSpacing => "word-spacing",
            Self::FontFeatures => "font-feature-settings",
            Self::FontVariations => "font-variation-settings",
            Self::FontVariantCaps => "font-variant-caps",
            Self::BorderTopLeftRadius => "border-top-left-radius",
            Self::BorderTopRightRadius => "border-top-right-radius",
            Self::BorderBottomRightRadius => "border-bottom-right-radius",
            Self::BorderBottomLeftRadius => "border-bottom-left-radius",
            Self::BorderTopWidth => "border-top-width",
            Self::BorderRightWidth => "border-right-width",
            Self::BorderBottomWidth => "border-bottom-width",
            Self::BorderLeftWidth => "border-left-width",
            Self::BorderTopStyle => "border-top-style",
            Self::BorderRightStyle => "border-right-style",
            Self::BorderBottomStyle => "border-bottom-style",
            Self::BorderLeftStyle => "border-left-style",
            Self::BorderTopColor => "border-top-color",
            Self::BorderRightColor => "border-right-color",
            Self::BorderBottomColor => "border-bottom-color",
            Self::BorderLeftColor => "border-left-color",
            Self::BoxShadow => "box-shadow",
            Self::Opacity => "opacity",
            Self::OverflowX => "overflow-x",
            Self::OverflowY => "overflow-y",
            Self::BoxSizing => "box-sizing",
            Self::Transform => "transform",
            Self::Filter => "filter",
            Self::ClipPath => "clip-path",
            Self::WillChange => "will-change",
            Self::Isolation => "isolation",
            Self::MixBlendMode => "mix-blend-mode",
            Self::TransformOrigin => "transform-origin",
            Self::Color => "color",
            Self::FontFamily => "font-family",
            Self::FontSize => "font-size",
            Self::FontWeight => "font-weight",
            Self::FontStyle => "font-style",
            Self::LineHeight => "line-height",
            Self::TextAlign => "text-align",
            Self::WhiteSpace => "white-space",
            Self::LetterSpacing => "letter-spacing",
            Self::ContainerType => "container-type",
            Self::ContainerName => "container-name",
            Self::Cursor => "cursor",
            Self::UserSelect => "user-select",
            Self::PointerEvents => "pointer-events",
            Self::CaretColor => "caret-color",
            Self::AccentColor => "accent-color",
            Self::OutlineWidth => "outline-width",
            Self::OutlineStyle => "outline-style",
            Self::OutlineColor => "outline-color",
            Self::OutlineOffset => "outline-offset",
            Self::Direction => "direction",
            Self::UnicodeBidi => "unicode-bidi",
            Self::MarginInlineStart => "margin-inline-start",
            Self::MarginInlineEnd => "margin-inline-end",
            Self::MarginBlockStart => "margin-block-start",
            Self::MarginBlockEnd => "margin-block-end",
            Self::PaddingInlineStart => "padding-inline-start",
            Self::PaddingInlineEnd => "padding-inline-end",
            Self::PaddingBlockStart => "padding-block-start",
            Self::PaddingBlockEnd => "padding-block-end",
            Self::InsetInlineStart => "inset-inline-start",
            Self::InsetInlineEnd => "inset-inline-end",
            Self::InsetBlockStart => "inset-block-start",
            Self::InsetBlockEnd => "inset-block-end",
            Self::BorderInlineStartWidth => "border-inline-start-width",
            Self::BorderInlineEndWidth => "border-inline-end-width",
            Self::BorderBlockStartWidth => "border-block-start-width",
            Self::BorderBlockEndWidth => "border-block-end-width",
            Self::BorderInlineStartStyle => "border-inline-start-style",
            Self::BorderInlineEndStyle => "border-inline-end-style",
            Self::BorderBlockStartStyle => "border-block-start-style",
            Self::BorderBlockEndStyle => "border-block-end-style",
            Self::BorderInlineStartColor => "border-inline-start-color",
            Self::BorderInlineEndColor => "border-inline-end-color",
            Self::BorderBlockStartColor => "border-block-start-color",
            Self::BorderBlockEndColor => "border-block-end-color",
            Self::BorderStartStartRadius => "border-start-start-radius",
            Self::BorderStartEndRadius => "border-start-end-radius",
            Self::BorderEndStartRadius => "border-end-start-radius",
            Self::BorderEndEndRadius => "border-end-end-radius",
            Self::Content => "content",
            Self::TextTransform => "text-transform",
            Self::OverflowWrap => "overflow-wrap",
            Self::WordBreak => "word-break",
            Self::ObjectFit => "object-fit",
            Self::ObjectPosition => "object-position",
            Self::Float => "float",
            Self::Clear => "clear",
            Self::GridTemplateAreas => "grid-template-areas",
            Self::GridRowStart => "grid-row-start",
            Self::GridRowEnd => "grid-row-end",
            Self::GridColumnStart => "grid-column-start",
            Self::GridColumnEnd => "grid-column-end",
        }
    }

    /// CSS property name → PropertyId (case-insensitive). Shorthand names
    /// return None (they are handled by the shorthand expander). Animation
    /// descriptor longhands (slots 96..103) are not in ALL (excluded from
    /// full-inventory materialization — they only land in a slot when
    /// declared), but longhand declarations must still be routable
    /// (css-animations-1: the animation-* longhands are first-class
    /// properties), hence the supplemental table merged into the sorted
    /// index. Lookup uses a OnceLock lazily built sorted index + binary
    /// search (O(log n); ALL/the supplemental table remain the single
    /// source).
    pub fn from_css_name(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        let index = Self::sorted_name_index();
        let idx = index
            .binary_search_by_key(&lower.as_str(), |(n, _)| *n)
            .ok()?;
        Some(index[idx].1)
    }

    /// Sorted name index (ALL ∪ animation descriptor longhands ∪ the
    /// word-wrap alias), built lazily once. Keys are ASCII lowercase
    /// literals, ordered the same way as the lookup key's
    /// `to_ascii_lowercase`.
    fn sorted_name_index() -> &'static [(&'static str, PropertyId)] {
        static INDEX: std::sync::OnceLock<Vec<(&'static str, PropertyId)>> =
            std::sync::OnceLock::new();
        INDEX.get_or_init(|| {
            let mut v: Vec<(&'static str, PropertyId)> =
                Self::ALL.iter().map(|id| (id.css_name(), *id)).collect();
            // 动画描述符长手（不在 ALL）+ overflow-wrap 的 legacy 别名
            //（css-text-3）。
            v.extend([
                ("animation-name", PropertyId::AnimationName),
                ("animation-duration", PropertyId::AnimationDuration),
                ("animation-delay", PropertyId::AnimationDelay),
                (
                    "animation-iteration-count",
                    PropertyId::AnimationIterationCount,
                ),
                (
                    "animation-timing-function",
                    PropertyId::AnimationTimingFunction,
                ),
                ("animation-direction", PropertyId::AnimationDirection),
                ("animation-fill-mode", PropertyId::AnimationFillMode),
                ("word-wrap", PropertyId::OverflowWrap),
            ]);
            v.sort_unstable_by_key(|(n, _)| *n);
            v
        })
    }
}

/// direction value family (A8, css-writing-modes-4): basis for the
/// logical→physical mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DirectionKind {
    /// ltr — inline direction left→right (default).
    Ltr,
    /// rtl — inline direction right→left.
    Rtl,
}

/// unicode-bidi value family (A8): text stack hint (consumed by the engine's
/// text leaves = a Tier B hint; readable from ComputedStyle for host /
/// rich-text pipelines).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnicodeBidiKind {
    /// normal (default).
    Normal,
    /// embed.
    Embed,
    /// isolate.
    Isolate,
    /// bidi-override.
    BidiOverride,
    /// isolate-override.
    IsolateOverride,
    /// plaintext.
    Plaintext,
}

/// content sequence piece (P5, ADR-0036 D1): a concatenation unit of
/// generated content.
/// Evaluation semantics live in the engine's sync_pseudo_text (counter
/// stack / quote depth / attrs).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ContentPiece {
    /// String literal.
    Str(String),
    /// counter(`<custom-ident>`, `<counter-style>`?) — innermost-scope value;
    /// style renders via the document-level @counter-style registry + builtin
    /// styles (css-counter-styles-3 §2/§6, see the counter_format module);
    /// unknown name → decimal.
    Counter {
        /// Counter name.
        name: String,
        /// Counter style grammar name (defaults to decimal; rendering
        /// semantics in counter_format).
        style: String,
    },
    /// counters(`<custom-ident>`, `<string>`, `<counter-style>`?) — all
    /// scopes, outermost to innermost, each formatted with style and joined.
    Counters {
        /// Counter name.
        name: String,
        /// Separator between levels.
        separator: String,
        /// Counter style grammar name (defaults to decimal; rendering
        /// semantics in counter_format).
        style: String,
    },
    /// attr(`<attr-name>`) — host element attribute; on a pseudo-element the
    /// originating element's attribute is used.
    Attr(String),
    /// open-quote — takes a pair from the computed quotes by quote depth;
    /// depth +1.
    OpenQuote,
    /// close-quote — depth −1 (clamped to 0 if <0), then takes a pair.
    CloseQuote,
    /// no-open-quote — depth +1 but nothing emitted.
    NoOpenQuote,
    /// no-close-quote — depth −1 but nothing emitted.
    NoCloseQuote,
}

/// content value family (C1, css-content-3; ADR-0015 MVP + the P5 ADR-0036
/// D1 sequence).
/// url()/element()/leader()/counter-style @-rules are not done (ADR-0036
/// D4).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ContentValue {
    /// none — no box generated.
    None,
    /// normal — initial; invalid on elements, generates nothing on
    /// pseudo-elements.
    Normal,
    /// String literal (single string; multiple strings / mixed use Seq).
    Str(String),
    /// P5 (ADR-0036 D1): a `<content-list>` sequence (strings / counter() /
    /// counters() / attr() / quote keywords, whitespace separated; an empty
    /// sequence is rejected).
    Seq(Vec<ContentPiece>),
}

/// text-transform value family (C2, css-text-3; ADR-0016): text case /
/// fullwidth transformation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum TextTransformKind {
    /// none — no transformation.
    #[default]
    None,
    /// uppercase — entire words uppercased.
    Uppercase,
    /// lowercase — entire words lowercased.
    Lowercase,
    /// capitalize — first letter of each word uppercased (word boundary ≈
    /// non-alphanumeric separator; FEATURES deviation entry: the spec uses
    /// UAX#29 word boundaries).
    Capitalize,
    /// full-width — latin/punctuation mapped to fullwidth (U+FF01-FF5E),
    /// space to U+3000.
    FullWidth,
    /// full-size-kana — small kana to regular kana (T2: accepted but not
    /// transformed, deviation entry documented in FEATURES).
    FullSizeKana,
}

/// overflow-wrap value family (C2, css-text-3; ADR-0016): line breaking of
/// overflowing long words (consumed natively by parley, one-to-one mapping
/// to parlance OverflowWrap).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum OverflowWrapKind {
    /// normal — breaks only at regular break points.
    #[default]
    Normal,
    /// break-word — breaks within a word at arbitrary points when needed
    /// (min-content is unaffected).
    BreakWord,
    /// anywhere — same as break-word and additionally participates in
    /// min-content computation.
    Anywhere,
}

/// hyphens value family (F4, ADR-0028 D2, css-text-3 §5.4): hyphenation
/// mode. initial=manual. Hyphenation effect boundary (upstream segmenter):
/// explicit break points (U+00AD soft hyphen / U+2010) are decided by
/// parley's UAX 14 segmentation (≈ the manual default semantics), `auto` has
/// no hyphenation dictionary, and `none` cannot suppress upstream break
/// points — all three values behave identically in v1, documented as Tier B
/// (the FEATURES hyphens entry); Revisit when: parley gains
/// hyphenation/break-point override APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum HyphensKind {
    /// manual — only explicit break points (soft hyphen U+00AD / hyphen
    /// U+2010) may break.
    #[default]
    Manual,
    /// none — hyphenation off (including explicit break points; v1 behaves
    /// the same as manual).
    None,
    /// auto — dictionary hyphenation (no upstream dictionary support; v1
    /// behaves the same as manual).
    Auto,
}

/// word-break value family (C2, css-text-3; ADR-0016): intra-word
/// line-breaking strength (consumed natively by parley, one-to-one mapping
/// to parlance WordBreak).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum WordBreakKind {
    /// normal — regular rules.
    #[default]
    Normal,
    /// break-all — breaks within words allowed.
    BreakAll,
    /// keep-all — breaks within words forbidden (including CJK gaps and
    /// hyphen seams).
    KeepAll,
}

/// object-fit value family (C3, css-images-3; ADR-0017): replaced content
/// fitting mode.
/// Semantics are defined by the scale s at which the source image (sw, sh)
/// fits into the content box (bw, bh) —
/// fill=stretch over the whole box; contain=s=min ratio (letterboxing);
/// cover=s=max ratio (overflow cropped); none=s=1 (natural size);
/// scale-down=s=min(1, min ratio).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ObjectFitKind {
    /// fill — stretched to the content box (default; aspect ratio not kept).
    #[default]
    Fill,
    /// contain — fits entirely (letterbox blank space).
    Contain,
    /// cover — covers the content box (overflow cropped).
    Cover,
    /// none — natural size (may overflow).
    None,
    /// scale-down — the smaller of none and contain.
    ScaleDown,
}

/// float value family (E4, css-position-3 / ADR-0019): direction floated out
/// of flow.
/// none=normal flow (default); left/right=float left/right out of flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum FloatKind {
    /// none — laid out in normal flow (default).
    #[default]
    None,
    /// left — floats left (box out of flow, right edge wrapped by following
    /// content).
    Left,
    /// right — floats right (box out of flow, left edge wrapped by following
    /// content).
    Right,
}

/// clear value family (E4, css-position-3 / ADR-0019): float clamp
/// direction.
/// none=no clamping (default); left/right/both = the box's y ≥ the bottom
/// edge of the last left/right/either-direction float.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ClearKind {
    /// none — no clamping (default).
    #[default]
    None,
    /// left — clamped below the bottom edge of the last left float.
    Left,
    /// right — clamped below the bottom edge of the last right float.
    Right,
    /// both — clamped below the bottom edge of the last float in either
    /// direction.
    Both,
}

// ---------- F3d（ADR-0026）：border-image 值族 + 字体深化值族 ----------

/// border-image-slice component (css-backgrounds-3): number (raster source =
/// source pixels, gradient source = border image area pixels) or percentage
/// (percentage of the source size).
/// The raw value is stored and resolved at paint time (no source dependency
/// at computed-value time).
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum BorderImageSliceComp {
    /// `<number [0,∞]>` — raster source pixels / gradient source area pixels.
    Number(f32),
    /// `<percentage [0,∞]>` — percentage of the source size.
    Percentage(f32),
}

/// border-image-slice: 1-4 values expanded TRBL + fill (the center region is
/// painted along with the edges).
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct BorderImageSlice {
    /// Four slice lines [top, right, bottom, left] (after TRBL expansion).
    pub slices: [BorderImageSliceComp; 4],
    /// fill — the center region is painted as ordinary background (placed
    /// under the edge regions).
    pub fill: bool,
}

/// border-image-width component: length-percentage (may be negative, clamped
/// to 0 at paint time), number (× the corresponding edge border-width), auto
/// (= the slice size).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum BorderImageWidthComp {
    /// `<length-percentage>` — absolute/relative painting-area edge width.
    Length(LengthPercentage),
    /// `<number [0,∞]>` — a multiple of the corresponding edge border-width.
    Number(f32),
    /// auto — uses the slice's own size.
    Auto,
}

/// border-image-width: 1-4 values expanded TRBL [top, right, bottom, left].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct BorderImageWidth {
    /// Four edge widths [top, right, bottom, left].
    pub comps: [BorderImageWidthComp; 4],
}

/// border-image-outset component: length-percentage (may be negative,
/// clamped to 0), number (× the corresponding edge border-width).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum BorderImageOutsetComp {
    /// `<length [0,∞]>` — absolute outset amount.
    Length(LengthPercentage),
    /// `<number [0,∞]>` — a multiple of the corresponding edge border-width.
    Number(f32),
}

/// border-image-outset: 1-4 values expanded TRBL [top, right, bottom, left].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct BorderImageOutset {
    /// Four edge outsets [top, right, bottom, left].
    pub comps: [BorderImageOutsetComp; 4],
}

/// border-image-repeat single-axis style. round/space are approximated by
/// repeat/stretch in v1 (Tier B documented, FEATURES — same boundary as the
/// F3b background space/round).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum BorderImageRepeatKind {
    /// stretch — the region image is stretched to fill (default).
    #[default]
    Stretch,
    /// repeat — tiled (last tile truncated when necessary).
    Repeat,
    /// round — an integer number of tiles stretched to fill (v1 ≈ repeat).
    Round,
    /// space — an integer number of tiles evenly spaced with gaps (v1 ≈
    /// repeat).
    Space,
}

/// border-image-repeat: x/y dual axes (a single value applies to both).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct BorderImageRepeatXY {
    /// Horizontal axis.
    pub x: BorderImageRepeatKind,
    /// Vertical axis.
    pub y: BorderImageRepeatKind,
}

/// font-variant-caps value family (css-fonts-4). The small-caps family's
/// four values are synthesized by the engine (lowercase→uppercase + font
/// size ×0.8, ADR-0026 D3); titling/unicase are folded into the OpenType
/// feature push.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum FontVariantCapsKind {
    /// normal — no transformation (default).
    #[default]
    Normal,
    /// small-caps — lowercase to small capitals (synthesized).
    SmallCaps,
    /// all-small-caps — everything to small capitals (synthesized).
    AllSmallCaps,
    /// petite-caps — lowercase to petite capitals (v1 synthesizes as
    /// small-caps, Tier B).
    PetiteCaps,
    /// all-petite-caps — everything to petite capitals (v1 as
    /// all-small-caps, Tier B).
    AllPetiteCaps,
    /// unicase — the 'unic' feature.
    Unicase,
    /// titling-caps — the 'titl' feature.
    TitlingCaps,
}

/// CSS wide keywords (B1, css-values-4): whole-value semantics, applicable
/// to every property.
/// Revert/RevertLayer roll back in the late cascade pass (cascade.rs
/// resolve_revert); Initial/Inherit/Unset are materialized at computed-value
/// time (computed.rs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WideKeyword {
    /// inherit — force-inherits the parent value (including non-inherited
    /// properties).
    Inherit,
    /// initial — initial value.
    Initial,
    /// unset — inherited properties = inherit, otherwise = initial.
    Unset,
    /// revert — rolls back to a lower cascade origin (author→user→UA→initial).
    Revert,
    /// revert-layer — rolls back to an earlier cascade layer (none → behaves
    /// as revert).
    RevertLayer,
}

/// mix-blend-mode value family (P1-2, css-compositing-1 §3 + css-compositing-2
/// plus-lighter): 16 standard blend modes + plus-lighter/darker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendMode {
    /// normal —— alpha compositing only (no blending; isolation groups are
    /// still provided by layer pairs).
    Normal,
    /// multiply —— Cb×Cs (multiply).
    Multiply,
    /// screen —— Cb+Cs−Cb×Cs.
    Screen,
    /// overlay —— Cs≤0.5 uses multiply, otherwise screen (HardLight
    /// commuted).
    Overlay,
    /// darken —— per-channel min.
    Darken,
    /// lighten —— per-channel max.
    Lighten,
    /// color-dodge —— brightens the backdrop (Cb/(1−Cs), clamped to 1).
    ColorDodge,
    /// color-burn —— darkens the backdrop (1−(1−Cb)/Cs, clamped to 0).
    ColorBurn,
    /// hard-light —— Cs≤0.5 uses multiply, otherwise screen.
    HardLight,
    /// soft-light —— W3C soft light (piecewise D functions).
    SoftLight,
    /// difference —— |Cb−Cs|.
    Difference,
    /// exclusion —— Cb+Cs−2·Cb·Cs.
    Exclusion,
    /// hue —— Cs hue + Cb saturation/luminosity (non-separable).
    Hue,
    /// saturation —— Cs saturation + Cb hue/luminosity (non-separable).
    Saturation,
    /// color —— Cs hue/saturation + Cb luminosity (non-separable).
    Color,
    /// luminosity —— Cb luminosity + Cs hue/saturation (non-separable).
    Luminosity,
    /// plus-lighter (css-compositing-2) —— premultiplied addition.
    PlusLighter,
    /// plus-darker (PDF/CG) —— premultiplied max(0, Db+Ds−1).
    PlusDarker,
}

/// Value families: declaration value representations reused by grammar shape
/// (no shorthands; shorthands are expanded at parse time).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum DeclValue {
    /// CSS wide keywords as whole values (B1:
    /// initial/inherit/unset/revert/revert-layer).
    WideKeyword(WideKeyword),
    /// direction (A8).
    Direction(DirectionKind),
    /// unicode-bidi (A8).
    UnicodeBidi(UnicodeBidiKind),
    /// hyphens hyphenation mode (F4, ADR-0028 D2).
    Hyphens(HyphensKind),
    /// margin/padding/gap etc. (letter-spacing's normal → None → 0).
    Len(LengthPercentage),
    /// width/height/min-*/max-*/flex-basis/top..left (auto → None).
    LenAuto(Option<LengthPercentage>),
    /// opacity/flex-grow/flex-shrink/z-index/font-weight.
    Number(f32),
    /// font-weight relative keywords (css-fonts-4 §2.2.1): bolder → true,
    /// lighter → false. During cascade materialization the parent computed
    /// weight is looked up via [`relative_font_weight`] and finalized into
    /// [`DeclValue::Number`] (the computed value is always an absolute
    /// weight; consumers and the transition layer only ever see Number).
    RelativeFontWeight(bool),
    /// font-size relative keywords (css-fonts-4 `<relative-size>`): larger →
    /// true, smaller → false. During cascade materialization the parent
    /// computed size is finalized via [`relative_font_size`] into `Len(Px)`
    /// (the computed value is always absolute px; consumers only see Len).
    RelativeFontSize(bool),
    /// color/background-color/border-*-color.
    Color(ColorValue),
    /// display value family.
    Display(Display),
    /// position value family.
    Position(Position),
    /// overflow-x/y value family.
    Overflow(Overflow),
    /// box-sizing value family.
    BoxSizing(BoxSizing),
    /// transform — list of 2D affine functions (none → empty list).
    Transform(Vec<TransformFn>),
    /// transform-origin (batch 5 ⑬): horizontal/vertical pair of components
    /// (length-percentage, keywords normalized to percentages at parse
    /// time), initial 50% 50%.
    TransformOrigin(LengthPercentage, LengthPercentage),
    /// Corner radii (batch 5 ⑪ elliptical corners): per corner (horizontal,
    /// vertical) pair of components — in the border-radius slash syntax `/`
    /// the parts before/after are horizontal/vertical radii, defaulting to
    /// vertical=horizontal (circular corners).
    Radius(LengthPercentage, LengthPercentage),
    /// Alignment value family shared by justify-content/align-*.
    Align(Align),
    /// flex-direction value family.
    FlexDirection(FlexDirection),
    /// flex-wrap value family.
    FlexWrap(FlexWrap),
    /// border-*-style line style value family (reused by column-rule-style).
    BorderStyle(BorderStyle),
    /// text-align value family.
    TextAlign(TextAlign),
    /// white-space value family.
    WhiteSpace(WhiteSpace),
    /// font-style value family.
    FontStyle(FontStyle),
    /// line-height value family.
    LineHeight(LineHeight),
    /// font-family — ordered font family list.
    FontFamily(FontFamilyList),
    /// grid-template-*/grid-auto-* — track lists.
    GridTracks(GridTemplate),
    /// aspect-ratio: none → None, otherwise width/height ratio.
    AspectRatio(Option<f32>),
    /// background-image — none|url()|gradients (F3b: comma-separated layer
    /// list).
    BackgroundImage(Vec<BackgroundImage>),
    /// background-repeat — layered list of tiling styles (F3b, ADR-0024).
    BackgroundRepeat(Vec<RepeatXY>),
    /// background-attachment — layered attachment list (F3b, ADR-0024).
    BackgroundAttachment(Vec<Attachment>),
    /// background-position — layered position list (F3b, ADR-0024).
    BackgroundPosition(Vec<Position2D>),
    /// background-size — layered size list (F3b, ADR-0024).
    BackgroundSize(Vec<BgSize>),
    /// background-origin — layered positioning-box list (F3b, ADR-0024).
    BackgroundOrigin(Vec<BackgroundBox>),
    /// background-clip — layered painting-box list (F3b, ADR-0024).
    BackgroundClip(Vec<BackgroundClip>),
    /// clip-path — clip shape (F3c, ADR-0025).
    ClipPath(ClipShape),
    /// box-shadow — shadow list (none → empty list).
    BoxShadows(BoxShadowList),
    /// grid-auto-flow.
    GridAutoFlow(GridAutoFlowKind),
    /// grid-template-areas (E5, ADR-0020): area template.
    GridAreas(GridAreas),
    /// grid-{row,column}-{start,end} (E5, ADR-0020): placement line spec.
    GridLine(GridLineSpec),
    /// border-*-width: none → None (width zeroed); thin/medium/thick → fixed
    /// values.
    BorderWidth(Option<LengthPercentage>),
    /// z-index: auto → None (cascade-absent equivalent; "present and Some"
    /// is the future ADR-0008 criterion for creating a stacking context),
    /// number → Some.
    ZIndex(Option<f32>),
    /// column-count (phase 2 ③): auto → None; `<integer [1,∞]>` → Some
    /// (0/negative/non-integer is an invalid declaration, dropped at parse
    /// time).
    ColumnCount(Option<u16>),
    /// break-inside (phase 3 ⑤a): avoid → Some(true), auto → Some(false).
    /// v1 contract: all blocks are unconditionally packed unbreakable (avoid
    /// is the engine's default semantics); auto's cross-column fragmentation
    /// semantics are not done (FEATURES.md Tier B boundary). Parsed and
    /// stored only, layout does not read it — kept for conformance cases and
    /// alignment with Chromium's line-breaking model.
    BreakInside(Option<bool>),
    /// column-span (phase 3 ⑤b): all → Some(true), none → Some(false);
    /// initial none. An `all` child cuts the column flow, and the segments
    /// before/after are balanced independently (the engine's row-wrapping
    /// model, FEATURES.md ⑤b boundary).
    ColumnSpan(Option<bool>),
    /// column-rule-width (phase 3 ⑤c): thin/medium/thick → fixed 1/3/5px;
    /// there is no none keyword (the rule's presence is expressed via
    /// column-rule-style: none), initial medium (the engine falls back to
    /// 3px when the declaration is absent).
    ColumnRuleWidth(Option<LengthPercentage>),
    /// column-rule-style (phase 3 ⑤c): reuses the BorderStyle value family,
    /// initial none.
    ColumnRuleStyle(BorderStyle),
    // 动画描述符（第五批⑰）：不可动画、不参与 DeclValue 插值
    /// animation-name: none → None.
    AnimationName(Option<String>),
    /// animation-duration / animation-delay (seconds; a negative delay is
    /// legal).
    AnimationTime(f32),
    /// animation-iteration-count: infinite → f32::INFINITY.
    AnimationIteration(f32),
    /// animation-timing-function — the easing function.
    AnimationTiming(TimingFn),
    /// animation-direction — the playback direction.
    AnimationDirection(AnimDirection),
    /// animation-fill-mode — style application outside the animation's
    /// active interval.
    AnimationFillMode(AnimFillMode),
    /// animation-name multi-value list (P7-②): `[ none | <custom-ident> ]#`.
    /// A `None` item = none (that group has no animation). The parser always
    /// produces a list (single value = single item).
    AnimationNameList(SmallVec<[Option<String>; 2]>),
    /// animation-duration / animation-delay multi-value lists (P7-②):
    /// `<time>#`, seconds. duration rejects negatives, delay allows them
    /// (fast-forward semantics).
    AnimationTimeList(SmallVec<[f32; 2]>),
    /// animation-iteration-count multi-value list (P7-②): `[ <number> | infinite ]#`.
    AnimationIterationList(SmallVec<[f32; 2]>),
    /// animation-timing-function multi-value list (P7-②): `<easing-function>#`.
    AnimationTimingList(SmallVec<[TimingFn; 2]>),
    /// animation-direction multi-value list (P7-②): `<single-animation-direction>#`.
    AnimationDirectionList(SmallVec<[AnimDirection; 2]>),
    /// animation-fill-mode multi-value list (P7-②): `<single-animation-fill-mode>#`.
    AnimationFillModeList(SmallVec<[AnimFillMode; 2]>),
    /// transition-property — list of transitionable property names
    /// (none|all|custom-ident#).
    TransitionProperty(TransitionPropertyList),
    /// transition-duration / transition-delay — duration/delay lists,
    /// seconds.
    TransitionTime(TransitionTimeList),
    /// transition-timing-function — easing function list.
    TransitionTiming(TransitionTimingList),
    /// transition-behavior — discrete-property transition policy (single
    /// value, not a list).
    TransitionBehavior(TransitionBehavior),
    /// vertical-align (P3, ADR-0034 D3): inline-participant vertical
    /// alignment value family.
    VerticalAlign(VerticalAlignKind),
    /// counter-reset / counter-increment (P5, ADR-0036 D2): counter
    /// `[(<custom-ident>, <integer>)]` list (none → empty Vec). reset's
    /// integer defaults to 0, increment's to 1 (materialized at parse time,
    /// uniform value shape).
    CounterList(Vec<(String, i64)>),
    /// quotes (P5, ADR-0036 D3): quote pair table.
    Quotes(QuotesValue),
    /// list-style-type (P9-3, css-lists-3 §3.4): `None` = the none keyword
    /// (marker suppressed); `Some(Name)` = counter style name (unknown names
    /// fall back to decimal at use time, css-counter-styles-3 §2);
    /// `Some(Str)` = literal string marker (no prefix/suffix added).
    ListStyleType(Option<ListStyleTypeValue>),
    /// list-style-position (P9-3, §3.5): inside|outside.
    ListStylePosition(ListStylePosition),
    /// list-style-image (P9-3, §3.3): `None` = none; `Some` = marker image
    /// (reuses the BackgroundImage value shape: url()/gradients).
    ListStyleImage(Option<BackgroundImage>),
    /// filter / backdrop-filter (P2 batch, ADR-0031 D1): ordered filter
    /// function chain (`Filters(vec![])` = none, a valid declaration with
    /// explicitly no filter). The old Effect(bool) presence semantics are
    /// retired (will-change/isolation still use Effect).
    Filters(Vec<FilterFn>),
    /// filter presence (batch 4 ④): true = value ≠ none, purely an SC
    /// trigger semantic bit (the ADR-0008 full set); carries and implements
    /// no filter effect.
    /// (clip-path has been upgraded to the ClipPath(ClipShape) shape value,
    /// F3c, ADR-0025.)
    Effect(bool),
    /// mix-blend-mode (P1-2): concrete blend mode (16 standard modes +
    /// plus-lighter/darker; normal = BlendMode::Normal).
    BlendMode(BlendMode),
    /// container-type (stage 2 ③): normal|size|inline-size.
    ContainerType(ContainerType),
    /// container-name (stage 2 ③): none → empty Vec; custom-ident# → name
    /// list.
    ContainerName(Vec<String>),
    /// cursor (A1 behavior hint): host pointer shape (keyword subset,
    /// url()=T2).
    Cursor(CursorKind),
    /// user-select (A1): text selectability semantics (not inherited).
    UserSelect(UserSelectKind),
    /// pointer-events (A1): hit-testing pass-through semantics.
    PointerEvents(PointerEventsKind),
    /// caret-color (A1): None = auto; Some = concrete color.
    CaretColor(Option<ColorValue>),
    /// accent-color (A1): None = auto; Some = widget accent color.
    AccentColor(Option<ColorValue>),
    /// outline-style (A2): line style value family + auto (host focus ring
    /// semantic bit).
    OutlineStyle(OutlineStyle),
    /// content (C1): pseudo-element generated content (invalid on host
    /// nodes, consumed on pseudo-elements).
    Content(ContentValue),
    /// text-transform (C2): text case/fullwidth transformation (inherited).
    TextTransform(TextTransformKind),
    /// overflow-wrap (C2): line breaking of overflowing long words (consumed
    /// natively by parley).
    OverflowWrap(OverflowWrapKind),
    /// word-break (C2): intra-word line-breaking strength (consumed natively
    /// by parley).
    WordBreak(WordBreakKind),
    /// object-fit (C3, css-images-3): replaced content fitting mode.
    ObjectFit(ObjectFitKind),
    /// object-position (C3, css-images-3): alignment of replaced content
    /// within its box (x, y).
    ObjectPosition(LengthPercentage, LengthPercentage),
    /// text-overflow (F2, ADR-0022 D2).
    TextOverflow(TextOverflowKind),
    /// -webkit-line-clamp line count (F2, ADR-0022 D3; 0=none).
    WebkitLineClamp(u32),
    /// text-decoration-line bit set (F2, ADR-0022 D4; 1=underline 2=overline
    /// 4=line-through).
    TextDecorationLine(u8),
    /// text-decoration-style (F2, ADR-0022 D4).
    TextDecorationStyle(TextDecoStyleKind),
    /// text-decoration-thickness (F2, ADR-0022 D4).
    TextDecorationThickness(TextDecoThickness),
    /// text-shadow shadow list (F2, ADR-0022 D5; empty=none).
    TextShadow(Vec<TextShadowSpec>),
    /// float (E4, css-position-3 / ADR-0019): floats out of flow.
    Float(FloatKind),
    /// clear (E4, css-position-3 / ADR-0019): clamps against floats.
    Clear(ClearKind),
    /// border-image-source — none|url()|gradient (reuses the background image
    /// single-layer value family).
    BorderImageSource(BackgroundImage),
    /// border-image-slice — four slice lines into the source
    /// (number=pixels/percentage) + fill.
    BorderImageSlice(BorderImageSlice),
    /// border-image-width — painting-area edge widths
    /// (length|number×border-width|auto).
    BorderImageWidth(BorderImageWidth),
    /// border-image-outset — painting-area outsets (length|number×border-width).
    BorderImageOutset(BorderImageOutset),
    /// border-image-repeat — region tiling (x/y dual axes).
    BorderImageRepeat(BorderImageRepeatXY),
    /// font-stretch — width axis (percent normalized to 50..=200,
    /// normal=100).
    FontStretch(f32),
    /// word-spacing — word spacing (normal → None → 0; % base = font-size).
    WordSpacing(Option<LengthPercentage>),
    /// font-feature-settings — OpenType feature (tag, value) list.
    FontFeatures(Vec<([u8; 4], u16)>),
    /// font-variation-settings — variable font axis (tag, value) list.
    FontVariations(Vec<([u8; 4], f32)>),
    /// font-variant-caps — capital-form variant (small-caps family
    /// synthesized, ADR-0026 D3).
    FontVariantCaps(FontVariantCapsKind),
}

/// container-type (stage 2 ③): size/inline-size makes the node a queryable
/// container; v1 does not enforce size containment (FEATURES.md Tier B
/// deviation), and an inline-size container only supplies inline-axis size
/// (block-axis features = unknown, no match).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ContainerType {
    #[default]
    /// normal — not a container (default).
    Normal,
    /// size — dual-axis size container (both block and inline axes
    /// queryable).
    Size,
    /// inline-size — inline-axis size container.
    InlineSize,
}

/// cursor (A1 behavior hint, CSS UI 4 keyword subset): consumed by the host,
/// zero drawing semantics; url() custom pointers=T2. Initial auto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum CursorKind {
    /// auto — host decides per element semantics (default).
    #[default]
    Auto,
    /// none — pointer hidden.
    None,
    /// default — system default arrow.
    Default,
    /// pointer — clickable (hand).
    Pointer,
    /// text — text I-beam.
    Text,
    /// wait — busy.
    Wait,
    /// progress — busy but interactive.
    Progress,
    /// help — help.
    Help,
    /// not-allowed — forbidden.
    NotAllowed,
    /// move — move.
    Move,
    /// grab — grab.
    Grab,
    /// grabbing — grabbing.
    Grabbing,
    /// crosshair — crosshair.
    Crosshair,
    /// zoom-in — zoom in.
    ZoomIn,
    /// zoom-out — zoom out.
    ZoomOut,
}

/// user-select (A1, CSS UI 4): text selection semantics; **not inherited**
/// (the spec is explicit that the auto value's behavior depends on the
/// parent's cascade rather than property inheritance; hosts implementing
/// selection resolve it themselves).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum UserSelectKind {
    /// auto — host decides per element semantics (default).
    #[default]
    Auto,
    /// none — text not selectable.
    None,
    /// text — selectable.
    Text,
    /// all — whole-element unit selection.
    All,
    /// contain — selection start/end clamped inside the element (host
    /// interprets).
    Contain,
}

/// pointer-events (A1): hit-testing semantics; inherited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum PointerEventsKind {
    /// auto — normal hit-testing (default).
    #[default]
    Auto,
    /// none — the element (including its subtree) does not participate in
    /// hit-testing.
    None,
}

/// outline-style (A2, css-ui-4): outline stroke line style; auto = the host
/// focus-ring semantic slot (the engine has no focus concept, so painting
/// approximates it as Solid = Tier B documented in FEATURES.md). Not
/// inherited; initial none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum OutlineStyle {
    /// none — no line drawn (default).
    #[default]
    None,
    /// auto — host focus ring (the engine paints it as a Solid
    /// approximation).
    Auto,
    /// solid — solid line.
    Solid,
    /// dashed — dashed line.
    Dashed,
    /// dotted — dotted line.
    Dotted,
    /// double — double line.
    Double,
    /// groove — grooved.
    Groove,
    /// ridge — ridged.
    Ridge,
    /// inset — inset bevel.
    Inset,
    /// outset — outset bevel.
    Outset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
/// grid-auto-flow value family (auto-placement direction).
pub enum GridAutoFlowKind {
    /// row — fill by rows (default).
    Row,
    /// column — fill by columns.
    Column,
}

/// list-style-type value (P9-3, css-lists-3 §3.4): counter style name or
/// literal string. The none keyword is carried by
/// DeclValue::ListStyleType(None).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListStyleTypeValue {
    /// `<counter-style-name>` — the registry is not validated at parse time
    /// (CSS dynamism: @counter-style may be registered later); unknown names
    /// fall back to decimal at use time (css-counter-styles-3 §2).
    Name(String),
    /// `<string>` — literal marker (no prefix/suffix added).
    Str(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// list-style-position value family (P9-3, css-lists-3 §3.5).
#[non_exhaustive]
pub enum ListStylePosition {
    /// inside — the marker box is the principal box's first inline-level
    /// content (naturally true in the engine: the marker pseudo-node's text
    /// leaf participates in the first line's packing).
    Inside,
    /// outside — the marker box hangs outside the inline box (the spec
    /// itself calls this handwavey; the engine renders as inside = a Tier B
    /// approximation, ADR-0041).
    Outside,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// display value family (inline/inline-block = F1 inline semantics, the rest
/// of inline* normalized at parse time).
#[non_exhaustive]
pub enum Display {
    /// taffy has no native IFC; before F1 (ADR-0021) all inline* normalize to
    /// block-level equivalents: inline-flex→Flex, inline-grid→Grid,
    /// inline-table→Table.
    Block,
    /// F1 (ADR-0021): display:inline — continuity marker (box-transparent):
    /// generates no participant box itself and flattens its text leaf
    /// descendants into the parent run; layout maps to taffy Block.
    Inline,
    /// F1 (ADR-0021): display:inline-block — atomic inline box (taffy layout
    /// size participates in the parent run's packing); layout maps to taffy
    /// Block.
    InlineBlock,
    /// flex / inline-flex — flex container.
    Flex,
    /// grid / inline-grid — grid container.
    Grid,
    /// none — no box generated.
    None,
    /// Phase 2 ②: display:table — mapped to a block container (rows =
    /// row-level Grid stacked vertically), column template settled by the
    /// engine at layout time (settle_tables).
    Table,
    /// display:table-row — mapped to a single-row taffy Grid; the column
    /// template is settled and shared from the owning table.
    TableRow,
    /// display:table-cell — mapped to a Grid item (block); width is left to
    /// the column template (stretch).
    TableCell,
    /// Phase 3 ④: display:table-row-group/header-group/footer-group — row
    /// groups are vertically transparent block wrappers (row Grids stacked
    /// directly); row discovery pierces them in settle_tables.
    TableRowGroup,
    /// Phase 3 ④: display:table-caption — table caption box (block flow
    /// placed above the row area, width = table content width; does not
    /// participate in column discovery).
    TableCaption,
    /// P9-3 (css-lists-3): display:list-item — list item: generates the
    /// principal block box + an engine-side ::marker pseudo-node (implicit
    /// list-item counter increments, §4.6); layout maps to taffy Block
    /// (blockified).
    ListItem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// position value family.
#[non_exhaustive]
pub enum Position {
    /// static — positioned in normal flow (default).
    Static,
    /// relative — relative positioning; top/right/bottom/left act as
    /// offsets.
    Relative,
    /// absolute — absolute positioning (relative to the nearest positioned
    /// ancestor).
    Absolute,
    /// fixed — viewport-anchored (A4: containing block = transformed
    /// ancestor or the initial containing block; a positioned ancestor does
    /// not form fixed's containing block).
    Fixed,
    /// sticky — sticky positioning (A4: layout = in-flow; sticky offsets are
    /// applied by the host per its scrolling runtime — the engine translates
    /// at paint time via scroll_offsets; ADR-0006: the engine has no runtime
    /// state).
    Sticky,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// overflow-x/y value family.
#[non_exhaustive]
pub enum Overflow {
    /// visible — overflow visible (default).
    Visible,
    /// hidden — overflow clipped.
    Hidden,
    /// clip — overflow clipped (scrolling forbidden).
    Clip,
    /// scroll — clipped and treated as scrollable (auto normalizes here).
    Scroll,
}

/// box-sizing (adopted case-driven via the Numeric Channel box-model,
/// ADR-0003):
/// CSS defaults to content-box — width/height cover the content box only;
/// border-box includes padding+border. taffy's size semantics is
/// border-box; the mapping site converts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BoxSizing {
    /// content-box — sizes cover the content box only (CSS default).
    ContentBox,
    /// border-box — sizes include padding+border.
    BorderBox,
}

/// transform functions (ADR-0009 v1 = 2D affine). Angles normalize to
/// degrees (value.rs convention), Percent stores decimals (0.5 = 50%).
/// TranslateX/Y fold into Translate, ScaleX/Y into Scale, SkewX/Y into Skew
/// (missing components = the identity).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TransformFn {
    /// translate / translateX / translateY: x/y offsets (x may be a
    /// percentage, base = own border-box width).
    Translate(LengthPercentage, LengthPercentage),
    /// scale / scaleX / scaleY: x/y scale factors.
    Scale(f32, f32),
    /// rotate: clockwise angle (degrees, y-down screen coordinates).
    Rotate(f32),
    /// skew / skewX / skewY: x/y skew angles (degrees).
    Skew(f32, f32),
    /// matrix(a, b, c, d, e, f): x' = a·x + c·y + e.
    Matrix(f32, f32, f32, f32, f32, f32),
}

/// CSS filter functions (P2 batch, ADR-0031 D1): elements of the ordered
/// `<filter-function-list>` (chain order preserved — invert→brightness ≠
/// brightness→invert).
/// Numeric parameters are clamped at parse time per css-filters-1 "clamped,
/// not invalid"; percentages normalize to decimals (0.5 = 50%). Length
/// components ride in `LengthPercentage` (em/rem/vw/vh/calc legal, finalized
/// at paint time — same convention as transform); colors via `ColorValue`
/// (defaults to currentcolor, finalized at paint time by pick_scheme).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum FilterFn {
    /// blur(`<length>`?): Gaussian blur; the painted layer's σ = the
    /// parameter / 2.
    Blur(LengthPercentage),
    /// brightness(`<number-percentage>`?): linear multiply (1 = unchanged),
    /// clamped ≥ 0.
    Brightness(f32),
    /// contrast(`<number-percentage>`?): the c·a+(0.5−0.5a) affine, clamped
    /// ≥ 0.
    Contrast(f32),
    /// grayscale(`<number-percentage>`?): sRGB desaturation matrix
    /// interpolation, clamped \[0,1\].
    Grayscale(f32),
    /// sepia(`<number-percentage>`?): sRGB sepia matrix interpolation,
    /// clamped \[0,1\].
    Sepia(f32),
    /// saturate(`<number-percentage>`?): sRGB saturation matrix
    /// interpolation, clamped ≥ 0.
    Saturate(f32),
    /// invert(`<number-percentage>`?): c'=(1−2a)c+a, clamped \[0,1\].
    Invert(f32),
    /// opacity(`<number-percentage>`?): alpha scaling, clamped \[0,1\].
    Opacity(f32),
    /// hue-rotate(`<angle>`?): degrees (sRGB linear approximation matrix,
    /// W3C §4 table).
    HueRotate(f32),
    /// drop-shadow(`<length>`{2,3} && `<color>`?): offset + optional blur
    /// radius (no spread, unlike box-shadow) + color (defaults to
    /// currentcolor).
    DropShadow {
        /// x offset.
        dx: LengthPercentage,
        /// y offset.
        dy: LengthPercentage,
        /// Blur radius (0 = hard edge).
        blur: LengthPercentage,
        /// Shadow color (defaults to currentcolor).
        color: ColorValue,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Alignment value family shared by justify-content/align-*.
#[non_exhaustive]
pub enum Align {
    /// normal — default alignment behavior.
    Normal,
    /// start — aligned to the start.
    Start,
    /// end — aligned to the end.
    End,
    /// center — centered.
    Center,
    /// stretch — stretched to fill.
    Stretch,
    /// baseline — first-baseline aligned.
    Baseline,
    /// flex-start — main-axis start alignment.
    FlexStart,
    /// flex-end — main-axis end alignment.
    FlexEnd,
    /// space-between — flush to both ends, remainder distributed between.
    SpaceBetween,
    /// space-around — equal-width gaps on both sides of every item.
    SpaceAround,
    /// space-evenly — equal-width gaps between and at both ends.
    SpaceEvenly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// flex-direction value family.
#[non_exhaustive]
pub enum FlexDirection {
    /// row — main axis along the row, forward (default).
    Row,
    /// row-reverse — main axis along the row, reversed.
    RowReverse,
    /// column — main axis along the column, forward.
    Column,
    /// column-reverse — main axis along the column, reversed.
    ColumnReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// flex-wrap value family.
#[non_exhaustive]
pub enum FlexWrap {
    /// nowrap — single line, no wrapping (default).
    NoWrap,
    /// wrap — wrapping allowed.
    Wrap,
    /// wrap-reverse — wrapping with the cross axis reversed.
    WrapReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// border-*-style / column-rule-style line style value family.
#[non_exhaustive]
pub enum BorderStyle {
    /// none / hidden — no line drawn.
    None,
    /// solid — solid line.
    Solid,
    /// dashed — dashed line (P4 D1: exact segmentation for rectangular
    /// frames; rounded frames degrade to Solid, Tier B).
    Dashed,
    /// dotted — dotted line (P4 D1: exact segmentation for rectangular
    /// frames; rounded frames degrade to Solid, Tier B).
    Dotted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// text-align value family.
#[non_exhaustive]
pub enum TextAlign {
    /// start — aligned to the writing-direction start (default).
    Start,
    /// end — aligned to the writing-direction end.
    End,
    /// center — centered.
    Center,
    /// left — left-aligned.
    Left,
    /// right — right-aligned.
    Right,
    /// Actually consumed as parley align Justify (batch 5 ⑳, last line
    /// start-aligned).
    Justify,
}

/// white-space value family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WhiteSpace {
    /// normal — whitespace collapsed, automatic line wrapping (default).
    Normal,
    /// nowrap — whitespace collapsed, no wrapping.
    NoWrap,
    /// pre — whitespace and newlines preserved, no automatic wrapping.
    Pre,
    /// pre-wrap (A5) — whitespace and newlines preserved, wraps to the
    /// containing block width.
    PreWrap,
    /// pre-line (A5) — spaces collapsed but newlines preserved, wraps to
    /// width.
    PreLine,
    /// break-spaces (A5) — same as pre-wrap, plus line breaking at any
    /// character (A5 v1: break points = regular wrapping approximation;
    /// break-anywhere = Tier B documented in FEATURES.md).
    BreakSpaces,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
/// text-overflow value family (F2, ADR-0022 D2).
pub enum TextOverflowKind {
    /// clip — plain clipping (default).
    #[default]
    Clip,
    /// ellipsis — truncated tail followed by an ellipsis (effective in a
    /// clipping context).
    Ellipsis,
}

/// -webkit-line-clamp integer|none → WebkitLineClamp (F2, ADR-0022 D3;
/// none→0; 0/negative integers invalid and dropped — semantically none is 0,
/// positive integers ≥1 valid).
pub fn parse_webkit_line_clamp(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(id) if id.eq_ignore_ascii_case("none") => Ok(DeclValue::WebkitLineClamp(0)),
        Token::Number {
            int_value: Some(n), ..
        } if *n >= 1 => {
            // 上限钳 i32::MAX（注意 u32::MAX as i32 回绕 -1 陷阱）。
            Ok(DeclValue::WebkitLineClamp((*n) as u32))
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

// ===== F2（ADR-0022 D4）：text-decoration =====

/// text-decoration-line bit: underline.
pub const TD_LINE_UNDERLINE: u8 = 1;
/// text-decoration-line bit: overline.
pub const TD_LINE_OVERLINE: u8 = 2;
/// text-decoration-line bit: line-through.
pub const TD_LINE_LINE_THROUGH: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
/// text-decoration-style value family (F2, ADR-0022 D4).
pub enum TextDecoStyleKind {
    /// solid — solid line (default).
    #[default]
    Solid,
    /// double — double line.
    Double,
    /// dotted — dotted line (Tier B: the soft sink draws it as segmented
    /// solid).
    Dotted,
    /// dashed — dashed line (Tier B).
    Dashed,
    /// wavy — wavy line (Tier B: polyline approximation).
    Wavy,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
/// text-decoration-thickness value family (F2, ADR-0022 D4).
pub enum TextDecoThickness {
    /// auto — font-size ratio approximation (paint-time font_size/12).
    #[default]
    Auto,
    /// from-font — the font's preferred thickness (missing metric → degrades
    /// to auto).
    FromFont,
    /// `<length-percentage>` declared value.
    Length(crate::css::value::LengthPercentage),
}

/// Line keyword single component → bit (shared by the longhand loop and the
/// shorthand's greedy scan; F2 D4).
pub(crate) fn parse_td_line_component(p: &mut Parser<'_>) -> ValResult<u8> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(id) => match_ignore_ascii_case!(id,
            "underline" => Ok(TD_LINE_UNDERLINE),
            "overline" => Ok(TD_LINE_OVERLINE),
            "line-through" => Ok(TD_LINE_LINE_THROUGH),
            _ => Err(p.new_error_for_next_token()),
        ),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// text-decoration-line (F2 D4): none | space-separated keywords.
pub fn parse_text_decoration_line(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    if let Ok(()) = p.try_parse(|p| -> ValResult<()> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(id) if id.eq_ignore_ascii_case("none") => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    }) {
        return Ok(DeclValue::TextDecorationLine(0));
    }
    let mut bits = parse_td_line_component(p)?;
    while let Ok(bit) = p.try_parse(|p| parse_td_line_component(p)) {
        bits |= bit;
    }
    Ok(DeclValue::TextDecorationLine(bits))
}

/// Style keyword single component (shared by the shorthand's greedy scan;
/// F2 D4).
pub(crate) fn parse_td_style_component(p: &mut Parser<'_>) -> ValResult<TextDecoStyleKind> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(id) => match_ignore_ascii_case!(id,
            "solid" => Ok(TextDecoStyleKind::Solid),
            "double" => Ok(TextDecoStyleKind::Double),
            "dotted" => Ok(TextDecoStyleKind::Dotted),
            "dashed" => Ok(TextDecoStyleKind::Dashed),
            "wavy" => Ok(TextDecoStyleKind::Wavy),
            _ => Err(p.new_error_for_next_token()),
        ),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// text-decoration-style (F2 D4).
pub fn parse_text_decoration_style(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_td_style_component(p).map(DeclValue::TextDecorationStyle)
}

/// Thickness single component (shared by the shorthand's greedy scan; F2
/// D4): auto | from-font | `<length-percentage>`.
pub(crate) fn parse_td_thickness_component(p: &mut Parser<'_>) -> ValResult<TextDecoThickness> {
    let kw = p.try_parse(|p| -> ValResult<Option<TextDecoThickness>> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(id) if id.eq_ignore_ascii_case("auto") => {
                Ok(Some(TextDecoThickness::Auto))
            }
            Token::Ident(id) if id.eq_ignore_ascii_case("from-font") => {
                Ok(Some(TextDecoThickness::FromFont))
            }
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if let Ok(Some(k)) = kw {
        return Ok(k);
    }
    let l = parse_length_percentage(p)?;
    Ok(TextDecoThickness::Length(l))
}

/// text-decoration-thickness (F2 D4).
pub fn parse_text_decoration_thickness(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_td_thickness_component(p).map(DeclValue::TextDecorationThickness)
}

/// Color single component (shared by the shorthand's greedy scan; F2 D4).
pub(crate) fn parse_td_color_component(p: &mut Parser<'_>) -> ValResult<ColorValue> {
    match parse_color(p)? {
        DeclValue::Color(cv) => Ok(cv),
        _ => Err(p.new_error_for_next_token()),
    }
}

// ===== F2（ADR-0022 D5）：text-shadow =====

/// text-shadow single shadow (F2, ADR-0022 D5).
#[derive(Debug, Clone, PartialEq)]
pub struct TextShadowSpec {
    /// Horizontal offset (`<length-percentage>`).
    pub dx: crate::css::value::LengthPercentage,
    /// Vertical offset (`<length-percentage>`).
    pub dy: crate::css::value::LengthPercentage,
    /// Blur radius (defaults to 0=sharp).
    pub blur: Option<crate::css::value::LengthPercentage>,
    /// Color (defaults to currentColor).
    pub color: Option<ColorValue>,
}

/// text-shadow (F2, ADR-0022 D5): none | [`<color>`? `<dx>` `<dy>` `<blur>`?
/// `<color>`?]# (the `<color>` may appear on either side — css-backgrounds-3
/// && combination; comma-separated multiple shadows).
pub fn parse_text_shadow(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    if let Ok(()) = p.try_parse(|p| -> ValResult<()> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(id) if id.eq_ignore_ascii_case("none") => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    }) {
        return Ok(DeclValue::TextShadow(Vec::new()));
    }
    let mut shadows = Vec::new();
    loop {
        let mut color: Option<ColorValue> = p.try_parse(parse_color_value).ok();
        let dx = parse_length_percentage(p)?;
        let dy = parse_length_percentage(p)?;
        let blur = p.try_parse(parse_length_percentage).ok();
        if color.is_none() {
            color = p.try_parse(parse_color_value).ok();
        }
        shadows.push(TextShadowSpec {
            dx,
            dy,
            blur,
            color,
        });
        if p.is_exhausted() {
            break;
        }
        if p.try_parse(|p| p.expect_comma()).is_err() {
            return Err(p.new_error_for_next_token());
        }
    }
    Ok(DeclValue::TextShadow(shadows))
}

/// text-overflow keyword → TextOverflow (F2, ADR-0022 D2).
pub fn parse_text_overflow(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "clip" => TextOverflowKind::Clip,
            "ellipsis" => TextOverflowKind::Ellipsis,
            _ => return None,
        ))
    })
    .map(DeclValue::TextOverflow)
}

/// font-style value family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FontStyle {
    /// normal — upright face (default).
    Normal,
    /// italic — italic face.
    Italic,
}

// LengthPercentage 含 Box（calc），非 Copy
/// line-height value family.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum LineHeight {
    /// normal — the UA default line height (roughly 1.2× the font size).
    Normal,
    /// Unitless number (a multiple).
    Number(f32),
    /// `<length-percentage>` — a fixed line height (percentage is relative
    /// to font-size).
    Len(LengthPercentage),
}

/// font-family — an ordered font family list (matched with successive
/// fallback).
#[derive(Debug, Clone, PartialEq)]
pub struct FontFamilyList(pub SmallVec<[FamilyName; 2]>);

/// A single font-family item: a named font or a generic family keyword.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FamilyName {
    /// `<family-name>` — a named font family (case-insensitive).
    Named(String),
    /// serif — the generic serif family.
    Serif,
    /// sans-serif — the generic sans-serif family.
    SansSerif,
    /// monospace — the generic monospace family.
    Monospace,
    /// cursive — the generic cursive family.
    Cursive,
    /// fantasy — the generic fantasy family.
    Fantasy,
    /// system-ui — the system UI font.
    SystemUi,
}

/// Track list of grid-template-columns/rows (subset: fixed tracks +
/// fixed-count repeat).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GridTemplate {
    /// Track size sequence (columns or rows; the direction follows the
    /// property).
    pub tracks: Vec<TrackSize>,
    /// Line-name slots (E5, ADR-0020): N tracks have N+1 slots —
    /// `line_names[i]` = the set of line names before track i,
    /// line_names[tracks.len()] = the trailing line names; parse product of
    /// the `[a b]` bracket segments (defaults to empty).
    pub line_names: Vec<Vec<String>>,
}

/// A single grid track size.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TrackSize {
    /// `<length-percentage>` — a fixed track.
    Len(LengthPercentage),
    /// `<flex>` — an fr flexible share.
    Fr(f32),
    /// auto — a track that grows/shrinks with content.
    Auto,
    /// max-content — the content's maximum intrinsic sizing.
    MaxContent,
    /// min-content — the content's minimum intrinsic sizing.
    MinContent,
    /// minmax(min, max) — closed-range track size.
    MinMax(Box<TrackSize>, Box<TrackSize>),
    /// Fixed-count repeat.
    Repeat(u16, Vec<TrackSize>),
    /// auto-fill / auto-fit repeat (stage 2 ①): fit=true is auto-fit (empty
    /// tracks collapse). The count is decided at layout time from the
    /// available space (taffy RepetitionCount supports this natively).
    RepeatAuto(bool, Vec<TrackSize>),
}

/// grid-template-areas value (E5, ADR-0020): area template. `rows[r][c]` =
/// an area name or `.` (empty cell); rectangularity + per-name rectangular
/// validation happen at parse time (a violation = invalid declaration, spec
/// §8.5).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GridAreas {
    /// Cell names row by row (`.` = an empty cell).
    pub rows: Vec<Vec<String>>,
}

/// grid-{row,column}-{start,end} value (E5, ADR-0020):
/// `auto | <ident> | <integer> | span <integer> | span <ident>`
/// (the spec's mixed form `<integer> && <ident>` is a v1 deviation
/// documented in FEATURES.md).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum GridLineSpec {
    /// auto — auto-placement.
    Auto,
    /// `<integer>` — a line number (1-based; 0 is invalid and rejected at
    /// parse time; negatives count from the end).
    Number(i16),
    /// span `<integer>` — spans k tracks (k ≥ 1).
    Span(u16),
    /// span `<ident>` — spans up to the k-th named line (resolved at parse
    /// time; unknown name = Auto).
    SpanName(String),
    /// `<ident>` — an area edge line or a line name (resolved at parse time;
    /// unknown name = Auto).
    Name(String),
}

/// background-image value family.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum BackgroundImage {
    /// none — no background image (default).
    None,
    /// url(`<string>`) — an image resource reference.
    Url(String),
    /// Gradient function (linear/radial).
    Gradient(Gradient),
}

/// One tiling axis item (css-backgrounds-3 repeat-style split by axis).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatAxis {
    /// repeat — tile.
    Repeat,
    /// space — evenly spaced with padding (Tier B: painted ≈repeat).
    Space,
    /// round — scale and round (Tier B: painted ≈repeat).
    Round,
    /// no-repeat — no tiling.
    NoRepeat,
}

/// Tiling on both axes (`repeat` alone = Repeat on both; `repeat-x` =
/// {Repeat, NoRepeat}; two values: first = x, second = y).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepeatXY {
    /// The horizontal axis.
    pub x: RepeatAxis,
    /// The vertical axis.
    pub y: RepeatAxis,
}

/// background-attachment value family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attachment {
    /// scroll — scrolls with the element's content (default).
    Scroll,
    /// fixed — viewport-anchored (element offsets are ignored at paint
    /// time).
    Fixed,
    /// local — scrolls with the scroll container's content (Tier B:
    /// ≈scroll).
    Local,
}

/// A single bg-position axis component: the keyword-normalized base
/// (left/top=0, center=50%, right/bottom=100%; lengths/percentages stored
/// directly) plus the optional offset of the three/four-value syntax.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionComp {
    /// The base (the keyword's equivalent percentage, or a length/percentage
    /// stored directly).
    pub base: LengthPercentage,
    /// Edge offset (the second segment of the `left 10px` four-value syntax;
    /// the semantic negation for right/bottom settles at paint time).
    pub offset: Option<LengthPercentage>,
}

/// bg-position on both axes.
#[derive(Debug, Clone, PartialEq)]
pub struct Position2D {
    /// Horizontal component.
    pub x: PositionComp,
    /// Vertical component.
    pub y: PositionComp,
}

/// Background box keywords (background-origin; clip additionally has Text).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundBox {
    /// border-box — the border box.
    BorderBox,
    /// padding-box — the padding box (origin's initial value).
    PaddingBox,
    /// content-box — the content box.
    ContentBox,
}

/// A single background-clip item (css-backgrounds-3 box family +
/// css-backgrounds-4 text; Text is accepted at parse time and degrades to
/// border-box + warn at paint time — Tier B deviation documented in
/// FEATURES.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundClip {
    /// A box keyword (border-box is the initial value).
    Box(BackgroundBox),
    /// text — text mask clipping (Tier B: paint-time degrade).
    Text,
}

/// A background-size component: `<length-percentage> | auto`.
#[derive(Debug, Clone, PartialEq)]
pub enum LPorAuto {
    /// Length/percentage.
    LP(LengthPercentage),
    /// auto (that axis follows the intrinsic ratio).
    Auto,
}

/// A single background-size item.
#[derive(Debug, Clone, PartialEq)]
pub enum BgSize {
    /// auto — intrinsic sizing (a gradient = the positioning area).
    Auto,
    /// cover — covers the positioning area (aspect-preserving, the larger
    /// scale).
    Cover,
    /// contain — fits the positioning area (aspect-preserving, the smaller
    /// scale).
    Contain,
    /// Explicit width/height (a single value = width with auto height).
    Explicit {
        /// Width.
        w: LPorAuto,
        /// Height.
        h: LPorAuto,
    },
}

/// F3b (ADR-0024): a single background layer after alignment (cycling
/// fill-in semantics).
#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundLayer {
    /// The layer's image.
    pub image: BackgroundImage,
    /// Tiling.
    pub repeat: RepeatXY,
    /// Attachment.
    pub attachment: Attachment,
    /// Position.
    pub position: Position2D,
    /// Size.
    pub size: BgSize,
    /// Positioning box (origin).
    pub origin: BackgroundBox,
    /// Painting box (clip).
    pub clip: BackgroundClip,
}

/// F3c (ADR-0025): the clip-path clip shape. Grammar `<basic-shape>` ||
/// `<geometry-box>` (either order) | none. The reference box is stored
/// alongside the shape (percentage radii/centers/closest-farthest edges all
/// resolve against the reference box); a lone geometry-box = inset(0) based
/// on that box (semantically equivalent, css-masking-1 §5.1). url()/path()
/// are accepted as Other (SVG resources and the path grammar = T2; painted
/// as none).
#[derive(Debug, Clone, PartialEq)]
pub enum ClipShape {
    /// none — no clipping (the initial value).
    None,
    /// inset( `<lp>`{1,4} [round `<lp>`{1,4} [ / `<lp>`{1,4} ]?]? ) —
    /// an inset rectangle (optional rounded corners; exact consumption
    /// reuses the PushClip radius capability).
    Inset {
        /// Inset on four edges (top-right-bottom-left, shorthand expanded at
        /// parse time).
        insets: [LengthPercentage; 4],
        /// Optional radius: (horizontal set, vertical set) of four corners
        /// each (top-right-bottom-left order, shorthand expanded at parse
        /// time; without a slash the vertical set = the horizontal set;
        /// None = square corners).
        radius: Option<([LengthPercentage; 4], [LengthPercentage; 4])>,
        /// Reference box (defaults to border-box).
        reference: BackgroundBox,
    },
    /// circle( `<radius>`? at `<position>`? ) — a circle.
    Circle {
        /// Radius (defaults to closest-side).
        radius: ClipRadius,
        /// Center (defaults to the box center 50% 50%).
        at: Position2D,
        /// Reference box (defaults to border-box).
        reference: BackgroundBox,
    },
    /// ellipse( `<rx>`? `<ry>`? at `<position>`? ) — ellipse.
    Ellipse {
        /// Horizontal radius (defaults to closest-side).
        rx: ClipRadius,
        /// Vertical radius (defaults to closest-side).
        ry: ClipRadius,
        /// Center (defaults to the box center 50% 50%).
        at: Position2D,
        /// Reference box (defaults to border-box).
        reference: BackgroundBox,
    },
    /// polygon( [nonzero|evenodd,]? `<x>` `<y>`, ... ) — a polygon (fewer
    /// than 3 coordinate pairs = the whole declaration is dropped at parse
    /// time).
    Polygon {
        /// Fill rule (defaults to nonzero).
        nonzero: bool,
        /// Vertex sequence (full length-percentage values, resolved against
        /// the reference box).
        points: Vec<(LengthPercentage, LengthPercentage)>,
        /// Reference box (defaults to border-box).
        reference: BackgroundBox,
    },
    /// Tolerant acceptance: url()/path() and future functions (paint
    /// semantics = none, with a tracing warning).
    Other,
}

/// clip-path circle/ellipse radius (css-shapes-1 §3.2.1): an explicit
/// length/percentage or an edge/corner keyword (resolved against the
/// reference box at paint time; a percentage radius is relative to
/// √(w²+h²)/√2 for circle and per-axis width/height for ellipse).
#[derive(Debug, Clone, PartialEq)]
pub enum ClipRadius {
    /// Explicit radius (length-percentage; negative values are rejected at
    /// parse time).
    Length(LengthPercentage),
    /// closest-side — the nearest edge.
    ClosestSide,
    /// farthest-side — the farthest edge.
    FarthestSide,
    /// closest-corner — the nearest corner (the radius = the distance to
    /// that corner).
    ClosestCorner,
    /// farthest-corner — the farthest corner.
    FarthestCorner,
}

/// MVP gradient: linear (angle / `to` direction) and radial (circle,
/// default farthest-corner).
#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    /// Gradient type and geometric parameters.
    pub kind: GradientKind,
    /// Whether repeating (css-images-3: `repeating-*-gradient()` — the stop
    /// pattern tiles infinitely along the gradient line/radius/angle, with
    /// the period = the span between the first and last stops; a period of 0
    /// paints transparent black). Parsed since P1-3; geometric tiling is
    /// finalized by the sink (vello Extend::Repeat / soft modular sampling).
    pub repeating: bool,
    /// Color stop sequence (at least 2; hints don't count).
    pub stops: Vec<ColorStop>,
    /// Color hints (css-images-3 color-stop-list: the `<length-percentage>`
    /// between two adjacent stops). Hints do not participate in stop
    /// position distribution; they are only expanded at sampling time into
    /// synthesized midpoint stops between the neighboring stop colors
    /// (`apply_gradient_hints`, a single source shared by the soft/vello
    /// sinks). The Gradient the engine emits at paint time passes the stops
    /// through; the sink consumes it after position fill-in.
    pub hints: Vec<GradientHint>,
}

/// A gradient color hint (css-images-3; P9-1a ADR-0038).
#[derive(Debug, Clone, PartialEq)]
pub struct GradientHint {
    /// The hint sits before the stop at this index (i.e. between
    /// `stops[idx-1]` and `stops[idx]`). Before the first stop (idx=0) and
    /// after the last stop (idx>=stops.len()) are syntactically illegal and
    /// rejected at parse time.
    pub after_stop: usize,
    /// Hint position (along the gradient line; same grammar family as stop
    /// positions, `<length-percentage>`).
    pub position: LengthPercentage,
}

/// Gradient type (linear/radial).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum GradientKind {
    /// Angle normalized to degrees; `to bottom` (the default) = 180deg.
    Linear(Angle),
    /// Radial: shape/size/position are semantic values; the paint layer
    /// resolves them into absolute geometry from the box (T4c).
    Radial(RadialSpec),
    /// Conic (C3, css-images-3): a `from` start angle + an `at` center, one
    /// clockwise turn.
    Conic(ConicSpec),
}

/// Radial gradient semantics (css-images-3 subset).
#[derive(Debug, Clone, PartialEq)]
pub struct RadialSpec {
    /// Shape (circle/ellipse).
    pub shape: RadialShape,
    /// Size keyword or explicit radii.
    pub size: RadialSize,
    /// Center (x, y); percentages are relative to the box width/height
    /// respectively.
    pub position: (LengthPercentage, LengthPercentage),
}

/// Conic gradient semantics (C3, css-images-3; ADR-0017).
/// CSS 0deg = the 12 o'clock direction, clockwise; the peniko mapping
/// translates the start to the +X axis (D1).
#[derive(Debug, Clone, PartialEq)]
pub struct ConicSpec {
    /// Start angle (`from <angle>`; defaults to 0deg).
    pub from: Angle,
    /// Center (x, y); percentages are relative to the box width/height
    /// respectively (`at <position>`, defaults to center).
    pub position: (LengthPercentage, LengthPercentage),
}

/// Radial gradient shape.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum RadialShape {
    /// circle — a circle.
    Circle,
    /// ellipse — an ellipse (default).
    Ellipse,
}

/// Radial gradient size (determines the gradient's ending radius).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RadialSize {
    /// closest-side — the end reaches the nearest edge.
    ClosestSide,
    /// closest-corner — the end reaches the nearest corner.
    ClosestCorner,
    /// farthest-side — the end reaches the farthest edge.
    FarthestSide,
    /// farthest-corner — the end reaches the farthest corner (default).
    FarthestCorner,
    /// Explicit radii (one for circle, two for ellipse; percentages are
    /// relative to width/height respectively).
    Explicit {
        /// Horizontal radius (the first `<length-percentage>`).
        rx: LengthPercentage,
        /// Vertical radius (the second `<length-percentage>`; defaults to
        /// None).
        ry: Option<LengthPercentage>,
    },
}

/// A gradient color stop.
#[derive(Debug, Clone, PartialEq)]
pub struct ColorStop {
    /// Stop color.
    pub color: ColorValue,
    /// Stop position (None → automatically evenly spaced along the axis).
    pub position: Option<LengthPercentage>,
}

/// css-images-3 §4.5.2 stop position fill-in (a single source shared by the
/// soft/vello sinks, P9-1a): a missing first stop = 0.0, a missing last stop
/// = 1.0; a middle missing segment is **evenly spaced** between the nearest
/// known neighbors (the span includes the first known stop after the
/// segment); explicit positions out of order are raised point by point to
/// the previous stop's position (monotonization). The input is the position
/// sequence after each sink completes unit normalization (px→fraction along
/// the line, % taken directly, context-less units = None) and clamping to
/// \[0,1\].
pub fn distribute_stop_positions(raw: &[Option<f32>]) -> Vec<f32> {
    let n = raw.len();
    if n == 0 {
        return Vec::new();
    }
    let mut positions: Vec<f32> = raw.iter().map(|p| p.unwrap_or(f32::NAN)).collect();
    if positions[0].is_nan() {
        positions[0] = 0.0;
    }
    if positions[n - 1].is_nan() {
        positions[n - 1] = 1.0;
    }
    let mut last_known = 0.0f32;
    let mut i = 0;
    while i < n {
        if positions[i].is_nan() {
            let mut j = i;
            while j < n && positions[j].is_nan() {
                j += 1;
            }
            let next = if j < n { positions[j] } else { 1.0 };
            let span = (j - i + 1) as f32;
            for (k, idx) in (i..j).enumerate() {
                positions[idx] = last_known + (next - last_known) * ((k + 1) as f32 / span);
            }
            i = j;
        } else {
            last_known = positions[i];
            i += 1;
        }
    }
    // css-images-3 §4.5.2：解析器不夹取位置，后停位 < 前停位时抬至前停位
    for w in 1..n {
        if positions[w] < positions[w - 1] {
            positions[w] = positions[w - 1];
        }
    }
    positions
}

/// css-images-3 color-hint expansion (soft/vello sink shared single source,
/// P9-1a): on the `(offset, color)` stop sequence whose positions have been
/// filled in, each hint `(after_stop, normalized position)` synthesizes a
/// midpoint stop (the average of the colors of the stops before and after).
/// The interpolation curve becomes piecewise-linear passing through the hint
/// positions (a linear approximation of the css-images-3 smooth curve;
/// deviation documented in SINK-MATRIX; revisit when: a sink-level curve
/// interpolation primitive exists). A hint is ignored when after_stop is out
/// of range (0 or ≥ the stop count) or the hint position falls outside its
/// interval. Normalizing hint positions (px → fraction along the line, etc.)
/// is the caller's job; units that cannot be normalized (em/rem/cq etc. —
/// the sink has no context) should drop the hint entirely (linear = no hint
/// behavior).
pub fn apply_gradient_hints(
    stops: &[(f32, AlphaColor<Srgb>)],
    hints: &[(usize, f32)],
) -> Vec<(f32, AlphaColor<Srgb>)> {
    let n = stops.len();
    if n == 0 {
        return Vec::new();
    }
    let mut out: Vec<(f32, AlphaColor<Srgb>)> = stops.to_vec();
    let mut synth: Vec<(f32, AlphaColor<Srgb>)> = Vec::with_capacity(hints.len());
    for &(idx, pos) in hints {
        if idx == 0 || idx >= n {
            continue; // 首停点前 / 末停点后：解析期已拒，防御忽略
        }
        let (p_prev, c_prev) = stops[idx - 1];
        let (p_next, c_next) = stops[idx];
        let a = c_prev.components;
        let b = c_next.components;
        let mid = AlphaColor::new([
            (a[0] + b[0]) * 0.5,
            (a[1] + b[1]) * 0.5,
            (a[2] + b[2]) * 0.5,
            (a[3] + b[3]) * 0.5,
        ]);
        let lo = p_prev.min(p_next);
        let hi = p_prev.max(p_next);
        synth.push((pos.clamp(lo, hi), mid));
    }
    out.extend(synth);
    out.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// A shadow (batch 5 ⑩: inset keyword support — inner/outer shadows are
/// emitted separately in CSS paint order).
#[derive(Debug, Clone, PartialEq)]
pub struct BoxShadow {
    /// Horizontal offset (positive = right).
    pub offset_x: LengthPercentage,
    /// Vertical offset (positive = down).
    pub offset_y: LengthPercentage,
    /// Blur radius (non-negative).
    pub blur: LengthPercentage,
    /// Spread radius (may be negative).
    pub spread: LengthPercentage,
    /// Shadow color.
    pub color: ColorValue,
    /// The inset keyword: an inner shadow (paint order = above the
    /// background, below the border).
    pub inset: bool,
}

/// box-shadow shadow list.
pub type BoxShadowList = SmallVec<[BoxShadow; 2]>;

// ---------- 值族解析 ----------
//
// 惯用法：先 try_parse 探测关键字（Err 时状态回滚），再走通用值解析，
// 避免"peek 后重放"（cssparser 无法回退重放单 token）。

/// Parse a single keyword (case-insensitive); a mapping failure is a syntax
/// error.
fn keyword<K>(p: &mut Parser<'_>, map: impl Fn(&str) -> Option<K>) -> ValResult<K> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) => map(name).ok_or_else(|| p.new_error_for_next_token()),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// Probe `auto` (or the given keywords) first → None; otherwise parse as the
/// length family.
fn len_auto_with(p: &mut Parser<'_>, autos: &[&str]) -> ValResult<Option<LengthPercentage>> {
    let is_auto = p.try_parse(|p| -> ValResult<()> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) => {
                if autos.iter().any(|a| name.eq_ignore_ascii_case(a)) {
                    Ok(())
                } else {
                    Err(p.new_error_for_next_token())
                }
            }
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if is_auto.is_ok() {
        return Ok(None);
    }
    Ok(Some(parse_length_percentage(p)?))
}

/// content parsing (C1, css-content-3 MVP): none/normal/string literals.
/// attr()/url()/counter()/quotes are T2 — rejected at parse time (the caller
/// drops the declaration with a Dropped warning). P5 (ADR-0036 D1): upgraded
/// to a `<content-list>` sequence — `none | normal | [ <string> | counter()
/// | counters() | attr() | open-quote | close-quote | no-open-quote |
/// no-close-quote ]+`; url() is still rejected (cssparser Token::Url is
/// neither a function nor a string).
pub fn parse_content(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    p.skip_whitespace();
    let t = p.next()?.clone();
    match t {
        cssparser::Token::Ident(ref id) if id.eq_ignore_ascii_case("none") => {
            Ok(DeclValue::Content(ContentValue::None))
        }
        cssparser::Token::Ident(ref id) if id.eq_ignore_ascii_case("normal") => {
            Ok(DeclValue::Content(ContentValue::Normal))
        }
        cssparser::Token::QuotedString(ref s) => {
            let mut pieces = vec![ContentPiece::Str(s.to_string())];
            // 序列续段（空格分隔）。
            loop {
                p.skip_whitespace();
                if p.is_exhausted() {
                    break;
                }
                // try_parse 失败回滚 token——否则 Err 段的 token 已被
                // 消费，expect_exhausted 放行造成静默丢段。
                match p.try_parse(parse_content_piece) {
                    Ok(piece) => pieces.push(piece),
                    Err(_) => break, // 语法错误交 expect_exhausted 统一拒绝
                }
            }
            p.expect_exhausted()?;
            Ok(DeclValue::Content(ContentValue::Seq(pieces)))
        }
        cssparser::Token::Function(ref name) => {
            let fname = name.clone();
            let piece = p.parse_nested_block(|p| parse_content_fn_body(p, &fname))?;
            // 单函数也可能是序列首段（counter() counter() ...）。
            let mut pieces = vec![piece];
            loop {
                p.skip_whitespace();
                if p.is_exhausted() {
                    break;
                }
                match p.try_parse(parse_content_piece) {
                    Ok(piece) => pieces.push(piece),
                    Err(_) => break,
                }
            }
            p.expect_exhausted()?;
            Ok(DeclValue::Content(ContentValue::Seq(pieces)))
        }
        cssparser::Token::Ident(ref id) => {
            let piece = parse_content_quote_keyword(p, id)?;
            let mut pieces = vec![piece];
            loop {
                p.skip_whitespace();
                if p.is_exhausted() {
                    break;
                }
                match p.try_parse(parse_content_piece) {
                    Ok(piece) => pieces.push(piece),
                    Err(_) => break,
                }
            }
            p.expect_exhausted()?;
            Ok(DeclValue::Content(ContentValue::Seq(pieces)))
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// Quote-keyword section (the open-quote family; the ident has been taken,
/// dispatched here).
fn parse_content_quote_keyword(p: &mut Parser<'_>, id: &str) -> ValResult<ContentPiece> {
    match id.to_ascii_lowercase().as_str() {
        "open-quote" => Ok(ContentPiece::OpenQuote),
        "close-quote" => Ok(ContentPiece::CloseQuote),
        "no-open-quote" => Ok(ContentPiece::NoOpenQuote),
        "no-close-quote" => Ok(ContentPiece::NoCloseQuote),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// Sequence continuation piece: string / function / quote keyword (the
/// counter() family dispatches inside the nested block, see
/// parse_content_fn_body).
fn parse_content_piece(p: &mut Parser<'_>) -> ValResult<ContentPiece> {
    let t = p.next()?.clone();
    match t {
        cssparser::Token::QuotedString(ref s) => Ok(ContentPiece::Str(s.to_string())),
        cssparser::Token::Function(ref name) => {
            let fname = name.clone();
            p.parse_nested_block(|p| parse_content_fn_body(p, &fname))
        }
        cssparser::Token::Ident(ref id) => parse_content_quote_keyword(p, id),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// counter()/counters()/attr() function-body dispatch (inside the nested
/// block; other function names are rejected).
fn parse_content_fn_body(p: &mut Parser<'_>, name: &str) -> ValResult<ContentPiece> {
    match name.to_ascii_lowercase().as_str() {
        "counter" => {
            let idt = parse_content_ident(p)?;
            let style = parse_content_opt_style(p)?;
            Ok(ContentPiece::Counter {
                name: idt,
                style: style.unwrap_or_else(|| "decimal".into()),
            })
        }
        "counters" => {
            let idt = parse_content_ident(p)?;
            p.skip_whitespace();
            p.expect_comma()?;
            let sep = parse_content_string(p)?;
            let style = parse_content_opt_style(p)?;
            Ok(ContentPiece::Counters {
                name: idt,
                separator: sep,
                style: style.unwrap_or_else(|| "decimal".into()),
            })
        }
        "attr" => {
            let idt = parse_content_ident(p)?;
            Ok(ContentPiece::Attr(idt))
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// `<custom-ident>` (the counter/attr name; taken directly from
/// cssparser Token::Ident).
fn parse_content_ident(p: &mut Parser<'_>) -> ValResult<String> {
    p.skip_whitespace();
    let t = p.next()?.clone();
    match t {
        cssparser::Token::Ident(ref id) => Ok(id.to_string()),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// `<string>` (the counters separator).
fn parse_content_string(p: &mut Parser<'_>) -> ValResult<String> {
    p.skip_whitespace();
    let t = p.next()?.clone();
    match t {
        cssparser::Token::QuotedString(ref s) => Ok(s.to_string()),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// The optional trailing `<counter-style>` argument (an ident; the grammar
/// name is kept as the source text, and rendering slices it via
/// counter_format: registry → builtin → unknown name decimal). The leading
/// comma is required: `counter(x, style)`.
fn parse_content_opt_style(p: &mut Parser<'_>) -> ValResult<Option<String>> {
    p.skip_whitespace();
    // 无逗号 → 无第二参（剩余 token 由上层 parse_entirely 判尾垃圾）。
    if p.try_parse(|p| p.expect_comma()).is_err() {
        return Ok(None);
    }
    p.skip_whitespace();
    let t = p.next()?.clone();
    match t {
        cssparser::Token::Ident(ref id) => Ok(Some(id.to_string())),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// text-transform parsing (C2, css-text-3): the five keywords +
/// full-size-kana (T2: accepted but not transformed, deviation entry
/// documented in FEATURES.md).
pub fn parse_text_transform(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "none" => TextTransformKind::None,
            "uppercase" => TextTransformKind::Uppercase,
            "lowercase" => TextTransformKind::Lowercase,
            "capitalize" => TextTransformKind::Capitalize,
            "full-width" => TextTransformKind::FullWidth,
            "full-size-kana" => TextTransformKind::FullSizeKana,
            _ => return None,
        ))
    })
    .map(DeclValue::TextTransform)
}

/// overflow-wrap parsing (C2, css-text-3; the legacy alias word-wrap is
/// routed via the from_css_name supplemental table).
pub fn parse_overflow_wrap(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "normal" => OverflowWrapKind::Normal,
            "break-word" => OverflowWrapKind::BreakWord,
            "anywhere" => OverflowWrapKind::Anywhere,
            _ => return None,
        ))
    })
    .map(DeclValue::OverflowWrap)
}

/// word-break parsing (C2, css-text-3).
pub fn parse_word_break(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "normal" => WordBreakKind::Normal,
            "break-all" => WordBreakKind::BreakAll,
            "keep-all" => WordBreakKind::KeepAll,
            _ => return None,
        ))
    })
    .map(DeclValue::WordBreak)
}

/// hyphens parsing (F4, ADR-0028 D2, css-text-3 §5.4): none|manual|auto.
pub fn parse_hyphens(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "none" => HyphensKind::None,
            "manual" => HyphensKind::Manual,
            "auto" => HyphensKind::Auto,
            _ => return None,
        ))
    })
    .map(DeclValue::Hyphens)
}

/// Length family + `auto` (auto → LenAuto(None)), for adaptive uses such as
/// width/height/margin.
pub fn parse_len_auto(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    len_auto_with(p, &["auto"]).map(DeclValue::LenAuto)
}

/// Length family (`<length-percentage>`, calc included).
pub fn parse_len(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_length_percentage(p).map(DeclValue::Len)
}

/// Bare `<number>` (numeric uses such as opacity, flex-grow/shrink,
/// aspect-ratio).
pub fn parse_number_value(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_number(p).map(DeclValue::Number)
}

/// column-count (phase 2 ③): auto → ColumnCount(None); `<integer [1,∞]>` →
/// ColumnCount(Some(n)) (0/negative/non-integer is an invalid declaration →
/// Err and dropped).
pub fn parse_column_count(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(id) if id.eq_ignore_ascii_case("auto") => Ok(DeclValue::ColumnCount(None)),
        Token::Number {
            int_value: Some(n), ..
        } if *n >= 1 => Ok(DeclValue::ColumnCount(Some(
            (*n).min(u16::MAX as i32) as u16
        ))),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// break-inside (phase 3 ⑤a): avoid → Some(true), auto → Some(false).
/// Parsed and stored only (v1 makes every block unconditionally
/// unbreakable, so avoid is already the default semantics).
pub fn parse_break_inside(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    match p.next()? {
        Token::Ident(id) if id.eq_ignore_ascii_case("avoid") => {
            Ok(DeclValue::BreakInside(Some(true)))
        }
        Token::Ident(id) if id.eq_ignore_ascii_case("auto") => {
            Ok(DeclValue::BreakInside(Some(false)))
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// column-span (phase 3 ⑤b): all → Some(true), none → Some(false);
/// everything else (auto etc.) is invalid.
pub fn parse_column_span(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    match p.next()? {
        Token::Ident(id) if id.eq_ignore_ascii_case("all") => Ok(DeclValue::ColumnSpan(Some(true))),
        Token::Ident(id) if id.eq_ignore_ascii_case("none") => {
            Ok(DeclValue::ColumnSpan(Some(false)))
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// container-name (stage 2 ③): grammar `none | <custom-ident>+` — a
/// space-separated name list (not a comma list); none must appear alone;
/// custom-ident is case-sensitive and kept as-is per the spec; the `--`
/// reserved prefix is invalid.
fn parse_container_name(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut names: Vec<String> = Vec::new();
    loop {
        let t = p.try_parse(|p| -> ValResult<(bool, String)> {
            let t = p.next()?.clone();
            let Token::Ident(id) = &t else {
                return Err(p.new_error_for_next_token());
            };
            if id.starts_with("--") {
                return Err(p.new_error_for_next_token());
            }
            Ok((id.eq_ignore_ascii_case("none"), id.to_string()))
        });
        let (is_none, id) = match t {
            Ok(x) => x,
            Err(_) => break,
        };
        if is_none {
            if names.is_empty() {
                return Ok(DeclValue::ContainerName(Vec::new()));
            }
            return Err(p.new_error_for_next_token());
        }
        names.push(id);
    }
    if names.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    Ok(DeclValue::ContainerName(names))
}

/// The border-style keyword family (none/hidden/solid/dashed/dotted) —
/// phase 3 ⑤c column-rule-style reuses the same keyword set.
pub(crate) fn parse_border_style_keywords(p: &mut Parser<'_>) -> ValResult<BorderStyle> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "none" | "hidden" => BorderStyle::None,
            "solid" => BorderStyle::Solid,
            "dashed" => BorderStyle::Dashed,
            "dotted" => BorderStyle::Dotted,
            _ => return None,
        ))
    })
}

/// column-rule-width (phase 3 ⑤c): `<length [0,∞]>`|thin|medium|thick;
/// keywords materialize to fixed values (thin=1/medium=3/thick=5px). There
/// is no none keyword (unlike border-width — the rule's presence is
/// expressed via column-rule-style: none); negative lengths are clamped by
/// the layout side's max(0).
pub fn parse_column_rule_width(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let kw = p.try_parse(|p| -> ValResult<LengthPercentage> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) => {
                if name.eq_ignore_ascii_case("thin") {
                    Ok(LengthPercentage::Px(1.0))
                } else if name.eq_ignore_ascii_case("medium") {
                    Ok(LengthPercentage::Px(3.0))
                } else if name.eq_ignore_ascii_case("thick") {
                    Ok(LengthPercentage::Px(5.0))
                } else {
                    Err(p.new_error_for_next_token())
                }
            }
            _ => Err(p.new_error_for_next_token()),
        }
    });
    match kw {
        Ok(len) => Ok(DeclValue::ColumnRuleWidth(Some(len))),
        Err(_) => parse_length_percentage(p).map(|l| DeclValue::ColumnRuleWidth(Some(l))),
    }
}

/// column-rule-style (phase 3 ⑤c): the keyword family is the same as
/// border-style.
pub fn parse_column_rule_style(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_border_style_keywords(p).map(DeclValue::ColumnRuleStyle)
}

/// z-index: auto → ZIndex(None); a number → ZIndex(Some(n)).
pub fn parse_z_index(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let auto = p.try_parse(|p| -> ValResult<()> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) if name.eq_ignore_ascii_case("auto") => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if auto.is_ok() {
        return Ok(DeclValue::ZIndex(None));
    }
    parse_number(p).map(|n| DeclValue::ZIndex(Some(n)))
}

/// Color value (`<color>`, currentcolor included; color mismatches are
/// handled on the lerp side).
pub fn parse_color(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_color_value(p).map(DeclValue::Color)
}

/// Border width: none→0, thin/medium/thick materialized to 1/3/5px, or a
/// `<length>`.
pub fn parse_border_width(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    // none（配合 style:none 才真正不画）与关键字宽度
    let kw = p.try_parse(|p| -> ValResult<LengthPercentage> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) => {
                if name.eq_ignore_ascii_case("none") {
                    Ok(LengthPercentage::zero())
                } else if name.eq_ignore_ascii_case("thin") {
                    Ok(LengthPercentage::Px(1.0))
                } else if name.eq_ignore_ascii_case("medium") {
                    Ok(LengthPercentage::Px(3.0))
                } else if name.eq_ignore_ascii_case("thick") {
                    Ok(LengthPercentage::Px(5.0))
                } else {
                    Err(p.new_error_for_next_token())
                }
            }
            _ => Err(p.new_error_for_next_token()),
        }
    });
    match kw {
        Ok(len) => Ok(DeclValue::BorderWidth(Some(len))),
        Err(_) => parse_length_percentage(p).map(|l| DeclValue::BorderWidth(Some(l))),
    }
}

/// display keyword → Display (inline* is normalized to its block-level
/// equivalent with a warning, per the batch-5 ⑧ contract).
pub fn parse_display(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        // display:inline* 归一化告警（第五批⑧契约收窄，F1 ADR-0021）：
        // inline/inline-block 已成真变体；inline-flex/inline-grid/
        // inline-table 仍归一并告警。
        if matches!(
            s.to_ascii_lowercase().as_str(),
            "inline-flex" | "inline-grid" | "inline-table"
        ) {
            tracing::warn!(
                target: "style_engine::css",
                display = s,
                "display:inline* normalized: no IFC, participates as block-level"
            );
        }
        Some(match_ignore_ascii_case!(s,
            "block" | "flow-root" => Display::Block,
            "inline" => Display::Inline,
            "inline-block" => Display::InlineBlock,
            "flex" | "inline-flex" => Display::Flex,
            "grid" | "inline-grid" => Display::Grid,
            // 二期②表格核心值（inline-table 归一 Block 级语义，同⑧契约）；
            // 三期④补行组（header/footer 组归一）与 caption。
            "table" | "inline-table" => Display::Table,
            "table-row" => Display::TableRow,
            "table-cell" => Display::TableCell,
            "table-row-group" | "table-header-group" | "table-footer-group" => Display::TableRowGroup,
            "table-caption" => Display::TableCaption,
            "list-item" => Display::ListItem,
            "none" => Display::None,
            _ => return None,
        ))
    })
    .map(DeclValue::Display)
}

/// position keyword → Position (full value support since A4).
pub fn parse_position(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "static" => Position::Static,
            "relative" => Position::Relative,
            "absolute" => Position::Absolute,
            "fixed" => Position::Fixed,
            "sticky" => Position::Sticky,
            _ => return None,
        ))
    })
    .map(DeclValue::Position)
}

/// overflow keyword → Overflow (auto is normalized to Scroll).
pub fn parse_overflow(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "visible" => Overflow::Visible,
            "hidden" => Overflow::Hidden,
            "clip" => Overflow::Clip,
            "scroll" | "auto" => Overflow::Scroll,
            _ => return None,
        ))
    })
    .map(DeclValue::Overflow)
}

/// cursor (A1): a keyword subset; url() custom cursors = T2, rejected at
/// parse time (the declaration is dropped).
pub fn parse_cursor(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "auto" => CursorKind::Auto,
            "none" => CursorKind::None,
            "default" => CursorKind::Default,
            "pointer" => CursorKind::Pointer,
            "text" => CursorKind::Text,
            "wait" => CursorKind::Wait,
            "progress" => CursorKind::Progress,
            "help" => CursorKind::Help,
            "not-allowed" => CursorKind::NotAllowed,
            "move" => CursorKind::Move,
            "grab" => CursorKind::Grab,
            "grabbing" => CursorKind::Grabbing,
            "crosshair" => CursorKind::Crosshair,
            "zoom-in" => CursorKind::ZoomIn,
            "zoom-out" => CursorKind::ZoomOut,
            _ => return None,
        ))
    })
    .map(DeclValue::Cursor)
}

/// user-select（A1）。
pub fn parse_user_select(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "auto" => UserSelectKind::Auto,
            "none" => UserSelectKind::None,
            "text" => UserSelectKind::Text,
            "all" => UserSelectKind::All,
            "contain" => UserSelectKind::Contain,
            _ => return None,
        ))
    })
    .map(DeclValue::UserSelect)
}

/// pointer-events（A1）。
pub fn parse_pointer_events(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "auto" => PointerEventsKind::Auto,
            "none" => PointerEventsKind::None,
            _ => return None,
        ))
    })
    .map(DeclValue::PointerEvents)
}

/// outline-style (A2): line styles + auto.
pub fn parse_outline_style(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "none" => OutlineStyle::None,
            "auto" => OutlineStyle::Auto,
            "solid" => OutlineStyle::Solid,
            "dashed" => OutlineStyle::Dashed,
            "dotted" => OutlineStyle::Dotted,
            "double" => OutlineStyle::Double,
            "groove" => OutlineStyle::Groove,
            "ridge" => OutlineStyle::Ridge,
            "inset" => OutlineStyle::Inset,
            "outset" => OutlineStyle::Outset,
            _ => return None,
        ))
    })
    .map(DeclValue::OutlineStyle)
}

/// caret-color / accent-color (A1): `auto | <color>` → `Option<ColorValue>`
/// (None = auto; the initial value is None of the same type). The `auto`
/// semantics match CSS UI 4 background-color `auto`.
pub fn parse_caret_color(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_auto_or_color(p).map(DeclValue::CaretColor)
}

/// accent-color (A1).
pub fn parse_accent_color(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_auto_or_color(p).map(DeclValue::AccentColor)
}

/// Shared `auto | <color>` grammar (A1 behavior-hint color properties).
fn parse_auto_or_color(p: &mut Parser<'_>) -> ValResult<Option<ColorValue>> {
    let start = p.state();
    if let Ok(name) = p.expect_ident()
        && name.eq_ignore_ascii_case("auto")
    {
        return Ok(None);
    }
    p.reset(&start);
    parse_color_value(p).map(Some)
}

/// box-sizing keyword → BoxSizing.
pub fn parse_box_sizing(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "content-box" => BoxSizing::ContentBox,
            "border-box" => BoxSizing::BorderBox,
            _ => return None,
        ))
    })
    .map(DeclValue::BoxSizing)
}

/// Angle → degrees (the value.rs convention: always normalized to degrees).
/// A bare number is taken as deg (a tolerant superset).
fn parse_angle_deg(p: &mut Parser<'_>) -> ValResult<f32> {
    match p.next()? {
        Token::Dimension { value, unit, .. } => match unit.to_ascii_lowercase().as_ref() {
            "deg" => Ok(*value),
            "grad" => Ok(value * 0.9),
            "rad" => Ok(value.to_degrees()),
            "turn" => Ok(value * 360.0),
            _ => Err(p.new_error_for_next_token()),
        },
        Token::Number { value, .. } => Ok(*value),
        _ => Err(p.new_error_for_next_token()),
    }
}

fn expect_comma(p: &mut Parser<'_>) -> ValResult<()> {
    match p.next()? {
        Token::Comma => Ok(()),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// transform (ADR-0009 v1): none | a list of 2D functions. 3D functions are
/// explicitly rejected (a parse error → the declaration is dropped with a
/// warning, the CSS-tolerant path); unknown functions are rejected likewise.
pub fn parse_transform(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let none = p.try_parse(|p| -> ValResult<()> {
        let t = p.next()?;
        match &t {
            Token::Ident(name) if name.eq_ignore_ascii_case("none") => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if none.is_ok() {
        return Ok(DeclValue::Transform(Vec::new()));
    }
    let mut fns = Vec::new();
    loop {
        // 列表头：Function → 解析；Comma → 宽容跳过；EOF/`;`/`}` → 正常终止
        // （不消费终止符，交回声明循环）；其他 token → 严格拒绝。
        let name = match p.next() {
            Ok(Token::Function(name)) => name.to_ascii_lowercase(),
            Ok(Token::Comma) => continue,
            Ok(_) => break,
            Err(_) => break, // EOF：值流尽
        };
        let f = p.parse_nested_block(|p| match name.as_str() {
            "translate" | "translatex" | "translatey" => {
                let tx = parse_length_percentage(p)?;
                let ty = match name.as_str() {
                    "translatex" => LengthPercentage::Px(0.0),
                    "translatey" => {
                        // translatey 只有一个实参：y = tx 槽位解析值
                        return Ok(TransformFn::Translate(LengthPercentage::Px(0.0), tx));
                    }
                    _ => {
                        // 单实参形式 translate(tx)：ty = 0（try_parse 失败自动回滚）
                        let comma = p.try_parse(|p| expect_comma(p)).is_ok();
                        if comma {
                            parse_length_percentage(p).unwrap_or(LengthPercentage::Px(0.0))
                        } else {
                            LengthPercentage::Px(0.0)
                        }
                    }
                };
                Ok(TransformFn::Translate(tx, ty))
            }
            "scale" | "scalex" | "scaley" => {
                let sx = match p.next()? {
                    Token::Number { value, .. } => *value,
                    _ => return Err(p.new_error_for_next_token()),
                };
                let sy = match name.as_str() {
                    "scalex" => 1.0,
                    "scaley" => {
                        return Ok(TransformFn::Scale(1.0, sx));
                    }
                    _ => {
                        let second = p.try_parse(|p| -> ValResult<f32> {
                            expect_comma(p)?;
                            match p.next()? {
                                Token::Number { value, .. } => Ok(*value),
                                _ => Err(p.new_error_for_next_token()),
                            }
                        });
                        second.unwrap_or(sx)
                    }
                };
                Ok(TransformFn::Scale(sx, sy))
            }
            "rotate" => Ok(TransformFn::Rotate(parse_angle_deg(p)?)),
            "skew" | "skewx" | "skewy" => {
                let ax = parse_angle_deg(p)?;
                let ay = match name.as_str() {
                    "skewx" => 0.0,
                    "skewy" => return Ok(TransformFn::Skew(0.0, ax)),
                    _ => {
                        let second = p.try_parse(|p| -> ValResult<f32> {
                            expect_comma(p)?;
                            parse_angle_deg(p)
                        });
                        second.unwrap_or(0.0)
                    }
                };
                Ok(TransformFn::Skew(ax, ay))
            }
            "matrix" => {
                let mut v = [0.0f32; 6];
                for (i, slot) in v.iter_mut().enumerate() {
                    if i > 0 {
                        expect_comma(p)?;
                    }
                    match p.next()? {
                        Token::Number { value, .. } => *slot = *value,
                        _ => return Err(p.new_error_for_next_token()),
                    }
                }
                Ok(TransformFn::Matrix(v[0], v[1], v[2], v[3], v[4], v[5]))
            }
            // ADR-0009：3D 函数（含 perspective）v1 拒绝——vello 0.10 为纯 2D 仿射
            "matrix3d" | "translate3d" | "translatez" | "rotate3d" | "rotatex" | "rotatey"
            | "rotatez" | "scale3d" | "scalez" | "perspective" => Err(p.new_error_for_next_token()),
            _ => Err(p.new_error_for_next_token()),
        })?;
        fns.push(f);
    }
    Ok(DeclValue::Transform(fns))
}

/// filter / backdrop-filter (P2 batch, ADR-0031 D2, superseding the
/// ADR-0028 D1 tolerant surface): strict grammar
/// `none | <filter-function>+` (whitespace-separated, no commas). An
/// unknown function / invalid argument rejects the whole declaration
/// (Err → dropped + warn, is_clean=false).
/// none = a valid declaration that explicitly turns filtering off
/// (`Filters(vec![])`, overriding the lower cascade origin). The old
/// tolerant existence-only `parse_sc_effect` is retired; will-change and
/// isolation still use Effect.
pub fn parse_filter_value_list(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let none = p.try_parse(|p| -> ValResult<()> {
        match p.next()? {
            Token::Ident(name) if name.eq_ignore_ascii_case("none") => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if none.is_ok() {
        return Ok(DeclValue::Filters(Vec::new()));
    }
    let mut fns = Vec::new();
    loop {
        // 列表头：Function → 严格解析；EOF/`;`/`}`/其他 token → 终止
        // （不消费终止符，交回声明循环）。函数块解析失败整条拒绝（`?`）。
        let name = match p.next() {
            Ok(Token::Function(name)) => name.to_ascii_lowercase(),
            Ok(_) => break,
            Err(_) => break, // EOF：值流尽
        };
        let f = p.parse_nested_block(|p| parse_filter_fn_args(p, &name))?;
        fns.push(f);
    }
    if fns.is_empty() {
        // 非 none 且无任何函数（如 `filter:;`）→ 无效声明
        return Err(p.new_error_for_next_token());
    }
    Ok(DeclValue::Filters(fns))
}

/// Argument parsing for a single filter function (already inside the
/// parse_nested_block block).
fn parse_filter_fn_args(p: &mut Parser<'_>, name: &str) -> ValResult<FilterFn> {
    match name {
        "blur" => {
            let len = parse_filter_opt_length(p)?.unwrap_or(LengthPercentage::Px(0.0));
            Ok(FilterFn::Blur(len))
        }
        "brightness" => Ok(FilterFn::Brightness(parse_filter_amount(p, 1.0)?.max(0.0))),
        "contrast" => Ok(FilterFn::Contrast(parse_filter_amount(p, 1.0)?.max(0.0))),
        "grayscale" => Ok(FilterFn::Grayscale(
            parse_filter_amount(p, 1.0)?.clamp(0.0, 1.0),
        )),
        "sepia" => Ok(FilterFn::Sepia(
            parse_filter_amount(p, 1.0)?.clamp(0.0, 1.0),
        )),
        "saturate" => Ok(FilterFn::Saturate(parse_filter_amount(p, 1.0)?.max(0.0))),
        "invert" => Ok(FilterFn::Invert(
            parse_filter_amount(p, 1.0)?.clamp(0.0, 1.0),
        )),
        "opacity" => Ok(FilterFn::Opacity(
            parse_filter_amount(p, 1.0)?.clamp(0.0, 1.0),
        )),
        "hue-rotate" => Ok(FilterFn::HueRotate(
            parse_filter_opt_angle(p)?.unwrap_or(0.0),
        )),
        "drop-shadow" => parse_filter_drop_shadow(p),
        // css-filters-1 §4：url(#svg) 引用 = T2——cssparser 将 url( lex 为
        // Token::Url（非 Function），在列表循环即 break → 空表整条拒绝。
        _ => Err(p.new_error_for_next_token()),
    }
}

/// `<number-percentage>?` (the function argument may be omitted → the
/// default): numbers are taken directly; percentages go through
/// unit_value (already /100). End of block → default; any other token →
/// reject.
fn parse_filter_amount(p: &mut Parser<'_>, default: f32) -> ValResult<f32> {
    match p.next() {
        Ok(Token::Number { value, .. }) => Ok(*value),
        Ok(Token::Percentage { unit_value, .. }) => Ok(*unit_value),
        Ok(_) => Err(p.new_error_for_next_token()),
        Err(_) => Ok(default), // 块尽：缺省实参
    }
}

/// `<length>?` (percentages rejected — css-filters-1 blur/drop-shadow only
/// have a length grammar; a bare 0 is handled as px by the value.rs length
/// parser). End of block → None (the argument defaults).
fn parse_filter_opt_length(p: &mut Parser<'_>) -> ValResult<Option<LengthPercentage>> {
    let start = p.state();
    match parse_length_percentage(p) {
        Ok(LengthPercentage::Percent(_)) => Err(p.new_error_for_next_token()),
        Ok(lenp) => Ok(Some(lenp)),
        Err(e) => {
            p.reset(&start);
            match p.next() {
                Err(_) => Ok(None), // 块尽：缺省
                Ok(_) => Err(e),    // 有实参但非法 → 严格拒绝
            }
        }
    }
}

/// `<angle>?` (a bare number is tolerated as deg — the same repository
/// convention as parse_angle_deg).
fn parse_filter_opt_angle(p: &mut Parser<'_>) -> ValResult<Option<f32>> {
    let start = p.state();
    match parse_angle_deg(p) {
        Ok(a) => Ok(Some(a)),
        Err(e) => {
            p.reset(&start);
            match p.next() {
                Err(_) => Ok(None),
                Ok(_) => Err(e),
            }
        }
    }
}

/// drop-shadow(`<length>`{2,3} && `<color>`?): `&&` — either order is
/// accepted (color first or last); 2 lengths = no blur, 3 lengths = blur;
/// the color defaults to currentcolor (finalized by pick_scheme at paint
/// time).
fn parse_filter_drop_shadow(p: &mut Parser<'_>) -> ValResult<FilterFn> {
    let mut color: Option<ColorValue> = None;
    let start = p.state();
    match parse_color_value(p) {
        Ok(c) => color = Some(c),
        Err(_) => p.reset(&start),
    }
    let dx = match parse_filter_opt_length(p)? {
        Some(v) => v,
        None => return Err(p.new_error_for_next_token()), // dx 必需
    };
    let dy = match parse_filter_opt_length(p)? {
        Some(v) => v,
        None => return Err(p.new_error_for_next_token()), // dy 必需
    };
    let blur = parse_filter_opt_length(p)?.unwrap_or(LengthPercentage::Px(0.0));
    if color.is_none() {
        let start = p.state();
        match parse_color_value(p) {
            Ok(c) => color = Some(c),
            Err(_) => p.reset(&start),
        }
    }
    Ok(FilterFn::DropShadow {
        dx,
        dy,
        blur,
        color: color.unwrap_or(ColorValue::CurrentColor),
    })
}

/// v0 will-change parsing (batch 5 ㉒, the full SC-trigger set): when the
/// list contains a property that creates an SC when non-initial
/// (transform/filter/opacity/mix-blend-mode/clip-path/isolation/
/// perspective), Effect(true) is set; auto, other properties, or empty →
/// false. Any ident is accepted tolerantly (these are optimization hints —
/// an unknown ident does not make the declaration invalid); the
/// optimization itself is not implemented.
fn parse_will_change(p: &mut Parser) -> ValResult<DeclValue> {
    let mut triggers = false;
    loop {
        match p.next() {
            Ok(Token::Ident(name)) => {
                if matches!(
                    name.to_ascii_lowercase().as_str(),
                    "transform"
                        | "filter"
                        | "opacity"
                        | "mix-blend-mode"
                        | "clip-path"
                        | "isolation"
                        | "perspective"
                ) {
                    triggers = true;
                }
            }
            Ok(Token::Comma) => {}
            Ok(Token::Semicolon) => break,
            Ok(_) => {}
            Err(_) => break, // EOF：值流尽
        }
    }
    Ok(DeclValue::Effect(triggers))
}

/// isolation (batch 5 ㉒): `isolate` sets the flag (the property has only
/// two values, auto|isolate, and isolate creates an SC); auto → false.
fn parse_isolation(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "auto" => DeclValue::Effect(false),
            "isolate" => DeclValue::Effect(true),
            _ => return None,
        ))
    })
}

/// mix-blend-mode (P1-2): the concrete mode is stored
/// (`DeclValue::BlendMode`) — all 16 standard blend modes plus
/// plus-lighter/plus-darker are accepted.
/// border-*-radius (batch 5 ⑪, elliptical corners): longhand grammar
/// `<lp>{1,2}` — the second value is the vertical radius and defaults to
/// the horizontal one (circular corner). (The slash syntax belongs to the
/// shorthand only; see decl.rs.)
fn parse_corner_radius(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let h = parse_length_percentage(p)?;
    let v = p
        .try_parse(parse_length_percentage)
        .unwrap_or_else(|_| h.clone());
    Ok(DeclValue::Radius(h, v))
}

fn parse_mix_blend_mode(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    use BlendMode as B;
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "normal" => B::Normal,
            "multiply" => B::Multiply,
            "screen" => B::Screen,
            "overlay" => B::Overlay,
            "darken" => B::Darken,
            "lighten" => B::Lighten,
            "color-dodge" => B::ColorDodge,
            "color-burn" => B::ColorBurn,
            "hard-light" => B::HardLight,
            "soft-light" => B::SoftLight,
            "difference" => B::Difference,
            "exclusion" => B::Exclusion,
            "hue" => B::Hue,
            "saturation" => B::Saturation,
            "color" => B::Color,
            "luminosity" => B::Luminosity,
            "plus-lighter" => B::PlusLighter,
            "plus-darker" => B::PlusDarker,
            _ => return None,
        ))
    })
    .map(DeclValue::BlendMode)
}

/// transform-origin (batch 5 ⑬): the v1 2D subset — 1~2 components
/// (length-percentage or left/center/right/top/bottom keywords); the second
/// component defaults to center (50%); the third component (z axis, for 3D
/// scenarios) is not parsed and is swallowed tolerantly up to the
/// terminator. Keywords are grouped by semantic axis: left/right are
/// horizontal only, top/bottom vertical only, center both — `top left` ≡
/// `left top`; a single component means horizontal first (the CSS
/// single-value syntax), and when top/bottom is the single value the
/// horizontal component defaults to center.
pub fn parse_transform_origin(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    enum Axis {
        X(LengthPercentage),
        Y(LengthPercentage),
        Any(LengthPercentage),
    }
    // 关键字优先（带轴语义，try_parse 失败回退重放），否则按 LP 解析（Any）
    fn comp_ident(p: &mut Parser<'_>) -> ValResult<Axis> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) => Ok(match_ignore_ascii_case!(name,
                "left" => Axis::X(LengthPercentage::Percent(0.0)),
                "center" => Axis::Any(LengthPercentage::Percent(0.5)),
                "right" => Axis::X(LengthPercentage::Percent(1.0)),
                "top" => Axis::Y(LengthPercentage::Percent(0.0)),
                "bottom" => Axis::Y(LengthPercentage::Percent(1.0)),
                _ => return Err(p.new_error_for_next_token()),
            )),
            _ => Err(p.new_error_for_next_token()),
        }
    }
    fn component(p: &mut Parser<'_>) -> ValResult<Axis> {
        if let Ok(a) = p.try_parse(comp_ident) {
            return Ok(a);
        }
        Ok(Axis::Any(parse_length_percentage(p)?))
    }
    fn put_axis(c: Axis, x: &mut Option<LengthPercentage>, y: &mut Option<LengthPercentage>) {
        match c {
            Axis::X(v) => *x = Some(v),
            Axis::Y(v) => *y = Some(v),
            // Any：首个占横向（CSS 单值语法），双组件时补空位
            Axis::Any(v) => {
                if x.is_none() {
                    *x = Some(v);
                } else if y.is_none() {
                    *y = Some(v);
                }
            }
        }
    }
    let mut x: Option<LengthPercentage> = None;
    let mut y: Option<LengthPercentage> = None;
    put_axis(component(p)?, &mut x, &mut y);
    if let Ok(c2) = p.try_parse(component) {
        put_axis(c2, &mut x, &mut y);
    }
    // 终止符/余量（z 轴长度等）吞下交回声明循环
    loop {
        match p.next() {
            Ok(Token::Semicolon) | Err(_) => break,
            _ => {}
        }
    }
    Ok(DeclValue::TransformOrigin(
        x.unwrap_or(LengthPercentage::Percent(0.5)),
        y.unwrap_or(LengthPercentage::Percent(0.5)),
    ))
}

fn parse_align(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "normal" => Align::Normal,
            "start" => Align::Start,
            "end" => Align::End,
            "center" => Align::Center,
            "stretch" => Align::Stretch,
            "baseline" => Align::Baseline,
            "flex-start" => Align::FlexStart,
            "flex-end" => Align::FlexEnd,
            "space-between" => Align::SpaceBetween,
            "space-around" => Align::SpaceAround,
            "space-evenly" => Align::SpaceEvenly,
            _ => return None,
        ))
    })
    .map(DeclValue::Align)
}

/// flex-direction keyword → FlexDirection.
pub fn parse_flex_direction(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "row" => FlexDirection::Row,
            "row-reverse" => FlexDirection::RowReverse,
            "column" => FlexDirection::Column,
            "column-reverse" => FlexDirection::ColumnReverse,
            _ => return None,
        ))
    })
    .map(DeclValue::FlexDirection)
}

/// flex-wrap keyword → FlexWrap.
pub fn parse_flex_wrap(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "nowrap" => FlexWrap::NoWrap,
            "wrap" => FlexWrap::Wrap,
            "wrap-reverse" => FlexWrap::WrapReverse,
            _ => return None,
        ))
    })
    .map(DeclValue::FlexWrap)
}

/// border-*-style keyword → BorderStyle (hidden normalizes to None).
pub fn parse_border_style(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "none" | "hidden" => BorderStyle::None,
            "solid" => BorderStyle::Solid,
            "dashed" => BorderStyle::Dashed,
            "dotted" => BorderStyle::Dotted,
            _ => return None,
        ))
    })
    .map(DeclValue::BorderStyle)
}

/// text-align keyword → TextAlign (justify is consumed by parley Justify).
pub fn parse_text_align(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "start" => TextAlign::Start,
            "end" => TextAlign::End,
            "center" => TextAlign::Center,
            "left" => TextAlign::Left,
            "right" => TextAlign::Right,
            "justify" => TextAlign::Justify,
            _ => return None,
        ))
    })
    .map(DeclValue::TextAlign)
}

/// white-space keyword → WhiteSpace (pre-wrap/break-spaces normalize to
/// Pre).
pub fn parse_white_space(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "normal" => WhiteSpace::Normal,
            "nowrap" => WhiteSpace::NoWrap,
            "pre" => WhiteSpace::Pre,
            // A5 全值：pre-wrap/break-spaces 不再容错映射 Pre（保留+换行
            // 是真实语义）；pre-line=折叠空格保留换行。
            "pre-wrap" => WhiteSpace::PreWrap,
            "pre-line" => WhiteSpace::PreLine,
            "break-spaces" => WhiteSpace::BreakSpaces,
            _ => return None,
        ))
    })
    .map(DeclValue::WhiteSpace)
}

/// font-style keyword → FontStyle (oblique normalizes to Italic).
pub fn parse_font_style(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "normal" => FontStyle::Normal,
            "italic" | "oblique" => FontStyle::Italic,
            _ => return None,
        ))
    })
    .map(DeclValue::FontStyle)
}

/// line-height: normal | `<number>` (a unitless multiplier) |
/// `<length-percentage>`.
pub fn parse_line_height(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    // normal | <number> | <length-percentage>（try_parse 失败自动回滚）
    let kw = p.try_parse(|p| -> ValResult<LineHeight> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) if name.eq_ignore_ascii_case("normal") => Ok(LineHeight::Normal),
            Token::Number { value, .. } => Ok(LineHeight::Number(*value)),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    match kw {
        Ok(lh) => Ok(DeclValue::LineHeight(lh)),
        Err(_) => parse_length_percentage(p).map(|l| DeclValue::LineHeight(LineHeight::Len(l))),
    }
}

/// The css-fonts-4 §2.2.1 relative font-weight table: given the inherited
/// weight w and the direction (bolder/lighter), yields the computed weight.
/// Equivalent to the spec chart — "pick the next bolder/lighter step within
/// a font family that only has the four weights thin(100)/normal(400)/
/// bold(700)/heavy(900)":
///
/// | w            | bolder | lighter |
/// |--------------|--------|---------|
/// | w < 100      | 400    | unchanged |
/// | 100 ≤ w < 350| 400    | 100     |
/// | 350 ≤ w < 550| 700    | 100     |
/// | 550 ≤ w < 750| 900    | 400     |
/// | 750 ≤ w < 900| 900    | 700     |
/// | 900 ≤ w      | unchanged | 700  |
pub fn relative_font_weight(inherited: f32, bolder: bool) -> f32 {
    if bolder {
        if inherited < 350.0 {
            400.0
        } else if inherited < 550.0 {
            700.0
        } else if inherited < 900.0 {
            900.0
        } else {
            inherited // 900.. 不变
        }
    } else {
        // lighter
        if inherited < 100.0 {
            inherited // 不变
        } else if inherited < 550.0 {
            100.0
        } else if inherited < 750.0 {
            400.0
        } else {
            700.0
        }
    }
}

/// font-size's larger/smaller (css-fonts-4 `<<relative-size>>`): when the
/// parent size is exactly a [`ABSOLUTE_FONT_SIZES_PX`] table value, step one
/// entry (endpoint clamping: xx-small going smaller / xxx-large going larger
/// stays put); otherwise scale by the simple 1.2 ratio — the spec allows a
/// UA-defined ratio (recommended 1.2–1.5); 1.2 is chosen to match Chromium's
/// behavior.
pub fn relative_font_size(parent_px: f32, larger: bool) -> f32 {
    const EPS: f32 = 1e-4;
    let idx = ABSOLUTE_FONT_SIZES_PX
        .iter()
        .position(|&s| (s - parent_px).abs() < EPS);
    match idx {
        Some(i) => {
            if larger {
                ABSOLUTE_FONT_SIZES_PX[(i + 1).min(ABSOLUTE_FONT_SIZES_PX.len() - 1)]
            } else {
                ABSOLUTE_FONT_SIZES_PX[i.saturating_sub(1)]
            }
        }
        None => {
            if larger {
                parent_px * 1.2
            } else {
                parent_px / 1.2
            }
        }
    }
}

/// The `<absolute-size>` keyword name table (index-aligned with
/// [`ABSOLUTE_FONT_SIZES_PX`]; css-fonts-4 `<<absolute-size>>`, medium =
/// the initial 16px).
pub const ABSOLUTE_FONT_SIZE_NAMES: [&str; 8] = [
    "xx-small",
    "x-small",
    "small",
    "medium",
    "large",
    "x-large",
    "xx-large",
    "xxx-large",
];

/// The `<absolute-size>` keyword materialized-px table (css-fonts-4
/// §absolute-size-mapping allows UA table customization; the engine takes
/// the materialized values of the classic CSS 2.1 ladder; [`relative_font_size`]
/// table stepping and [`parse_font_size`] share this single source).
pub const ABSOLUTE_FONT_SIZES_PX: [f32; 8] = [9.0, 10.0, 13.0, 16.0, 18.0, 24.0, 32.0, 48.0];

/// font-size: a `<relative-size>` keyword, a CSS absolute font-size keyword
/// (materialized via the px table), or `<length-percentage>`.
pub fn parse_font_size(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    // 相对字号关键字——级联物化期按父计算字号终结（[`relative_font_size`]）
    let rel = p.try_parse(|p| -> ValResult<bool> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) if name.eq_ignore_ascii_case("larger") => Ok(true),
            Token::Ident(name) if name.eq_ignore_ascii_case("smaller") => Ok(false),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if let Ok(larger) = rel {
        return Ok(DeclValue::RelativeFontSize(larger));
    }
    let kw = p.try_parse(|p| -> ValResult<LengthPercentage> {
        let t = p.next()?.clone();
        // CSS 绝对字号表（medium = 初始 16px）
        let px = match &t {
            Token::Ident(name) => {
                let lower = name.to_ascii_lowercase();
                match ABSOLUTE_FONT_SIZE_NAMES
                    .iter()
                    .position(|n| *n == lower.as_str())
                {
                    Some(i) => ABSOLUTE_FONT_SIZES_PX[i],
                    None => return Err(p.new_error_for_next_token()),
                }
            }
            _ => return Err(p.new_error_for_next_token()),
        };
        Ok(LengthPercentage::Px(px))
    });
    match kw {
        Ok(len) => Ok(DeclValue::Len(len)),
        Err(_) => parse_length_percentage(p).map(DeclValue::Len),
    }
}

/// font-weight: normal→400, bold→700, a `<number>` in 1–1000, or the
/// relative keywords bolder/lighter (css-fonts-4 §2.2.1; for the
/// materialization rules see [`relative_font_weight`]).
pub fn parse_font_weight(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) if name.eq_ignore_ascii_case("normal") => Ok(DeclValue::Number(400.0)),
        Token::Ident(name) if name.eq_ignore_ascii_case("bold") => Ok(DeclValue::Number(700.0)),
        Token::Ident(name) if name.eq_ignore_ascii_case("bolder") => {
            Ok(DeclValue::RelativeFontWeight(true))
        }
        Token::Ident(name) if name.eq_ignore_ascii_case("lighter") => {
            Ok(DeclValue::RelativeFontWeight(false))
        }
        Token::Number { value, .. } => {
            // CSS 允许 1–1000；越界按容错拒绝
            if (1.0..=1000.0).contains(value) {
                Ok(DeclValue::Number(*value))
            } else {
                Err(p.new_error_for_next_token())
            }
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// aspect-ratio: auto | `<ratio>` (where `<ratio>` = number [/ number]).
pub fn parse_aspect_ratio(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let auto = p.try_parse(|p| -> ValResult<()> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) if name.eq_ignore_ascii_case("auto") => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if auto.is_ok() {
        return Ok(DeclValue::AspectRatio(None));
    }
    let w = parse_number(p)?;
    let h = p.try_parse(|p| -> ValResult<f32> {
        p.expect_delim('/')?;
        parse_number(p)
    });
    let ratio = match h {
        Ok(h) => {
            if h == 0.0 {
                return Err(p.new_error_for_next_token());
            }
            w / h
        }
        Err(_) => w,
    };
    if ratio <= 0.0 {
        return Err(p.new_error_for_next_token());
    }
    Ok(DeclValue::AspectRatio(Some(ratio)))
}

/// font-family: comma-separated; each item is a quoted string or a
/// sequence of consecutive Idents (joined with spaces).
pub fn parse_font_family(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut families: SmallVec<[FamilyName; 2]> = SmallVec::new();
    loop {
        p.skip_whitespace();
        let t = p.next_including_whitespace()?.clone();
        let family = match &t {
            Token::QuotedString(s) => FamilyName::Named(s.to_string()),
            Token::Ident(first) => {
                let mut name = first.to_string();
                // 连续 Ident（如 Times New Roman）以空格连接；遇 Comma/其他停止
                loop {
                    let save = p.state();
                    match p.next_including_whitespace() {
                        Ok(Token::WhiteSpace(_)) => continue,
                        Ok(Token::Ident(next)) => {
                            name.push(' ');
                            name.push_str(next);
                        }
                        Ok(_) => {
                            p.reset(&save);
                            break;
                        }
                        Err(_) => break,
                    }
                }
                generic_or_named(&name)
            }
            _ => return Err(p.new_error_for_next_token()),
        };
        families.push(family);
        // 逗号分隔
        let has_comma = p.try_parse(|p| -> ValResult<()> {
            p.expect_comma()?;
            Ok(())
        });
        if has_comma.is_err() {
            break;
        }
    }
    if families.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    Ok(DeclValue::FontFamily(FontFamilyList(families)))
}

fn generic_or_named(name: &str) -> FamilyName {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "serif" => FamilyName::Serif,
        "sans-serif" => FamilyName::SansSerif,
        "monospace" => FamilyName::Monospace,
        "cursive" => FamilyName::Cursive,
        "fantasy" => FamilyName::Fantasy,
        "system-ui" => FamilyName::SystemUi,
        _ => FamilyName::Named(name.to_string()),
    }
}

// ---------- grid track 列表 ----------

/// Bracketed-section probe: cssparser lexes `[ … ]` as a SquareBracketBlock
/// block token (not a Delim, by the same precedent as `(` →
/// ParenthesisBlock) → a hit returns Ok (parse_nested_block then reads the
/// block content); otherwise Err (try_parse rolls back). A standalone
/// helper lets the error type E be inferred through ValResult (inlining the
/// closure = E0282).
fn bracket_open(p: &mut Parser<'_>) -> ValResult<()> {
    match p.next() {
        Ok(Token::SquareBracketBlock) => Ok(()),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// Line-name collection inside a bracket block (SquareBracketBlock content
/// → name list; stops when the block is exhausted). Only
/// `<custom-ident>` is legal (spec §8.3); any other token invalidates the
/// declaration.
fn bracket_names(p: &mut Parser<'_>) -> ValResult<Vec<String>> {
    let mut names = Vec::new();
    loop {
        match p.next() {
            Ok(Token::Ident(id)) => names.push(id.to_string()),
            Ok(_) => return Err(p.new_error_for_next_token()),
            Err(_) => break,
        }
    }
    Ok(names)
}

/// Shared by grid-template-columns/rows and grid-auto-*: a track list
/// (repeat(`<integer>`, …) / repeat(auto-fill|auto-fit, …)).
pub fn parse_grid_tracks(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut tracks = Vec::new();
    let mut line_names: Vec<Vec<String>> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    loop {
        // [名1 名2] 括号线名段（E5，ADR-0020）——任意轨前/轨间/尾合法；
        // 同线多段（`[a] [b]`）合并为一槽（try_parse 失败自动回滚）。
        // cssparser 把 `[ … ]` 整体给成 SquareBracketBlock 块 token →
        // try_parse 命中后 parse_nested_block 取块内容读名。
        while p.try_parse(bracket_open).is_ok() {
            pending.extend(p.parse_nested_block(bracket_names)?);
        }
        if p.is_exhausted() {
            // 尾线名槽（第 N+1 槽）。
            line_names.push(std::mem::take(&mut pending));
            break;
        }
        // 轨前线名槽（第 i 槽）。
        line_names.push(std::mem::take(&mut pending));
        tracks.push(parse_track_size(p)?);
    }
    if tracks.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    Ok(DeclValue::GridTracks(GridTemplate { tracks, line_names }))
}

/// Rectangularity plus per-name rectangle validation (shared with shorthand
/// expansion): every row has the same cell count, and for each name the
/// cell count == row span × column span (a min/max bounds check misses
/// diagonal cells, so counting is required); a violation = invalid
/// declaration (spec §8.5).
pub(crate) fn validate_area_rows(p: &mut Parser<'_>, rows: &[Vec<String>]) -> ValResult<()> {
    let w = rows[0].len();
    if rows.iter().any(|r| r.len() != w) {
        return Err(p.new_error_for_next_token());
    }
    // (r_min, r_max, c_min, c_max, 格数)
    let mut spans: std::collections::HashMap<&str, (usize, usize, usize, usize, usize)> =
        std::collections::HashMap::new();
    for (r, row) in rows.iter().enumerate() {
        for (c, name) in row.iter().enumerate() {
            if name == "." {
                continue;
            }
            let e = spans.entry(name.as_str()).or_insert((r, r, c, c, 0));
            e.0 = e.0.min(r);
            e.1 = e.1.max(r);
            e.2 = e.2.min(c);
            e.3 = e.3.max(c);
            e.4 += 1;
        }
    }
    for (_r, _c, name) in rows
        .iter()
        .enumerate()
        .flat_map(|(r, row)| row.iter().enumerate().map(move |(c, n)| (r, c, n)))
    {
        if name == "." {
            continue;
        }
        let (r0, r1, c0, c1, count) = spans[name.as_str()];
        if count != (r1 - r0 + 1) * (c1 - c0 + 1) {
            return Err(p.new_error_for_next_token());
        }
    }
    Ok(())
}

/// grid-template-areas (E5, ADR-0020): a list of quoted strings; each row
/// is split on whitespace, `.` = an empty cell. Validation rules are in
/// `validate_area_rows`.
pub fn parse_grid_areas(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    loop {
        let s = match p.next() {
            Ok(Token::QuotedString(s)) => s.to_string(),
            _ => return Err(p.new_error_for_next_token()),
        };
        let row: Vec<String> = s.split_ascii_whitespace().map(String::from).collect();
        if row.is_empty() {
            return Err(p.new_error_for_next_token());
        }
        rows.push(row);
        if p.is_exhausted() {
            break;
        }
    }
    validate_area_rows(p, &rows)?;
    Ok(DeclValue::GridAreas(GridAreas { rows }))
}

/// Shared with shorthand expansion (P9-2, ADR-0040): an explicit track list
/// (css-grid-1 §7.3 `<explicit-track-list>` = [`<line-names>`?
/// `<track-size>`]+ `<line-names>?`). **Stops at `/` or exhaustion (does not
/// consume `/`)**. At least one track; line names without any track = Err.
pub(crate) fn parse_track_list_until_slash(p: &mut Parser<'_>) -> ValResult<GridTemplate> {
    let mut tracks = Vec::new();
    let mut line_names: Vec<Vec<String>> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    loop {
        while p.try_parse(bracket_open).is_ok() {
            pending.extend(p.parse_nested_block(bracket_names)?);
        }
        // 耗尽或 `/`（回滚不消费）都终止——终止时 pending 归尾线名槽。
        let at_slash = {
            let save = p.state();
            let hit = matches!(p.next(), Ok(Token::Delim(d)) if *d == '/');
            p.reset(&save);
            hit
        };
        if p.is_exhausted() || at_slash {
            line_names.push(std::mem::take(&mut pending));
            break;
        }
        line_names.push(std::mem::take(&mut pending));
        tracks.push(parse_track_size(p)?);
    }
    if tracks.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    Ok(GridTemplate { tracks, line_names })
}

/// Shared with shorthand expansion (P9-2, ADR-0040): grid-template's areas
/// form (css-grid-1 §7.3 `[<line-names>? <string> <track-size>?
/// <line-names>?]+ [/ `<explicit-track-list>`]?`). Each string is one row:
/// split on whitespace per row into grid-template-areas; the row track size
/// defaults to auto; the pre-row and post-row line-name segments merge into
/// slot i (the next group's pre-row names join the same slot). Returns
/// (rows template, optional columns, areas rows).
pub(crate) fn parse_template_areas_form(
    p: &mut Parser<'_>,
) -> ValResult<(GridTemplate, Option<GridTemplate>, Vec<Vec<String>>)> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row_tracks: Vec<TrackSize> = Vec::new();
    let mut line_names: Vec<Vec<String>> = Vec::new();
    let mut slot: Vec<String> = Vec::new();
    loop {
        // 行前线名段（可多段）。
        while p.try_parse(bracket_open).is_ok() {
            slot.extend(p.parse_nested_block(bracket_names)?);
        }
        let at_slash = {
            let save = p.state();
            let hit = matches!(p.next(), Ok(Token::Delim(d)) if *d == '/');
            p.reset(&save);
            hit
        };
        if at_slash || p.is_exhausted() {
            break;
        }
        let s = match p.next() {
            Ok(Token::QuotedString(s)) => s.to_string(),
            _ => return Err(p.new_error_for_next_token()),
        };
        let row: Vec<String> = s.split_ascii_whitespace().map(String::from).collect();
        if row.is_empty() {
            return Err(p.new_error_for_next_token());
        }
        line_names.push(std::mem::take(&mut slot));
        rows.push(row);
        // 行轨尺寸（缺省 auto；try_parse 失败自动回滚）。
        row_tracks.push(p.try_parse(parse_track_size).unwrap_or(TrackSize::Auto));
        // 行后线名段（归下一槽/尾槽）。
        while p.try_parse(bracket_open).is_ok() {
            slot.extend(p.parse_nested_block(bracket_names)?);
        }
        if p.is_exhausted() {
            break;
        }
    }
    if rows.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    line_names.push(slot);
    validate_area_rows(p, &rows)?;
    let cols = {
        let save = p.state();
        let at_slash = matches!(p.next(), Ok(Token::Delim(d)) if *d == '/');
        p.reset(&save);
        if at_slash {
            p.next()?; // 消费 '/'
            Some(parse_track_list_until_slash(p)?)
        } else {
            None
        }
    };
    Ok((
        GridTemplate {
            tracks: row_tracks,
            line_names,
        },
        cols,
        rows,
    ))
}

/// grid-{row,column}-{start,end} (E5, ADR-0020):
/// auto | `<integer>` | span `<integer>` | span `<ident>` | `<ident>`.
/// (Line number 0 is invalid; the spec's mixed form `<integer> && <ident>`
/// is a v1 deviation documented in FEATURES.md.)
pub fn parse_grid_line_spec(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    Ok(DeclValue::GridLine(parse_line_spec_value(p)?))
}

/// `<grid-line>` value parsing (no DeclValue wrapper; shared with shorthand
/// expansion).
pub(crate) fn parse_line_spec_value(p: &mut Parser<'_>) -> ValResult<GridLineSpec> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(id) if id.eq_ignore_ascii_case("auto") => Ok(GridLineSpec::Auto),
        Token::Ident(id) if id.eq_ignore_ascii_case("span") => {
            let t2 = p.next()?.clone();
            match &t2 {
                Token::Number { value, .. } => {
                    let k = *value as u16;
                    if *value < 1.0 || *value != k as f32 {
                        return Err(p.new_error_for_next_token());
                    }
                    Ok(GridLineSpec::Span(k))
                }
                Token::Ident(id2) => Ok(GridLineSpec::SpanName(id2.to_string())),
                _ => Err(p.new_error_for_next_token()),
            }
        }
        Token::Number { value, .. } => {
            let n = *value as i64;
            if n == 0 || n < i16::MIN as i64 || n > i16::MAX as i64 || n as f32 != *value {
                return Err(p.new_error_for_next_token());
            }
            Ok(GridLineSpec::Number(n as i16))
        }
        Token::Ident(id) => Ok(GridLineSpec::Name(id.to_string())),
        _ => Err(p.new_error_for_next_token()),
    }
}

fn parse_track_min(p: &mut Parser<'_>) -> ValResult<TrackSize> {
    parse_track_size(p)
}

/// A single `<track-size>`: length first (a try_parse failure rolls back
/// automatically), then keywords/functions.
fn parse_track_size(p: &mut Parser<'_>) -> ValResult<TrackSize> {
    if let Ok(len) = p.try_parse(|p| parse_length_percentage(p)) {
        return Ok(TrackSize::Len(len));
    }
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) if name.eq_ignore_ascii_case("auto") => Ok(TrackSize::Auto),
        Token::Ident(name) if name.eq_ignore_ascii_case("max-content") => Ok(TrackSize::MaxContent),
        Token::Ident(name) if name.eq_ignore_ascii_case("min-content") => Ok(TrackSize::MinContent),
        Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("fr") => {
            Ok(TrackSize::Fr(*value))
        }
        Token::Function(name) if name.eq_ignore_ascii_case("minmax") => {
            let (a, b) = p.parse_nested_block(|p| {
                let a = parse_track_min(p)?;
                p.expect_comma()?;
                let b = parse_track_size(p)?;
                Ok((a, b))
            })?;
            Ok(TrackSize::MinMax(Box::new(a), Box::new(b)))
        }
        Token::Function(name) if name.eq_ignore_ascii_case("repeat") => {
            let size = p.parse_nested_block(|p| {
                // 首参数：auto-fill / auto-fit 关键字（阶段2①）或固定次数。
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(id) if id.eq_ignore_ascii_case("auto-fill") => {
                        p.expect_comma()?;
                        Ok(TrackSize::RepeatAuto(false, parse_repeat_track_list(p)?))
                    }
                    Token::Ident(id) if id.eq_ignore_ascii_case("auto-fit") => {
                        p.expect_comma()?;
                        Ok(TrackSize::RepeatAuto(true, parse_repeat_track_list(p)?))
                    }
                    Token::Number { value, .. } => {
                        let n = *value as u16;
                        if n == 0 {
                            return Err(p.new_error_for_next_token());
                        }
                        p.expect_comma()?;
                        Ok(TrackSize::Repeat(n, parse_repeat_track_list(p)?))
                    }
                    _ => Err(p.new_error_for_next_token()),
                }
            })?;
            Ok(size)
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// repeat(…) track list: a comma-separated track-size sequence (shared by
/// fixed-count and auto repeats). auto-repeat cannot nest inside any repeat
/// (CSS spec; Chromium also deems it invalid); fixed-count nesting keeps the
/// existing parse tolerance (defensively treated as auto at layout time).
fn parse_repeat_track_list(p: &mut Parser<'_>) -> ValResult<Vec<TrackSize>> {
    let mut list = Vec::new();
    loop {
        let item = parse_track_size(p)?;
        if matches!(item, TrackSize::RepeatAuto(..)) {
            return Err(p.new_error_for_next_token());
        }
        list.push(item);
        if p.is_exhausted() {
            break;
        }
        p.expect_comma()?;
    }
    Ok(list)
}

// ---------- background / box-shadow ----------

/// background-image: `<bg-image>#` = none | url() | linear-gradient() |
/// radial-gradient() | conic-gradient() (comma-separated layers; F3b,
/// ADR-0024).
pub fn parse_background_image(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut layers = Vec::new();
    loop {
        layers.push(parse_background_image_one(p)?);
        match p.next() {
            Ok(Token::Comma) => continue,
            _ => break,
        }
    }
    Ok(DeclValue::BackgroundImage(layers))
}

/// A single background layer image (none | url() | gradient functions).
/// pub(crate): reused by the background shorthand.
pub(crate) fn parse_background_image_one(p: &mut Parser<'_>) -> ValResult<BackgroundImage> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) if name.eq_ignore_ascii_case("none") => Ok(BackgroundImage::None),
        Token::UnquotedUrl(url) => Ok(BackgroundImage::Url(url.to_string())),
        Token::Function(name) if name.eq_ignore_ascii_case("url") => {
            let url: String = p.parse_nested_block(|p| {
                let t = p.next()?.clone();
                match t {
                    Token::QuotedString(s) => Ok(s.to_string()),
                    _ => Err(p.new_error_for_next_token()),
                }
            })?;
            Ok(BackgroundImage::Url(url))
        }
        Token::Function(name) => {
            // linear/radial/conic-gradient 与 repeating- 前缀族（P1-3，
            // css-images-3）：内层文法逐一相同，仅 repeating 标记不同。
            let lower = name.to_ascii_lowercase();
            let (base, repeating) = match lower.strip_prefix("repeating-") {
                Some(b) => (b, true),
                None => (lower.as_str(), false),
            };
            let parser: fn(&mut Parser<'_>) -> ValResult<Gradient> = match base {
                "linear-gradient" => parse_linear_gradient,
                "radial-gradient" => parse_radial_gradient,
                "conic-gradient" => parse_conic_gradient,
                _ => return Err(p.new_error_for_next_token()),
            };
            let mut g = p.parse_nested_block(parser)?;
            g.repeating = repeating;
            Ok(BackgroundImage::Gradient(g))
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// repeat-style single keyword → axis value.
fn repeat_axis_from(name: &str) -> Option<RepeatAxis> {
    if name.eq_ignore_ascii_case("repeat") {
        Some(RepeatAxis::Repeat)
    } else if name.eq_ignore_ascii_case("space") {
        Some(RepeatAxis::Space)
    } else if name.eq_ignore_ascii_case("round") {
        Some(RepeatAxis::Round)
    } else if name.eq_ignore_ascii_case("no-repeat") {
        Some(RepeatAxis::NoRepeat)
    } else {
        None
    }
}

/// A single-layer repeat-style: repeat-x | repeat-y |
/// [repeat|space|round|no-repeat]{1,2} (two values: first = x, second = y;
/// a single value applies to both axes). pub(crate): reused by the
/// background shorthand.
pub(crate) fn parse_repeat_xy(p: &mut Parser<'_>) -> ValResult<RepeatXY> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) if name.eq_ignore_ascii_case("repeat-x") => Ok(RepeatXY {
            x: RepeatAxis::Repeat,
            y: RepeatAxis::NoRepeat,
        }),
        Token::Ident(name) if name.eq_ignore_ascii_case("repeat-y") => Ok(RepeatXY {
            x: RepeatAxis::NoRepeat,
            y: RepeatAxis::Repeat,
        }),
        Token::Ident(name) => {
            let first = repeat_axis_from(name).ok_or_else(|| p.new_error_for_next_token())?;
            // 可选第二关键字（try_parse 失败回滚 = 单值形式）
            let second = p.try_parse(|p| -> ValResult<RepeatAxis> {
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(name) => {
                        repeat_axis_from(name).ok_or_else(|| p.new_error_for_next_token())
                    }
                    _ => Err(p.new_error_for_next_token()),
                }
            });
            Ok(match second {
                Ok(y) => RepeatXY { x: first, y },
                Err(_) => RepeatXY { x: first, y: first },
            })
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// background-repeat: `<repeat-style>#` (F3b, ADR-0024).
pub fn parse_background_repeat(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut layers = Vec::new();
    loop {
        layers.push(parse_repeat_xy(p)?);
        match p.next() {
            Ok(Token::Comma) => continue,
            _ => break,
        }
    }
    Ok(DeclValue::BackgroundRepeat(layers))
}

/// A single attachment component (scroll | fixed | local). pub(crate):
/// reused by the background shorthand (ADR-0024).
pub(crate) fn parse_attachment_one(p: &mut Parser<'_>) -> ValResult<Attachment> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) if name.eq_ignore_ascii_case("scroll") => Ok(Attachment::Scroll),
        Token::Ident(name) if name.eq_ignore_ascii_case("fixed") => Ok(Attachment::Fixed),
        Token::Ident(name) if name.eq_ignore_ascii_case("local") => Ok(Attachment::Local),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// background-attachment: `<attachment>#` = scroll | fixed | local.
pub fn parse_background_attachment(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut layers = Vec::new();
    loop {
        layers.push(parse_attachment_one(p)?);
        match p.next() {
            Ok(Token::Comma) => continue,
            _ => break,
        }
    }
    Ok(DeclValue::BackgroundAttachment(layers))
}

/// A single bg-position token read (a keyword or a length/percentage).
/// pub(crate): the background shorthand reuses this via parse_pos_toks.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PosTok {
    /// left.
    Left,
    /// right.
    Right,
    /// top.
    Top,
    /// bottom.
    Bottom,
    /// center.
    Center,
    /// Length/percentage.
    LP(LengthPercentage),
}

/// Read a single bg-position token: LP first (a try_parse failure rolls
/// back), then keywords.
fn pos_tok(p: &mut Parser<'_>) -> ValResult<PosTok> {
    if let Ok(lp) = p.try_parse(|p| -> ValResult<LengthPercentage> { parse_length_percentage(p) }) {
        return Ok(PosTok::LP(lp));
    }
    let t = p.next()?.clone();
    Ok(match &t {
        Token::Ident(name) if name.eq_ignore_ascii_case("left") => PosTok::Left,
        Token::Ident(name) if name.eq_ignore_ascii_case("right") => PosTok::Right,
        Token::Ident(name) if name.eq_ignore_ascii_case("top") => PosTok::Top,
        Token::Ident(name) if name.eq_ignore_ascii_case("bottom") => PosTok::Bottom,
        Token::Ident(name) if name.eq_ignore_ascii_case("center") => PosTok::Center,
        _ => return Err(p.new_error_for_next_token()),
    })
}

/// Read one layer of bg-position tokens (the grammar caps it at 4 values).
pub(crate) fn parse_pos_toks(p: &mut Parser<'_>) -> ValResult<Vec<PosTok>> {
    let mut toks = Vec::new();
    while toks.len() < 4
        && let Ok(t) = p.try_parse(|p| -> ValResult<PosTok> { pos_tok(p) })
    {
        toks.push(t);
    }
    if toks.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    Ok(toks)
}

/// Edge keyword → the equivalent base (left/top = 0%, right/bottom = 100%).
fn edge_base(tok: &PosTok) -> LengthPercentage {
    match tok {
        PosTok::Right | PosTok::Bottom => LengthPercentage::Percent(1.0),
        _ => LengthPercentage::Percent(0.0),
    }
}

/// right/bottom offsets are negated (paint time uses one unified
/// forward-only formula: pos = pct(base)·(area−img) + len(base) +
/// pct(offset)·(area−img) + len(offset)).
fn neg_lp(lp: LengthPercentage) -> LengthPercentage {
    match lp {
        LengthPercentage::Px(v) => LengthPercentage::Px(-v),
        LengthPercentage::Em(v) => LengthPercentage::Em(-v),
        LengthPercentage::Rem(v) => LengthPercentage::Rem(-v),
        LengthPercentage::Percent(v) => LengthPercentage::Percent(-v),
        LengthPercentage::Vw(v) => LengthPercentage::Vw(-v),
        LengthPercentage::Vh(v) => LengthPercentage::Vh(-v),
        LengthPercentage::Cqw(v) => LengthPercentage::Cqw(-v),
        LengthPercentage::Cqh(v) => LengthPercentage::Cqh(-v),
        LengthPercentage::Cqi(v) => LengthPercentage::Cqi(-v),
        LengthPercentage::Cqb(v) => LengthPercentage::Cqb(-v),
        LengthPercentage::Ch(v) => LengthPercentage::Ch(-v),
        LengthPercentage::Ex(v) => LengthPercentage::Ex(-v),
        LengthPercentage::Ic(v) => LengthPercentage::Ic(-v),
        // calc() 结构性取负不支持（CalcNode 无取负变换）——B 级：原样保留，
        // right/bottom calc 偏移语义偏差在案（ADR-0024）。
        LengthPercentage::Calc(c) => LengthPercentage::Calc(c),
    }
}

/// bg-position token stream → Position2D (the full css-backgrounds-3 §4
/// grammar: edge + optional offset form groups; bare LPs fill x/y in order;
/// center fills the missing axis; a lone center = both axes).
pub(crate) fn interpret_position(toks: &[PosTok]) -> Option<Position2D> {
    let mut x: Option<PositionComp> = None;
    let mut y: Option<PositionComp> = None;
    let mut bare: Vec<LengthPercentage> = Vec::new();
    let mut centers = 0usize;
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            PosTok::Left | PosTok::Right => {
                if x.is_some() {
                    return None;
                }
                let base = edge_base(&toks[i]);
                let offset = match toks.get(i + 1) {
                    Some(PosTok::LP(lp)) => {
                        let off = if matches!(toks[i], PosTok::Right) {
                            neg_lp(lp.clone())
                        } else {
                            lp.clone()
                        };
                        i += 1;
                        Some(off)
                    }
                    _ => None,
                };
                x = Some(PositionComp { base, offset });
            }
            PosTok::Top | PosTok::Bottom => {
                if y.is_some() {
                    return None;
                }
                let base = edge_base(&toks[i]);
                let offset = match toks.get(i + 1) {
                    Some(PosTok::LP(lp)) => {
                        let off = if matches!(toks[i], PosTok::Bottom) {
                            neg_lp(lp.clone())
                        } else {
                            lp.clone()
                        };
                        i += 1;
                        Some(off)
                    }
                    _ => None,
                };
                y = Some(PositionComp { base, offset });
            }
            PosTok::LP(lp) => bare.push(lp.clone()),
            PosTok::Center => centers += 1,
        }
        i += 1;
    }
    // 裸 LP：第一 → x，第二 → y（production 2 顺序语义）；多余非法。
    for lp in bare {
        if x.is_none() {
            x = Some(PositionComp {
                base: lp,
                offset: None,
            });
        } else if y.is_none() {
            y = Some(PositionComp {
                base: lp,
                offset: None,
            });
        } else {
            return None;
        }
    }
    // center：双缺=双轴 Center；单缺按轴补；有余非法。
    for _ in 0..centers {
        if x.is_none() && y.is_none() {
            // 单/双 center 开局：首个占 x（结尾统一补 y 缺省）
            x = Some(PositionComp {
                base: LengthPercentage::Percent(0.5),
                offset: None,
            });
        } else if y.is_none() {
            y = Some(PositionComp {
                base: LengthPercentage::Percent(0.5),
                offset: None,
            });
        } else {
            return None;
        }
    }
    // 缺省补 Center（如 `left` → y=center；`10px` → y=center）。
    let center = PositionComp {
        base: LengthPercentage::Percent(0.5),
        offset: None,
    };
    Some(Position2D {
        x: x.unwrap_or_else(|| center.clone()),
        y: y.unwrap_or(center),
    })
}

/// background-position：`<bg-position>#`（F3b，ADR-0024）。
pub fn parse_background_position(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut layers = Vec::new();
    loop {
        let toks = parse_pos_toks(p)?;
        let pos = interpret_position(&toks).ok_or_else(|| p.new_error_for_next_token())?;
        layers.push(pos);
        match p.next() {
            Ok(Token::Comma) => continue,
            _ => break,
        }
    }
    Ok(DeclValue::BackgroundPosition(layers))
}

/// A single background-size component (LP tried first via try_parse, then
/// the auto keyword as fallback).
fn parse_lpor_auto(p: &mut Parser<'_>) -> ValResult<LPorAuto> {
    if let Ok(lp) =
        p.try_parse(|p| -> ValResult<LPorAuto> { Ok(LPorAuto::LP(parse_length_percentage(p)?)) })
    {
        return Ok(lp);
    }
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) if name.eq_ignore_ascii_case("auto") => Ok(LPorAuto::Auto),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// One bg-size component: auto | cover | contain | [`<LP>` | auto]{1,2}
/// (a single value = the width gets the value, the height is auto).
/// pub(crate): reused by the background shorthand (ADR-0024).
pub(crate) fn parse_bg_size_one(p: &mut Parser<'_>) -> ValResult<BgSize> {
    // 关键字三项先试（try_parse 失败回滚）。
    let kw = p.try_parse(|p| -> ValResult<BgSize> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) if name.eq_ignore_ascii_case("auto") => Ok(BgSize::Auto),
            Token::Ident(name) if name.eq_ignore_ascii_case("cover") => Ok(BgSize::Cover),
            Token::Ident(name) if name.eq_ignore_ascii_case("contain") => Ok(BgSize::Contain),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    match kw {
        Ok(s) => Ok(s),
        Err(_) => {
            let w = parse_lpor_auto(p)?;
            // 可选第二分量
            let h = match p.try_parse(|p| parse_lpor_auto(p)) {
                Ok(a) => a,
                Err(_) => LPorAuto::Auto,
            };
            Ok(BgSize::Explicit { w, h })
        }
    }
}

/// background-size: `<bg-size>#` = auto | cover | contain |
/// [`<length-percentage>` | auto]{1,2} (a single value = the width gets the
/// value, the height is auto; F3b ADR-0024).
pub fn parse_background_size(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut layers = Vec::new();
    loop {
        layers.push(parse_bg_size_one(p)?);
        match p.next() {
            Ok(Token::Comma) => continue,
            _ => break,
        }
    }
    Ok(DeclValue::BackgroundSize(layers))
}

/// Background-box keyword → value.
pub(crate) fn background_box_from(name: &str) -> Option<BackgroundBox> {
    if name.eq_ignore_ascii_case("border-box") {
        Some(BackgroundBox::BorderBox)
    } else if name.eq_ignore_ascii_case("padding-box") {
        Some(BackgroundBox::PaddingBox)
    } else if name.eq_ignore_ascii_case("content-box") {
        Some(BackgroundBox::ContentBox)
    } else {
        None
    }
}

/// background-origin: `<box>#` = border-box | padding-box | content-box.
pub fn parse_background_origin(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut layers = Vec::new();
    loop {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) => {
                let b = background_box_from(name).ok_or_else(|| p.new_error_for_next_token())?;
                layers.push(b);
            }
            _ => return Err(p.new_error_for_next_token()),
        }
        match p.next() {
            Ok(Token::Comma) => continue,
            _ => break,
        }
    }
    Ok(DeclValue::BackgroundOrigin(layers))
}

/// background-clip: `<box># | text` (the css-backgrounds-4 text value is
/// accepted; painting degrades, Tier B, ADR-0024).
pub fn parse_background_clip(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut layers = Vec::new();
    loop {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) if name.eq_ignore_ascii_case("text") => {
                layers.push(BackgroundClip::Text);
            }
            Token::Ident(name) => {
                let b = background_box_from(name).ok_or_else(|| p.new_error_for_next_token())?;
                layers.push(BackgroundClip::Box(b));
            }
            _ => return Err(p.new_error_for_next_token()),
        }
        match p.next() {
            Ok(Token::Comma) => continue,
            _ => break,
        }
    }
    Ok(DeclValue::BackgroundClip(layers))
}

// ---------- clip-path（F3c，ADR-0025）----------

/// geometry-box keyword → reference box. margin-box degrades to border-box
/// (Tier B, documented in FEATURES.md; the full css-masking-1 reference-box
/// family includes margin-box).
fn geometry_box_from(name: &str) -> Option<BackgroundBox> {
    if name.eq_ignore_ascii_case("border-box") {
        Some(BackgroundBox::BorderBox)
    } else if name.eq_ignore_ascii_case("padding-box") {
        Some(BackgroundBox::PaddingBox)
    } else if name.eq_ignore_ascii_case("content-box") {
        Some(BackgroundBox::ContentBox)
    } else if name.eq_ignore_ascii_case("margin-box") {
        tracing::warn!("clip-path: margin-box 参考盒降级为 border-box（B 级）");
        Some(BackgroundBox::BorderBox)
    } else {
        None
    }
}

/// Splice the reference box into the shape (the second step of the
/// `<basic-shape> || <geometry-box>` combination; Other/None do not
/// combine).
fn clip_shape_with_reference(s: ClipShape, b: BackgroundBox) -> ClipShape {
    match s {
        ClipShape::Inset {
            insets,
            radius,
            reference: _,
        } => ClipShape::Inset {
            insets,
            radius,
            reference: b,
        },
        ClipShape::Circle {
            radius,
            at,
            reference: _,
        } => ClipShape::Circle {
            radius,
            at,
            reference: b,
        },
        ClipShape::Ellipse {
            rx,
            ry,
            at,
            reference: _,
        } => ClipShape::Ellipse {
            rx,
            ry,
            at,
            reference: b,
        },
        ClipShape::Polygon {
            nonzero,
            points,
            reference: _,
        } => ClipShape::Polygon {
            nonzero,
            points,
            reference: b,
        },
        other => other,
    }
}

/// margin-shorthand 1..=4 values → the four top/right/bottom/left values
/// (shared by inset() insets and corner radii).
fn expand_lp_shorthand(vals: Vec<LengthPercentage>) -> [LengthPercentage; 4] {
    match vals.as_slice() {
        [a] => [a.clone(), a.clone(), a.clone(), a.clone()],
        [a, b] => [a.clone(), b.clone(), a.clone(), b.clone()],
        [a, b, c] => [a.clone(), b.clone(), c.clone(), b.clone()],
        [a, b, c, d] => [a.clone(), b.clone(), c.clone(), d.clone()],
        _ => unreachable!("调用方保证 1..=4 个值"),
    }
}

/// Read 1..=4 consecutive LPs (the margin-shorthand section; the 5th onward
/// is not consumed).
fn parse_lp_run(p: &mut Parser<'_>) -> ValResult<Vec<LengthPercentage>> {
    let mut vals = Vec::new();
    while vals.len() < 4 {
        match p.try_parse(parse_length_percentage) {
            Ok(lp) => vals.push(lp),
            Err(_) => break,
        }
    }
    if vals.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    Ok(vals)
}

/// clip-path circle/ellipse explicit radius: a non-negative
/// length-percentage (css-shapes-1 §3.2.1; for circle the percentage base is
/// √(w²+h²)/√2 and for ellipse the width/height per axis — parsed in sync in
/// paint.rs; negative values are rejected; Calc, whose sign cannot be
/// determined statically, is accepted).
fn parse_clip_radius_lp(p: &mut Parser<'_>, allow_percent: bool) -> ValResult<LengthPercentage> {
    let lp = parse_length_percentage(p)?;
    match &lp {
        LengthPercentage::Percent(_) if !allow_percent => {
            return Err(p.new_error_for_next_token());
        }
        LengthPercentage::Px(v)
        | LengthPercentage::Em(v)
        | LengthPercentage::Rem(v)
        | LengthPercentage::Vw(v)
        | LengthPercentage::Vh(v)
        | LengthPercentage::Cqw(v)
        | LengthPercentage::Cqh(v)
        | LengthPercentage::Cqi(v)
        | LengthPercentage::Cqb(v)
        | LengthPercentage::Ch(v)
        | LengthPercentage::Ex(v)
        | LengthPercentage::Ic(v)
            if *v < 0.0 =>
        {
            return Err(p.new_error_for_next_token());
        }
        _ => {}
    }
    Ok(lp)
}

/// One clip-path radius item: an explicit LP or the closest-side/
/// farthest-side/closest-corner/farthest-corner keywords.
fn parse_clip_radius(p: &mut Parser<'_>, allow_percent: bool) -> ValResult<ClipRadius> {
    if let Ok(lp) = p.try_parse(|p| parse_clip_radius_lp(p, allow_percent)) {
        return Ok(ClipRadius::Length(lp));
    }
    let t = p.next()?.clone();
    Ok(match &t {
        Token::Ident(name) if name.eq_ignore_ascii_case("closest-side") => ClipRadius::ClosestSide,
        Token::Ident(name) if name.eq_ignore_ascii_case("farthest-side") => {
            ClipRadius::FarthestSide
        }
        Token::Ident(name) if name.eq_ignore_ascii_case("closest-corner") => {
            ClipRadius::ClosestCorner
        }
        Token::Ident(name) if name.eq_ignore_ascii_case("farthest-corner") => {
            ClipRadius::FarthestCorner
        }
        _ => return Err(p.new_error_for_next_token()),
    })
}

/// The `at <bg-position>` of circle/ellipse (reuses the full bg-position
/// grammar tools).
fn parse_clip_at(p: &mut Parser<'_>) -> ValResult<Position2D> {
    let toks = parse_pos_toks(p)?;
    interpret_position(&toks).ok_or_else(|| p.new_error_for_next_token())
}

/// A single `<basic-shape>` item: the inset()/circle()/ellipse()/polygon()
/// functions; url()/path() are swallowed tolerantly → Other (SVG resources
/// and the path syntax = T2, Tier B). The reference box is spliced in by the
/// caller (border-box is set here as a placeholder).
fn parse_basic_shape(p: &mut Parser<'_>) -> ValResult<ClipShape> {
    let t = p.next()?.clone();
    match &t {
        Token::Function(name) if name.eq_ignore_ascii_case("inset") => {
            p.parse_nested_block(|p| {
                let insets = expand_lp_shorthand(parse_lp_run(p)?);
                // round <水平集> [ / <垂直集> ]?（无斜杠 = 垂直集取水平集）
                let radius = p
                    .try_parse(
                        |p| -> ValResult<([LengthPercentage; 4], [LengthPercentage; 4])> {
                            let t = p.next()?.clone();
                            match &t {
                                Token::Ident(n) if n.eq_ignore_ascii_case("round") => {
                                    let h = expand_lp_shorthand(parse_lp_run(p)?);
                                    let v = p
                                        .try_parse(|p| -> ValResult<[LengthPercentage; 4]> {
                                            match p.next()? {
                                                Token::Delim('/') => {}
                                                _ => return Err(p.new_error_for_next_token()),
                                            }
                                            Ok(expand_lp_shorthand(parse_lp_run(p)?))
                                        })
                                        .unwrap_or_else(|_| h.clone());
                                    Ok((h, v))
                                }
                                _ => Err(p.new_error_for_next_token()),
                            }
                        },
                    )
                    .ok();
                Ok(ClipShape::Inset {
                    insets,
                    radius,
                    reference: BackgroundBox::BorderBox,
                })
            })
        }
        Token::Function(name) if name.eq_ignore_ascii_case("circle") => p.parse_nested_block(|p| {
            let radius = p
                .try_parse(|p| parse_clip_radius(p, true))
                .unwrap_or(ClipRadius::ClosestSide);
            let at = p.try_parse(|p| -> ValResult<Position2D> {
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(n) if n.eq_ignore_ascii_case("at") => parse_clip_at(p),
                    _ => Err(p.new_error_for_next_token()),
                }
            });
            let at = at.unwrap_or(Position2D {
                x: PositionComp {
                    base: LengthPercentage::Percent(0.5),
                    offset: None,
                },
                y: PositionComp {
                    base: LengthPercentage::Percent(0.5),
                    offset: None,
                },
            });
            Ok(ClipShape::Circle {
                radius,
                at,
                reference: BackgroundBox::BorderBox,
            })
        }),
        Token::Function(name) if name.eq_ignore_ascii_case("ellipse") => {
            p.parse_nested_block(|p| {
                let rx = p.try_parse(|p| parse_clip_radius(p, true));
                let ry = p.try_parse(|p| parse_clip_radius(p, true));
                let at = p.try_parse(|p| -> ValResult<Position2D> {
                    let t = p.next()?.clone();
                    match &t {
                        Token::Ident(n) if n.eq_ignore_ascii_case("at") => parse_clip_at(p),
                        _ => Err(p.new_error_for_next_token()),
                    }
                });
                let at = at.unwrap_or(Position2D {
                    x: PositionComp {
                        base: LengthPercentage::Percent(0.5),
                        offset: None,
                    },
                    y: PositionComp {
                        base: LengthPercentage::Percent(0.5),
                        offset: None,
                    },
                });
                Ok(ClipShape::Ellipse {
                    rx: rx.unwrap_or(ClipRadius::ClosestSide),
                    ry: ry.unwrap_or(ClipRadius::ClosestSide),
                    at,
                    reference: BackgroundBox::BorderBox,
                })
            })
        }
        Token::Function(name) if name.eq_ignore_ascii_case("polygon") => {
            p.parse_nested_block(|p| {
                let mut nonzero = true;
                if let Ok(rule) = p.try_parse(|p| -> ValResult<bool> {
                    let t = p.next()?.clone();
                    match &t {
                        Token::Ident(n) if n.eq_ignore_ascii_case("nonzero") => Ok(true),
                        Token::Ident(n) if n.eq_ignore_ascii_case("evenodd") => Ok(false),
                        _ => Err(p.new_error_for_next_token()),
                    }
                }) {
                    nonzero = rule;
                    if !matches!(p.next(), Ok(Token::Comma)) {
                        return Err(p.new_error_for_next_token());
                    }
                }
                let mut points = Vec::new();
                loop {
                    let x = parse_length_percentage(p)?;
                    let y = parse_length_percentage(p)?;
                    points.push((x, y));
                    match p.next() {
                        Ok(Token::Comma) => continue,
                        _ => break,
                    }
                }
                if points.len() < 3 {
                    return Err(p.new_error_for_next_token());
                }
                Ok(ClipShape::Polygon {
                    nonzero,
                    points,
                    reference: BackgroundBox::BorderBox,
                })
            })
        }
        Token::Function(name) if name.eq_ignore_ascii_case("url") => {
            p.parse_nested_block(|p| -> ValResult<()> {
                while p.next().is_ok() {}
                Ok(())
            })?;
            tracing::warn!("clip-path: url() 收容为无效（SVG clipPath = T2）");
            Ok(ClipShape::Other)
        }
        Token::Function(name) if name.eq_ignore_ascii_case("path") => {
            p.parse_nested_block(|p| -> ValResult<()> {
                while p.next().is_ok() {}
                Ok(())
            })?;
            tracing::warn!("clip-path: path() 收容为无效（path 语法 = T2）");
            Ok(ClipShape::Other)
        }
        // cssparser 词法化：无引号 URL 是 UnquotedUrl 顶层 token，而非
        // Function("url")——两形态都必须接住，否则宽容路径失效。
        Token::UnquotedUrl(_) => {
            tracing::warn!("clip-path: url() 收容为无效（SVG clipPath = T2）");
            Ok(ClipShape::Other)
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// clip-path (css-masking-1 §5.1 / css-shapes-1 §3, F3c, ADR-0025):
/// `<basic-shape> || <geometry-box>` | none. Any order; a geometry-box
/// alone = inset(0) referencing that box; a duplicated or leftover
/// combination part rejects the whole declaration.
pub fn parse_clip_path(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let none = p.try_parse(|p| -> ValResult<()> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) if name.eq_ignore_ascii_case("none") => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if none.is_ok() {
        return Ok(DeclValue::ClipPath(ClipShape::None));
    }
    let mut shape: Option<ClipShape> = None;
    let mut reference: Option<BackgroundBox> = None;
    // `||` = 次序不限、各至多一次；两轮试探。
    for _ in 0..2 {
        if shape.is_none()
            && let Ok(s) = p.try_parse(parse_basic_shape)
        {
            shape = Some(s);
            continue;
        }
        if reference.is_none()
            && let Ok(b) = p.try_parse(|p| -> ValResult<BackgroundBox> {
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(name) => {
                        geometry_box_from(name).ok_or_else(|| p.new_error_for_next_token())
                    }
                    _ => Err(p.new_error_for_next_token()),
                }
            })
        {
            reference = Some(b);
            continue;
        }
        break;
    }
    let value = match (shape, reference) {
        (Some(s), Some(b)) => clip_shape_with_reference(s, b),
        (Some(s), None) => s,
        (None, Some(b)) => ClipShape::Inset {
            insets: [
                LengthPercentage::Px(0.0),
                LengthPercentage::Px(0.0),
                LengthPercentage::Px(0.0),
                LengthPercentage::Px(0.0),
            ],
            radius: None,
            reference: b,
        },
        (None, None) => return Err(p.new_error_for_next_token()),
    };
    Ok(DeclValue::ClipPath(value))
}

fn parse_linear_gradient(p: &mut Parser<'_>) -> ValResult<Gradient> {
    let mut kind = GradientKind::Linear(Angle(180.0));
    // 可选方向：<angle> | to <side>（corner 形式依赖盒子尺寸，MVP 容错拒绝，
    // 偏差记录于 FEATURES.md）
    let dir = p.try_parse(|p| -> ValResult<Angle> {
        let t = p.next()?.clone();
        match &t {
            Token::Dimension { value, unit, .. } => {
                let deg = angle_deg_from_unit(*value, unit)
                    .ok_or_else(|| p.new_error_for_next_token())?;
                Ok(Angle(deg))
            }
            Token::Ident(name) if name.eq_ignore_ascii_case("to") => {
                let side = p.next()?.clone();
                let Token::Ident(s) = &side else {
                    return Err(p.new_error_for_next_token());
                };
                let deg = match s.to_ascii_lowercase().as_str() {
                    "top" => 0.0,
                    "right" => 90.0,
                    "bottom" => 180.0,
                    "left" => 270.0,
                    _ => return Err(p.new_error_for_next_token()),
                };
                Ok(Angle(deg))
            }
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if let Ok(angle) = dir {
        kind = GradientKind::Linear(angle);
        // 方向段之后必须跟逗号
        p.expect_comma().map_err(cssparser::ParseError::from)?;
    }
    let (stops, hints) = parse_gradient_stops(p)?;
    Ok(Gradient {
        kind,
        repeating: false, // 分派器（parse_background_image_one）按函数名改写
        stops,
        hints,
    })
}

/// Dimension value + unit → degrees.
fn angle_deg_from_unit(value: f32, unit: &str) -> Option<f32> {
    if unit.eq_ignore_ascii_case("deg") {
        Some(value)
    } else if unit.eq_ignore_ascii_case("grad") {
        Some(value * 0.9)
    } else if unit.eq_ignore_ascii_case("rad") {
        Some(value.to_degrees())
    } else if unit.eq_ignore_ascii_case("turn") {
        Some(value * 360.0)
    } else {
        None
    }
}

fn parse_radial_gradient(p: &mut Parser<'_>) -> ValResult<Gradient> {
    // 语法（css-images-3 子集）：[circle|ellipse] || [closest-side|closest-corner|
    // farthest-side|farthest-corner|<length>{1,2}] [at <position>]? <stops>
    // `||` 组合按容错实现为顺序无关的前导解析（T4c）。
    let mut shape: Option<RadialShape> = None;
    let mut size: Option<RadialSize> = None;
    let mut position: Option<(LengthPercentage, LengthPercentage)> = None;
    loop {
        if position.is_none() {
            let at = p.try_parse(|p| -> ValResult<()> {
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(name) if name.eq_ignore_ascii_case("at") => Ok(()),
                    _ => Err(p.new_error_for_next_token()),
                }
            });
            if at.is_ok() {
                let x = parse_position_component(p)?;
                let y = parse_position_component(p)?;
                position = Some((x, y));
                continue;
            }
        }
        if shape.is_none() {
            let s = p.try_parse(|p| -> ValResult<RadialShape> {
                let t = p.next()?.clone();
                let Token::Ident(name) = &t else {
                    return Err(p.new_error_for_next_token());
                };
                match name.to_ascii_lowercase().as_str() {
                    "circle" => Ok(RadialShape::Circle),
                    "ellipse" => Ok(RadialShape::Ellipse),
                    _ => Err(p.new_error_for_next_token()),
                }
            });
            if let Ok(s) = s {
                shape = Some(s);
                continue;
            }
        }
        if size.is_none() {
            let kw = p.try_parse(|p| -> ValResult<RadialSize> {
                let t = p.next()?.clone();
                let Token::Ident(name) = &t else {
                    return Err(p.new_error_for_next_token());
                };
                match name.to_ascii_lowercase().as_str() {
                    "closest-side" => Ok(RadialSize::ClosestSide),
                    "closest-corner" => Ok(RadialSize::ClosestCorner),
                    "farthest-side" => Ok(RadialSize::FarthestSide),
                    "farthest-corner" => Ok(RadialSize::FarthestCorner),
                    _ => Err(p.new_error_for_next_token()),
                }
            });
            if let Ok(k) = kw {
                size = Some(k);
                continue;
            }
            let rx = p.try_parse(parse_length_percentage);
            if let Ok(rx) = rx {
                let ry = p.try_parse(parse_length_percentage).ok();
                size = Some(RadialSize::Explicit { rx, ry });
                continue;
            }
        }
        break;
    }
    if shape.is_some() || size.is_some() || position.is_some() {
        p.expect_comma().map_err(cssparser::ParseError::from)?;
    }
    let (stops, hints) = parse_gradient_stops(p)?;
    Ok(Gradient {
        repeating: false, // 分派器按函数名改写
        kind: GradientKind::Radial(RadialSpec {
            shape: shape.unwrap_or(RadialShape::Ellipse),
            size: size.unwrap_or(RadialSize::FarthestCorner),
            position: position.unwrap_or((
                LengthPercentage::Percent(0.5),
                LengthPercentage::Percent(0.5),
            )),
        }),
        stops,
        hints,
    })
}

/// Conic gradient (C3, css-images-3; ADR-0017):
/// `conic-gradient([from <angle>]? [at <position>]? ,? <stop-list>)`.
/// CSS 0deg = 12 o'clock, clockwise; the angle is stored in degrees and
/// shifted to start at the +X axis when mapped to peniko.
fn parse_conic_gradient(p: &mut Parser<'_>) -> ValResult<Gradient> {
    let mut from: Option<Angle> = None;
    let mut position: Option<(LengthPercentage, LengthPercentage)> = None;
    loop {
        if from.is_none() {
            let f = p.try_parse(|p| -> ValResult<()> {
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(name) if name.eq_ignore_ascii_case("from") => Ok(()),
                    _ => Err(p.new_error_for_next_token()),
                }
            });
            if f.is_ok() {
                from = Some(Angle(parse_angle_deg(p)?));
                continue;
            }
        }
        if position.is_none() {
            let at = p.try_parse(|p| -> ValResult<()> {
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(name) if name.eq_ignore_ascii_case("at") => Ok(()),
                    _ => Err(p.new_error_for_next_token()),
                }
            });
            if at.is_ok() {
                let x = parse_position_component(p)?;
                let y = parse_position_component(p)?;
                position = Some((x, y));
                continue;
            }
        }
        break;
    }
    if from.is_some() || position.is_some() {
        p.expect_comma().map_err(cssparser::ParseError::from)?;
    }
    let (stops, hints) = parse_gradient_stops(p)?;
    Ok(Gradient {
        repeating: false, // 分派器按函数名改写
        kind: GradientKind::Conic(ConicSpec {
            from: from.unwrap_or(Angle(0.0)),
            position: position.unwrap_or((
                LengthPercentage::Percent(0.5),
                LengthPercentage::Percent(0.5),
            )),
        }),
        stops,
        hints,
    })
}

/// object-fit value parsing (C3, css-images-3):
/// fill|contain|cover|none|scale-down.
pub fn parse_object_fit(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "fill" => ObjectFitKind::Fill,
            "contain" => ObjectFitKind::Contain,
            "cover" => ObjectFitKind::Cover,
            "none" => ObjectFitKind::None,
            "scale-down" => ObjectFitKind::ScaleDown,
            _ => return None,
        ))
    })
    .map(DeclValue::ObjectFit)
}

/// float value parsing (E4, css-position-3 / ADR-0019): none|left|right.
pub fn parse_float(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "none" => FloatKind::None,
            "left" => FloatKind::Left,
            "right" => FloatKind::Right,
            _ => return None,
        ))
    })
    .map(DeclValue::Float)
}

/// clear value parsing (E4, css-position-3 / ADR-0019):
/// none|left|right|both.
pub fn parse_clear(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "none" => ClearKind::None,
            "left" => ClearKind::Left,
            "right" => ClearKind::Right,
            "both" => ClearKind::Both,
            _ => return None,
        ))
    })
    .map(DeclValue::Clear)
}

/// object-position value parsing (C3, css-images-3): a two-component
/// `<position>` (x, y). The component grammar is the same as radial
/// `at <position>` (parse_position_component).
pub fn parse_object_position(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let x = parse_position_component(p)?;
    let y = parse_position_component(p)?;
    Ok(DeclValue::ObjectPosition(x, y))
}

/// Position component: `<length-percentage>` | left | center | right | top |
/// bottom.
fn parse_position_component(p: &mut Parser<'_>) -> ValResult<LengthPercentage> {
    if let Ok(lp) = p.try_parse(parse_length_percentage) {
        return Ok(lp);
    }
    let t = p.next()?.clone();
    let Token::Ident(name) = &t else {
        return Err(p.new_error_for_next_token());
    };
    match name.to_ascii_lowercase().as_str() {
        "left" | "top" => Ok(LengthPercentage::Percent(0.0)),
        "center" => Ok(LengthPercentage::Percent(0.5)),
        "right" | "bottom" => Ok(LengthPercentage::Percent(1.0)),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// Gradient stop list parsing (css-images-3 color-stop-list; completed by
/// P9-1a ADR-0038). Each item = `<color> && <length-percentage>{0,2}` (any
/// order):
/// - color + 0 positions → a stop (positions are distributed evenly);
/// - color + 1 position → a stop;
/// - color + 2 positions → the css-images-4 double-position desugar (two
///   stops of the same color = a clamped band);
/// - a lone position (no color) → a color hint: it does not join the
///   even distribution; at sampling time `apply_gradient_hints` expands it
///   into a synthetic midpoint stop of the colors before and after;
///   hints before the first stop / after the last stop are a syntax error
///   (the whole value is IACVT).
///
/// Returns (stop list, hint list); ≥ 2 stops (hints do not count).
fn parse_gradient_stops(p: &mut Parser<'_>) -> ValResult<(Vec<ColorStop>, Vec<GradientHint>)> {
    let mut stops: Vec<ColorStop> = Vec::new();
    let mut hints: Vec<GradientHint> = Vec::new();
    loop {
        let mut lps: SmallVec<[LengthPercentage; 2]> = SmallVec::new();
        let mut color: Option<ColorValue> = None;
        loop {
            if lps.len() < 2
                && let Ok(lp) = p.try_parse(parse_length_percentage)
            {
                lps.push(lp);
                continue;
            }
            if color.is_none()
                && let Ok(c) = p.try_parse(parse_color_value)
            {
                color = Some(c);
                continue;
            }
            break;
        }
        match (color.take(), lps.len()) {
            (Some(c), 0) => stops.push(ColorStop {
                color: c,
                position: None,
            }),
            (Some(c), 1) => stops.push(ColorStop {
                color: c,
                position: Some(lps[0].clone()),
            }),
            // css-images-4 双位置：`red 10% 90%` ≡ red@10% 与 red@90%
            // 两个同色停点（钳制区间；位置序保持输入序，逆序由
            // §4.5.2 单调化兜底）。
            (Some(c), 2) => {
                stops.push(ColorStop {
                    color: c,
                    position: Some(lps[0].clone()),
                });
                stops.push(ColorStop {
                    color: c,
                    position: Some(lps[1].clone()),
                });
            }
            // 色彩提示：无颜色的单项 <length-percentage>；
            // after_stop = 下一停点索引（解析期即锚定）。
            (None, 1) => {
                if stops.is_empty() {
                    return Err(p.new_error_for_next_token()); // 首停点前提示非法
                }
                hints.push(GradientHint {
                    after_stop: stops.len(),
                    position: lps[0].clone(),
                });
            }
            (None, _) => return Err(p.new_error_for_next_token()), // 2 位置无颜色 / 空项
            // lps 收集循环上限 2，此臂不可达；防御视为语法错
            (Some(_), _) => return Err(p.new_error_for_next_token()),
        }
        let more = p.try_parse(|p| p.expect_comma());
        if more.is_err() {
            break;
        }
    }
    if stops.len() < 2 {
        return Err(p.new_error_for_next_token());
    }
    // 尾随提示（末停点之后无后继停点）非法
    if hints.last().is_some_and(|h| h.after_stop >= stops.len()) {
        return Err(p.new_error_for_next_token());
    }
    Ok((stops, hints))
}

/// box-shadow: none or a comma-separated shadow list (a leading inset,
/// 2/3/4 lengths + a color).
pub fn parse_box_shadow(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let none = p.try_parse(|p| -> ValResult<()> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) if name.eq_ignore_ascii_case("none") => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if none.is_ok() {
        return Ok(DeclValue::BoxShadows(SmallVec::new()));
    }
    let mut shadows = BoxShadowList::new();
    loop {
        let mut lens: SmallVec<[LengthPercentage; 4]> = SmallVec::new();
        let mut color: Option<ColorValue> = None;
        let mut inset = false;
        loop {
            // inset 关键字（第五批⑩）：前置或尾随皆许（CSS 文法两端）；
            // 重复 inset 落入长度/颜色均失败 → 尾随 token 错误 → 整条丢弃
            if !inset {
                let kw = p.try_parse(|p| -> ValResult<()> {
                    let t = p.next()?.clone();
                    match &t {
                        Token::Ident(name) if name.eq_ignore_ascii_case("inset") => Ok(()),
                        _ => Err(p.new_error_for_next_token()),
                    }
                });
                if kw.is_ok() {
                    inset = true;
                    continue;
                }
            }
            // 长度优先，其次颜色
            if let Ok(l) = p.try_parse(parse_length_percentage) {
                if lens.len() == 4 {
                    return Err(p.new_error_for_next_token());
                }
                lens.push(l);
                continue;
            }
            let col = p.try_parse(parse_color_value);
            match col {
                Ok(c) => {
                    if color.is_some() {
                        return Err(p.new_error_for_next_token());
                    }
                    color = Some(c);
                    continue;
                }
                Err(_) => break,
            }
        }
        if lens.len() < 2 {
            return Err(p.new_error_for_next_token());
        }
        let color = color.unwrap_or(ColorValue::CurrentColor);
        shadows.push(BoxShadow {
            offset_x: lens[0].clone(),
            offset_y: lens[1].clone(),
            blur: lens.get(2).cloned().unwrap_or_else(LengthPercentage::zero),
            spread: lens.get(3).cloned().unwrap_or_else(LengthPercentage::zero),
            color,
            inset,
        });
        let more = p.try_parse(|p| p.expect_comma());
        if more.is_err() {
            break;
        }
    }
    Ok(DeclValue::BoxShadows(shadows))
}

// ---------- F3d（ADR-0026）：border-image + 字体深化解析器 ----------

// border-image 源图直接复用 parse_background_image_one（dispatch 与简写
// 均引用之，无独立别名）。

/// A border-image-slice component: `<number [0,∞]>` |
/// `<percentage [0,∞]>` (negative values are rejected).
fn parse_bi_slice_comp(p: &mut Parser<'_>) -> ValResult<BorderImageSliceComp> {
    let t = p.next()?.clone();
    match &t {
        Token::Number { value, .. } => {
            if *value < 0.0 {
                return Err(p.new_error_for_next_token());
            }
            Ok(BorderImageSliceComp::Number(*value))
        }
        Token::Percentage { unit_value, .. } => {
            if *unit_value < 0.0 {
                return Err(p.new_error_for_next_token());
            }
            Ok(BorderImageSliceComp::Percentage(*unit_value))
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// border-image-slice at the value level: `[<number>|<percentage>]{1,4} &&
/// fill?` (fill in any position, tolerated per Chromium). pub(crate):
/// reused by the border-image shorthand.
pub(crate) fn parse_bi_slice_value(p: &mut Parser<'_>) -> ValResult<BorderImageSlice> {
    let mut vals: Vec<BorderImageSliceComp> = Vec::new();
    let mut fill = false;
    loop {
        if vals.len() < 4
            && let Ok(v) = p.try_parse(parse_bi_slice_comp)
        {
            vals.push(v);
            continue;
        }
        if !fill
            && p.try_parse(|p| -> ValResult<()> {
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(name) if name.eq_ignore_ascii_case("fill") => Ok(()),
                    _ => Err(p.new_error_for_next_token()),
                }
            })
            .is_ok()
        {
            fill = true;
            continue;
        }
        break;
    }
    if vals.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    Ok(BorderImageSlice {
        slices: trbl_expand(&vals),
        fill,
    })
}

/// border-image-slice: the value-level wrapper (the declaration entry).
pub fn parse_border_image_slice(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_bi_slice_value(p).map(DeclValue::BorderImageSlice)
}

/// Negative simple-dimension LP rejection (calc cannot be judged statically
/// → accepted, per the F3c precedent).
fn lp_reject_negative(p: &mut Parser<'_>, lp: &LengthPercentage) -> ValResult<()> {
    match lp {
        LengthPercentage::Px(v)
        | LengthPercentage::Em(v)
        | LengthPercentage::Rem(v)
        | LengthPercentage::Vw(v)
        | LengthPercentage::Vh(v)
        | LengthPercentage::Cqw(v)
        | LengthPercentage::Cqh(v)
        | LengthPercentage::Cqi(v)
        | LengthPercentage::Cqb(v)
        | LengthPercentage::Ch(v)
        | LengthPercentage::Ex(v)
        | LengthPercentage::Ic(v)
            if *v < 0.0 =>
        {
            Err(p.new_error_for_next_token())
        }
        _ => Ok(()),
    }
}

/// A border-image-width component: auto | `<number [0,∞]>` |
/// `<length-percentage>`.
fn parse_bi_width_comp(p: &mut Parser<'_>) -> ValResult<BorderImageWidthComp> {
    // auto（try_parse 回滚语义，避免预消费后重复 next）
    let auto = p.try_parse(|p| -> ValResult<()> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) if name.eq_ignore_ascii_case("auto") => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if auto.is_ok() {
        return Ok(BorderImageWidthComp::Auto);
    }
    // <number [0,∞]> — 对应边 border-width 倍数
    let num = p.try_parse(|p| -> ValResult<f32> {
        let t = p.next()?.clone();
        match &t {
            Token::Number { value, .. } if *value >= 0.0 => Ok(*value),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if let Ok(v) = num {
        return Ok(BorderImageWidthComp::Number(v));
    }
    // <length-percentage>（负拒绝；% 相对绘制域，calc 收容）
    let lp = parse_length_percentage(p)?;
    lp_reject_negative(p, &lp)?;
    Ok(BorderImageWidthComp::Length(lp))
}

/// border-image-width at the value level: 1-4 values TRBL-expanded.
/// pub(crate): reused by the shorthand.
pub(crate) fn parse_bi_width_value(p: &mut Parser<'_>) -> ValResult<BorderImageWidth> {
    let mut vals: Vec<BorderImageWidthComp> = Vec::new();
    loop {
        if vals.len() >= 4 {
            break;
        }
        match p.try_parse(parse_bi_width_comp) {
            Ok(v) => vals.push(v),
            Err(_) => break,
        }
    }
    if vals.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    Ok(BorderImageWidth {
        comps: trbl_expand(&vals),
    })
}

/// border-image-width: the value-level wrapper (the declaration entry).
pub fn parse_border_image_width(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_bi_width_value(p).map(DeclValue::BorderImageWidth)
}

/// A border-image-outset component: `<length [0,∞]>` | `<number [0,∞]>`
/// (negatives rejected; percentages invalid — the spec only allows
/// length|number; calc accepted).
fn parse_bi_outset_comp(p: &mut Parser<'_>) -> ValResult<BorderImageOutsetComp> {
    // <number [0,∞]>
    let num = p.try_parse(|p| -> ValResult<f32> {
        let t = p.next()?.clone();
        match &t {
            Token::Number { value, .. } if *value >= 0.0 => Ok(*value),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if let Ok(v) = num {
        return Ok(BorderImageOutsetComp::Number(v));
    }
    // <length>（% 拒绝；calc 收容）
    let lp = parse_length_percentage(p)?;
    if matches!(lp, LengthPercentage::Percent(_)) {
        return Err(p.new_error_for_next_token());
    }
    lp_reject_negative(p, &lp)?;
    Ok(BorderImageOutsetComp::Length(lp))
}

/// border-image-outset at the value level: 1-4 values TRBL-expanded.
/// pub(crate): reused by the shorthand.
pub(crate) fn parse_bi_outset_value(p: &mut Parser<'_>) -> ValResult<BorderImageOutset> {
    let mut vals: Vec<BorderImageOutsetComp> = Vec::new();
    loop {
        if vals.len() >= 4 {
            break;
        }
        match p.try_parse(parse_bi_outset_comp) {
            Ok(v) => vals.push(v),
            Err(_) => break,
        }
    }
    if vals.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    Ok(BorderImageOutset {
        comps: trbl_expand(&vals),
    })
}

/// border-image-outset: the value-level wrapper (the declaration entry).
pub fn parse_border_image_outset(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_bi_outset_value(p).map(DeclValue::BorderImageOutset)
}

/// A single-axis border-image-repeat keyword.
fn bi_repeat_axis_from(name: &str) -> Option<BorderImageRepeatKind> {
    if name.eq_ignore_ascii_case("stretch") {
        Some(BorderImageRepeatKind::Stretch)
    } else if name.eq_ignore_ascii_case("repeat") {
        Some(BorderImageRepeatKind::Repeat)
    } else if name.eq_ignore_ascii_case("round") {
        Some(BorderImageRepeatKind::Round)
    } else if name.eq_ignore_ascii_case("space") {
        Some(BorderImageRepeatKind::Space)
    } else {
        None
    }
}

/// border-image-repeat at the value level:
/// `<stretch|repeat|round|space>{1,2}` (a single value covers both axes).
/// pub(crate): reused by the shorthand.
pub(crate) fn parse_bi_repeat_value(p: &mut Parser<'_>) -> ValResult<BorderImageRepeatXY> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) => {
            let first = bi_repeat_axis_from(name).ok_or_else(|| p.new_error_for_next_token())?;
            let second = p.try_parse(|p| -> ValResult<BorderImageRepeatKind> {
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(name) => {
                        bi_repeat_axis_from(name).ok_or_else(|| p.new_error_for_next_token())
                    }
                    _ => Err(p.new_error_for_next_token()),
                }
            });
            Ok(match second {
                Ok(y) => BorderImageRepeatXY { x: first, y },
                Err(_) => BorderImageRepeatXY { x: first, y: first },
            })
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// border-image-repeat: the value-level wrapper (the declaration entry).
pub fn parse_border_image_repeat(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_bi_repeat_value(p).map(DeclValue::BorderImageRepeat)
}

/// font-stretch: normal | `<percentage [50,200]>` | nine keywords
/// (css-fonts-4).
pub fn parse_font_stretch(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) => parse_font_stretch_ident(p, name),
        Token::Percentage { unit_value, .. } => {
            // unit_value 是 f32 分数（1.25 = 125%），×100 归一到百分比刻度
            let v = *unit_value * 100.0;
            if !(50.0..=200.0).contains(&v) {
                return Err(p.new_error_for_next_token());
            }
            Ok(DeclValue::FontStretch(v))
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// Keyword-only form (shared with the P9-2 font shorthand: css-fonts-4
/// `<font-width-css3>` has no percentage — the shorthand's width component
/// accepts only the nine keywords).
pub fn parse_font_stretch_kw(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) => parse_font_stretch_ident(p, name),
        _ => Err(p.new_error_for_next_token()),
    }
}

fn parse_font_stretch_ident(p: &mut Parser<'_>, name: &str) -> ValResult<DeclValue> {
    let v = if name.eq_ignore_ascii_case("normal") {
        100.0
    } else if name.eq_ignore_ascii_case("ultra-condensed") {
        50.0
    } else if name.eq_ignore_ascii_case("extra-condensed") {
        62.5
    } else if name.eq_ignore_ascii_case("condensed") {
        75.0
    } else if name.eq_ignore_ascii_case("semi-condensed") {
        87.5
    } else if name.eq_ignore_ascii_case("semi-expanded") {
        112.5
    } else if name.eq_ignore_ascii_case("expanded") {
        125.0
    } else if name.eq_ignore_ascii_case("extra-expanded") {
        150.0
    } else if name.eq_ignore_ascii_case("ultra-expanded") {
        200.0
    } else {
        return Err(p.new_error_for_next_token());
    };
    Ok(DeclValue::FontStretch(v))
}

/// The OpenType feature tag of font-feature-settings /
/// font-variation-settings: must be a `<string>` (css-fonts-4 strict form;
/// Chromium likewise rejects an ident) of exactly 4 characters.
fn parse_feature_tag(p: &mut Parser<'_>) -> ValResult<[u8; 4]> {
    let t = p.next()?.clone();
    match &t {
        Token::QuotedString(s) => {
            let b = s.as_bytes();
            if b.len() != 4 {
                return Err(p.new_error_for_next_token());
            }
            Ok([b[0], b[1], b[2], b[3]])
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// font-feature-settings: `normal | <feature-tag-value>#`, where value =
/// on|off|`<integer [0,65535]>` (default on = 1).
pub fn parse_font_features(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let t = p.next()?.clone();
    if let Token::Ident(name) = &t {
        if name.eq_ignore_ascii_case("normal") {
            return Ok(DeclValue::FontFeatures(Vec::new()));
        }
        return Err(p.new_error_for_next_token());
    }
    let mut list: Vec<([u8; 4], u16)> = Vec::new();
    // 首 tag 已随 t 消费（string）；后续轮次逐个读 tag
    let mut pending_tag: Option<[u8; 4]> = match &t {
        Token::QuotedString(s) => {
            let b = s.as_bytes();
            if b.len() != 4 {
                return Err(p.new_error_for_next_token());
            }
            Some([b[0], b[1], b[2], b[3]])
        }
        _ => None,
    };
    loop {
        let tag = match pending_tag.take() {
            Some(t) => t,
            None => parse_feature_tag(p)?,
        };
        // 可选 value：on|off|<integer>（缺省 on=1；try_parse 失败回滚）
        let value: u16 = p
            .try_parse(|p| -> ValResult<u16> {
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(name) if name.eq_ignore_ascii_case("on") => Ok(1),
                    Token::Ident(name) if name.eq_ignore_ascii_case("off") => Ok(0),
                    Token::Number {
                        value,
                        int_value: Some(_),
                        ..
                    } => {
                        if !(0.0..=65535.0).contains(value) {
                            return Err(p.new_error_for_next_token());
                        }
                        Ok(*value as u16)
                    }
                    _ => Err(p.new_error_for_next_token()),
                }
            })
            .unwrap_or(1);
        list.push((tag, value));
        match p.next() {
            Ok(Token::Comma) => continue,
            _ => break,
        }
    }
    Ok(DeclValue::FontFeatures(list))
}

/// font-variation-settings: `normal | [ <string> <number> ]#` (the value is
/// mandatory and may be negative).
pub fn parse_font_variations(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let t = p.next()?.clone();
    if let Token::Ident(name) = &t {
        if name.eq_ignore_ascii_case("normal") {
            return Ok(DeclValue::FontVariations(Vec::new()));
        }
        return Err(p.new_error_for_next_token());
    }
    let mut list: Vec<([u8; 4], f32)> = Vec::new();
    let mut tag = match &t {
        Token::QuotedString(s) => {
            let b = s.as_bytes();
            if b.len() != 4 {
                return Err(p.new_error_for_next_token());
            }
            Some([b[0], b[1], b[2], b[3]])
        }
        _ => None,
    };
    loop {
        let tag = match tag.take() {
            Some(t) => t,
            None => parse_feature_tag(p)?,
        };
        let t = p.next()?.clone();
        let value = match &t {
            Token::Number { value, .. } => *value,
            _ => return Err(p.new_error_for_next_token()),
        };
        list.push((tag, value));
        match p.next() {
            Ok(Token::Comma) => continue,
            _ => break,
        }
    }
    Ok(DeclValue::FontVariations(list))
}

/// font-variant-caps: the seven-keyword value family.
pub fn parse_font_variant_caps(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "normal" => FontVariantCapsKind::Normal,
            "small-caps" => FontVariantCapsKind::SmallCaps,
            "all-small-caps" => FontVariantCapsKind::AllSmallCaps,
            "petite-caps" => FontVariantCapsKind::PetiteCaps,
            "all-petite-caps" => FontVariantCapsKind::AllPetiteCaps,
            "unicase" => FontVariantCapsKind::Unicase,
            "titling-caps" => FontVariantCapsKind::TitlingCaps,
            _ => return None,
        ))
    })
    .map(DeclValue::FontVariantCaps)
}

/// The F3d generic 1-4-value TRBL expansion (shared by the three
/// border-image longhands; same rules as decl.rs collect_sides, duplicated
/// here as a standalone generic to avoid cross-file borrowing).
fn trbl_expand<T: Clone>(vals: &[T]) -> [T; 4] {
    match vals {
        [a] => [a.clone(), a.clone(), a.clone(), a.clone()],
        [a, b] => [a.clone(), b.clone(), a.clone(), b.clone()],
        [a, b, c] => [a.clone(), b.clone(), c.clone(), b.clone()],
        [a, b, c, d] => [a.clone(), b.clone(), c.clone(), d.clone()],
        _ => unreachable!("调用方保证 1..=4 个值"),
    }
}

// ---------- 属性分派 ----------

/// The per-declaration value parsing entry: PropertyId → value-family
/// parser.
pub fn parse_declaration(id: PropertyId, p: &mut Parser<'_>) -> ValResult<DeclValue> {
    use PropertyId as P;
    match id {
        P::Width
        | P::Height
        | P::MinWidth
        | P::MinHeight
        | P::MaxWidth
        | P::MaxHeight
        | P::FlexBasis
        | P::Top
        | P::Right
        | P::Bottom
        | P::Left
        | P::LetterSpacing
        | P::MarginTop
        | P::MarginRight
        | P::MarginBottom
        | P::MarginLeft
        | P::ColumnWidth => {
            // letter-spacing 的 normal → LenAuto(None)（0 尺寸）
            if matches!(id, P::LetterSpacing) {
                len_auto_with(p, &["auto", "normal"]).map(DeclValue::LenAuto)
            } else {
                parse_len_auto(p)
            }
        }
        // padding 物理四长手 = 单一 <length-percentage> → Len（与简写展开
        // decl.rs "padding" 同族同型；computed 初始值亦 Len）。历史 bug：
        // 曾误路由 parse_corner_radius → Radius 值族，读取方 cs.padding()
        // 的 len() 只认 Len → 全部长手静默归零（简写/calc 直通路径不受
        // 影响，故既有测试未暴露；P1-1 边距重估探针实证）。
        P::PaddingTop | P::PaddingRight | P::PaddingBottom | P::PaddingLeft => parse_len(p),
        P::BorderTopLeftRadius
        | P::BorderTopRightRadius
        | P::BorderBottomRightRadius
        | P::BorderBottomLeftRadius => parse_corner_radius(p),
        // gap 族（gap/row-gap/column-gap）：单一 <length-percentage> → Len；
        // 不经 parse_corner_radius（其 Radius 值族无 gap 读者，会静默归零），
        // `normal` 关键字同样被拒（multicol 语义 normal=1em 由声明缺席表达）。
        P::Gap | P::RowGap | P::ColumnGap => parse_len(p),
        P::Opacity | P::FlexGrow | P::FlexShrink => parse_number_value(p),
        P::ZIndex => parse_z_index(p),
        P::ColumnCount => parse_column_count(p),
        P::BreakInside => parse_break_inside(p),
        P::ColumnSpan => parse_column_span(p),
        P::ColumnRuleWidth => parse_column_rule_width(p),
        P::ColumnRuleStyle => parse_column_rule_style(p),
        P::ContainerType => keyword(p, |s| {
            Some(match_ignore_ascii_case!(s,
                "normal" => ContainerType::Normal,
                "size" => ContainerType::Size,
                "inline-size" => ContainerType::InlineSize,
                _ => return None,
            ))
        })
        .map(DeclValue::ContainerType),
        P::ContainerName => parse_container_name(p),
        // A1 行为提示（宿主可读、零绘制——docs/BEHAVIOR-HINT-PROPS.md）
        P::Cursor => parse_cursor(p),
        P::UserSelect => parse_user_select(p),
        P::PointerEvents => parse_pointer_events(p),
        P::CaretColor => parse_caret_color(p),
        P::AccentColor => parse_accent_color(p),
        // outline（A2）：width 复用 <line-width> 文法（BorderWidth 值族，
        // none→None；槽位区分），color 复用 Color 值族，offset <length> 可负
        P::OutlineWidth => parse_border_width(p),
        P::OutlineStyle => parse_outline_style(p),
        P::OutlineColor => parse_color(p),
        P::OutlineOffset => parse_len(p),
        P::AnimationName => parse_animation_name(p),
        P::AnimationDuration => parse_animation_time(p, false),
        P::AnimationDelay => parse_animation_time(p, true),
        P::AnimationIterationCount => parse_iteration_count(p),
        P::AnimationTimingFunction => parse_timing_fn(p),
        P::AnimationDirection => parse_anim_direction(p),
        P::AnimationFillMode => parse_anim_fill_mode(p),
        // G1（ADR-0032）：transition 五长手
        P::TransitionProperty => parse_transition_property(p),
        P::TransitionDuration => parse_transition_time(p, false),
        P::TransitionDelay => parse_transition_time(p, true),
        P::TransitionTimingFunction => parse_transition_timing(p),
        P::TransitionBehavior => parse_transition_behavior(p),
        P::VerticalAlign => parse_vertical_align(p),
        P::CounterReset => parse_counter_list(p, 0),
        P::CounterIncrement => parse_counter_list(p, 1),
        P::Quotes => parse_quotes(p),
        P::ListStyleType => parse_list_style_type(p),
        P::ListStylePosition => parse_list_style_position(p),
        P::ListStyleImage => parse_list_style_image(p),
        P::Hyphens => parse_hyphens(p),
        P::BackdropFilter => parse_filter_value_list(p),
        P::TextOverflow => parse_text_overflow(p),
        P::WebkitLineClamp => parse_webkit_line_clamp(p),
        P::TextDecorationLine => parse_text_decoration_line(p),
        P::TextDecorationStyle => parse_text_decoration_style(p),
        P::TextDecorationColor => parse_color(p),
        P::TextDecorationThickness => parse_text_decoration_thickness(p),
        P::TextShadow => parse_text_shadow(p),
        P::FontWeight => parse_font_weight(p),
        P::Color
        | P::BackgroundColor
        | P::BorderTopColor
        | P::BorderRightColor
        | P::BorderBottomColor
        | P::BorderLeftColor
        | P::ColumnRuleColor => parse_color(p),
        P::Display => parse_display(p),
        P::Position => parse_position(p),
        P::OverflowX | P::OverflowY => parse_overflow(p),
        P::BoxSizing => parse_box_sizing(p),
        P::Transform => parse_transform(p),
        P::Filter => parse_filter_value_list(p),
        P::JustifyContent | P::AlignItems | P::AlignSelf | P::AlignContent => parse_align(p),
        P::FlexDirection => parse_flex_direction(p),
        P::FlexWrap => parse_flex_wrap(p),
        P::BorderTopStyle | P::BorderRightStyle | P::BorderBottomStyle | P::BorderLeftStyle => {
            parse_border_style(p)
        }
        P::TextAlign => parse_text_align(p),
        P::WhiteSpace => parse_white_space(p),
        P::WillChange => parse_will_change(p),
        P::Isolation => parse_isolation(p),
        P::MixBlendMode => parse_mix_blend_mode(p),
        P::TransformOrigin => parse_transform_origin(p),
        P::FontStyle => parse_font_style(p),
        P::LineHeight => parse_line_height(p),
        P::FontFamily => parse_font_family(p),
        P::FontSize => parse_font_size(p),
        P::GridTemplateColumns | P::GridTemplateRows | P::GridAutoRows | P::GridAutoColumns => {
            parse_grid_tracks(p)
        }
        P::GridTemplateAreas => parse_grid_areas(p),
        P::GridRowStart | P::GridRowEnd | P::GridColumnStart | P::GridColumnEnd => {
            parse_grid_line_spec(p)
        }
        P::GridAutoFlow => keyword(p, |s| {
            Some(match_ignore_ascii_case!(s,
                "row" => GridAutoFlowKind::Row,
                "column" => GridAutoFlowKind::Column,
                _ => return None,
            ))
        })
        .map(DeclValue::GridAutoFlow),
        P::AspectRatio => parse_aspect_ratio(p),
        P::BackgroundImage => parse_background_image(p),
        P::BackgroundRepeat => parse_background_repeat(p),
        P::BackgroundAttachment => parse_background_attachment(p),
        P::BackgroundPosition => parse_background_position(p),
        P::BackgroundSize => parse_background_size(p),
        P::BackgroundOrigin => parse_background_origin(p),
        P::BackgroundClip => parse_background_clip(p),
        P::ClipPath => parse_clip_path(p),
        // F3d（ADR-0026）：border-image 全集五长手
        P::BorderImageSource => parse_background_image_one(p).map(DeclValue::BorderImageSource),
        P::BorderImageSlice => parse_border_image_slice(p),
        P::BorderImageWidth => parse_border_image_width(p),
        P::BorderImageOutset => parse_border_image_outset(p),
        P::BorderImageRepeat => parse_border_image_repeat(p),
        // F3d（ADR-0026）：字体深化五属性
        P::FontStretch => parse_font_stretch(p),
        P::WordSpacing => len_auto_with(p, &["normal"]).map(DeclValue::LenAuto),
        P::FontFeatures => parse_font_features(p),
        P::FontVariations => parse_font_variations(p),
        P::FontVariantCaps => parse_font_variant_caps(p),
        P::BoxShadow => parse_box_shadow(p),
        P::BorderTopWidth | P::BorderRightWidth | P::BorderBottomWidth | P::BorderLeftWidth => {
            parse_border_width(p)
        }
        // A8 逻辑属性（css-logical-1）：值族与物理同族完全一致（槽位区分
        // + computed 期按 direction 定夺映射）；margin/inset 复用 LenAuto
        //（auto 语义），padding 复用物理 padding 解析器，border 逻辑三族
        // 复用 width/style/color 解析器，radius 逻辑四角复用角解析器。
        P::Direction => keyword(p, |s| {
            Some(match_ignore_ascii_case!(s,
                "ltr" => DirectionKind::Ltr,
                "rtl" => DirectionKind::Rtl,
                _ => return None,
            ))
        })
        .map(DeclValue::Direction),
        P::UnicodeBidi => keyword(p, |s| {
            Some(match_ignore_ascii_case!(s,
                "normal" => UnicodeBidiKind::Normal,
                "embed" => UnicodeBidiKind::Embed,
                "isolate" => UnicodeBidiKind::Isolate,
                "bidi-override" => UnicodeBidiKind::BidiOverride,
                "isolate-override" => UnicodeBidiKind::IsolateOverride,
                "plaintext" => UnicodeBidiKind::Plaintext,
                _ => return None,
            ))
        })
        .map(DeclValue::UnicodeBidi),
        P::MarginInlineStart
        | P::MarginInlineEnd
        | P::MarginBlockStart
        | P::MarginBlockEnd
        | P::InsetInlineStart
        | P::InsetInlineEnd
        | P::InsetBlockStart
        | P::InsetBlockEnd => parse_len_auto(p),
        // 逻辑 padding 同修：单一 <length-percentage> → Len（computed 期
        // direction 定夺映射物理槽位，落点仍是 Len 族读取）。
        P::PaddingInlineStart | P::PaddingInlineEnd | P::PaddingBlockStart | P::PaddingBlockEnd => {
            parse_len(p)
        }
        P::BorderStartStartRadius
        | P::BorderStartEndRadius
        | P::BorderEndStartRadius
        | P::BorderEndEndRadius => parse_corner_radius(p),
        P::Content => parse_content(p),
        P::TextTransform => parse_text_transform(p),
        P::OverflowWrap => parse_overflow_wrap(p),
        P::WordBreak => parse_word_break(p),
        P::ObjectFit => parse_object_fit(p),
        P::ObjectPosition => parse_object_position(p),
        P::Float => parse_float(p),
        P::Clear => parse_clear(p),
        P::BorderInlineStartWidth
        | P::BorderInlineEndWidth
        | P::BorderBlockStartWidth
        | P::BorderBlockEndWidth => parse_border_width(p),
        P::BorderInlineStartStyle
        | P::BorderInlineEndStyle
        | P::BorderBlockStartStyle
        | P::BorderBlockEndStyle => parse_border_style(p),
        P::BorderInlineStartColor
        | P::BorderInlineEndColor
        | P::BorderBlockStartColor
        | P::BorderBlockEndColor => parse_color(p),
    }
}

// ---------- 动画（第五批⑰）：缓动/方向/fill 与关键帧采样插值 ----------

/// Timing function (batch 5 ⑰): linear/ease cubic béziers + steps().
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum TimingFn {
    /// linear — constant speed.
    Linear,
    /// ease — cubic-bezier(0.25, 0.1, 0.25, 1).
    Ease,
    /// ease-in — cubic-bezier(0.42, 0, 1, 1).
    EaseIn,
    /// ease-out — cubic-bezier(0, 0, 0.58, 1).
    EaseOut,
    /// ease-in-out — cubic-bezier(0.42, 0, 0.58, 1).
    EaseInOut,
    /// (n, jump_end): jump_end=true → the step occurs at the end of each
    /// interval (CSS steps defaults to end).
    Steps(u32, bool),
}

impl TimingFn {
    /// Easing evaluation: t ∈ \[0,1\] → progress ∈ \[0,1\].
    pub fn sample(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::Steps(n, jump_end) => {
                let n = n.max(1) as f32;
                // css-easing-1：steps(n, end)（默认）= 阶跃在段尾 = ⌊p·n⌋/n
                //（p=1 端点 floor(n·1)/n=1 恰为终值）；steps(n, start) =
                // 段首跳变 = ⌈p·n⌉/n（p=0 端点 ceil(0)=0 恰为初值）。
                if jump_end {
                    (t * n).floor() / n
                } else {
                    (t * n).ceil() / n
                }
            }
            Self::Ease => Self::bezier(0.25, 0.1, 0.25, 1.0, t),
            Self::EaseIn => Self::bezier(0.42, 0.0, 1.0, 1.0, t),
            Self::EaseOut => Self::bezier(0.0, 0.0, 0.58, 1.0, t),
            Self::EaseInOut => Self::bezier(0.42, 0.0, 0.58, 1.0, t),
        }
    }

    /// Cubic bézier (x1,y1,x2,y2) solving: bisect x → parameter over 24
    /// rounds (a CSS timing function's x is strictly monotonic on [0,1], so
    /// bisection is stable), then read y.
    fn bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
        fn at(p1: f32, p2: f32, t: f32) -> f32 {
            // B(t) = 3(1-t)²t·p1 + 3(1-t)t²·p2 + t³（端点 0/1）
            3.0 * (1.0 - t) * (1.0 - t) * t * p1 + 3.0 * (1.0 - t) * t * t * p2 + t * t * t
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..24 {
            let mid = (lo + hi) * 0.5;
            if at(x1, x2, mid) < x {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        at(y1, y2, (lo + hi) * 0.5)
    }
}

/// animation-direction (batch 5 ⑰).
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum AnimDirection {
    /// normal — plays forward every cycle.
    Normal,
    /// reverse — plays in reverse every cycle.
    Reverse,
    /// alternate — alternates forward/reverse per cycle.
    Alternate,
    /// alternate-reverse — alternates reverse/forward per cycle.
    AlternateReverse,
}

/// animation-fill-mode (batch 5 ⑰).
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum AnimFillMode {
    /// none — no keyframe styles applied outside the animation interval
    /// (default).
    None,
    /// forwards — retains the last frame after the animation ends.
    Forwards,
    /// backwards — applies the first frame during the delay.
    Backwards,
    /// both — fills on both sides (backwards + forwards).
    Both,
}

/// transition-property target (G1, ADR-0032): none | all | custom-ident.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TransitionTarget {
    /// none — no property is transitioned.
    None,
    /// all — every transitionable property.
    All,
    /// A custom property name (including names unknown to the engine —
    /// parsing does not validate knownness; the engine matches by property
    /// name; a failed match means that name produces no transition).
    Ident(String),
}

/// transition-property comma list (G1). A smallvec keeps 2 entries inline.
#[derive(Debug, Clone, PartialEq)]
pub struct TransitionPropertyList(pub SmallVec<[TransitionTarget; 2]>);

/// transition-duration / transition-delay comma list, in seconds (G1).
#[derive(Debug, Clone, PartialEq)]
pub struct TransitionTimeList(pub SmallVec<[f32; 2]>);

/// transition-timing-function comma list (G1). Reuses the animation
/// TimingFn grammar and type (ADR-0032 D1: no separate easing type).
#[derive(Debug, Clone, PartialEq)]
pub struct TransitionTimingList(pub SmallVec<[TimingFn; 2]>);

/// transition-behavior (G1): the transition policy for discrete properties
/// (single value, not a list — css-transitions-2 §2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransitionBehavior {
    /// normal — discrete properties do not transition (an immediate jump;
    /// the default).
    Normal,
    /// allow-discrete — discrete properties also transition; sampling flips
    /// at 50% per the discrete rule.
    AllowDiscrete,
}

/// vertical-align (P3, ADR-0034 D3, css2 §10.8.1): the value family for the
/// vertical alignment of inline participants relative to the line baseline.
/// Not inherited; the initial value is baseline. The percentage base = line
/// height (the settle_lines boxed value); the sub/super offset constant is
/// 0.34em (Chromium-magnitude calibration; the spec does not fix a UA value
/// — documented in FEATURES.md as Tier B).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum VerticalAlignKind {
    /// baseline (initial) — the participant's baseline aligns with the line
    /// baseline (v1 TOP boxing has exactly this semantics; no extra offset
    /// is produced).
    Baseline,
    /// sub — sunk 0.34em below the baseline.
    Sub,
    /// super — raised 0.34em above the baseline.
    Super,
    /// text-top — the participant font's ascent top aligns with the strut
    /// (container font) ascent top.
    TextTop,
    /// text-bottom — the participant font's descent bottom aligns with the
    /// strut descent bottom.
    TextBottom,
    /// middle — the participant box midpoint aligns with baseline −
    /// x-height/2 (x-height from the fontprobe ex metric; an unregistered
    /// font falls back to 0.5em).
    Middle,
    /// top — the participant line box top aligns with the line top (native
    /// TOP boxing; offset 0).
    Top,
    /// bottom — the participant line box bottom aligns with the line bottom
    /// (after line-box expansion).
    Bottom,
    /// `<length-percentage>` — an explicit offset; positive = raised; the
    /// percentage base is line height.
    Length(LengthPercentage),
}

/// vertical-align (P3, ADR-0034 D3): the full keyword family |
/// `<length-percentage>`.
pub fn parse_vertical_align(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    // 关键字先行
    if let Ok(v) = p.try_parse(|p| {
        keyword(p, |k| match k.to_ascii_lowercase().as_str() {
            "baseline" => Some(DeclValue::VerticalAlign(VerticalAlignKind::Baseline)),
            "sub" => Some(DeclValue::VerticalAlign(VerticalAlignKind::Sub)),
            "super" => Some(DeclValue::VerticalAlign(VerticalAlignKind::Super)),
            "text-top" => Some(DeclValue::VerticalAlign(VerticalAlignKind::TextTop)),
            "text-bottom" => Some(DeclValue::VerticalAlign(VerticalAlignKind::TextBottom)),
            "middle" => Some(DeclValue::VerticalAlign(VerticalAlignKind::Middle)),
            "top" => Some(DeclValue::VerticalAlign(VerticalAlignKind::Top)),
            "bottom" => Some(DeclValue::VerticalAlign(VerticalAlignKind::Bottom)),
            _ => None,
        })
    }) {
        return Ok(v);
    }
    // <length-percentage>（% 基准=行高，settle_lines 结算期换算）
    let lp = crate::css::value::parse_length_percentage(p)?;
    Ok(DeclValue::VerticalAlign(VerticalAlignKind::Length(lp)))
}

/// quotes value family (P5, ADR-0036 D3): auto | none | [`<string>`
/// `<string>`]#.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum QuotesValue {
    /// auto — the UA default quote table ("" ''; materialized into Pairs at
    /// computed time; see ComputedStyle::quotes_pairs).
    Auto,
    /// none — open-quote/no-close-quote emit nothing.
    None,
    /// An explicit pair table (pairs selected by nesting depth; past the
    /// end, the last pair is reused).
    Pairs(Vec<(String, String)>),
}

/// counter-reset / counter-increment parsing (P5, ADR-0036 D2):
/// `none | [ <custom-ident> <integer>? ]#`. The integer is materialized
/// when omitted (reset = 0 / increment = 1); for a repeated ident the
/// latter wins (evaluation-time semantics; parsing preserves order).
pub fn parse_counter_list(p: &mut Parser<'_>, default_step: i64) -> ValResult<DeclValue> {
    p.skip_whitespace();
    let t = p.next()?.clone();
    if let Token::Ident(ref id) = t
        && id.eq_ignore_ascii_case("none")
    {
        return Ok(DeclValue::CounterList(Vec::new()));
    }
    // 首项 ident（回退重放：try_parse 借用内的 next 已消费——直接用 t）。
    let mut items: Vec<(String, i64)> = Vec::new();
    match t {
        Token::Ident(ref id)
            if !id.starts_with("--")
                && !matches!(
                    id.to_ascii_lowercase().as_str(),
                    "initial" | "inherit" | "unset" | "revert" | "none"
                ) =>
        {
            items.push((id.to_string(), default_step));
        }
        _ => return Err(p.new_error_for_next_token()),
    }
    // css-lists-3 语法：[ <counter-name> <integer>? ]+ ——空格分隔，无逗号。
    loop {
        p.skip_whitespace();
        if p.is_exhausted() {
            break;
        }
        // 可选 <integer>（整数；小数拒绝）——绑定最后一个 ident 的步长。
        if let Ok(step) = p.try_parse(|p| -> ValResult<i64> {
            let t = p.next()?.clone();
            match t {
                Token::Number {
                    int_value: Some(v), ..
                } => Ok(v as i64),
                _ => Err(p.new_error_for_next_token()),
            }
        }) {
            if let Some(last) = items.last_mut() {
                last.1 = step;
            }
            continue;
        }
        // 下一项 ident。
        let t = p.next()?.clone();
        match t {
            Token::Ident(ref id)
                if !id.starts_with("--")
                    && !matches!(
                        id.to_ascii_lowercase().as_str(),
                        "initial" | "inherit" | "unset" | "revert" | "none"
                    ) =>
            {
                items.push((id.to_string(), default_step));
            }
            _ => return Err(p.new_error_for_next_token()),
        }
    }
    Ok(DeclValue::CounterList(items))
}

/// quotes parsing (P5, ADR-0036 D3).
pub fn parse_quotes(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    p.skip_whitespace();
    let t = p.next()?.clone();
    match t {
        Token::Ident(ref id) if id.eq_ignore_ascii_case("auto") => {
            Ok(DeclValue::Quotes(QuotesValue::Auto))
        }
        Token::Ident(ref id) if id.eq_ignore_ascii_case("none") => {
            Ok(DeclValue::Quotes(QuotesValue::None))
        }
        Token::QuotedString(ref s) => {
            // css-content-3：[ <string> <string> ]+ ——空格分隔对，无逗号。
            let mut pairs: Vec<(String, String)> = Vec::new();
            let mut open: Option<String> = Some(s.to_string());
            loop {
                p.skip_whitespace();
                // 对内第二串（必需）。
                let close = match p.try_parse(parse_content_string) {
                    Ok(c) => c,
                    Err(_) => return Err(p.new_error_for_next_token()),
                };
                if let Some(o) = open.take() {
                    pairs.push((o, close));
                }
                p.skip_whitespace();
                if p.is_exhausted() {
                    break;
                }
                // 下一对首串（必需）。
                let t = p.next()?.clone();
                open = match t {
                    Token::QuotedString(ref s) => Some(s.to_string()),
                    _ => return Err(p.new_error_for_next_token()),
                };
            }
            Ok(DeclValue::Quotes(QuotesValue::Pairs(pairs)))
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// list-style-type parsing (P9-3, css-lists-3 §3.4):
/// `<counter-style> | <string> | none` — the `<counter-style>` form accepts
/// only `<counter-style-name>` (an ident; the function forms symbols() and
/// counter() are outside the list marker grammar); unknown names are not
/// validated at parse time (@counter-style registers later) and fall back
/// to decimal at use time (css-counter-styles-3 §2). CSS-wide keywords are
/// intercepted upstream.
pub fn parse_list_style_type(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    p.skip_whitespace();
    let t = p.next()?.clone();
    match &t {
        Token::Ident(id) if id.eq_ignore_ascii_case("none") => Ok(DeclValue::ListStyleType(None)),
        Token::Ident(id) => Ok(DeclValue::ListStyleType(Some(ListStyleTypeValue::Name(
            id.to_string(),
        )))),
        Token::QuotedString(s) => Ok(DeclValue::ListStyleType(Some(ListStyleTypeValue::Str(
            s.to_string(),
        )))),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// list-style-position parsing (P9-3, css-lists-3 §3.5): inside | outside.
pub fn parse_list_style_position(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "inside" => ListStylePosition::Inside,
            "outside" => ListStylePosition::Outside,
            _ => return None,
        ))
    })
    .map(DeclValue::ListStylePosition)
}

/// list-style-image parsing (P9-3, css-lists-3 §3.3): `<image> | none`
/// (single value; reuses the background single-layer image parsing of
/// none|url()|the gradient family).
pub fn parse_list_style_image(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    p.skip_whitespace();
    let img = parse_background_image_one(p)?;
    match img {
        BackgroundImage::None => Ok(DeclValue::ListStyleImage(None)),
        other => Ok(DeclValue::ListStyleImage(Some(other))),
    }
}

/// animation-name (P7-② listified): `[ none | <custom-ident> ]#`.
/// Custom identifiers starting with -- and CSS-wide keywords are rejected
/// as animation names; always produces the list variant.
fn parse_animation_name(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut list = SmallVec::new();
    loop {
        p.skip_whitespace();
        let t = p.next()?.clone();
        match &t {
            Token::Ident(id) if id.eq_ignore_ascii_case("none") => list.push(None),
            Token::Ident(id)
                if !id.starts_with("--")
                    && !matches!(
                        id.to_ascii_lowercase().as_str(),
                        "initial" | "inherit" | "unset" | "revert"
                    ) =>
            {
                list.push(Some(id.to_string()))
            }
            _ => return Err(p.new_error_for_next_token()),
        }
        p.skip_whitespace();
        if p.is_exhausted() {
            break;
        }
        match p.next()? {
            Token::Comma => continue,
            _ => return Err(p.new_error_for_next_token()),
        }
    }
    Ok(DeclValue::AnimationNameList(list))
}

/// animation-duration / animation-delay share `<time>#` (P7-② listified):
/// s/ms → seconds. duration rejects negatives (t∈[0,∞)); delay allows them
/// (fast-forward semantics).
fn parse_animation_time(p: &mut Parser<'_>, allow_negative: bool) -> ValResult<DeclValue> {
    let mut list = SmallVec::new();
    loop {
        p.skip_whitespace();
        let t = p.next()?.clone();
        let Token::Dimension { value, unit, .. } = &t else {
            return Err(p.new_error_for_next_token());
        };
        let secs = if unit.eq_ignore_ascii_case("s") {
            *value
        } else if unit.eq_ignore_ascii_case("ms") {
            value / 1000.0
        } else {
            return Err(p.new_error_for_next_token());
        };
        if secs < 0.0 && !allow_negative {
            return Err(p.new_error_for_next_token());
        }
        list.push(secs);
        p.skip_whitespace();
        if p.is_exhausted() {
            break;
        }
        match p.next()? {
            Token::Comma => continue,
            _ => return Err(p.new_error_for_next_token()),
        }
    }
    Ok(DeclValue::AnimationTimeList(list))
}

/// animation-iteration-count (P7-② listified): `[ <number> | infinite ]#`.
fn parse_iteration_count(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut list = SmallVec::new();
    loop {
        p.skip_whitespace();
        let t = p.next()?.clone();
        match &t {
            Token::Ident(id) if id.eq_ignore_ascii_case("infinite") => list.push(f32::INFINITY),
            Token::Number { value, .. } if *value >= 0.0 => list.push(*value),
            _ => return Err(p.new_error_for_next_token()),
        }
        p.skip_whitespace();
        if p.is_exhausted() {
            break;
        }
        match p.next()? {
            Token::Comma => continue,
            _ => return Err(p.new_error_for_next_token()),
        }
    }
    Ok(DeclValue::AnimationIterationList(list))
}

/// animation-timing-function (P7-② listified): `<easing-function>#`.
fn parse_timing_fn(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut list = SmallVec::new();
    loop {
        p.skip_whitespace();
        list.push(parse_timing_fn_one(p)?);
        p.skip_whitespace();
        if p.is_exhausted() {
            break;
        }
        match p.next()? {
            Token::Comma => continue,
            _ => return Err(p.new_error_for_next_token()),
        }
    }
    Ok(DeclValue::AnimationTimingList(list))
}

/// `<easing-function>` (the batch 5 ⑰ grammar; reused by the G1
/// transition-timing-function): the steps(n[, start|end]) function form
/// first, otherwise a linear/ease-family keyword. Returns a bare TimingFn
/// (the shorthand and list parsing reuse this same entry).
pub(crate) fn parse_timing_fn_one(p: &mut Parser<'_>) -> ValResult<TimingFn> {
    // steps(n[, start|end]) 函数形优先（缺省第二参=end；try_parse 失败
    // 自动回滚落回关键字形）
    let is_steps = p.try_parse(|p| -> ValResult<()> {
        let t = p.next()?.clone();
        match &t {
            Token::Function(f) if f.eq_ignore_ascii_case("steps") => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if is_steps.is_ok() {
        return match timing_fn_steps_body(p)? {
            DeclValue::AnimationTiming(f) => Ok(f),
            _ => Err(p.new_error_for_next_token()),
        };
    }
    keyword(p, |k| match k.to_ascii_lowercase().as_str() {
        "linear" => Some(TimingFn::Linear),
        "ease" => Some(TimingFn::Ease),
        "ease-in" => Some(TimingFn::EaseIn),
        "ease-out" => Some(TimingFn::EaseOut),
        "ease-in-out" => Some(TimingFn::EaseInOut),
        _ => None,
    })
}

/// The steps() block body (the Function token is already consumed — the
/// animation shorthand reuses this same entry).
pub(crate) fn timing_fn_steps_body(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    p.parse_nested_block(|p| {
        p.skip_whitespace();
        let t = p.next()?.clone();
        let Token::Number { value, .. } = &t else {
            return Err(p.new_error_for_next_token());
        };
        let n = (*value as u32).max(1);
        p.skip_whitespace();
        let jump_end = p
            .try_parse(|p| -> ValResult<bool> {
                let c = p.next()?.clone();
                match c {
                    Token::Comma => {}
                    _ => return Err(p.new_error_for_next_token()),
                }
                p.skip_whitespace();
                let t = p.next()?.clone();
                let Token::Ident(id) = &t else {
                    return Err(p.new_error_for_next_token());
                };
                if id.eq_ignore_ascii_case("end") {
                    Ok(true)
                } else if id.eq_ignore_ascii_case("start") {
                    Ok(false)
                } else {
                    Err(p.new_error_for_next_token())
                }
            })
            .unwrap_or(true);
        Ok(DeclValue::AnimationTiming(TimingFn::Steps(n, jump_end)))
    })
}

/// animation-direction (P7-② listified): `<single-animation-direction>#`.
fn parse_anim_direction(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut list = SmallVec::new();
    loop {
        p.skip_whitespace();
        list.push(keyword(p, |k| match k.to_ascii_lowercase().as_str() {
            "normal" => Some(AnimDirection::Normal),
            "reverse" => Some(AnimDirection::Reverse),
            "alternate" => Some(AnimDirection::Alternate),
            "alternate-reverse" => Some(AnimDirection::AlternateReverse),
            _ => None,
        })?);
        p.skip_whitespace();
        if p.is_exhausted() {
            break;
        }
        match p.next()? {
            Token::Comma => continue,
            _ => return Err(p.new_error_for_next_token()),
        }
    }
    Ok(DeclValue::AnimationDirectionList(list))
}

/// animation-fill-mode (P7-② listified): `<single-animation-fill-mode>#`.
fn parse_anim_fill_mode(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut list = SmallVec::new();
    loop {
        p.skip_whitespace();
        list.push(keyword(p, |k| match k.to_ascii_lowercase().as_str() {
            "none" => Some(AnimFillMode::None),
            "forwards" => Some(AnimFillMode::Forwards),
            "backwards" => Some(AnimFillMode::Backwards),
            "both" => Some(AnimFillMode::Both),
            _ => None,
        })?);
        p.skip_whitespace();
        if p.is_exhausted() {
            break;
        }
        match p.next()? {
            Token::Comma => continue,
            _ => return Err(p.new_error_for_next_token()),
        }
    }
    Ok(DeclValue::AnimationFillModeList(list))
}

// ---------- transition（G1，ADR-0032）：五长手解析 ----------

/// transition-property: none | all | `<custom-ident>` (multiple comma
/// groups).
fn parse_transition_property(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut list = SmallVec::new();
    loop {
        p.skip_whitespace();
        let t = p.next()?.clone();
        let target = match &t {
            Token::Ident(id) if id.eq_ignore_ascii_case("none") => TransitionTarget::None,
            Token::Ident(id) if id.eq_ignore_ascii_case("all") => TransitionTarget::All,
            // custom-ident：--* 与 CSS 宽关键字非法（同 animation-name 文法）
            Token::Ident(id)
                if !id.starts_with("--")
                    && !matches!(
                        id.to_ascii_lowercase().as_str(),
                        "initial" | "inherit" | "unset" | "revert" | "revert-layer"
                    ) =>
            {
                TransitionTarget::Ident(id.to_string())
            }
            _ => return Err(p.new_error_for_next_token()),
        };
        list.push(target);
        p.skip_whitespace();
        if p.is_exhausted() {
            break;
        }
        match p.next()? {
            Token::Comma => continue,
            _ => return Err(p.new_error_for_next_token()),
        }
    }
    Ok(DeclValue::TransitionProperty(TransitionPropertyList(list)))
}

/// transition-duration / transition-delay share `<time>`#: s/ms → seconds.
/// duration rejects negatives (t∈[0,∞)); delay allows them (fast-forward
/// semantics).
fn parse_transition_time(p: &mut Parser<'_>, allow_negative: bool) -> ValResult<DeclValue> {
    let mut list = SmallVec::new();
    loop {
        p.skip_whitespace();
        let t = p.next()?.clone();
        let Token::Dimension { value, unit, .. } = &t else {
            return Err(p.new_error_for_next_token());
        };
        let secs = if unit.eq_ignore_ascii_case("s") {
            *value
        } else if unit.eq_ignore_ascii_case("ms") {
            value / 1000.0
        } else {
            return Err(p.new_error_for_next_token());
        };
        if secs < 0.0 && !allow_negative {
            return Err(p.new_error_for_next_token());
        }
        list.push(secs);
        p.skip_whitespace();
        if p.is_exhausted() {
            break;
        }
        match p.next()? {
            Token::Comma => continue,
            _ => return Err(p.new_error_for_next_token()),
        }
    }
    Ok(DeclValue::TransitionTime(TransitionTimeList(list)))
}

/// transition-timing-function: `<easing-function>`# (reuses the animation
/// grammar).
fn parse_transition_timing(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut list = SmallVec::new();
    loop {
        p.skip_whitespace();
        list.push(parse_timing_fn_one(p)?);
        p.skip_whitespace();
        if p.is_exhausted() {
            break;
        }
        match p.next()? {
            Token::Comma => continue,
            _ => return Err(p.new_error_for_next_token()),
        }
    }
    Ok(DeclValue::TransitionTiming(TransitionTimingList(list)))
}

/// transition-behavior: normal | allow-discrete (single value).
fn parse_transition_behavior(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |k| match k.to_ascii_lowercase().as_str() {
        "normal" => Some(DeclValue::TransitionBehavior(TransitionBehavior::Normal)),
        "allow-discrete" => Some(DeclValue::TransitionBehavior(
            TransitionBehavior::AllowDiscrete,
        )),
        _ => None,
    })
}

/// Keyframe interpolation (batch 5 ⑰): an interpolatable pair → the
/// intermediate value; a non-interpolatable pair → None (the sampling side
/// takes the first-frame value of the segment per the CSS discrete rule).
/// Colors are resolved to sRGB via pick_scheme and blended directly
/// (including alpha); lengths interpolate linearly within the same variant
/// (discrete across units); transforms with the same function name
/// interpolate per-parameter (different function-sequence lengths are
/// discrete).
pub fn lerp_decl(a: &DeclValue, b: &DeclValue, t: f32, dark: bool) -> Option<DeclValue> {
    use DeclValue as D;
    let color = |x: &ColorValue, y: &ColorValue| -> Option<ColorValue> {
        use peniko::color::AlphaColor;
        // 先终解（light-dark → 单色），仅 Absolute 对可插值
        let (ColorValue::Absolute(c0), ColorValue::Absolute(c1)) =
            (x.pick_scheme(dark), y.pick_scheme(dark))
        else {
            return None;
        };
        let mix: [f32; 4] =
            std::array::from_fn(|i| c0.components[i] + (c1.components[i] - c0.components[i]) * t);
        Some(ColorValue::Absolute(AlphaColor::new(mix)))
    };
    let transforms = |x: &[TransformFn], y: &[TransformFn]| -> Option<Vec<TransformFn>> {
        if x.len() != y.len() {
            return None;
        }
        x.iter()
            .zip(y.iter())
            .map(|(a, b)| match (a, b) {
                (TransformFn::Translate(x, y), TransformFn::Translate(u, v)) => {
                    let ix = lerp_lenp(x, u, t)?;
                    let iy = lerp_lenp(y, v, t)?;
                    Some(TransformFn::Translate(ix, iy))
                }
                (TransformFn::Rotate(x), TransformFn::Rotate(u)) => {
                    Some(TransformFn::Rotate(x + (u - x) * t))
                }
                (TransformFn::Scale(x, y), TransformFn::Scale(u, v)) => {
                    Some(TransformFn::Scale(x + (u - x) * t, y + (v - y) * t))
                }
                (TransformFn::Matrix(a, b, c, d, e, f), TransformFn::Matrix(u, v, w, z, s, q)) => {
                    Some(TransformFn::Matrix(
                        a + (u - a) * t,
                        b + (v - b) * t,
                        c + (w - c) * t,
                        d + (z - d) * t,
                        e + (s - e) * t,
                        f + (q - f) * t,
                    ))
                }
                _ => None,
            })
            .collect()
    };
    Some(match (a, b) {
        (D::Number(x), D::Number(y)) => D::Number(x + (y - x) * t),
        (D::Color(x), D::Color(y)) => D::Color(color(x, y)?),
        (D::Len(x), D::Len(y)) => D::Len(lerp_lenp(x, y, t)?),
        (D::LenAuto(Some(x)), D::LenAuto(Some(y))) => D::LenAuto(Some(lerp_lenp(x, y, t)?)),
        (D::Radius(x1, y1), D::Radius(x2, y2)) => {
            D::Radius(lerp_lenp(x1, x2, t)?, lerp_lenp(y1, y2, t)?)
        }
        (D::TransformOrigin(x1, y1), D::TransformOrigin(x2, y2)) => {
            D::TransformOrigin(lerp_lenp(x1, x2, t)?, lerp_lenp(y1, y2, t)?)
        }
        (D::Transform(x), D::Transform(y)) => D::Transform(transforms(x, y)?),
        _ => return None,
    })
}

fn lerp_lenp(x: &LengthPercentage, y: &LengthPercentage, t: f32) -> Option<LengthPercentage> {
    Some(match (x, y) {
        (LengthPercentage::Px(u), LengthPercentage::Px(v)) => LengthPercentage::Px(u + (v - u) * t),
        (LengthPercentage::Em(u), LengthPercentage::Em(v)) => LengthPercentage::Em(u + (v - u) * t),
        (LengthPercentage::Rem(u), LengthPercentage::Rem(v)) => {
            LengthPercentage::Rem(u + (v - u) * t)
        }
        (LengthPercentage::Percent(u), LengthPercentage::Percent(v)) => {
            LengthPercentage::Percent(u + (v - u) * t)
        }
        (LengthPercentage::Vw(u), LengthPercentage::Vw(v)) => LengthPercentage::Vw(u + (v - u) * t),
        (LengthPercentage::Vh(u), LengthPercentage::Vh(v)) => LengthPercentage::Vh(u + (v - u) * t),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Consistency lock between the slot table and ALL: the leading section
    /// must match ALL order bit for bit, and the 7 animation descriptors
    /// land after ALL; SLOT_COUNT must cover every variant.
    /// When a new PropertyId variant is added, this test failing = a
    /// reminder to sync slot() and SLOT_COUNT.
    #[test]
    fn slot_alignment() {
        for (i, pid) in PropertyId::ALL.iter().enumerate() {
            assert_eq!(pid.slot(), i, "{} 槽位与 ALL 顺序不一致", pid.css_name());
        }
        let anim = [
            PropertyId::AnimationName,
            PropertyId::AnimationDuration,
            PropertyId::AnimationDelay,
            PropertyId::AnimationIterationCount,
            PropertyId::AnimationTimingFunction,
            PropertyId::AnimationDirection,
            PropertyId::AnimationFillMode,
        ];
        for (k, pid) in anim.iter().enumerate() {
            assert_eq!(
                pid.slot(),
                PropertyId::ALL.len() + k,
                "{} 动画描述符槽位漂移",
                pid.css_name()
            );
        }
        assert_eq!(PropertyId::ALL.len() + anim.len(), PropertyId::SLOT_COUNT);
    }

    /// from_css_name sorted-index lock: the full ALL set round-trips
    /// (css_name → id → css_name is an identity), animation-descriptor
    /// longhands are routable, the word-wrap alias resolves, matching is
    /// case-insensitive, and shorthand names do not route (None).
    #[test]
    fn from_css_name_index_roundtrip() {
        for pid in PropertyId::ALL {
            assert_eq!(
                PropertyId::from_css_name(pid.css_name()),
                Some(*pid),
                "{} 往返失败",
                pid.css_name()
            );
        }
        // 动画描述符长手（不在 ALL）仍可路由。
        assert_eq!(
            PropertyId::from_css_name("animation-iteration-count"),
            Some(PropertyId::AnimationIterationCount)
        );
        // legacy 别名。
        assert_eq!(
            PropertyId::from_css_name("word-wrap"),
            Some(PropertyId::OverflowWrap)
        );
        // 大小写不敏感。
        assert_eq!(
            PropertyId::from_css_name("FONT-WEIGHT"),
            Some(PropertyId::FontWeight)
        );
        // 简写名不路由（由简写展开器处理）。
        for shorthand in ["margin", "padding", "border", "background", "font", "flex"] {
            assert_eq!(
                PropertyId::from_css_name(shorthand),
                None,
                "{shorthand} 不应经长手路由"
            );
        }
        // 未知名。
        assert_eq!(PropertyId::from_css_name("not-a-real-property"), None);
    }
}
