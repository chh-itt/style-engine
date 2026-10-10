//! Computed styles: cascade winners → var() substitution →
//! inheritance/initial values (the L1 semantic endpoint).
//!
//! Pipeline (ADR-0003/ADR-0004):
//! 1. Custom properties: inherited values as the base, winning raw values
//!    override, var() chains resolved on demand (cycle detection; failure =
//!    guaranteed-invalid, the name is absent);
//! 2. Winning declarations: `Parsed` taken directly; `Var` is substituted
//!    then re-parsed, with failure falling to IACVT (inherited properties
//!    take the parent value, otherwise the initial value);
//! 3. Full-set materialization: the 64 properties are filled in one by one
//!    (parent inheritance or initial value) for T3/T4 to read directly;
//! 4. Font-size resolution: em is relative to the parent font size,
//!    materialized as absolute px.

use crate::cascade::{ContainerCtx, cascade_declarations};
use crate::css::decl::{DeclSource, token_buf_to_string};
use crate::css::property::{
    Align, AnimDirection, AnimFillMode, BackgroundImage, ContainerType, CursorKind, DeclValue,
    Display, FamilyName, FlexDirection, FlexWrap, FontFamilyList, FontStyle, GridAutoFlowKind,
    GridTemplate, LineHeight, OutlineStyle, Overflow, PointerEventsKind, Position, PropertyId,
    TextAlign, TimingFn, TransitionBehavior, TransitionPropertyList, TransitionTarget,
    TransitionTimeList, TransitionTimingList, UserSelectKind, WhiteSpace,
};
use crate::css::stylesheet::{MediaEnv, Stylesheet};
use crate::css::value::{ColorValue, LengthPercentage, ResolveCtx};
use crate::tree::{NodeId, StyleTree};
use peniko::color::AlphaColor;
use smallvec::{SmallVec, smallvec};
use std::collections::BTreeMap;

/// The computed style of a single node (full-set materialization).
#[derive(Debug, Clone, PartialEq)]
#[must_use = "dropping the computed style makes this cascade resolution meaningless"]
pub struct ComputedStyle {
    /// Full-set slot storage: index = [`PropertyId::slot()`] (0..183),
    /// None = not materialized. O(1) indexed slot writes replace BTreeMap's
    /// log n walk + node allocation — full-set materialization was ~94
    /// inserts per node and used to dominate restyle cost (phase-5
    /// attribution, PERFORMANCE.md).
    values: Vec<Option<DeclValue>>,
    /// Resolved custom properties (final value text).
    custom: BTreeMap<String, String>,
    /// Font-relative unit metrics (A9: ch/ex/ic bases, per em; rewritten by
    /// the engine during restyle from font-family's first family, with
    /// default approximations — unregistered families get ch/ex = 0.5em and
    /// ic = 1em).
    font_metrics: crate::css::value::FontMetrics,
    /// Pseudo-element marker (C1/ADR-0015): Some = this style belongs to an
    /// engine-materialized pseudo node (map_style maps Display::None when
    /// content is none/normal based on this).
    pseudo: Option<crate::tree::PseudoWhich>,
}

impl Default for ComputedStyle {
    fn default() -> Self {
        Self {
            values: vec![None; PropertyId::SLOT_COUNT],
            custom: BTreeMap::new(),
            font_metrics: crate::css::value::FontMetrics::default(),
            pseudo: None,
        }
    }
}

impl ComputedStyle {
    /// Animation override (fifth batch ⑰): after the cascade, individual
    /// properties are overwritten by keyframe sampling.
    pub fn set_value(&mut self, id: PropertyId, v: DeclValue) {
        self.values[id.slot()] = Some(v);
    }

    /// Reads a slot value (host-readable contract, within the ADR-0003
    /// public API surface): `None` = the property never appeared explicitly
    /// in the cascade, keeping initial/inherit default semantics. The read
    /// entry for behavior-hint properties (host consumption channels such as
    /// cursor/user-select).
    pub fn value(&self, id: PropertyId) -> Option<&DeclValue> {
        self.values[id.slot()].as_ref()
    }

    /// Reads a resolved custom property's final value (after var()
    /// substitution; host-readable contract). Keys are custom property names
    /// including the `--` prefix.
    pub fn custom_value(&self, name: &str) -> Option<&str> {
        self.custom.get(name).map(|s| s.as_str())
    }

    /// Font-relative unit metrics (A9; rewritten by the engine during
    /// restyle based on font-family).
    pub fn font_metrics(&self) -> &crate::css::value::FontMetrics {
        &self.font_metrics
    }

    /// Font metrics write (crate-internal: called by the engine during
    /// restyle).
    pub(crate) fn set_font_metrics(&mut self, m: crate::css::value::FontMetrics) {
        self.font_metrics = m;
    }

    /// content computed value (C1): host nodes are always Normal (content
    /// only applies to pseudo elements); pseudo nodes are consumed by
    /// map_style (none/normal → no box).
    pub fn content(&self) -> crate::css::property::ContentValue {
        match self.values[PropertyId::Content.slot()].as_ref() {
            Some(DeclValue::Content(c)) => c.clone(),
            _ => crate::css::property::ContentValue::Normal,
        }
    }

    /// Pseudo-element marker read (C1).
    pub fn pseudo(&self) -> Option<crate::tree::PseudoWhich> {
        self.pseudo
    }

    /// Pseudo-element marker write (crate-internal: synced from the tree
    /// node during compute).
    pub(crate) fn set_pseudo(&mut self, p: Option<crate::tree::PseudoWhich>) {
        self.pseudo = p;
    }

    /// text-transform computed value (C2; inherited).
    pub fn text_transform(&self) -> crate::css::property::TextTransformKind {
        match self.values[PropertyId::TextTransform.slot()].as_ref() {
            Some(DeclValue::TextTransform(k)) => *k,
            _ => crate::css::property::TextTransformKind::None,
        }
    }

    /// overflow-wrap computed value (C2; not inherited).
    pub fn overflow_wrap(&self) -> crate::css::property::OverflowWrapKind {
        match self.values[PropertyId::OverflowWrap.slot()].as_ref() {
            Some(DeclValue::OverflowWrap(k)) => *k,
            _ => crate::css::property::OverflowWrapKind::Normal,
        }
    }

    /// word-break computed value (C2; not inherited).
    pub fn word_break(&self) -> crate::css::property::WordBreakKind {
        match self.values[PropertyId::WordBreak.slot()].as_ref() {
            Some(DeclValue::WordBreak(k)) => *k,
            _ => crate::css::property::WordBreakKind::Normal,
        }
    }

    /// object-fit computed value (C3; not inherited).
    pub fn object_fit(&self) -> crate::css::property::ObjectFitKind {
        match self.values[PropertyId::ObjectFit.slot()].as_ref() {
            Some(DeclValue::ObjectFit(k)) => *k,
            _ => crate::css::property::ObjectFitKind::Fill,
        }
    }

    /// object-position computed value (x, y) (C3; not inherited; initial
    /// center).
    pub fn object_position(
        &self,
    ) -> (
        crate::css::value::LengthPercentage,
        crate::css::value::LengthPercentage,
    ) {
        match self.values[PropertyId::ObjectPosition.slot()].as_ref() {
            Some(DeclValue::ObjectPosition(x, y)) => (x.clone(), y.clone()),
            _ => (
                crate::css::value::LengthPercentage::Percent(0.5),
                crate::css::value::LengthPercentage::Percent(0.5),
            ),
        }
    }

    /// text-overflow computed value (F2, ADR-0022 D2; not inherited).
    pub fn text_overflow(&self) -> crate::css::property::TextOverflowKind {
        match self.values[PropertyId::TextOverflow.slot()].as_ref() {
            Some(DeclValue::TextOverflow(k)) => *k,
            _ => crate::css::property::TextOverflowKind::Clip,
        }
    }

    /// -webkit-line-clamp line count (F2, ADR-0022 D3; 0 = none; not
    /// inherited).
    pub fn webkit_line_clamp(&self) -> u32 {
        match self.values[PropertyId::WebkitLineClamp.slot()].as_ref() {
            Some(DeclValue::WebkitLineClamp(n)) => *n,
            _ => 0,
        }
    }

    /// text-decoration-line bit set (F2, ADR-0022 D4; 1 = underline,
    /// 2 = overline, 4 = line-through; not inherited).
    pub fn text_decoration_line(&self) -> u8 {
        match self.values[PropertyId::TextDecorationLine.slot()].as_ref() {
            Some(DeclValue::TextDecorationLine(b)) => *b,
            _ => 0,
        }
    }

    /// text-decoration-style (F2, ADR-0022 D4; not inherited).
    pub fn text_decoration_style(&self) -> crate::css::property::TextDecoStyleKind {
        match self.values[PropertyId::TextDecorationStyle.slot()].as_ref() {
            Some(DeclValue::TextDecorationStyle(k)) => *k,
            _ => crate::css::property::TextDecoStyleKind::Solid,
        }
    }

    /// text-decoration-color (F2, ADR-0022 D4; defaults to currentColor; not
    /// inherited).
    pub fn text_decoration_color(&self) -> ColorValue {
        match self.values[PropertyId::TextDecorationColor.slot()].as_ref() {
            Some(DeclValue::Color(cv)) => *cv,
            _ => ColorValue::CurrentColor,
        }
    }

    /// text-decoration-thickness (F2, ADR-0022 D4; not inherited).
    pub fn text_decoration_thickness(&self) -> crate::css::property::TextDecoThickness {
        match self.values[PropertyId::TextDecorationThickness.slot()].as_ref() {
            Some(DeclValue::TextDecorationThickness(t)) => t.clone(),
            _ => crate::css::property::TextDecoThickness::Auto,
        }
    }

    /// text-shadow shadow list (F2, ADR-0022 D5; empty = none; inherited).
    pub fn text_shadows(&self) -> Vec<crate::css::property::TextShadowSpec> {
        match self.values[PropertyId::TextShadow.slot()].as_ref() {
            Some(DeclValue::TextShadow(v)) => v.clone(),
            _ => Vec::new(),
        }
    }

    /// float computed value (E4, css-position-3 / ADR-0019; not inherited).
    pub fn float(&self) -> crate::css::property::FloatKind {
        match self.values[PropertyId::Float.slot()].as_ref() {
            Some(DeclValue::Float(k)) => *k,
            _ => crate::css::property::FloatKind::None,
        }
    }

    /// clear computed value (E4, css-position-3 / ADR-0019; not inherited).
    pub fn clear(&self) -> crate::css::property::ClearKind {
        match self.values[PropertyId::Clear.slot()].as_ref() {
            Some(DeclValue::Clear(k)) => *k,
            _ => crate::css::property::ClearKind::None,
        }
    }

    /// border-image-source computed value (F3d, ADR-0026 D1; not inherited).
    pub fn border_image_source(&self) -> crate::css::property::BackgroundImage {
        match self.values[PropertyId::BorderImageSource.slot()].as_ref() {
            Some(DeclValue::BorderImageSource(v)) => v.clone(),
            _ => crate::css::property::BackgroundImage::None,
        }
    }

    /// border-image-slice computed value (F3d, ADR-0026 D1; not inherited).
    pub fn border_image_slice(&self) -> crate::css::property::BorderImageSlice {
        match self.values[PropertyId::BorderImageSlice.slot()].as_ref() {
            Some(DeclValue::BorderImageSlice(v)) => *v,
            _ => crate::css::property::BorderImageSlice {
                slices: [crate::css::property::BorderImageSliceComp::Percentage(100.0); 4],
                fill: false,
            },
        }
    }

    /// border-image-width computed value (F3d, ADR-0026 D1; not inherited).
    pub fn border_image_width(&self) -> crate::css::property::BorderImageWidth {
        match self.values[PropertyId::BorderImageWidth.slot()].as_ref() {
            Some(DeclValue::BorderImageWidth(v)) => v.clone(),
            _ => crate::css::property::BorderImageWidth {
                comps: std::array::from_fn(|_| crate::css::property::BorderImageWidthComp::Auto),
            },
        }
    }

    /// border-image-outset computed value (F3d, ADR-0026 D1; not inherited).
    pub fn border_image_outset(&self) -> crate::css::property::BorderImageOutset {
        match self.values[PropertyId::BorderImageOutset.slot()].as_ref() {
            Some(DeclValue::BorderImageOutset(v)) => v.clone(),
            _ => crate::css::property::BorderImageOutset {
                comps: std::array::from_fn(|_| {
                    crate::css::property::BorderImageOutsetComp::Length(LengthPercentage::Px(0.0))
                }),
            },
        }
    }

    /// border-image-repeat computed value (F3d, ADR-0026 D1; not inherited).
    pub fn border_image_repeat(&self) -> crate::css::property::BorderImageRepeatXY {
        match self.values[PropertyId::BorderImageRepeat.slot()].as_ref() {
            Some(DeclValue::BorderImageRepeat(v)) => *v,
            _ => crate::css::property::BorderImageRepeatXY {
                x: crate::css::property::BorderImageRepeatKind::Stretch,
                y: crate::css::property::BorderImageRepeatKind::Stretch,
            },
        }
    }

    /// font-stretch computed value (F3d, ADR-0026 D3; inherited; normalized
    /// percentage 50..=200, 100 = normal).
    pub fn font_stretch(&self) -> f32 {
        match self.values[PropertyId::FontStretch.slot()].as_ref() {
            Some(DeclValue::FontStretch(v)) => *v,
            _ => 100.0,
        }
    }

    /// word-spacing computed value (F3d, ADR-0026 D3; inherited;
    /// None = normal = 0, percentage basis is font-size, materialized after
    /// parsing).
    pub fn word_spacing(&self) -> Option<LengthPercentage> {
        match self.values[PropertyId::WordSpacing.slot()].as_ref() {
            Some(DeclValue::LenAuto(v)) => v.clone(),
            _ => None,
        }
    }

    /// font-feature-settings computed value (F3d, ADR-0026 D3; inherited;
    /// OpenType tag + value pair list).
    pub fn font_features(&self) -> Vec<([u8; 4], u16)> {
        match self.values[PropertyId::FontFeatures.slot()].as_ref() {
            Some(DeclValue::FontFeatures(v)) => v.clone(),
            _ => Vec::new(),
        }
    }

    /// font-variation-settings computed value (F3d, ADR-0026 D3; inherited;
    /// axis tag + value pair list).
    pub fn font_variations(&self) -> Vec<([u8; 4], f32)> {
        match self.values[PropertyId::FontVariations.slot()].as_ref() {
            Some(DeclValue::FontVariations(v)) => v.clone(),
            _ => Vec::new(),
        }
    }

    /// font-variant-caps computed value (F3d, ADR-0026 D3; inherited).
    pub fn font_variant_caps(&self) -> crate::css::property::FontVariantCapsKind {
        match self.values[PropertyId::FontVariantCaps.slot()].as_ref() {
            Some(DeclValue::FontVariantCaps(k)) => *k,
            _ => crate::css::property::FontVariantCapsKind::Normal,
        }
    }
}

/// Property inheritance (CSS cascade inheritance semantics: text/font
/// properties inherit, box-model properties do not).
pub fn inherits(id: PropertyId) -> bool {
    use PropertyId as P;
    matches!(
        id,
        P::Color
            | P::FontFamily
            | P::FontSize
            | P::FontWeight
            | P::FontStyle
            | P::LineHeight
            | P::TextAlign
            | P::WhiteSpace
            | P::LetterSpacing
            // A1 行为提示：cursor/pointer-events/caret-color/accent-color
            // 继承；user-select 明确不继承（CSS UI 4，见 BEHAVIOR-HINT-PROPS）
            | P::Cursor
            | P::PointerEvents
            | P::CaretColor
            | P::AccentColor
            // A8：direction 继承（逻辑→物理映射基准）；unicode-bidi 继承。
            // 逻辑 margin/padding/inset/border/radius 全不继承（同物理）。
            | P::Direction
            | P::UnicodeBidi
            // C2（css-text-3）：text-transform 继承；overflow-wrap/
            // word-break 不继承（spec）。
            | P::TextTransform
            // F2（ADR-0022 D5）：text-shadow 继承（css-backgrounds-3）。
            | P::TextShadow
            // F3d（ADR-0026）：字体深化五属性全继承（css-fonts-4）。
            | P::FontStretch
            | P::WordSpacing
            | P::FontFeatures
            | P::FontVariations
            | P::FontVariantCaps
            // F4（ADR-0028）：hyphens 继承（css-text-3 §5.4）；
            // backdrop-filter 不继承（同 filter，无需列出）。
            | P::Hyphens
            // P5（ADR-0036 D3）：quotes 继承（css-content-3）；
            // counter-reset/counter-increment 不继承（css-lists-3）。
            | P::Quotes
            // P9-3（css-lists-3 §3.3-3.5）：list-style 三长手全继承。
            | P::ListStyleType
            | P::ListStylePosition
            | P::ListStyleImage
    )
}

/// Property initial values (CSS initial; engine deviations are noted).
pub fn initial_value(id: PropertyId) -> DeclValue {
    use PropertyId as P;
    match id {
        // 枚举无 Inline：initial inline 块化（taffy 无 inline 上下文，偏差见 FEATURES.md）
        P::Display => DeclValue::Display(Display::Block),
        P::Position => DeclValue::Position(Position::Static),
        P::Top
        | P::Right
        | P::Bottom
        | P::Left
        | P::Width
        | P::Height
        | P::MaxWidth
        | P::MaxHeight
        | P::FlexBasis
        | P::GridAutoRows
        | P::GridAutoColumns => DeclValue::LenAuto(None),
        // z-index 初始 auto → ZIndex(None)；显式数字 → Some（auto 不再物化为 0）
        P::ZIndex => DeclValue::ZIndex(None),
        // 动画描述符（第五批⑰）：不可动画、不参与插值
        P::AnimationName => DeclValue::AnimationName(None),
        P::AnimationDuration => DeclValue::AnimationTime(0.0),
        P::AnimationDelay => DeclValue::AnimationTime(0.0),
        P::AnimationIterationCount => DeclValue::AnimationIteration(1.0),
        P::AnimationTimingFunction => DeclValue::AnimationTiming(TimingFn::Ease),
        P::AnimationDirection => DeclValue::AnimationDirection(AnimDirection::Normal),
        P::AnimationFillMode => DeclValue::AnimationFillMode(AnimFillMode::None),
        // transition 描述符（G1，ADR-0032）：all / 0s / ease / 0s / normal。
        // 不继承（inherits() 白名单未加）。
        P::TransitionProperty => {
            DeclValue::TransitionProperty(TransitionPropertyList(smallvec![TransitionTarget::All]))
        }
        P::TransitionDuration => DeclValue::TransitionTime(TransitionTimeList(smallvec![0.0])),
        P::TransitionTimingFunction => {
            DeclValue::TransitionTiming(TransitionTimingList(smallvec![TimingFn::Ease]))
        }
        P::TransitionDelay => DeclValue::TransitionTime(TransitionTimeList(smallvec![0.0])),
        P::TransitionBehavior => DeclValue::TransitionBehavior(TransitionBehavior::Normal),
        // P3（ADR-0034 D3）：vertical-align 初始 baseline（不继承）。
        P::VerticalAlign => {
            DeclValue::VerticalAlign(crate::css::property::VerticalAlignKind::Baseline)
        }
        // P5（ADR-0036 D2/D3）：计数器空表（不继承）/ quotes auto（继承）。
        P::CounterReset | P::CounterIncrement => DeclValue::CounterList(Vec::new()),
        P::Quotes => DeclValue::Quotes(crate::css::property::QuotesValue::Auto),
        // P9-3（css-lists-3 §3.3-3.5）：list-style 初始 disc/outside/none
        // （三长手全继承）。
        P::ListStyleType => DeclValue::ListStyleType(Some(
            crate::css::property::ListStyleTypeValue::Name("disc".to_string()),
        )),
        P::ListStylePosition => {
            DeclValue::ListStylePosition(crate::css::property::ListStylePosition::Outside)
        }
        P::ListStyleImage => DeclValue::ListStyleImage(None),
        P::MinWidth | P::MinHeight => DeclValue::Len(LengthPercentage::Px(0.0)),
        P::AspectRatio => DeclValue::AspectRatio(None),
        // margin 初始值为 0（CSS）；显式 auto 仍解析为 LenAuto(None) → 居中语义保留
        P::MarginTop | P::MarginRight | P::MarginBottom | P::MarginLeft => {
            DeclValue::LenAuto(Some(LengthPercentage::Px(0.0)))
        }
        P::PaddingTop
        | P::PaddingRight
        | P::PaddingBottom
        | P::PaddingLeft
        | P::Gap
        | P::RowGap
        | P::ColumnGap => DeclValue::Len(LengthPercentage::Px(0.0)),
        // ③multi-column 初始：count/width 均 auto（缺席语义）
        P::ColumnCount => DeclValue::ColumnCount(None),
        P::ColumnWidth => DeclValue::LenAuto(None),
        // ⑤a css-break 初始：auto（可断）——v1 布局不读，块一律不可断。
        P::BreakInside => DeclValue::BreakInside(None),
        P::ColumnSpan => DeclValue::ColumnSpan(None),
        // ⑤c 列规初始：width=medium、style=none、color=currentcolor。
        P::ColumnRuleWidth => DeclValue::ColumnRuleWidth(Some(LengthPercentage::Px(3.0))),
        P::ColumnRuleStyle => DeclValue::ColumnRuleStyle(crate::css::property::BorderStyle::None),
        P::ColumnRuleColor => DeclValue::Color(ColorValue::CurrentColor),
        P::FlexDirection => DeclValue::FlexDirection(FlexDirection::Row),
        P::FlexWrap => DeclValue::FlexWrap(FlexWrap::NoWrap),
        P::FlexGrow => DeclValue::Number(0.0),
        P::FlexShrink => DeclValue::Number(1.0),
        P::JustifyContent | P::AlignItems | P::AlignSelf | P::AlignContent => {
            DeclValue::Align(Align::Normal)
        }
        P::GridTemplateColumns | P::GridTemplateRows => {
            DeclValue::GridTracks(GridTemplate::default())
        }
        P::GridAutoFlow => DeclValue::GridAutoFlow(GridAutoFlowKind::Row),
        P::GridTemplateAreas => {
            DeclValue::GridAreas(crate::css::property::GridAreas { rows: Vec::new() })
        }
        P::GridRowStart | P::GridRowEnd | P::GridColumnStart | P::GridColumnEnd => {
            DeclValue::GridLine(crate::css::property::GridLineSpec::Auto)
        }
        P::BackgroundColor => {
            DeclValue::Color(ColorValue::Absolute(AlphaColor::new([0.0, 0.0, 0.0, 0.0])))
        }
        P::BackgroundImage => DeclValue::BackgroundImage(vec![BackgroundImage::None]),
        // F3b（ADR-0024）：background 全集初始值（单元素列表参与 cycling）。
        P::BackgroundRepeat => DeclValue::BackgroundRepeat(vec![crate::css::property::RepeatXY {
            x: crate::css::property::RepeatAxis::Repeat,
            y: crate::css::property::RepeatAxis::Repeat,
        }]),
        P::BackgroundAttachment => {
            DeclValue::BackgroundAttachment(vec![crate::css::property::Attachment::Scroll])
        }
        P::BackgroundPosition => {
            DeclValue::BackgroundPosition(vec![crate::css::property::Position2D {
                x: crate::css::property::PositionComp {
                    base: crate::css::value::LengthPercentage::Percent(0.0),
                    offset: None,
                },
                y: crate::css::property::PositionComp {
                    base: crate::css::value::LengthPercentage::Percent(0.0),
                    offset: None,
                },
            }])
        }
        P::BackgroundSize => DeclValue::BackgroundSize(vec![crate::css::property::BgSize::Auto]),
        P::BackgroundOrigin => {
            DeclValue::BackgroundOrigin(vec![crate::css::property::BackgroundBox::PaddingBox])
        }
        P::BackgroundClip => {
            DeclValue::BackgroundClip(vec![crate::css::property::BackgroundClip::Box(
                crate::css::property::BackgroundBox::BorderBox,
            )])
        }
        // F3c（ADR-0025）：clip-path 初始 none。
        P::ClipPath => DeclValue::ClipPath(crate::css::property::ClipShape::None),
        // F3d（ADR-0026 D1）：border-image 初始（css-backgrounds-3 §6）。
        P::BorderImageSource => {
            DeclValue::BorderImageSource(crate::css::property::BackgroundImage::None)
        }
        P::BorderImageSlice => {
            DeclValue::BorderImageSlice(crate::css::property::BorderImageSlice {
                slices: [crate::css::property::BorderImageSliceComp::Percentage(100.0); 4],
                fill: false,
            })
        }
        P::BorderImageWidth => {
            DeclValue::BorderImageWidth(crate::css::property::BorderImageWidth {
                comps: std::array::from_fn(|_| crate::css::property::BorderImageWidthComp::Auto),
            })
        }
        P::BorderImageOutset => {
            DeclValue::BorderImageOutset(crate::css::property::BorderImageOutset {
                comps: std::array::from_fn(|_| {
                    crate::css::property::BorderImageOutsetComp::Length(LengthPercentage::Px(0.0))
                }),
            })
        }
        P::BorderImageRepeat => {
            DeclValue::BorderImageRepeat(crate::css::property::BorderImageRepeatXY {
                x: crate::css::property::BorderImageRepeatKind::Stretch,
                y: crate::css::property::BorderImageRepeatKind::Stretch,
            })
        }
        // F3d（ADR-0026 D3）：字体深化初始（font-stretch=100/word-spacing
        // =0/特性·变差空表/caps=normal）。
        P::FontStretch => DeclValue::FontStretch(100.0),
        P::WordSpacing => DeclValue::LenAuto(None),
        P::FontFeatures => DeclValue::FontFeatures(Vec::new()),
        P::FontVariations => DeclValue::FontVariations(Vec::new()),
        P::FontVariantCaps => {
            DeclValue::FontVariantCaps(crate::css::property::FontVariantCapsKind::Normal)
        }
        P::BorderTopLeftRadius
        | P::BorderTopRightRadius
        | P::BorderBottomRightRadius
        | P::BorderBottomLeftRadius => {
            DeclValue::Radius(LengthPercentage::Px(0.0), LengthPercentage::Px(0.0))
        }
        P::BorderTopWidth | P::BorderRightWidth | P::BorderBottomWidth | P::BorderLeftWidth => {
            DeclValue::BorderWidth(Some(LengthPercentage::Px(3.0))) // medium
        }
        P::BorderTopStyle | P::BorderRightStyle | P::BorderBottomStyle | P::BorderLeftStyle => {
            DeclValue::BorderStyle(crate::css::property::BorderStyle::None)
        }
        P::BorderTopColor | P::BorderRightColor | P::BorderBottomColor | P::BorderLeftColor => {
            DeclValue::Color(ColorValue::CurrentColor)
        }
        P::BoxShadow => DeclValue::BoxShadows(SmallVec::new()),
        P::Opacity => DeclValue::Number(1.0),
        // F2（ADR-0022 D2）：text-overflow 初始 clip。
        P::TextOverflow => DeclValue::TextOverflow(crate::css::property::TextOverflowKind::Clip),
        // F2（ADR-0022 D3）：-webkit-line-clamp 初始 none（0）。
        P::WebkitLineClamp => DeclValue::WebkitLineClamp(0),
        // F2（ADR-0022 D4）：text-decoration 四长手初始（none/solid/
        // currentColor/auto）。
        P::TextDecorationLine => DeclValue::TextDecorationLine(0),
        P::TextDecorationStyle => {
            DeclValue::TextDecorationStyle(crate::css::property::TextDecoStyleKind::Solid)
        }
        P::TextDecorationColor => DeclValue::Color(ColorValue::CurrentColor),
        P::TextDecorationThickness => {
            DeclValue::TextDecorationThickness(crate::css::property::TextDecoThickness::Auto)
        }
        // F2（ADR-0022 D5）：text-shadow 初始 none（空表）。
        P::TextShadow => DeclValue::TextShadow(Vec::new()),
        P::OverflowX | P::OverflowY => DeclValue::Overflow(Overflow::Visible),
        // CSS 默认 content-box（width 只含内容盒）；box-model 用例约束该语义
        P::BoxSizing => DeclValue::BoxSizing(crate::css::property::BoxSizing::ContentBox),
        P::Transform => DeclValue::Transform(Vec::new()),
        // transform-origin 初始 50% 50%（第五批⑬：paint 期 origin 环绕消费）
        P::TransformOrigin => DeclValue::TransformOrigin(
            LengthPercentage::Percent(0.5),
            LengthPercentage::Percent(0.5),
        ),
        // filter/backdrop-filter 初始 none（P2，ADR-0031 D1：Filters(vec![])=
        // 有效声明显式无滤镜，携带函数链本体）；will-change/isolation 仍为
        // 存在性语义位（第四批④）；mix-blend-mode 具体 BlendMode（P1-2）；
        // clip-path 已升级为形状（F3c，ADR-0025，初始 none）。
        P::Filter | P::BackdropFilter => DeclValue::Filters(Vec::new()),
        P::WillChange | P::Isolation | P::MixBlendMode => DeclValue::Effect(false),
        // hyphens 初始 manual（F4，css-text-3 §5.4）。
        P::Hyphens => DeclValue::Hyphens(crate::css::property::HyphensKind::Manual),
        P::Color => DeclValue::Color(ColorValue::Absolute(AlphaColor::new([0.0, 0.0, 0.0, 1.0]))),
        P::FontFamily => DeclValue::FontFamily(FontFamilyList(smallvec![FamilyName::SansSerif])),
        P::FontSize => DeclValue::Len(LengthPercentage::Px(16.0)), // medium
        P::FontWeight => DeclValue::Number(400.0),
        P::FontStyle => DeclValue::FontStyle(FontStyle::Normal),
        P::LineHeight => DeclValue::LineHeight(LineHeight::Normal),
        P::TextAlign => DeclValue::TextAlign(TextAlign::Start),
        P::WhiteSpace => DeclValue::WhiteSpace(WhiteSpace::Normal),
        P::LetterSpacing => DeclValue::LenAuto(None), // normal：与解析产物同型（盘点修复）
        // 容器查询（阶段2③）初始：normal / 名单空
        P::ContainerType => DeclValue::ContainerType(ContainerType::Normal),
        P::ContainerName => DeclValue::ContainerName(Vec::new()),
        // A1 行为提示：初始全 auto（色值属性初始 None=auto，与解析产物同型）
        P::Cursor => DeclValue::Cursor(CursorKind::Auto),
        P::UserSelect => DeclValue::UserSelect(UserSelectKind::Auto),
        P::PointerEvents => DeclValue::PointerEvents(PointerEventsKind::Auto),
        P::CaretColor => DeclValue::CaretColor(None),
        P::AccentColor => DeclValue::AccentColor(None),
        // outline（A2）：width medium、style none、color currentcolor、offset 0
        P::OutlineWidth => DeclValue::BorderWidth(Some(LengthPercentage::Px(3.0))),
        P::OutlineStyle => DeclValue::OutlineStyle(OutlineStyle::None),
        P::OutlineColor => DeclValue::Color(ColorValue::CurrentColor),
        P::OutlineOffset => DeclValue::Len(LengthPercentage::Px(0.0)),
        // content（C1）：初始 normal（伪元素生成内容；宿主节点无效）
        P::Content => DeclValue::Content(crate::css::property::ContentValue::Normal),
        // C2 文本三属性：text-transform 初始 none；wrap 二属性初始 normal
        P::TextTransform => DeclValue::TextTransform(crate::css::property::TextTransformKind::None),
        P::OverflowWrap => DeclValue::OverflowWrap(crate::css::property::OverflowWrapKind::Normal),
        P::WordBreak => DeclValue::WordBreak(crate::css::property::WordBreakKind::Normal),
        // C3 替换内容两属性：object-fit 初始 fill；object-position 初始中心
        P::ObjectFit => DeclValue::ObjectFit(crate::css::property::ObjectFitKind::Fill),
        P::ObjectPosition => DeclValue::ObjectPosition(
            crate::css::value::LengthPercentage::Percent(0.5),
            crate::css::value::LengthPercentage::Percent(0.5),
        ),
        // E4 浮动两属性：float 初始 none；clear 初始 none（ADR-0019）
        P::Float => DeclValue::Float(crate::css::property::FloatKind::None),
        P::Clear => DeclValue::Clear(crate::css::property::ClearKind::None),
        // A8 逻辑属性：初始值与映射物理槽完全一致（css-logical-1）
        P::Direction => DeclValue::Direction(crate::css::property::DirectionKind::Ltr),
        P::UnicodeBidi => DeclValue::UnicodeBidi(crate::css::property::UnicodeBidiKind::Normal),
        P::MarginInlineStart | P::MarginInlineEnd | P::MarginBlockStart | P::MarginBlockEnd => {
            DeclValue::LenAuto(Some(LengthPercentage::Px(0.0)))
        }
        P::PaddingInlineStart | P::PaddingInlineEnd | P::PaddingBlockStart | P::PaddingBlockEnd => {
            DeclValue::Len(LengthPercentage::Px(0.0))
        }
        P::InsetInlineStart | P::InsetInlineEnd | P::InsetBlockStart | P::InsetBlockEnd => {
            DeclValue::LenAuto(None)
        }
        P::BorderInlineStartWidth
        | P::BorderInlineEndWidth
        | P::BorderBlockStartWidth
        | P::BorderBlockEndWidth => DeclValue::BorderWidth(Some(LengthPercentage::Px(3.0))),
        P::BorderInlineStartStyle
        | P::BorderInlineEndStyle
        | P::BorderBlockStartStyle
        | P::BorderBlockEndStyle => DeclValue::BorderStyle(crate::css::property::BorderStyle::None),
        P::BorderInlineStartColor
        | P::BorderInlineEndColor
        | P::BorderBlockStartColor
        | P::BorderBlockEndColor => DeclValue::Color(ColorValue::CurrentColor),
        P::BorderStartStartRadius
        | P::BorderStartEndRadius
        | P::BorderEndStartRadius
        | P::BorderEndEndRadius => {
            DeclValue::Radius(LengthPercentage::Px(0.0), LengthPercentage::Px(0.0))
        }
    }
}

/// Custom property resolver: inherited final values as the base, winning
/// raw values override, on-demand resolution + cycle detection.
struct CustomResolver<'a> {
    inherited: &'a BTreeMap<String, String>,
    own_raw: BTreeMap<String, String>,
    /// B4: registered property table (@property syntax gate / initial-value
    /// fallback criterion).
    registered: &'a BTreeMap<String, crate::css::property_rule::PropertyRule>,
    memo: BTreeMap<String, String>,
}

impl<'a> CustomResolver<'a> {
    fn get(&mut self, name: &str, stack: &mut Vec<String>) -> Option<String> {
        if let Some(v) = self.memo.get(name) {
            return Some(v.clone());
        }
        if stack.iter().any(|n| n == name) {
            return None; // 环 → guaranteed-invalid
        }
        let raw = match self.own_raw.get(name) {
            Some(r) => r.clone(),
            // 继承值已是终值，直接可用
            None => return self.inherited.get(name).cloned(),
        };
        // B1-3（css-variables-1 §3 / css-properties-1 §3）：custom property
        // 整值恰为 CSS 宽关键字 → 语义作用于 custom property 自身，不进入
        // 文本代换：initial（及级联已剔除的 revert 族，防御同）= guaranteed-
        // invalid，注册属性回退 initial-value；inherit/unset（custom property
        // 必继承）= 父 custom 终值，缺席时注册属性回退 initial-value，否则
        // guaranteed-invalid。
        let inherited = self.inherited;
        let registered = self.registered;
        let resolved = if let Some(kind) = crate::css::decl::whole_value_wide_keyword(&raw) {
            use crate::css::property::WideKeyword as WK;
            let reg_iv = || {
                registered
                    .get(name)
                    .and_then(|r| r.initial_value.as_ref())
                    .map(|iv| token_buf_to_string(iv))
            };
            match kind {
                WK::Initial | WK::Revert | WK::RevertLayer => reg_iv(),
                WK::Inherit | WK::Unset => inherited.get(name).cloned().or_else(reg_iv),
            }
        } else {
            stack.push(name.to_string());
            let sub = self.substitute(&raw, stack);
            stack.pop();
            sub
        };
        // B4：注册属性语法门——终值不匹配 syntax → unset → initial-value
        //（Chrome 一致：var(--x) 解析到 initial 而非触发 fallback；Named
        // 注册必有 initial（注册有效性保证），门失败恒有回值）。
        let resolved = resolved.and_then(|t| match registered.get(name) {
            Some(rule) if !crate::css::property_rule::syntax_matches(&rule.syntax, &t) => rule
                .initial_value
                .as_ref()
                .map(|iv| token_buf_to_string(iv)),
            _ => Some(t),
        });
        if let Some(t) = &resolved {
            self.memo.insert(name.to_string(), t.clone());
        }
        resolved
    }

    /// Substitutes all var() references in raw; None = guaranteed-invalid.
    fn substitute(&mut self, raw: &str, stack: &mut Vec<String>) -> Option<String> {
        if !raw.contains("var(") {
            return Some(raw.to_string());
        }
        let mut out = String::with_capacity(raw.len());
        let mut rest = raw;
        while let Some(pos) = rest.find("var(") {
            out.push_str(&rest[..pos]);
            let after = &rest[pos + 4..];
            let (args, consumed) = split_var_call(after)?;
            rest = &after[consumed..];
            let (name_raw, fallback) = split_var_args(args);
            let name = name_raw.trim();
            if !is_custom_name(name) {
                return None;
            }
            match self.get(name, stack) {
                Some(v) => out.push_str(&v),
                None => {
                    let fb = fallback?;
                    let resolved = self.substitute(fb.trim(), stack)?;
                    out.push_str(&resolved);
                }
            }
        }
        out.push_str(rest);
        Some(out)
    }
}

/// Scans from after "var(" to the matching ')'; returns (argument string,
/// total consumed length).
fn split_var_call(s: &str) -> Option<(&str, usize)> {
    let mut depth = 1i32;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&s[..i], i + 1));
                }
            }
            _ => {}
        }
    }
    None
}

/// Splits at the first top-level comma into the name argument and fallback.
fn split_var_args(args: &str) -> (&str, Option<&str>) {
    let mut depth = 0i32;
    for (i, c) in args.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => return (&args[..i], Some(&args[i + 1..])),
            _ => {}
        }
    }
    (args, None)
}

fn is_custom_name(name: &str) -> bool {
    name.starts_with("--") && name.len() > 2
}

impl ComputedStyle {
    /// Reads a property's computed value (None if absent).
    pub fn get(&self, id: PropertyId) -> Option<&DeclValue> {
        self.values[id.slot()].as_ref()
    }

    /// Reads a resolved custom property's final text (guaranteed-invalid →
    /// None).
    pub fn custom(&self, name: &str) -> Option<&str> {
        self.custom.get(name).map(String::as_str)
    }

    /// display (default block).
    pub fn display(&self) -> Display {
        match self.values[PropertyId::Display.slot()].as_ref() {
            Some(DeclValue::Display(d)) => *d,
            _ => Display::Block,
        }
    }

    /// position (default static).
    pub fn position(&self) -> Position {
        match self.values[PropertyId::Position.slot()].as_ref() {
            Some(DeclValue::Position(p)) => *p,
            _ => Position::Static,
        }
    }

    /// box-sizing (default content-box; converted to size semantics when
    /// mapped onto taffy).
    pub fn box_sizing(&self) -> crate::css::property::BoxSizing {
        match self.values[PropertyId::BoxSizing.slot()].as_ref() {
            Some(DeclValue::BoxSizing(b)) => *b,
            _ => crate::css::property::BoxSizing::ContentBox,
        }
    }

    /// transform function list (empty = none).
    pub fn transform(&self) -> &[crate::css::property::TransformFn] {
        match self.values[PropertyId::Transform.slot()].as_ref() {
            Some(DeclValue::Transform(list)) => list,
            _ => &[],
        }
    }

    /// transform ≠ none (ADR-0009 single decision predicate: the L2 cb
    /// semantic bit and the L3 SC trigger share it).
    pub fn has_transform(&self) -> bool {
        !self.transform().is_empty()
    }

    /// filter ≠ none (P2, ADR-0031 D1: carries the function chain itself;
    /// non-empty = an active filter).
    pub fn has_filter(&self) -> bool {
        matches!(self.get(PropertyId::Filter), Some(DeclValue::Filters(f)) if !f.is_empty())
    }

    /// backdrop-filter ≠ none (P2, ADR-0031 D1/D3: the function chain is
    /// materialized; non-empty triggers the backdrop SC; the effect is
    /// implemented by the sink — native in soft, T2 in vello).
    pub fn has_backdrop_filter(&self) -> bool {
        matches!(self.get(PropertyId::BackdropFilter), Some(DeclValue::Filters(f)) if !f.is_empty())
    }

    /// filter function chain access (P2): empty chain = none. Consumed by
    /// the paint layer and the dump.
    pub fn filter_chain(&self) -> &[crate::css::property::FilterFn] {
        match self.get(PropertyId::Filter) {
            Some(DeclValue::Filters(f)) => f,
            _ => &[],
        }
    }

    /// backdrop-filter function chain access (P2): empty chain = none.
    pub fn backdrop_filter_chain(&self) -> &[crate::css::property::FilterFn] {
        match self.get(PropertyId::BackdropFilter) {
            Some(DeclValue::Filters(f)) => f,
            _ => &[],
        }
    }

    /// vertical-align computed value (P3, ADR-0034 D3). A missing slot falls
    /// back to the initial Baseline.
    pub fn vertical_align(&self) -> crate::css::property::VerticalAlignKind {
        match self.get(PropertyId::VerticalAlign) {
            Some(DeclValue::VerticalAlign(v)) => v.clone(),
            _ => crate::css::property::VerticalAlignKind::Baseline,
        }
    }

    /// content sequence-segment view (P5, ADR-0036 D1): returns the segment
    /// table for `Seq`; `Str`/`None`/`Normal` → None (Str takes the
    /// single-string `content()` fast path).
    pub fn content_pieces(&self) -> Option<&[crate::css::property::ContentPiece]> {
        match self.get(PropertyId::Content) {
            Some(DeclValue::Content(crate::css::property::ContentValue::Seq(p))) => Some(p),
            _ => None,
        }
    }

    /// list-style-type computed value (P9-3, css-lists-3 §3.4): `None` =
    /// the keyword none (marker suppressed); a missing slot falls back to
    /// the initial disc.
    pub fn list_style_type(&self) -> Option<crate::css::property::ListStyleTypeValue> {
        match self.get(PropertyId::ListStyleType) {
            Some(DeclValue::ListStyleType(t)) => t.clone(),
            _ => Some(crate::css::property::ListStyleTypeValue::Name(
                "disc".to_string(),
            )),
        }
    }

    /// list-style-position computed value (P9-3, §3.5): a missing slot falls
    /// back to the initial outside.
    pub fn list_style_position(&self) -> crate::css::property::ListStylePosition {
        match self.get(PropertyId::ListStylePosition) {
            Some(DeclValue::ListStylePosition(p)) => *p,
            _ => crate::css::property::ListStylePosition::Outside,
        }
    }

    /// list-style-image computed value (P9-3, §3.3): `None` = none; a
    /// missing slot falls back to the initial none.
    pub fn list_style_image(&self) -> Option<crate::css::property::BackgroundImage> {
        match self.get(PropertyId::ListStyleImage) {
            Some(DeclValue::ListStyleImage(i)) => i.clone(),
            _ => None,
        }
    }

    /// counter-reset computed value (P5, ADR-0036 D2): a `[(name, initial
    /// value)]` table (none → empty table).
    pub fn counter_reset(&self) -> &[(String, i64)] {
        match self.get(PropertyId::CounterReset) {
            Some(DeclValue::CounterList(items)) => items,
            _ => &[],
        }
    }

    /// counter-increment computed value (P5, ADR-0036 D2): a `[(name,
    /// increment)]` table (none → empty table).
    pub fn counter_increment(&self) -> &[(String, i64)] {
        match self.get(PropertyId::CounterIncrement) {
            Some(DeclValue::CounterList(items)) => items,
            _ => &[],
        }
    }

    /// quotes pair table (P5, ADR-0036 D3): auto materializes the default
    /// pair table `“” ‘’` (css-content-3 §3); none → empty table.
    pub fn quotes_pairs(&self) -> Vec<(String, String)> {
        match self.get(PropertyId::Quotes) {
            Some(DeclValue::Quotes(crate::css::property::QuotesValue::Pairs(p))) => p.clone(),
            Some(DeclValue::Quotes(crate::css::property::QuotesValue::None)) => Vec::new(),
            _ => vec![
                ("\u{201C}".to_string(), "\u{201D}".to_string()),
                ("\u{2018}".to_string(), "\u{2019}".to_string()),
            ],
        }
    }

    /// hyphens hyphenation mode (F4, ADR-0028 D2; initial = manual; actual
    /// breaking depends on upstream segmenter boundaries; all three values
    /// behave identically in v1).
    pub fn hyphens(&self) -> crate::css::property::HyphensKind {
        match self.values[PropertyId::Hyphens.slot()].as_ref() {
            Some(DeclValue::Hyphens(k)) => *k,
            _ => crate::css::property::HyphensKind::Manual,
        }
    }

    /// clip-path presence (fourth batch ④ SC bit; F3c upgrade: any shape
    /// ≠ none triggers, ADR-0025).
    pub fn has_clip_path(&self) -> bool {
        !matches!(self.clip_path(), crate::css::property::ClipShape::None)
    }

    /// will-change contains an SC-triggering property (fifth batch ㉒, the
    /// full SC trigger set).
    pub fn has_will_change_sc(&self) -> bool {
        matches!(
            self.get(PropertyId::WillChange),
            Some(DeclValue::Effect(true))
        )
    }

    /// isolation: isolate (fifth batch ㉒).
    pub fn has_isolation(&self) -> bool {
        matches!(
            self.get(PropertyId::Isolation),
            Some(DeclValue::Effect(true))
        )
    }

    /// mix-blend-mode computed value (P1-2): absent or normal →
    /// `BlendMode::Normal`.
    pub fn mix_blend(&self) -> crate::css::property::BlendMode {
        match self.get(PropertyId::MixBlendMode) {
            Some(crate::css::property::DeclValue::BlendMode(m)) => *m,
            _ => crate::css::property::BlendMode::Normal,
        }
    }

    /// mix-blend-mode ≠ normal (SC trigger since fifth batch ㉒; carries the
    /// effect since P1-2).
    pub fn has_mix_blend(&self) -> bool {
        self.mix_blend() != crate::css::property::BlendMode::Normal
    }

    /// overflow-x (default visible).
    pub fn overflow_x(&self) -> Overflow {
        match self.values[PropertyId::OverflowX.slot()].as_ref() {
            Some(DeclValue::Overflow(o)) => *o,
            _ => Overflow::Visible,
        }
    }

    /// overflow-y (default visible).
    pub fn overflow_y(&self) -> Overflow {
        match self.values[PropertyId::OverflowY.slot()].as_ref() {
            Some(DeclValue::Overflow(o)) => *o,
            _ => Overflow::Visible,
        }
    }

    /// flex-direction (default row).
    pub fn flex_direction(&self) -> FlexDirection {
        match self.values[PropertyId::FlexDirection.slot()].as_ref() {
            Some(DeclValue::FlexDirection(d)) => *d,
            _ => FlexDirection::Row,
        }
    }

    /// flex-wrap (default nowrap).
    pub fn flex_wrap(&self) -> FlexWrap {
        match self.values[PropertyId::FlexWrap.slot()].as_ref() {
            Some(DeclValue::FlexWrap(w)) => *w,
            _ => FlexWrap::NoWrap,
        }
    }

    /// LenAuto family read: auto → None.
    pub fn len_auto(&self, id: PropertyId) -> Option<&LengthPercentage> {
        match self.values[id.slot()].as_ref() {
            Some(DeclValue::LenAuto(Some(lp))) => Some(lp),
            _ => None,
        }
    }

    /// Len family read.
    pub fn len(&self, id: PropertyId) -> Option<&LengthPercentage> {
        match self.values[id.slot()].as_ref() {
            Some(DeclValue::Len(lp)) => Some(lp),
            _ => None,
        }
    }

    /// Four-side margin (top, right, bottom, left; auto → None).
    pub fn margin(&self) -> [Option<&LengthPercentage>; 4] {
        [
            self.len_auto(PropertyId::MarginTop),
            self.len_auto(PropertyId::MarginRight),
            self.len_auto(PropertyId::MarginBottom),
            self.len_auto(PropertyId::MarginLeft),
        ]
    }

    /// Four-side padding (top, right, bottom, left).
    pub fn padding(&self) -> [Option<&LengthPercentage>; 4] {
        [
            self.len(PropertyId::PaddingTop),
            self.len(PropertyId::PaddingRight),
            self.len(PropertyId::PaddingBottom),
            self.len(PropertyId::PaddingLeft),
        ]
    }

    /// color (default opaque black).
    pub fn color(&self) -> ColorValue {
        match self.values[PropertyId::Color.slot()].as_ref() {
            Some(DeclValue::Color(c)) => *c,
            _ => ColorValue::Absolute(AlphaColor::new([0.0, 0.0, 0.0, 1.0])),
        }
    }

    /// background-color (default transparent).
    /// F3b (ADR-0024): background layer-aligned view — layer count =
    /// max(each longhand's layer count, 1); short lists are cycled to fill
    /// (css-backgrounds-3 §3); missing longhands participate with their
    /// initial values.
    pub fn background_layers(&self) -> Vec<crate::css::property::BackgroundLayer> {
        use crate::css::property::{
            Attachment, BackgroundBox, BackgroundClip, BackgroundImage, BgSize, Position2D,
            PositionComp, PropertyId as P, RepeatAxis, RepeatXY,
        };
        let images: Vec<BackgroundImage> = match self.get(P::BackgroundImage) {
            Some(DeclValue::BackgroundImage(v)) => v.clone(),
            _ => vec![BackgroundImage::None],
        };
        let repeats: Vec<RepeatXY> = match self.get(P::BackgroundRepeat) {
            Some(DeclValue::BackgroundRepeat(v)) => v.clone(),
            _ => vec![RepeatXY {
                x: RepeatAxis::Repeat,
                y: RepeatAxis::Repeat,
            }],
        };
        let attachments: Vec<Attachment> = match self.get(P::BackgroundAttachment) {
            Some(DeclValue::BackgroundAttachment(v)) => v.clone(),
            _ => vec![Attachment::Scroll],
        };
        let positions: Vec<Position2D> = match self.get(P::BackgroundPosition) {
            Some(DeclValue::BackgroundPosition(v)) => v.clone(),
            _ => vec![Position2D {
                x: PositionComp {
                    base: crate::css::value::LengthPercentage::Percent(0.0),
                    offset: None,
                },
                y: PositionComp {
                    base: crate::css::value::LengthPercentage::Percent(0.0),
                    offset: None,
                },
            }],
        };
        let sizes: Vec<BgSize> = match self.get(P::BackgroundSize) {
            Some(DeclValue::BackgroundSize(v)) => v.clone(),
            _ => vec![BgSize::Auto],
        };
        let origins: Vec<BackgroundBox> = match self.get(P::BackgroundOrigin) {
            Some(DeclValue::BackgroundOrigin(v)) => v.clone(),
            _ => vec![BackgroundBox::PaddingBox],
        };
        let clips: Vec<BackgroundClip> = match self.get(P::BackgroundClip) {
            Some(DeclValue::BackgroundClip(v)) => v.clone(),
            _ => vec![BackgroundClip::Box(BackgroundBox::BorderBox)],
        };
        let n = images
            .len()
            .max(repeats.len())
            .max(attachments.len())
            .max(positions.len())
            .max(sizes.len())
            .max(origins.len())
            .max(clips.len())
            .max(1);
        (0..n)
            .map(|i| crate::css::property::BackgroundLayer {
                image: images[i % images.len()].clone(),
                repeat: repeats[i % repeats.len()],
                attachment: attachments[i % attachments.len()],
                position: positions[i % positions.len()].clone(),
                size: sizes[i % sizes.len()].clone(),
                origin: origins[i % origins.len()],
                clip: clips[i % clips.len()],
            })
            .collect()
    }

    /// background-color — the background color computed value (ColorValue
    /// as-is; environment-resolved via resolve_color at paint time).
    pub fn background_color(&self) -> ColorValue {
        match self.values[PropertyId::BackgroundColor.slot()].as_ref() {
            Some(DeclValue::Color(c)) => *c,
            _ => ColorValue::Absolute(AlphaColor::new([0.0, 0.0, 0.0, 0.0])),
        }
    }

    /// clip-path — the clip shape (F3c, ADR-0025; initial none).
    pub fn clip_path(&self) -> crate::css::property::ClipShape {
        match self.values[PropertyId::ClipPath.slot()].as_ref() {
            Some(DeclValue::ClipPath(s)) => s.clone(),
            _ => crate::css::property::ClipShape::None,
        }
    }

    /// The resolved absolute font size (px).
    pub fn font_size_px(&self) -> f32 {
        match self.values[PropertyId::FontSize.slot()].as_ref() {
            Some(DeclValue::Len(LengthPercentage::Px(v))) => *v,
            _ => 16.0,
        }
    }

    /// font-weight (default 400).
    pub fn font_weight(&self) -> f32 {
        match self.values[PropertyId::FontWeight.slot()].as_ref() {
            Some(DeclValue::Number(n)) => *n,
            _ => 400.0,
        }
    }

    /// font-style (default normal).
    pub fn font_style(&self) -> FontStyle {
        match self.values[PropertyId::FontStyle.slot()].as_ref() {
            Some(DeclValue::FontStyle(s)) => *s,
            _ => FontStyle::Normal,
        }
    }

    /// font-family list (full-set materialization guarantees presence).
    pub fn font_family(&self) -> &FontFamilyList {
        match self.values[PropertyId::FontFamily.slot()].as_ref() {
            Some(DeclValue::FontFamily(list)) => list,
            _ => unreachable!("font family is materialized"),
        }
    }

    /// line-height (full-set materialization guarantees presence).
    pub fn line_height(&self) -> &LineHeight {
        match self.values[PropertyId::LineHeight.slot()].as_ref() {
            Some(DeclValue::LineHeight(lh)) => lh,
            _ => unreachable!("line-height is materialized"),
        }
    }

    /// text-align (default start).
    pub fn text_align(&self) -> TextAlign {
        match self.values[PropertyId::TextAlign.slot()].as_ref() {
            Some(DeclValue::TextAlign(a)) => *a,
            _ => TextAlign::Start,
        }
    }

    /// Line height resolved to px (None = normal; the consumer falls back to
    /// the typesetter's default font metrics ≈ CSS normal). The percentage
    /// basis is this element's font size (CSS line-height semantics);
    /// inventory fix: this property used to be resolved and stored but had
    /// no consumer at all (an invalid declaration).
    pub(crate) fn resolved_line_height_px(&self, env: &MediaEnv) -> Option<f32> {
        let fs = self.font_size_px();
        // P0 rem 修复：rem 基准 = MediaEnv.rem（引擎接线文档根字号）。
        let ctx = ResolveCtx::base(fs, env.rem, env.viewport_w, env.viewport_h);
        match self.line_height() {
            LineHeight::Normal => None,
            LineHeight::Number(n) => Some(n * fs),
            LineHeight::Len(lp) => lp.resolve(&ctx, fs),
        }
    }

    /// Letter spacing resolved to px (letter-spacing percentage basis is the
    /// font size). The parse product is LenAuto (normal → None); the initial
    /// value used to be Len(Px(0.0)), inconsistent with the parsed type —
    /// fixed in the inventory to the same-shaped LenAuto(None).
    pub(crate) fn resolved_letter_spacing_px(&self, env: &MediaEnv) -> f32 {
        let fs = self.font_size_px();
        // P0 rem 修复：rem 基准 = MediaEnv.rem（引擎接线文档根字号）。
        let ctx = ResolveCtx::base(fs, env.rem, env.viewport_w, env.viewport_h);
        match self.get(PropertyId::LetterSpacing) {
            Some(DeclValue::Len(lp)) => lp.resolve(&ctx, fs).unwrap_or(0.0),
            Some(DeclValue::LenAuto(Some(lp))) => lp.resolve(&ctx, fs).unwrap_or(0.0),
            _ => 0.0,
        }
    }

    /// Word spacing resolved to px (F3d, ADR-0026 D5; word-spacing
    /// percentage basis is the font size, same pattern as letter-spacing).
    /// None = normal = no consumption (parley defaults to 0).
    pub(crate) fn resolved_word_spacing_px(&self, env: &MediaEnv) -> Option<f32> {
        let fs = self.font_size_px();
        // P0 rem 修复：rem 基准 = MediaEnv.rem（引擎接线文档根字号）。
        let ctx = ResolveCtx::base(fs, env.rem, env.viewport_w, env.viewport_h);
        self.word_spacing()?.resolve(&ctx, fs)
    }

    /// Effective font-feature-settings (F3d, ADR-0026 D5): the explicit
    /// feature list unioned with font-variant-caps derivations
    /// (TitlingCaps→'titl', Unicase→'unic', CSS Fonts 4 semantics); when the
    /// same tag conflicts, the explicit feature-settings win (the derived tag
    /// is pushed only when not already present).
    pub(crate) fn effective_font_features(&self) -> Vec<([u8; 4], u16)> {
        let mut out = self.font_features();
        let derived: Option<([u8; 4], u16)> = match self.font_variant_caps() {
            crate::css::property::FontVariantCapsKind::TitlingCaps => Some((*b"titl", 1)),
            crate::css::property::FontVariantCapsKind::Unicase => Some((*b"unic", 1)),
            _ => None,
        };
        if let Some((tag, val)) = derived {
            // 同 tag 冲突时显式 feature-settings 优先（派生 tag 不重复才推）
            if !out.iter().any(|(t, _)| *t == tag) {
                out.push((tag, val));
            }
        }
        out
    }

    /// white-space (default normal).
    pub fn white_space(&self) -> WhiteSpace {
        match self.values[PropertyId::WhiteSpace.slot()].as_ref() {
            Some(DeclValue::WhiteSpace(w)) => *w,
            _ => WhiteSpace::Normal,
        }
    }

    /// direction (A8: the logical→physical mapping basis; default ltr).
    pub fn direction(&self) -> crate::css::property::DirectionKind {
        match self.values[PropertyId::Direction.slot()].as_ref() {
            Some(DeclValue::Direction(d)) => *d,
            _ => crate::css::property::DirectionKind::Ltr,
        }
    }

    /// unicode-bidi (A8: a text-stack hint; default normal).
    pub fn unicode_bidi(&self) -> crate::css::property::UnicodeBidiKind {
        match self.values[PropertyId::UnicodeBidi.slot()].as_ref() {
            Some(DeclValue::UnicodeBidi(b)) => *b,
            _ => crate::css::property::UnicodeBidiKind::Normal,
        }
    }

    /// opacity (default 1.0).
    pub fn opacity(&self) -> f32 {
        match self.values[PropertyId::Opacity.slot()].as_ref() {
            Some(DeclValue::Number(n)) => *n,
            _ => 1.0,
        }
    }

    /// z-index numeric value (auto or absent reads 0.0).
    pub fn z_index(&self) -> f32 {
        match self.values[PropertyId::ZIndex.slot()].as_ref() {
            Some(DeclValue::Number(n)) => *n,
            _ => 0.0,
        }
    }

    /// container-type (phase 2 ③; default Normal).
    pub fn container_type(&self) -> ContainerType {
        match self.values[PropertyId::ContainerType.slot()].as_ref() {
            Some(DeclValue::ContainerType(t)) => *t,
            _ => ContainerType::Normal,
        }
    }

    /// container-name list (phase 2 ③).
    pub fn container_names(&self) -> &[String] {
        match self.values[PropertyId::ContainerName.slot()].as_ref() {
            Some(DeclValue::ContainerName(list)) => list,
            _ => &[],
        }
    }

    /// Debug human-readable view (F3e, ADR-0027 D1): explicitly materialized
    /// slots (in `PropertyId::ALL` order, one `css_name: {Debug}` per line;
    /// None slots = never appeared explicitly in the cascade and are not
    /// printed) + custom properties (`--name: value`, lexicographic) + font
    /// metrics + the pseudo-element marker. The first entry point for hosts
    /// diagnosing "why is it drawn wrong / why is the layout wrong"; the
    /// machine round-trip channel is `paint_dump` (serde feature). Zero new
    /// state, zero branch semantics — a pure projection that takes no part in
    /// any cascade/settlement path.
    pub fn debug_dump(&self) -> String {
        let mut out = String::new();
        for pid in PropertyId::ALL {
            if let Some(v) = &self.values[pid.slot()] {
                out.push_str(&format!("{}: {:?}\n", pid.css_name(), v));
            }
        }
        for (name, value) in &self.custom {
            out.push_str(&format!("{name}: {value}\n"));
        }
        let m = &self.font_metrics;
        out.push_str(&format!(
            "[metrics] ch={:.4}/em ex={:.4}/em ic={:.4}/em\n",
            m.ch_per_em, m.ex_per_em, m.ic_per_em
        ));
        if let Some(p) = self.pseudo {
            out.push_str(&format!("[pseudo] {p:?}\n"));
        }
        out
    }
}

/// Computes a single node's style (called level by level from the root
/// downward; `parent` is the parent node's computed style).
pub fn compute_node<'a>(
    tree: &'a StyleTree,
    id: NodeId,
    sheet: &'a Stylesheet,
    env: &MediaEnv,
    parent: Option<&ComputedStyle>,
) -> ComputedStyle {
    compute_node_in(
        tree,
        id,
        vec![sheet],
        None,
        &std::collections::BTreeMap::new(),
        env,
        parent,
        &[],
        None,
    )
}

/// Compute with container snapshots (phase 2 ③): `container_ctx` is the
/// ancestor container stack (maintained by the restyle DFS, outermost to
/// innermost; zero-length and zero-cost without @container). B1: `user_sheet`
/// is the user-origin stylesheet (None = none). B2: `sheets` is the author
/// sheet group passed by value (main sheet first, extra sheets in
/// registration order = document order). B4: `registered` is the
/// document-level @property registry (merged during engine attachment; a
/// standalone compute_node has no registry = empty).
/// P5 (ADR-0033): `ua_sheet` is the UA-origin stylesheet (None = none;
/// collected first).
#[allow(clippy::too_many_arguments)] // 公开 API 形状保持（B2 user_sheet/B4 registered/P5 ua_sheet 扩展位）
pub fn compute_node_in<'a>(
    tree: &'a StyleTree,
    id: NodeId,
    sheets: Vec<&'a Stylesheet>,
    user_sheet: Option<&'a Stylesheet>,
    registered: &BTreeMap<String, crate::css::property_rule::PropertyRule>,
    env: &MediaEnv,
    parent: Option<&ComputedStyle>,
    container_ctx: &[ContainerCtx],
    ua_sheet: Option<&'a Stylesheet>,
) -> ComputedStyle {
    let cascaded = cascade_declarations(tree, id, sheets, user_sheet, env, container_ctx, ua_sheet);
    compute_node_from_cascade(tree, id, cascaded, registered, env, parent)
}

/// C4 (ADR-0018): computes a node's style from existing cascade output —
/// the body of compute_node_in extracted (public signature unchanged) for
/// reuse by the channel cascade (::selection/::placeholder); parent
/// semantics unchanged (channel calls pass the origin main style = the
/// css-pseudo-4 inheritance base).
pub fn compute_node_from_cascade(
    tree: &StyleTree,
    id: NodeId,
    cascaded: crate::cascade::CascadeOutput<'_>,
    registered: &BTreeMap<String, crate::css::property_rule::PropertyRule>,
    env: &MediaEnv,
    parent: Option<&ComputedStyle>,
) -> ComputedStyle {
    // A8：逻辑属性解析——direction 由自身声明冠军定夺（var 挂起则退回
    // 继承/初始），继承自父级，缺省 ltr；随后逻辑槽按级联序键并入物理槽。
    let direction = cascaded
        .winners
        .iter()
        .find(|(p, _)| *p == crate::css::property::PropertyId::Direction)
        .and_then(|(_, cands)| crate::cascade::cascade_winner(cands))
        .and_then(|c| match c.value {
            crate::css::decl::DeclSource::Parsed(crate::css::property::DeclValue::Direction(d)) => {
                Some(*d)
            }
            _ => None,
        })
        .or_else(|| parent.map(|p| p.direction()))
        .unwrap_or(crate::css::property::DirectionKind::Ltr);
    let cascaded = crate::cascade::resolve_logical(
        cascaded,
        direction == crate::css::property::DirectionKind::Rtl,
    );
    let mut style = ComputedStyle::default();
    // C1：伪元素标记自树节点同步（宿主节点恒 None）。
    style.set_pseudo(tree.node(id).pseudo);

    // 1) custom properties（B1：冠军取 beats 序最大者——revert 族已在
    //    级联内回滚剔除）
    let own_raw: BTreeMap<String, String> = cascaded
        .custom_winners
        .iter()
        .map(|(name, cands)| {
            (
                name.clone(),
                crate::cascade::cascade_winner_custom(cands)
                    .map(|c| token_buf_to_string(c.tokens))
                    .unwrap_or_default(),
            )
        })
        .collect();
    let empty = BTreeMap::new();
    let inherited = parent.map_or(&empty, |p| &p.custom);
    let mut resolver = CustomResolver {
        inherited,
        own_raw: own_raw.clone(),
        memo: BTreeMap::new(),
        registered,
    };
    let mut final_custom = BTreeMap::new();
    if let Some(p) = parent {
        for (k, v) in &p.custom {
            // 自身声明（哪怕解析失败）完全遮蔽继承值；B4：inherits=false
            // 注册属性不进继承通道（子代取 initial-value）。
            if !own_raw.contains_key(k) && !registered.get(k).is_some_and(|r| !r.inherits) {
                final_custom.insert(k.clone(), v.clone());
            }
        }
    }
    for name in own_raw.keys() {
        let mut stack = Vec::new();
        let _ = resolver.get(name, &mut stack); // 失败 = guaranteed-invalid，缺席
    }
    for (k, v) in &resolver.memo {
        final_custom.insert(k.clone(), v.clone());
    }
    // B4：注册属性 initial 填充——声明/继承双缺 → initial-value（无
    // initial 的 universal 注册 = 缺席 = guaranteed-invalid）。
    for (name, rule) in registered {
        if !final_custom.contains_key(name)
            && let Some(iv) = &rule.initial_value
        {
            final_custom.insert(name.clone(), token_buf_to_string(iv));
        }
    }
    style.custom = final_custom;

    // 2) 胜出声明（B1：冠军 = beats 序最大候选）：Parsed 直取；Var 代换
    //    重解析（失败 IACVT）；PendingShorthand 代换后 expand_shorthand
    //    展开，本长手取展开结果，代换失败/文法失败 → IACVT
    for (pid, cands) in &cascaded.winners {
        let Some(champ) = crate::cascade::cascade_winner(cands) else {
            continue;
        };
        let value =
            match champ.value {
                DeclSource::Parsed(v) => Some(v.clone()),
                DeclSource::Var(tokens) => {
                    let raw = token_buf_to_string(tokens);
                    let mut subst = CustomResolver {
                        inherited: &style.custom,
                        own_raw: BTreeMap::new(),
                        memo: BTreeMap::new(),
                        registered,
                    };
                    let mut stack = Vec::new();
                    match subst.substitute(&raw, &mut stack) {
                        Some(text) => {
                            let mut input = cssparser::Parser::new(&text);
                            // B1：var() 代换结果恰为宽关键字 → 整值语义
                            crate::css::decl::try_parse_wide_keyword(&mut input)
                                .map(crate::css::property::DeclValue::WideKeyword)
                                .or_else(|| {
                                    crate::css::property::parse_declaration(*pid, &mut input).ok()
                                })
                        }
                        None => None,
                    }
                }
                DeclSource::PendingShorthand { shorthand, tokens } => {
                    let raw = token_buf_to_string(tokens);
                    let mut subst = CustomResolver {
                        inherited: &style.custom,
                        own_raw: BTreeMap::new(),
                        memo: BTreeMap::new(),
                        registered,
                    };
                    let mut stack = Vec::new();
                    subst
                        .substitute(&raw, &mut stack)
                        .and_then(|text| {
                            let mut input = cssparser::Parser::new(&text);
                            // B1：代换结果恰为宽关键字 → 简写长手全集各得该语义
                            if let Some(kind) = crate::css::decl::try_parse_wide_keyword(&mut input)
                            {
                                return Some(
                                    crate::css::decl::shorthand_longhands(shorthand)
                                        .map(|lh| {
                                            lh.into_iter().map(|p| {
                                            (p, crate::css::property::DeclValue::WideKeyword(kind))
                                        })
                                        .collect::<Vec<_>>()
                                        })
                                        .unwrap_or_default(),
                                );
                            }
                            crate::css::decl::expand_shorthand(shorthand, &mut input)
                                .ok()
                                .and_then(|o| o)
                        })
                        .and_then(|longhands| {
                            longhands
                                .into_iter()
                                .find(|(p, _)| p == pid)
                                .map(|(_, v)| v)
                        })
                }
            };
        match value {
            Some(crate::css::property::DeclValue::WideKeyword(kind)) => {
                // B1：宽关键字物化（css-values-4）。Revert/RevertLayer 已在
                // 级联赛后回滚（防御性按 absent 处理 → 初值物化）。
                use crate::css::property::WideKeyword as WK;
                let resolved = match kind {
                    WK::Initial => Some(initial_value(*pid)),
                    WK::Inherit => parent.and_then(|p| p.values[pid.slot()].as_ref()).cloned(),
                    WK::Unset => {
                        if inherits(*pid) {
                            parent.and_then(|p| p.values[pid.slot()].as_ref()).cloned()
                        } else {
                            Some(initial_value(*pid))
                        }
                    }
                    WK::Revert | WK::RevertLayer => None,
                };
                if let Some(v) = resolved {
                    style.values[pid.slot()] = Some(v);
                }
            }
            Some(v) => {
                style.values[pid.slot()] = Some(v);
            }
            None => {
                // IACVT：继承属性取父值，否则初始值
                if inherits(*pid)
                    && let Some(pv) = parent.and_then(|p| p.values[pid.slot()].as_ref())
                {
                    style.values[pid.slot()] = Some(pv.clone());
                    continue;
                }
                style.values[pid.slot()] = Some(initial_value(*pid));
            }
        }
    }

    // 3) 全集物化：继承或初始值（按 pid.slot() 落槽；A8 起 ALL 序与槽位
    //    序不再一致——逻辑槽排在动画描述符之后；胜出声明已占槽的跳过）
    for pid in PropertyId::ALL.iter() {
        let slot = pid.slot();
        if style.values[slot].is_some() {
            continue;
        }
        if inherits(*pid)
            && let Some(pv) = parent.and_then(|p| p.values[slot].as_ref())
        {
            style.values[slot] = Some(pv.clone());
            continue;
        }
        style.values[slot] = Some(initial_value(*pid));
    }

    // 4) 字号解析（em 基于父字号；rem 按传入 env.rem——根元素 font-size
    // 的 rem 由引擎在求值前置为初始值 16，CSS Values 语义）
    if let Some(DeclValue::Len(lp)) = style.values[PropertyId::FontSize.slot()].clone() {
        let parent_font = parent.map_or(16.0, |p| p.font_size_px());
        let ctx = ResolveCtx::base(parent_font, env.rem, env.viewport_w, env.viewport_h);
        if let Some(px) = lp.resolve(&ctx, parent_font) {
            style.values[PropertyId::FontSize.slot()] =
                Some(DeclValue::Len(LengthPercentage::Px(px)));
        }
    }

    // 4b) 相对字号物化（css-fonts-4 `<<relative-size>>`）：larger/smaller
    // 按父计算字号终结为 px（父槽自顶向下先算完，恒为已解析绝对值；
    // 根节点无父 = 以初始 16px 为基）。
    if let Some(DeclValue::RelativeFontSize(larger)) =
        style.values[PropertyId::FontSize.slot()].clone()
    {
        let px = parent.map_or(16.0, |p| p.font_size_px());
        style.values[PropertyId::FontSize.slot()] = Some(DeclValue::Len(LengthPercentage::Px(
            crate::css::property::relative_font_size(px, larger),
        )));
    }

    // 5) 相对字重物化（css-fonts-4 §2.2.1）：bolder/lighter 按父计算权重
    // 查表终结为 Number（父槽自顶向下先算完，恒为已解析的绝对权重；
    // 根节点无父 = 以初始权重 400 为基，与浏览器一致）。
    if let Some(DeclValue::RelativeFontWeight(bolder)) =
        style.values[PropertyId::FontWeight.slot()].clone()
    {
        let w = parent.map_or(400.0, |p| p.font_weight());
        style.values[PropertyId::FontWeight.slot()] = Some(DeclValue::Number(
            crate::css::property::relative_font_weight(w, bolder),
        ));
    }

    style
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{StyleNode, StyleTree};

    fn sheet(src: &str) -> Stylesheet {
        let s = crate::css::stylesheet::parse_stylesheet(src);
        assert!(s.report.is_clean(), "{:?}", s.report);
        s
    }

    fn node(name: &str, classes: &[&str], inline: &str) -> StyleNode {
        StyleNode {
            name: Some(name.into()),
            classes: classes.iter().map(|c| c.to_string()).collect(),
            declarations: crate::css::decl::parse_inline_declarations(inline).0,
            ..Default::default()
        }
    }

    fn color_rgb(style: &ComputedStyle) -> [f32; 4] {
        match style.color() {
            ColorValue::Absolute(c) => c.components,
            ColorValue::CurrentColor => [f32::NAN; 4],
            ColorValue::LightDark(..) => [f32::NAN; 4],
        }
    }

    #[test]
    fn inheritance_and_initials() {
        let s = sheet(".btn { color: red }");
        let mut tree = StyleTree::new();
        let root = tree.root();
        let mid = tree.insert_child(root, node("div", &["btn"], ""));
        let leaf = tree.insert_child(mid, node("span", &[], ""));

        let root_style = compute_node(&tree, root, &s, &MediaEnv::default(), None);
        assert_eq!(color_rgb(&root_style)[0], 0.0); // 初始黑
        let mid_style = compute_node(&tree, mid, &s, &MediaEnv::default(), Some(&root_style));
        assert_eq!(color_rgb(&mid_style)[0], 1.0); // red
        let leaf_style = compute_node(&tree, leaf, &s, &MediaEnv::default(), Some(&mid_style));
        assert_eq!(color_rgb(&leaf_style)[0], 1.0); // color 继承
        // width 不继承 → 初始 auto
        assert!(matches!(
            leaf_style.get(PropertyId::Width),
            Some(DeclValue::LenAuto(None))
        ));
    }

    #[test]
    fn background_layers_cycling_alignment() {
        // F3b（ADR-0024）：层列表 cycling 锁——层数 = max(各长手层数,1)，
        // 短列表按 i%len 循环补齐（css-backgrounds-3 §3）；缺省长手以初始值参与。
        let s = sheet(".b { background-image: url(a.png), url(b.png), url(c.png) }");
        let mut tree = StyleTree::new();
        let root = tree.root();
        let n = tree.insert_child(root, node("div", &["b"], "background-repeat: no-repeat"));
        let style = compute_node(&tree, n, &s, &MediaEnv::default(), None);
        let layers = style.background_layers();
        assert_eq!(layers.len(), 3);
        // repeat 单条 → cycling 补齐三层
        assert!(layers.iter().all(|l| l.repeat
            == crate::css::property::RepeatXY {
                x: crate::css::property::RepeatAxis::NoRepeat,
                y: crate::css::property::RepeatAxis::NoRepeat,
            }));
        // 缺省长手 = 初始值参与
        assert!(
            layers
                .iter()
                .all(|l| matches!(l.attachment, crate::css::property::Attachment::Scroll))
        );
        assert!(
            layers
                .iter()
                .all(|l| matches!(l.origin, crate::css::property::BackgroundBox::PaddingBox))
        );
        assert!(layers.iter().all(|l| matches!(
            l.clip,
            crate::css::property::BackgroundClip::Box(
                crate::css::property::BackgroundBox::BorderBox
            )
        )));
        assert!(
            layers
                .iter()
                .all(|l| matches!(l.size, crate::css::property::BgSize::Auto))
        );
        // images 逐层对应
        assert!(
            matches!(layers[0].image, crate::css::property::BackgroundImage::Url(ref u) if u == "a.png")
        );
        assert!(
            matches!(layers[2].image, crate::css::property::BackgroundImage::Url(ref u) if u == "c.png")
        );

        // 反向 cycling：position 3 条 > image 2 条 → 层数=3，image 循环 a,b,a
        let n2 = tree.insert_child(
            root,
            node(
                "div",
                &[],
                "background-image: url(a.png), url(b.png); \
                 background-position: 0% 0%, 100% 100%, 50% 50%",
            ),
        );
        let style2 = compute_node(&tree, n2, &s, &MediaEnv::default(), None);
        let l2 = style2.background_layers();
        assert_eq!(l2.len(), 3);
        assert!(
            matches!(l2[0].image, crate::css::property::BackgroundImage::Url(ref u) if u == "a.png")
        );
        assert!(
            matches!(l2[1].image, crate::css::property::BackgroundImage::Url(ref u) if u == "b.png")
        );
        assert!(
            matches!(l2[2].image, crate::css::property::BackgroundImage::Url(ref u) if u == "a.png")
        );
        assert!(matches!(
            l2[2].position.x.base,
            crate::css::value::LengthPercentage::Percent(v) if (v - 0.5).abs() < 1e-6
        ));
    }

    #[test]
    fn inline_and_important_ladder() {
        let s = sheet("div { color: red !important }");
        let mut tree = StyleTree::new();
        let root = tree.root();
        let n = tree.insert_child(root, node("div", &[], "color: blue"));
        let style = compute_node(&tree, n, &s, &MediaEnv::default(), None);
        assert_eq!(color_rgb(&style)[0], 1.0); // important 样式表胜内联 normal（red 胜）
    }

    #[test]
    fn specificity_prefers_class() {
        let s = sheet("div { color: blue } .btn { color: red }");
        let mut tree = StyleTree::new();
        let root = tree.root();
        let n = tree.insert_child(root, node("div", &["btn"], ""));
        let style = compute_node(&tree, n, &s, &MediaEnv::default(), None);
        assert_eq!(color_rgb(&style)[0], 1.0); // .btn 胜
    }

    #[test]
    fn var_chain_and_fallback() {
        let s = sheet(
            "div { --a: var(--b); --b: 12px; width: var(--a); \
             height: var(--missing, 40px) }",
        );
        let mut tree = StyleTree::new();
        let root = tree.root();
        let n = tree.insert_child(root, node("div", &[], ""));
        let style = compute_node(&tree, n, &s, &MediaEnv::default(), None);
        assert_eq!(style.custom("--b"), Some("12px"));
        assert_eq!(style.custom("--a"), Some("12px"));
        assert!(matches!(
            style.get(PropertyId::Width),
            Some(DeclValue::LenAuto(Some(LengthPercentage::Px(v)))) if *v == 12.0
        ));
        assert!(matches!(
            style.get(PropertyId::Height),
            Some(DeclValue::LenAuto(Some(LengthPercentage::Px(v)))) if *v == 40.0
        ));
    }

    #[test]
    fn var_iacvt_inherits_or_initial() {
        let s = sheet(".p { color: red } .c { color: var(--missing); width: var(--missing) }");
        let mut tree = StyleTree::new();
        let root = tree.root();
        let p = tree.insert_child(root, node("div", &["p"], ""));
        let c = tree.insert_child(p, node("span", &["c"], ""));
        let p_style = compute_node(&tree, p, &s, &MediaEnv::default(), None);
        let c_style = compute_node(&tree, c, &s, &MediaEnv::default(), Some(&p_style));
        // color 继承属性 IACVT → 父值 red
        assert_eq!(color_rgb(&c_style)[0], 1.0);
        // width 非继承 IACVT → 初始 auto
        assert!(matches!(
            c_style.get(PropertyId::Width),
            Some(DeclValue::LenAuto(None))
        ));
    }

    #[test]
    fn var_custom_wide_keyword_initial_guaranteed_invalid() {
        // B1-3（css-variables-1 §3）：--k: initial → guaranteed-invalid，
        // 不是文本 "initial"。带 fallback → fallback 胜；无 fallback →
        // IACVT（继承属性取父值）。
        let s = sheet(
            ".p { color: red } .c1 { --k: initial; color: var(--k, lime) } \
             .c2 { --k: initial; color: var(--k) }",
        );
        let mut tree = StyleTree::new();
        let root = tree.root();
        let p = tree.insert_child(root, node("div", &["p"], ""));
        let c1 = tree.insert_child(p, node("span", &["c1"], ""));
        let p_style = compute_node(&tree, p, &s, &MediaEnv::default(), None);
        let c1_style = compute_node(&tree, c1, &s, &MediaEnv::default(), Some(&p_style));
        assert_eq!(
            color_rgb(&c1_style)[1],
            1.0,
            "initial → guaranteed-invalid → fallback lime"
        );
        // 无 fallback：color: var(--k) → IACVT → 父值 red
        let c2 = tree.insert_child(p, node("span", &["c2"], ""));
        let c2_style = compute_node(&tree, c2, &s, &MediaEnv::default(), Some(&p_style));
        assert_eq!(color_rgb(&c2_style)[0], 1.0, "initial → IACVT → 父值 red");
    }

    #[test]
    fn var_custom_wide_keyword_inherit_and_unset() {
        // B1-3：--k: inherit / unset（custom property 必继承）→ 父 custom
        // 终值；父缺席 = guaranteed-invalid（IACVT 归 auto），而非文本
        // "inherit" 被代换成 width 的整值关键字。
        let s = sheet(
            ".p { --k: 5px; --j: 7px; color: blue } .c { --k: inherit; \
             --j: unset; padding-top: var(--k); padding-bottom: var(--j) }",
        );
        let mut tree = StyleTree::new();
        let root = tree.root();
        let p = tree.insert_child(root, node("div", &["p"], ""));
        let c = tree.insert_child(p, node("span", &["c"], ""));
        let p_style = compute_node(&tree, p, &s, &MediaEnv::default(), None);
        let c_style = compute_node(&tree, c, &s, &MediaEnv::default(), Some(&p_style));
        let px = |v: Option<&DeclValue>| match v {
            Some(DeclValue::LenAuto(Some(LengthPercentage::Px(x)))) => *x,
            Some(DeclValue::Len(LengthPercentage::Px(x))) => *x,
            other => panic!("px: {other:?}"),
        };
        assert_eq!(
            px(c_style.get(PropertyId::PaddingTop)),
            5.0,
            "inherit → 父 --k"
        );
        assert_eq!(
            px(c_style.get(PropertyId::PaddingBottom)),
            7.0,
            "unset → 父 --j"
        );
        // 父无 --k：inherit → guaranteed-invalid → width IACVT → auto
        let s2 = sheet(".c { --k: inherit; width: var(--k) }");
        let c2 = tree.insert_child(root, node("div", &["c"], ""));
        let c2_style = compute_node(&tree, c2, &s2, &MediaEnv::default(), None);
        assert!(
            matches!(
                c2_style.get(PropertyId::Width),
                Some(DeclValue::LenAuto(None))
            ),
            "父缺席 → guaranteed-invalid → auto（非代换文本 inherit）"
        );
    }

    #[test]
    fn var_fallback_text_wide_keyword_lock() {
        // B1 锁：fallback 文本代换结果恰为宽关键字 → 整值语义
        //（computed.rs 代换后 try_parse_wide_keyword 通路）。
        let s = sheet(".p { color: red } .c { color: var(--undef, inherit) }");
        let mut tree = StyleTree::new();
        let root = tree.root();
        let p = tree.insert_child(root, node("div", &["p"], ""));
        let c = tree.insert_child(p, node("span", &["c"], ""));
        let p_style = compute_node(&tree, p, &s, &MediaEnv::default(), None);
        let c_style = compute_node(&tree, c, &s, &MediaEnv::default(), Some(&p_style));
        assert_eq!(color_rgb(&c_style)[0], 1.0, "fallback inherit → 父值 red");
    }

    #[test]
    fn var_shorthand_expands_at_computed_time() {
        // 阶段2②：var() 简写计算值期代换+expand_shorthand 展开——TRBL 槽位
        // 分配在代换后完成；同块长手与简写逐槽竞争（margin-top 5px 胜简写
        // 对应槽，其余槽来自展开）。
        let s = sheet(
            "div { --m: 10px; margin: var(--m) 20px; margin-top: 5px; \
             padding: var(--p, 8px) 0 }",
        );
        let mut tree = StyleTree::new();
        let root = tree.root();
        let n = tree.insert_child(root, node("div", &[], ""));
        let style = compute_node(&tree, n, &s, &MediaEnv::default(), None);
        let px = |v: Option<&DeclValue>, what: &str| match v {
            Some(DeclValue::LenAuto(Some(LengthPercentage::Px(x)))) => *x,
            Some(DeclValue::Len(LengthPercentage::Px(x))) => *x,
            other => panic!("{what} = {other:?}"),
        };
        assert_eq!(
            px(style.get(PropertyId::MarginTop), "margin-top"),
            5.0,
            "同块长手胜简写槽"
        );
        assert_eq!(px(style.get(PropertyId::MarginRight), "margin-right"), 20.0);
        assert_eq!(
            px(style.get(PropertyId::MarginBottom), "margin-bottom"),
            10.0,
            "两值 TRBL：上下取第一分量"
        );
        assert_eq!(
            px(style.get(PropertyId::MarginLeft), "margin-left"),
            20.0,
            "两值 TRBL：左右取第二分量"
        );
        assert_eq!(
            px(style.get(PropertyId::PaddingTop), "padding-top"),
            8.0,
            "fallback 代换"
        );
        assert_eq!(
            px(style.get(PropertyId::PaddingRight), "padding-right"),
            0.0
        );
        assert_eq!(
            px(style.get(PropertyId::PaddingBottom), "padding-bottom"),
            8.0
        );
        assert_eq!(px(style.get(PropertyId::PaddingLeft), "padding-left"), 0.0);
    }

    #[test]
    fn var_shorthand_invalid_substitution_iacvt() {
        // 代换失败（--undef 无 fallback）→ 简写全部手 IACVT：
        // margin 非继承 → 初始 0px（LenAuto(Some(Px(0)))）。
        let s = sheet("div { margin: var(--undef) 20px }");
        let mut tree = StyleTree::new();
        let root = tree.root();
        let n = tree.insert_child(root, node("div", &[], ""));
        let style = compute_node(&tree, n, &s, &MediaEnv::default(), None);
        for pid in [
            PropertyId::MarginTop,
            PropertyId::MarginRight,
            PropertyId::MarginBottom,
            PropertyId::MarginLeft,
        ] {
            assert!(
                matches!(
                    style.get(pid),
                    Some(DeclValue::LenAuto(Some(LengthPercentage::Px(v)))) if *v == 0.0
                ),
                "{pid:?} 应 IACVT 归初始 0"
            );
        }
    }

    #[test]
    fn custom_inheritance_and_cycle() {
        let s = sheet(
            ".p { --brand: #ff0000 } .c { color: var(--brand); --x: var(--y); --y: var(--x) }",
        );
        let mut tree = StyleTree::new();
        let root = tree.root();
        let p = tree.insert_child(root, node("div", &["p"], ""));
        let c = tree.insert_child(p, node("span", &["c"], ""));
        let p_style = compute_node(&tree, p, &s, &MediaEnv::default(), None);
        let c_style = compute_node(&tree, c, &s, &MediaEnv::default(), Some(&p_style));
        assert_eq!(color_rgb(&c_style)[0], 1.0); // 继承的 custom property 生效
        assert_eq!(c_style.custom("--x"), None); // 环 → guaranteed-invalid
    }

    #[test]
    fn font_size_em_resolves_against_parent() {
        let s = sheet(".p { font-size: 20px } .c { font-size: 0.5em }");
        let mut tree = StyleTree::new();
        let root = tree.root();
        let p = tree.insert_child(root, node("div", &["p"], ""));
        let c = tree.insert_child(p, node("span", &["c"], ""));
        let p_style = compute_node(&tree, p, &s, &MediaEnv::default(), None);
        assert_eq!(p_style.font_size_px(), 20.0);
        let c_style = compute_node(&tree, c, &s, &MediaEnv::default(), Some(&p_style));
        assert_eq!(c_style.font_size_px(), 10.0);
    }

    #[test]
    fn relative_font_weight_table_edges() {
        // css-fonts-4 §2.2.1 图表逐行边界。
        use crate::css::property::relative_font_weight as rfw;
        // bolder
        assert_eq!(rfw(1.0, true), 400.0); // w<100 → 400
        assert_eq!(rfw(99.9, true), 400.0);
        assert_eq!(rfw(100.0, true), 400.0);
        assert_eq!(rfw(349.0, true), 400.0);
        assert_eq!(rfw(350.0, true), 700.0);
        assert_eq!(rfw(400.0, true), 700.0);
        assert_eq!(rfw(549.0, true), 700.0);
        assert_eq!(rfw(550.0, true), 900.0);
        assert_eq!(rfw(750.0, true), 900.0);
        assert_eq!(rfw(899.0, true), 900.0);
        assert_eq!(rfw(900.0, true), 900.0); // 不变（900 即 900）
        assert_eq!(rfw(1000.0, true), 1000.0); // 不变（>900 保原值）
        // lighter
        assert_eq!(rfw(50.0, false), 50.0); // w<100 不变
        assert_eq!(rfw(100.0, false), 100.0);
        assert_eq!(rfw(349.0, false), 100.0);
        assert_eq!(rfw(350.0, false), 100.0);
        assert_eq!(rfw(549.0, false), 100.0);
        assert_eq!(rfw(550.0, false), 400.0);
        assert_eq!(rfw(749.0, false), 400.0);
        assert_eq!(rfw(750.0, false), 700.0);
        assert_eq!(rfw(899.0, false), 700.0);
        assert_eq!(rfw(900.0, false), 700.0);
        assert_eq!(rfw(1000.0, false), 700.0);
    }

    #[test]
    fn bolder_lighter_parse_and_materialize_chain() {
        let s = sheet(
            ".a { font-weight: bolder } .b { font-weight: lighter } \
             .p { font-weight: 100 } .q { font-weight: lighter }",
        );
        let mut tree = StyleTree::new();
        let root = tree.root();
        // 根（无父）bolder → 基 400 → 700。
        let a = tree.insert_child(root, node("div", &["a"], ""));
        let a_style = compute_node(&tree, a, &s, &MediaEnv::default(), None);
        assert_eq!(a_style.font_weight(), 700.0, "根级 bolder = 700");
        // 700 → bolder → 900 → bolder → 900（不变）。
        let b = tree.insert_child(a, node("div", &["a"], ""));
        let b_style = compute_node(&tree, b, &s, &MediaEnv::default(), Some(&a_style));
        assert_eq!(b_style.font_weight(), 900.0);
        let c = tree.insert_child(b, node("div", &["a"], ""));
        let c_style = compute_node(&tree, c, &s, &MediaEnv::default(), Some(&b_style));
        assert_eq!(c_style.font_weight(), 900.0, "900 bolder 不变");
        // lighter 链：550 → 400 → 100 → 100。
        let mut w550 = a_style.clone();
        w550.values[PropertyId::FontWeight.slot()] = Some(DeclValue::Number(550.0));
        let l1 = tree.insert_child(root, node("div", &["b"], ""));
        let l1_style = compute_node(&tree, l1, &s, &MediaEnv::default(), Some(&w550));
        assert_eq!(l1_style.font_weight(), 400.0);
        let l2 = tree.insert_child(l1, node("div", &["b"], ""));
        let l2_style = compute_node(&tree, l2, &s, &MediaEnv::default(), Some(&l1_style));
        assert_eq!(l2_style.font_weight(), 100.0);
        // 父 100 → lighter → 100（100..350 档 → 100，不变）。
        let p = tree.insert_child(root, node("div", &["p"], ""));
        let p_style = compute_node(&tree, p, &s, &MediaEnv::default(), None);
        assert_eq!(p_style.font_weight(), 100.0);
        let q = tree.insert_child(p, node("div", &["q"], ""));
        let q_style = compute_node(&tree, q, &s, &MediaEnv::default(), Some(&p_style));
        assert_eq!(q_style.font_weight(), 100.0);
    }

    #[test]
    fn font_weight_number_still_absolute_and_rejects_garbage() {
        // 尾随多余组件 → 声明非法（裸声明毒化后续规则为已知 StyleSheetParser
        // 行为，故单独解析该条）。
        let bad = crate::css::stylesheet::parse_stylesheet("b { font-weight: bolder lighter }");
        assert!(!bad.report.is_clean(), "bolder lighter 应为非法声明");
        let s = sheet("a { font-weight: 550 }");
        let mut tree = StyleTree::new();
        let root = tree.root();
        let n = tree.insert_child(root, node("a", &[], ""));
        let style = compute_node(&tree, n, &s, &MediaEnv::default(), None);
        assert_eq!(style.font_weight(), 550.0, "数字权重保持绝对语义");
    }

    #[test]
    fn relative_font_size_table_steps_and_ratio_fallback() {
        use crate::css::property::{ABSOLUTE_FONT_SIZES_PX, relative_font_size as rfs};
        // 表命中：步进一格。
        assert_eq!(rfs(16.0, true), 18.0); // medium larger → large
        assert_eq!(rfs(18.0, true), 24.0);
        assert_eq!(rfs(16.0, false), 13.0); // medium smaller → small
        assert_eq!(rfs(13.0, false), 10.0);
        // 端点钳制。
        assert_eq!(rfs(9.0, false), 9.0, "xx-small smaller 不变");
        assert_eq!(rfs(48.0, true), 48.0, "xxx-large larger 不变");
        // 非表值：1.2 比例。
        assert!((rfs(20.0, true) - 24.0).abs() < 1e-4);
        assert!((rfs(20.0, false) - 20.0 / 1.2).abs() < 1e-4);
        // 表值完整性（与 parse_font_size 物化同源）。
        assert_eq!(ABSOLUTE_FONT_SIZES_PX.len(), 8);
    }

    #[test]
    fn larger_smaller_parse_and_materialize_chain() {
        let s = sheet(
            ".a { font-size: larger } .b { font-size: smaller } \
             .p { font-size: 100px } .q { font-size: larger }",
        );
        let fs = |st: &ComputedStyle| match st.values[PropertyId::FontSize.slot()].clone() {
            Some(DeclValue::Len(LengthPercentage::Px(px))) => px,
            other => panic!("font-size 应为绝对 px，实得 {:?}", other.map(|_| ())),
        };
        let mut tree = StyleTree::new();
        let root = tree.root();
        // 根（无父）larger → 基 16（表命中）→ 18。
        let a = tree.insert_child(root, node("div", &["a"], ""));
        let a_style = compute_node(&tree, a, &s, &MediaEnv::default(), None);
        assert_eq!(fs(&a_style), 18.0, "根级 larger = large(18)");
        // 18 → larger → 24 → larger → 32。
        let b = tree.insert_child(a, node("div", &["a"], ""));
        let b_style = compute_node(&tree, b, &s, &MediaEnv::default(), Some(&a_style));
        assert_eq!(fs(&b_style), 24.0);
        let c = tree.insert_child(b, node("div", &["a"], ""));
        let c_style = compute_node(&tree, c, &s, &MediaEnv::default(), Some(&b_style));
        assert_eq!(fs(&c_style), 32.0);
        // smaller 链：16 → 13 → 10。
        let r = tree.insert_child(root, node("div", &["b"], ""));
        let r_style = compute_node(&tree, r, &s, &MediaEnv::default(), None);
        assert_eq!(fs(&r_style), 13.0);
        let r2 = tree.insert_child(r, node("div", &["b"], ""));
        let r2_style = compute_node(&tree, r2, &s, &MediaEnv::default(), Some(&r_style));
        assert_eq!(fs(&r2_style), 10.0);
        // 非表值父（100px）：larger → 1.2 比例 → 120。
        let p = tree.insert_child(root, node("div", &["p"], ""));
        let p_style = compute_node(&tree, p, &s, &MediaEnv::default(), None);
        assert_eq!(fs(&p_style), 100.0);
        let q = tree.insert_child(p, node("div", &["q"], ""));
        let q_style = compute_node(&tree, q, &s, &MediaEnv::default(), Some(&p_style));
        assert!(
            (fs(&q_style) - 120.0).abs() < 1e-4,
            "100px larger = 1.2 比例"
        );
    }

    #[test]
    fn font_size_number_and_keyword_still_absolute() {
        // 绝对关键字物化与旧值一致（表重构后不漂移）+ 尾随垃圾仍非法。
        let s = sheet("a { font-size: xx-small } b { font-size: xxx-large }");
        let mut tree = StyleTree::new();
        let root = tree.root();
        let n = tree.insert_child(root, node("a", &[], ""));
        let style = compute_node(&tree, n, &s, &MediaEnv::default(), None);
        let px = match style.values[PropertyId::FontSize.slot()].clone() {
            Some(DeclValue::Len(LengthPercentage::Px(v))) => v,
            _ => panic!(),
        };
        assert_eq!(px, 9.0);
        let bad = crate::css::stylesheet::parse_stylesheet("a { font-size: larger smaller }");
        assert!(!bad.report.is_clean(), "larger smaller 应为非法声明");
    }
}
