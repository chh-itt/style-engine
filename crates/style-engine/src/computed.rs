//! 计算样式：级联胜出 → var() 代换 → 继承/初始值（L1 语义终点）。
//!
//! 流程（ADR-0003/ADR-0004）：
//! 1. custom properties：继承值为底、胜出原始值覆盖，var() 链按需解析
//!    （环检测；失败 = guaranteed-invalid，该名缺席）；
//! 2. 胜出声明：`Parsed` 直取；`Var` 代换后重解析，失败走 IACVT
//!    （继承属性取父值，否则初始值）；
//! 3. 全集物化：64 个属性逐一填充（父继承或初始值），供 T3/T4 直接读取；
//! 4. 字号解析：em 相对父字号，物化为绝对 px。

use crate::cascade::{ContainerCtx, cascade_declarations};
use crate::css::decl::{DeclSource, token_buf_to_string};
use crate::css::property::{
    Align, AnimDirection, AnimFillMode, BackgroundImage, ContainerType, CursorKind, DeclValue,
    Display, FamilyName, FlexDirection, FlexWrap, FontFamilyList, FontStyle, GridAutoFlowKind,
    GridTemplate, LineHeight, OutlineStyle, Overflow, PointerEventsKind, Position, PropertyId,
    TextAlign, TimingFn, UserSelectKind, WhiteSpace,
};
use crate::css::stylesheet::{MediaEnv, Stylesheet};
use crate::css::value::{ColorValue, LengthPercentage, ResolveCtx};
use crate::tree::{NodeId, StyleTree};
use peniko::color::AlphaColor;
use smallvec::{SmallVec, smallvec};
use std::collections::BTreeMap;

/// 单节点计算样式（全集物化）。
#[derive(Debug, Clone, PartialEq)]
#[must_use = "计算样式被丢弃则该次级联求解无意义"]
pub struct ComputedStyle {
    /// 全集槽位存储：下标 = [`PropertyId::slot()`]（0..94），None = 未物化。
    /// 槽位 O(1) 下标写替代 BTreeMap 的 log n 走查 + 节点分配——全集物化
    /// 每节点 ~94 次插入曾是 restyle 成本主体（阶段5 归因，PERFORMANCE.md）。
    values: Vec<Option<DeclValue>>,
    /// 已解析 custom properties（终值文本）。
    custom: BTreeMap<String, String>,
    /// 字体相对单位度量（A9：ch/ex/ic 基准，每 em；restyle 期由引擎按
    /// font-family 首族补写，缺省近似值——未注册族 ch/ex=0.5em、ic=1em）。
    font_metrics: crate::css::value::FontMetrics,
    /// 伪元素标记（C1/ADR-0015）：Some = 本样式属于引擎实体化的伪节点
    ///（map_style 据此在 content none/normal 时映射 Display::None）。
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
    /// 动画覆盖（第五批⑰）：级联后按关键帧采样覆写单个属性。
    pub fn set_value(&mut self, id: PropertyId, v: DeclValue) {
        self.values[id.slot()] = Some(v);
    }

    /// 读槽位值（宿主可读契约，ADR-0003 公有 API 面内）：
    /// `None` = 该属性未在级联中显式出现，保持初始/继承缺省语义。
    /// 行为提示属性（cursor/user-select 等宿主消费通道）的读取入口。
    pub fn value(&self, id: PropertyId) -> Option<&DeclValue> {
        self.values[id.slot()].as_ref()
    }

    /// 读已解析 custom property 终值（var() 代换完成后；宿主可读契约）。
    /// 键为含 `--` 前缀的自定义属性名。
    pub fn custom_value(&self, name: &str) -> Option<&str> {
        self.custom.get(name).map(|s| s.as_str())
    }

    /// 字体相对单位度量（A9；引擎 restyle 期按 font-family 补写）。
    pub fn font_metrics(&self) -> &crate::css::value::FontMetrics {
        &self.font_metrics
    }

    /// 字体度量写入（crate 内部：engine restyle 期调用）。
    pub(crate) fn set_font_metrics(&mut self, m: crate::css::value::FontMetrics) {
        self.font_metrics = m;
    }

    /// content 计算值（C1）：宿主节点恒 Normal（content 仅作用于伪元素）；
    /// 伪节点由 map_style 消费（none/normal → 无盒）。
    pub fn content(&self) -> crate::css::property::ContentValue {
        match self.values[PropertyId::Content.slot()].as_ref() {
            Some(DeclValue::Content(c)) => c.clone(),
            _ => crate::css::property::ContentValue::Normal,
        }
    }

    /// 伪元素标记读取（C1）。
    pub fn pseudo(&self) -> Option<crate::tree::PseudoWhich> {
        self.pseudo
    }

    /// 伪元素标记写入（crate 内部：compute 期自树节点同步）。
    pub(crate) fn set_pseudo(&mut self, p: Option<crate::tree::PseudoWhich>) {
        self.pseudo = p;
    }

    /// text-transform 计算值（C2；继承）。
    pub fn text_transform(&self) -> crate::css::property::TextTransformKind {
        match self.values[PropertyId::TextTransform.slot()].as_ref() {
            Some(DeclValue::TextTransform(k)) => *k,
            _ => crate::css::property::TextTransformKind::None,
        }
    }

    /// overflow-wrap 计算值（C2；不继承）。
    pub fn overflow_wrap(&self) -> crate::css::property::OverflowWrapKind {
        match self.values[PropertyId::OverflowWrap.slot()].as_ref() {
            Some(DeclValue::OverflowWrap(k)) => *k,
            _ => crate::css::property::OverflowWrapKind::Normal,
        }
    }

    /// word-break 计算值（C2；不继承）。
    pub fn word_break(&self) -> crate::css::property::WordBreakKind {
        match self.values[PropertyId::WordBreak.slot()].as_ref() {
            Some(DeclValue::WordBreak(k)) => *k,
            _ => crate::css::property::WordBreakKind::Normal,
        }
    }

    /// object-fit 计算值（C3；不继承）。
    pub fn object_fit(&self) -> crate::css::property::ObjectFitKind {
        match self.values[PropertyId::ObjectFit.slot()].as_ref() {
            Some(DeclValue::ObjectFit(k)) => *k,
            _ => crate::css::property::ObjectFitKind::Fill,
        }
    }

    /// object-position 计算值 (x, y)（C3；不继承；初始中心）。
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

    /// text-overflow 计算值（F2，ADR-0022 D2；不继承）。
    pub fn text_overflow(&self) -> crate::css::property::TextOverflowKind {
        match self.values[PropertyId::TextOverflow.slot()].as_ref() {
            Some(DeclValue::TextOverflow(k)) => *k,
            _ => crate::css::property::TextOverflowKind::Clip,
        }
    }

    /// -webkit-line-clamp 行数（F2，ADR-0022 D3；0=none；不继承）。
    pub fn webkit_line_clamp(&self) -> u32 {
        match self.values[PropertyId::WebkitLineClamp.slot()].as_ref() {
            Some(DeclValue::WebkitLineClamp(n)) => *n,
            _ => 0,
        }
    }

    /// text-decoration-line 位集（F2，ADR-0022 D4；1=underline 2=overline
    /// 4=line-through；不继承）。
    pub fn text_decoration_line(&self) -> u8 {
        match self.values[PropertyId::TextDecorationLine.slot()].as_ref() {
            Some(DeclValue::TextDecorationLine(b)) => *b,
            _ => 0,
        }
    }

    /// text-decoration-style（F2，ADR-0022 D4；不继承）。
    pub fn text_decoration_style(&self) -> crate::css::property::TextDecoStyleKind {
        match self.values[PropertyId::TextDecorationStyle.slot()].as_ref() {
            Some(DeclValue::TextDecorationStyle(k)) => *k,
            _ => crate::css::property::TextDecoStyleKind::Solid,
        }
    }

    /// text-decoration-color（F2，ADR-0022 D4；缺省 currentColor；不继承）。
    pub fn text_decoration_color(&self) -> ColorValue {
        match self.values[PropertyId::TextDecorationColor.slot()].as_ref() {
            Some(DeclValue::Color(cv)) => *cv,
            _ => ColorValue::CurrentColor,
        }
    }

    /// text-decoration-thickness（F2，ADR-0022 D4；不继承）。
    pub fn text_decoration_thickness(&self) -> crate::css::property::TextDecoThickness {
        match self.values[PropertyId::TextDecorationThickness.slot()].as_ref() {
            Some(DeclValue::TextDecorationThickness(t)) => t.clone(),
            _ => crate::css::property::TextDecoThickness::Auto,
        }
    }

    /// text-shadow 影列表（F2，ADR-0022 D5；空=none；继承）。
    pub fn text_shadows(&self) -> Vec<crate::css::property::TextShadowSpec> {
        match self.values[PropertyId::TextShadow.slot()].as_ref() {
            Some(DeclValue::TextShadow(v)) => v.clone(),
            _ => Vec::new(),
        }
    }

    /// float 计算值（E4，css-position-3 / ADR-0019；不继承）。
    pub fn float(&self) -> crate::css::property::FloatKind {
        match self.values[PropertyId::Float.slot()].as_ref() {
            Some(DeclValue::Float(k)) => *k,
            _ => crate::css::property::FloatKind::None,
        }
    }

    /// clear 计算值（E4，css-position-3 / ADR-0019；不继承）。
    pub fn clear(&self) -> crate::css::property::ClearKind {
        match self.values[PropertyId::Clear.slot()].as_ref() {
            Some(DeclValue::Clear(k)) => *k,
            _ => crate::css::property::ClearKind::None,
        }
    }

    /// border-image-source 计算值（F3d，ADR-0026 D1；不继承）。
    pub fn border_image_source(&self) -> crate::css::property::BackgroundImage {
        match self.values[PropertyId::BorderImageSource.slot()].as_ref() {
            Some(DeclValue::BorderImageSource(v)) => v.clone(),
            _ => crate::css::property::BackgroundImage::None,
        }
    }

    /// border-image-slice 计算值（F3d，ADR-0026 D1；不继承）。
    pub fn border_image_slice(&self) -> crate::css::property::BorderImageSlice {
        match self.values[PropertyId::BorderImageSlice.slot()].as_ref() {
            Some(DeclValue::BorderImageSlice(v)) => *v,
            _ => crate::css::property::BorderImageSlice {
                slices: [crate::css::property::BorderImageSliceComp::Percentage(100.0); 4],
                fill: false,
            },
        }
    }

    /// border-image-width 计算值（F3d，ADR-0026 D1；不继承）。
    pub fn border_image_width(&self) -> crate::css::property::BorderImageWidth {
        match self.values[PropertyId::BorderImageWidth.slot()].as_ref() {
            Some(DeclValue::BorderImageWidth(v)) => v.clone(),
            _ => crate::css::property::BorderImageWidth {
                comps: std::array::from_fn(|_| crate::css::property::BorderImageWidthComp::Auto),
            },
        }
    }

    /// border-image-outset 计算值（F3d，ADR-0026 D1；不继承）。
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

    /// border-image-repeat 计算值（F3d，ADR-0026 D1；不继承）。
    pub fn border_image_repeat(&self) -> crate::css::property::BorderImageRepeatXY {
        match self.values[PropertyId::BorderImageRepeat.slot()].as_ref() {
            Some(DeclValue::BorderImageRepeat(v)) => *v,
            _ => crate::css::property::BorderImageRepeatXY {
                x: crate::css::property::BorderImageRepeatKind::Stretch,
                y: crate::css::property::BorderImageRepeatKind::Stretch,
            },
        }
    }

    /// font-stretch 计算值（F3d，ADR-0026 D3；继承；归一百分比
    /// 50..=200，100=normal）。
    pub fn font_stretch(&self) -> f32 {
        match self.values[PropertyId::FontStretch.slot()].as_ref() {
            Some(DeclValue::FontStretch(v)) => *v,
            _ => 100.0,
        }
    }

    /// word-spacing 计算值（F3d，ADR-0026 D3；继承；None=normal=0，
    /// 百分比基=font-size，解析期后物化）。
    pub fn word_spacing(&self) -> Option<LengthPercentage> {
        match self.values[PropertyId::WordSpacing.slot()].as_ref() {
            Some(DeclValue::LenAuto(v)) => v.clone(),
            _ => None,
        }
    }

    /// font-feature-settings 计算值（F3d，ADR-0026 D3；继承；
    /// OpenType tag + value 对列表）。
    pub fn font_features(&self) -> Vec<([u8; 4], u16)> {
        match self.values[PropertyId::FontFeatures.slot()].as_ref() {
            Some(DeclValue::FontFeatures(v)) => v.clone(),
            _ => Vec::new(),
        }
    }

    /// font-variation-settings 计算值（F3d，ADR-0026 D3；继承；
    /// 轴 tag + value 对列表）。
    pub fn font_variations(&self) -> Vec<([u8; 4], f32)> {
        match self.values[PropertyId::FontVariations.slot()].as_ref() {
            Some(DeclValue::FontVariations(v)) => v.clone(),
            _ => Vec::new(),
        }
    }

    /// font-variant-caps 计算值（F3d，ADR-0026 D3；继承）。
    pub fn font_variant_caps(&self) -> crate::css::property::FontVariantCapsKind {
        match self.values[PropertyId::FontVariantCaps.slot()].as_ref() {
            Some(DeclValue::FontVariantCaps(k)) => *k,
            _ => crate::css::property::FontVariantCapsKind::Normal,
        }
    }
}

/// 属性继承性（CSS 级联继承语义：文本/字体类继承，盒模型不继承）。
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
    )
}

/// 属性初始值（CSS initial；引擎偏差处注明）。
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
        // filter/will-change/isolation/mix-blend-mode/backdrop-filter 初始
        // 缺席（第四批④ + 第五批㉒ + F4：仅存在性语义位触发 SC）；
        // clip-path 已升级为形状（F3c，ADR-0025，初始 none）。
        P::Filter | P::WillChange | P::Isolation | P::MixBlendMode | P::BackdropFilter => {
            DeclValue::Effect(false)
        }
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

/// custom property 代换器：继承终值为底、胜出原始值覆盖，按需解析 + 环检测。
struct CustomResolver<'a> {
    inherited: &'a BTreeMap<String, String>,
    own_raw: BTreeMap<String, String>,
    /// B4：注册属性表（@property 语法门/initial 回退判据）。
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
        stack.push(name.to_string());
        let resolved = self.substitute(&raw, stack);
        stack.pop();
        // B4：注册属性语法门——终值不匹配 syntax → unset → initial-value
        //（Chrome 一致：var(--x) 解析到 initial 而非触发 fallback；Named
        // 注册必有 initial（注册有效性保证），门失败恒有回值）。
        let resolved = resolved.and_then(|t| match self.registered.get(name) {
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

    /// 代换 raw 中所有 var() 引用；None = guaranteed-invalid。
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

/// 从 "var(" 之后扫描到匹配 ')'；返回 (参数串, 总消耗长度)。
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

/// 首个顶层逗号分隔参数与 fallback。
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
    /// 读取属性计算值（无则 None）。
    pub fn get(&self, id: PropertyId) -> Option<&DeclValue> {
        self.values[id.slot()].as_ref()
    }

    /// 读取已解析 custom property 终值文本（guaranteed-invalid → None）。
    pub fn custom(&self, name: &str) -> Option<&str> {
        self.custom.get(name).map(String::as_str)
    }

    /// display（默认 block）。
    pub fn display(&self) -> Display {
        match self.values[PropertyId::Display.slot()].as_ref() {
            Some(DeclValue::Display(d)) => *d,
            _ => Display::Block,
        }
    }

    /// position（默认 static）。
    pub fn position(&self) -> Position {
        match self.values[PropertyId::Position.slot()].as_ref() {
            Some(DeclValue::Position(p)) => *p,
            _ => Position::Static,
        }
    }

    /// box-sizing（默认 content-box；映射到 taffy 时换算 size 语义）。
    pub fn box_sizing(&self) -> crate::css::property::BoxSizing {
        match self.values[PropertyId::BoxSizing.slot()].as_ref() {
            Some(DeclValue::BoxSizing(b)) => *b,
            _ => crate::css::property::BoxSizing::ContentBox,
        }
    }

    /// transform 函数列表（空 = none）。
    pub fn transform(&self) -> &[crate::css::property::TransformFn] {
        match self.values[PropertyId::Transform.slot()].as_ref() {
            Some(DeclValue::Transform(list)) => list,
            _ => &[],
        }
    }

    /// transform ≠ none（ADR-0009 判定谓词单点：L2 cb 语义位与 L3 SC 触发共享）。
    pub fn has_transform(&self) -> bool {
        !self.transform().is_empty()
    }

    /// filter 存在性（第四批④：仅 SC 触发语义位，无滤镜效果实现）。
    pub fn has_filter(&self) -> bool {
        matches!(self.get(PropertyId::Filter), Some(DeclValue::Effect(true)))
    }

    /// backdrop-filter 存在性（F4，ADR-0028 D1：非 none 触发 SC；效果
    /// 本体不在范围，同 filter 先例）。
    pub fn has_backdrop_filter(&self) -> bool {
        matches!(
            self.get(PropertyId::BackdropFilter),
            Some(DeclValue::Effect(true))
        )
    }

    /// hyphens 连字符断字模式（F4，ADR-0028 D2；initial=manual；断词
    /// 效果受上游分段器边界，三值 v1 行为一致）。
    pub fn hyphens(&self) -> crate::css::property::HyphensKind {
        match self.values[PropertyId::Hyphens.slot()].as_ref() {
            Some(DeclValue::Hyphens(k)) => *k,
            _ => crate::css::property::HyphensKind::Manual,
        }
    }

    /// clip-path 存在性（第四批④ SC 位；F3c 升级：形状 ≠ none 即触发，
    /// ADR-0025）。
    pub fn has_clip_path(&self) -> bool {
        !matches!(self.clip_path(), crate::css::property::ClipShape::None)
    }

    /// will-change 含可触发 SC 的属性（第五批㉒ SC 触发全集）。
    pub fn has_will_change_sc(&self) -> bool {
        matches!(
            self.get(PropertyId::WillChange),
            Some(DeclValue::Effect(true))
        )
    }

    /// isolation: isolate（第五批㉒）。
    pub fn has_isolation(&self) -> bool {
        matches!(
            self.get(PropertyId::Isolation),
            Some(DeclValue::Effect(true))
        )
    }

    /// mix-blend-mode 计算值（P1-2）：缺席或 normal →
    /// [`BlendMode::Normal`]。
    pub fn mix_blend(&self) -> crate::css::property::BlendMode {
        match self.get(PropertyId::MixBlendMode) {
            Some(crate::css::property::DeclValue::BlendMode(m)) => *m,
            _ => crate::css::property::BlendMode::Normal,
        }
    }

    /// mix-blend-mode ≠ normal（第五批㉒ 起 SC 触发；P1-2 起携带效果）。
    pub fn has_mix_blend(&self) -> bool {
        self.mix_blend() != crate::css::property::BlendMode::Normal
    }

    /// overflow-x（默认 visible）。
    pub fn overflow_x(&self) -> Overflow {
        match self.values[PropertyId::OverflowX.slot()].as_ref() {
            Some(DeclValue::Overflow(o)) => *o,
            _ => Overflow::Visible,
        }
    }

    /// overflow-y（默认 visible）。
    pub fn overflow_y(&self) -> Overflow {
        match self.values[PropertyId::OverflowY.slot()].as_ref() {
            Some(DeclValue::Overflow(o)) => *o,
            _ => Overflow::Visible,
        }
    }

    /// flex-direction（默认 row）。
    pub fn flex_direction(&self) -> FlexDirection {
        match self.values[PropertyId::FlexDirection.slot()].as_ref() {
            Some(DeclValue::FlexDirection(d)) => *d,
            _ => FlexDirection::Row,
        }
    }

    /// flex-wrap（默认 nowrap）。
    pub fn flex_wrap(&self) -> FlexWrap {
        match self.values[PropertyId::FlexWrap.slot()].as_ref() {
            Some(DeclValue::FlexWrap(w)) => *w,
            _ => FlexWrap::NoWrap,
        }
    }

    /// LenAuto 族读取：auto → None。
    pub fn len_auto(&self, id: PropertyId) -> Option<&LengthPercentage> {
        match self.values[id.slot()].as_ref() {
            Some(DeclValue::LenAuto(Some(lp))) => Some(lp),
            _ => None,
        }
    }

    /// Len 族读取。
    pub fn len(&self, id: PropertyId) -> Option<&LengthPercentage> {
        match self.values[id.slot()].as_ref() {
            Some(DeclValue::Len(lp)) => Some(lp),
            _ => None,
        }
    }

    /// 四边 margin（top, right, bottom, left；auto → None）。
    pub fn margin(&self) -> [Option<&LengthPercentage>; 4] {
        [
            self.len_auto(PropertyId::MarginTop),
            self.len_auto(PropertyId::MarginRight),
            self.len_auto(PropertyId::MarginBottom),
            self.len_auto(PropertyId::MarginLeft),
        ]
    }

    /// 四边 padding（top, right, bottom, left）。
    pub fn padding(&self) -> [Option<&LengthPercentage>; 4] {
        [
            self.len(PropertyId::PaddingTop),
            self.len(PropertyId::PaddingRight),
            self.len(PropertyId::PaddingBottom),
            self.len(PropertyId::PaddingLeft),
        ]
    }

    /// color（默认不透黑）。
    pub fn color(&self) -> ColorValue {
        match self.values[PropertyId::Color.slot()].as_ref() {
            Some(DeclValue::Color(c)) => *c,
            _ => ColorValue::Absolute(AlphaColor::new([0.0, 0.0, 0.0, 1.0])),
        }
    }

    /// background-color（默认透明）。
    /// F3b（ADR-0024）：背景层对齐视图——层数 = max(各长手层数, 1)，
    /// 短列表 cycling 补齐（css-backgrounds-3 §3）；缺省长手以初始值参与。
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

    /// background-color — 背景色计算值（ColorValue 原样；绘制期经
    /// resolve_color 环境化）。
    pub fn background_color(&self) -> ColorValue {
        match self.values[PropertyId::BackgroundColor.slot()].as_ref() {
            Some(DeclValue::Color(c)) => *c,
            _ => ColorValue::Absolute(AlphaColor::new([0.0, 0.0, 0.0, 0.0])),
        }
    }

    /// clip-path — 裁剪形状（F3c，ADR-0025；初始 none）。
    pub fn clip_path(&self) -> crate::css::property::ClipShape {
        match self.values[PropertyId::ClipPath.slot()].as_ref() {
            Some(DeclValue::ClipPath(s)) => s.clone(),
            _ => crate::css::property::ClipShape::None,
        }
    }

    /// 已解析的绝对字号（px）。
    pub fn font_size_px(&self) -> f32 {
        match self.values[PropertyId::FontSize.slot()].as_ref() {
            Some(DeclValue::Len(LengthPercentage::Px(v))) => *v,
            _ => 16.0,
        }
    }

    /// font-weight（默认 400）。
    pub fn font_weight(&self) -> f32 {
        match self.values[PropertyId::FontWeight.slot()].as_ref() {
            Some(DeclValue::Number(n)) => *n,
            _ => 400.0,
        }
    }

    /// font-style（默认 normal）。
    pub fn font_style(&self) -> FontStyle {
        match self.values[PropertyId::FontStyle.slot()].as_ref() {
            Some(DeclValue::FontStyle(s)) => *s,
            _ => FontStyle::Normal,
        }
    }

    /// font-family 列表（全集物化保证存在）。
    pub fn font_family(&self) -> &FontFamilyList {
        match self.values[PropertyId::FontFamily.slot()].as_ref() {
            Some(DeclValue::FontFamily(list)) => list,
            _ => unreachable!("font family is materialized"),
        }
    }

    /// line-height（全集物化保证存在）。
    pub fn line_height(&self) -> &LineHeight {
        match self.values[PropertyId::LineHeight.slot()].as_ref() {
            Some(DeclValue::LineHeight(lh)) => lh,
            _ => unreachable!("line-height is materialized"),
        }
    }

    /// text-align（默认 start）。
    pub fn text_align(&self) -> TextAlign {
        match self.values[PropertyId::TextAlign.slot()].as_ref() {
            Some(DeclValue::TextAlign(a)) => *a,
            _ => TextAlign::Start,
        }
    }

    /// 行高解析为 px（None = normal，消费侧走排版器默认字体度量 ≈ CSS normal）。
    /// 百分比基 = 本元素字号（CSS line-height 语义）；盘点修复：此前该属性
    /// 已解析入库但无任何消费者（无效声明）。
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

    /// 字距解析为 px（letter-spacing 百分比基 = 字号）。
    /// 解析产物为 LenAuto（normal → None）；initial 曾为 Len(Px(0.0))，
    /// 与解析类型不一致——盘点修复为同型 LenAuto(None)。
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

    /// 词距解析为 px（F3d，ADR-0026 D5；word-spacing 百分比基 = 字号，
    /// 与 letter-spacing 同范式）。None = normal = 无消费（parley 默认 0）。
    pub(crate) fn resolved_word_spacing_px(&self, env: &MediaEnv) -> Option<f32> {
        let fs = self.font_size_px();
        // P0 rem 修复：rem 基准 = MediaEnv.rem（引擎接线文档根字号）。
        let ctx = ResolveCtx::base(fs, env.rem, env.viewport_w, env.viewport_h);
        self.word_spacing()?.resolve(&ctx, fs)
    }

    /// 生效 font-feature-settings（F3d，ADR-0026 D5）：显式 feature 列表
    /// 并上 font-variant-caps 派生（TitlingCaps→'titl'、Unicase→'unic'，
    /// CSS Fonts 4 语义）；同 tag 冲突时显式 feature-settings 优先
    /// （派生 tag 不重复才推）。
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

    /// white-space（默认 normal）。
    pub fn white_space(&self) -> WhiteSpace {
        match self.values[PropertyId::WhiteSpace.slot()].as_ref() {
            Some(DeclValue::WhiteSpace(w)) => *w,
            _ => WhiteSpace::Normal,
        }
    }

    /// direction（A8：逻辑→物理映射基准；默认 ltr）。
    pub fn direction(&self) -> crate::css::property::DirectionKind {
        match self.values[PropertyId::Direction.slot()].as_ref() {
            Some(DeclValue::Direction(d)) => *d,
            _ => crate::css::property::DirectionKind::Ltr,
        }
    }

    /// unicode-bidi（A8：文本栈提示；默认 normal）。
    pub fn unicode_bidi(&self) -> crate::css::property::UnicodeBidiKind {
        match self.values[PropertyId::UnicodeBidi.slot()].as_ref() {
            Some(DeclValue::UnicodeBidi(b)) => *b,
            _ => crate::css::property::UnicodeBidiKind::Normal,
        }
    }

    /// opacity（默认 1.0）。
    pub fn opacity(&self) -> f32 {
        match self.values[PropertyId::Opacity.slot()].as_ref() {
            Some(DeclValue::Number(n)) => *n,
            _ => 1.0,
        }
    }

    /// z-index 数值（auto 或缺席取 0.0）。
    pub fn z_index(&self) -> f32 {
        match self.values[PropertyId::ZIndex.slot()].as_ref() {
            Some(DeclValue::Number(n)) => *n,
            _ => 0.0,
        }
    }

    /// container-type（阶段2③；默认 Normal）。
    pub fn container_type(&self) -> ContainerType {
        match self.values[PropertyId::ContainerType.slot()].as_ref() {
            Some(DeclValue::ContainerType(t)) => *t,
            _ => ContainerType::Normal,
        }
    }

    /// container-name 名单（阶段2③）。
    pub fn container_names(&self) -> &[String] {
        match self.values[PropertyId::ContainerName.slot()].as_ref() {
            Some(DeclValue::ContainerName(list)) => list,
            _ => &[],
        }
    }

    /// 调试人读视图（F3e，ADR-0027 D1）：显式物化槽位（`PropertyId::ALL`
    /// 序，`css_name: {Debug}` 一行一个；None 槽位 = 未显式出现在级联中，
    /// 不输出）+ custom properties（`--name: value`，字典序）+ 字体度量 +
    /// 伪元素标记。宿主排查「为什么画错/为什么布局不对」的第一入口；
    /// 机器往返通道见 `paint_dump`（serde feature）。零新状态、零分支语义
    /// ——纯投影，不参与级联/结算任何路径。
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

/// 计算单节点样式（自根向下逐层调用；`parent` 为父节点计算样式）。
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
    )
}

/// 带容器快照的计算（阶段2③）：`container_ctx` 为祖先容器栈（restyle
/// DFS 维护，自最外向最内；无 @container 时零长度零成本）。B1：`user_sheet`
/// 为用户起源样式表（None = 无）。B2：`sheets` 为 author 表组按值（主表
/// 在前、附加表按登记序 = 文档序）。B4：`registered` 为文档级 @property
/// 注册表（引擎附着期合并；独立 compute_node 无注册表 = 空）。
#[allow(clippy::too_many_arguments)] // 公开 API 形状保持（B2 user_sheet/B4 registered 扩展位）
pub fn compute_node_in<'a>(
    tree: &'a StyleTree,
    id: NodeId,
    sheets: Vec<&'a Stylesheet>,
    user_sheet: Option<&'a Stylesheet>,
    registered: &BTreeMap<String, crate::css::property_rule::PropertyRule>,
    env: &MediaEnv,
    parent: Option<&ComputedStyle>,
    container_ctx: &[ContainerCtx],
) -> ComputedStyle {
    let cascaded = cascade_declarations(tree, id, sheets, user_sheet, env, container_ctx);
    compute_node_from_cascade(tree, id, cascaded, registered, env, parent)
}

/// C4（ADR-0018）：从既有级联输出计算节点样式——compute_node_in 的主体
/// 抽取（公开签名零改），供通道级联（::selection/::placeholder）复用；
/// parent 语义不变（通道调用传 origin 主样式 = css-pseudo-4 继承基）。
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
}
