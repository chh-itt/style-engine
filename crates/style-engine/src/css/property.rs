//! 属性清单与声明解析：T0 属性集（FEATURES.md）、按值族复用文法。
//!
//! PropertyId 是声明块与 ComputedStyle 之间的稳定词汇表；`PropertyId`
//! 到 CSS 属性名的映射一一对应（解析大小写不敏感，这里统一小写）。

use crate::css::value::{
    Angle, ColorValue, LengthPercentage, ValResult, parse_color_value, parse_length_percentage,
    parse_number,
};
use cssparser::{Parser, Token, match_ignore_ascii_case};
use smallvec::SmallVec;

/// T0 属性（FEATURES.md 语法层清单）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PropertyId {
    // 布局：显示与定位
    Display,
    Position,
    Top,
    Right,
    Bottom,
    Left,
    ZIndex,
    // 布局：盒子
    Width,
    Height,
    MinWidth,
    MinHeight,
    MaxWidth,
    MaxHeight,
    AspectRatio,
    // 布局：盒间距
    MarginTop,
    MarginRight,
    MarginBottom,
    MarginLeft,
    PaddingTop,
    PaddingRight,
    PaddingBottom,
    PaddingLeft,
    Gap,
    RowGap,
    ColumnGap,
    // 布局：flex
    FlexDirection,
    FlexWrap,
    FlexGrow,
    FlexShrink,
    FlexBasis,
    JustifyContent,
    AlignItems,
    AlignSelf,
    AlignContent,
    // 布局：grid
    GridTemplateColumns,
    GridTemplateRows,
    GridAutoFlow,
    GridAutoRows,
    GridAutoColumns,
    // 绘制
    BackgroundColor,
    BackgroundImage,
    BorderTopLeftRadius,
    BorderTopRightRadius,
    BorderBottomRightRadius,
    BorderBottomLeftRadius,
    BorderTopWidth,
    BorderRightWidth,
    BorderBottomWidth,
    BorderLeftWidth,
    BorderTopStyle,
    BorderRightStyle,
    BorderBottomStyle,
    BorderLeftStyle,
    BorderTopColor,
    BorderRightColor,
    BorderBottomColor,
    BorderLeftColor,
    BoxShadow,
    Opacity,
    OverflowX,
    OverflowY,
    // 文本
    Color,
    FontFamily,
    FontSize,
    FontWeight,
    FontStyle,
    LineHeight,
    TextAlign,
    WhiteSpace,
    LetterSpacing,
}

impl PropertyId {
    /// 全量清单（from_css_name 的线性扫描表）。
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
        Self::Color,
        Self::FontFamily,
        Self::FontSize,
        Self::FontWeight,
        Self::FontStyle,
        Self::LineHeight,
        Self::TextAlign,
        Self::WhiteSpace,
        Self::LetterSpacing,
    ];

    /// CSS 属性名（小写）。解析与诊断共用。
    pub fn css_name(self) -> &'static str {
        match self {
            Self::Display => "display",
            Self::Position => "position",
            Self::Top => "top",
            Self::Right => "right",
            Self::Bottom => "bottom",
            Self::Left => "left",
            Self::ZIndex => "z-index",
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
            Self::Color => "color",
            Self::FontFamily => "font-family",
            Self::FontSize => "font-size",
            Self::FontWeight => "font-weight",
            Self::FontStyle => "font-style",
            Self::LineHeight => "line-height",
            Self::TextAlign => "text-align",
            Self::WhiteSpace => "white-space",
            Self::LetterSpacing => "letter-spacing",
        }
    }

    /// CSS 属性名 → PropertyId（大小写不敏感）。简写名返回 None（由简写
    /// 展开器处理）。
    pub fn from_css_name(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        Self::ALL.iter().copied().find(|id| id.css_name() == lower)
    }
}

/// 值族：按文法形状复用的声明值表示（不含简写；简写在解析期展开）。
#[derive(Debug, Clone, PartialEq)]
pub enum DeclValue {
    /// margin/padding/gap 等（letter-spacing 的 normal → None → 0）。
    Len(LengthPercentage),
    /// width/height/min-*/max-*/flex-basis/top..left（auto → None）。
    LenAuto(Option<LengthPercentage>),
    /// opacity/flex-grow/flex-shrink/z-index/font-weight。
    Number(f32),
    /// color/background-color/border-*-color。
    Color(ColorValue),
    Display(Display),
    Position(Position),
    Overflow(Overflow),
    Align(Align),
    FlexDirection(FlexDirection),
    FlexWrap(FlexWrap),
    BorderStyle(BorderStyle),
    TextAlign(TextAlign),
    WhiteSpace(WhiteSpace),
    FontStyle(FontStyle),
    LineHeight(LineHeight),
    FontFamily(FontFamilyList),
    GridTracks(GridTemplate),
    /// aspect-ratio：none → None，否则宽/高比。
    AspectRatio(Option<f32>),
    BackgroundImage(BackgroundImage),
    BoxShadows(BoxShadowList),
    /// border-*-width：none → None（宽度归零）；thin/medium/thick → 定值。
    GridAutoFlow(GridAutoFlowKind),
    /// border-*-width：none → None（宽度归零）；thin/medium/thick → 定值。
    BorderWidth(Option<LengthPercentage>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridAutoFlowKind {
    Row,
    Column,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    /// taffy 无 inline formatting context；`inline*` 块化并告警（偏差已记录）。
    Block,
    Flex,
    Grid,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    Static,
    Relative,
    Absolute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overflow {
    Visible,
    Hidden,
    Clip,
    Scroll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Normal,
    Start,
    End,
    Center,
    Stretch,
    Baseline,
    FlexStart,
    FlexEnd,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlexDirection {
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlexWrap {
    NoWrap,
    Wrap,
    WrapReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderStyle {
    None,
    Solid,
    Dashed,
    Dotted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    Start,
    End,
    Center,
    Left,
    Right,
    /// MVP：按 Start 渲染（偏差记录于 FEATURES.md）。
    Justify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhiteSpace {
    Normal,
    NoWrap,
    Pre,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontStyle {
    Normal,
    Italic,
}

// LengthPercentage 含 Box（calc），非 Copy
#[derive(Debug, Clone, PartialEq)]
pub enum LineHeight {
    Normal,
    /// 无单位数字（倍数）。
    Number(f32),
    Len(LengthPercentage),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FontFamilyList(pub SmallVec<[FamilyName; 2]>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FamilyName {
    Named(String),
    Serif,
    SansSerif,
    Monospace,
    Cursive,
    Fantasy,
    SystemUi,
}

/// grid-template-columns/rows 的轨道列表（子集：定值轨道 + 固定次数 repeat）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GridTemplate {
    pub tracks: Vec<TrackSize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TrackSize {
    Len(LengthPercentage),
    Fr(f32),
    Auto,
    MaxContent,
    MinContent,
    MinMax(Box<TrackSize>, Box<TrackSize>),
    /// 固定次数 repeat（auto-fill/auto-fit 属 T1，见 FEATURES.md）。
    Repeat(u16, Vec<TrackSize>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum BackgroundImage {
    None,
    Url(String),
    Gradient(Gradient),
}

/// MVP 渐变：linear（角度/to 方向）与 radial（正圆、默认 farthest-corner）。
#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    pub kind: GradientKind,
    pub stops: Vec<ColorStop>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GradientKind {
    /// 角度归一为度；`to bottom`（默认）= 180deg。
    Linear(Angle),
    Radial,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColorStop {
    pub color: ColorValue,
    pub position: Option<LengthPercentage>,
}

/// 外阴影（MVP：不支持 inset；inset 声明整条容错丢弃）。
#[derive(Debug, Clone, PartialEq)]
pub struct BoxShadow {
    pub offset_x: LengthPercentage,
    pub offset_y: LengthPercentage,
    pub blur: LengthPercentage,
    pub spread: LengthPercentage,
    pub color: ColorValue,
}

pub type BoxShadowList = SmallVec<[BoxShadow; 2]>;

// ---------- 值族解析 ----------
//
// 惯用法：先 try_parse 探测关键字（Err 时状态回滚），再走通用值解析，
// 避免"peek 后重放"（cssparser 无法回退重放单 token）。

/// 解析一个关键字（大小写不敏感），映射失败即语法错误。
fn keyword<K>(p: &mut Parser<'_>, map: impl Fn(&str) -> Option<K>) -> ValResult<K> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) => map(name).ok_or_else(|| p.new_error_for_next_token()),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// 先探测 `auto`（或给定关键字）→ None，否则按长度族解析。
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

pub fn parse_len_auto(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    len_auto_with(p, &["auto"]).map(DeclValue::LenAuto)
}

pub fn parse_len(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_length_percentage(p).map(DeclValue::Len)
}

pub fn parse_number_value(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_number(p).map(DeclValue::Number)
}

pub fn parse_color(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_color_value(p).map(DeclValue::Color)
}

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

pub fn parse_display(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "block" | "inline" | "inline-block" | "flow-root" => Display::Block,
            "flex" | "inline-flex" => Display::Flex,
            "grid" | "inline-grid" => Display::Grid,
            "none" => Display::None,
            _ => return None,
        ))
    })
    .map(DeclValue::Display)
}

pub fn parse_position(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "static" => Position::Static,
            "relative" => Position::Relative,
            "absolute" => Position::Absolute,
            // sticky/fixed 属 FEATURES.md T2；这里按容错拒绝（ warn 由上层统一发）
            _ => return None,
        ))
    })
    .map(DeclValue::Position)
}

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

pub fn parse_white_space(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "normal" => WhiteSpace::Normal,
            "nowrap" => WhiteSpace::NoWrap,
            "pre" | "pre-wrap" | "break-spaces" => WhiteSpace::Pre,
            _ => return None,
        ))
    })
    .map(DeclValue::WhiteSpace)
}

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

pub fn parse_font_size(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let kw = p.try_parse(|p| -> ValResult<LengthPercentage> {
        let t = p.next()?.clone();
        // CSS 绝对字号表（medium = 初始 16px）
        let px = match &t {
            Token::Ident(name) => match name.to_ascii_lowercase().as_str() {
                "xx-small" => 9.0,
                "x-small" => 10.0,
                "small" => 13.0,
                "medium" => 16.0,
                "large" => 18.0,
                "x-large" => 24.0,
                "xx-large" => 32.0,
                "xxx-large" => 48.0,
                _ => return Err(p.new_error_for_next_token()),
            },
            _ => return Err(p.new_error_for_next_token()),
        };
        Ok(LengthPercentage::Px(px))
    });
    match kw {
        Ok(len) => Ok(DeclValue::Len(len)),
        Err(_) => parse_length_percentage(p).map(DeclValue::Len),
    }
}

pub fn parse_font_weight(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) if name.eq_ignore_ascii_case("normal") => Ok(DeclValue::Number(400.0)),
        Token::Ident(name) if name.eq_ignore_ascii_case("bold") => Ok(DeclValue::Number(700.0)),
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

/// aspect-ratio: auto | <ratio>（<ratio> = number [/ number]）。
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

/// font-family: 逗号分隔；每项为带引号字符串或连续 Ident 序列（空格连接）。
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

pub fn parse_grid_tracks(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut tracks = Vec::new();
    loop {
        tracks.push(parse_track_size(p)?);
        if p.is_exhausted() {
            break;
        }
    }
    if tracks.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    Ok(DeclValue::GridTracks(GridTemplate { tracks }))
}

fn parse_track_min(p: &mut Parser<'_>) -> ValResult<TrackSize> {
    parse_track_size(p)
}

/// 单个 <track-size>：长度优先（try_parse 失败自动回滚），再关键字/函数。
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
            let (n, list) = p.parse_nested_block(|p| {
                let n = parse_number(p)? as u16;
                if n == 0 {
                    return Err(p.new_error_for_next_token());
                }
                p.expect_comma()?;
                let mut list = Vec::new();
                loop {
                    list.push(parse_track_size(p)?);
                    if p.is_exhausted() {
                        break;
                    }
                    p.expect_comma()?;
                }
                Ok((n, list))
            })?;
            Ok(TrackSize::Repeat(n, list))
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

// ---------- background / box-shadow ----------

pub fn parse_background_image(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) if name.eq_ignore_ascii_case("none") => {
            Ok(DeclValue::BackgroundImage(BackgroundImage::None))
        }
        Token::UnquotedUrl(url) => Ok(DeclValue::BackgroundImage(BackgroundImage::Url(
            url.to_string(),
        ))),
        Token::Function(name) if name.eq_ignore_ascii_case("url") => {
            let url: String = p.parse_nested_block(|p| {
                let t = p.next()?.clone();
                match t {
                    Token::QuotedString(s) => Ok(s.to_string()),
                    _ => Err(p.new_error_for_next_token()),
                }
            })?;
            Ok(DeclValue::BackgroundImage(BackgroundImage::Url(url)))
        }
        Token::Function(name) if name.eq_ignore_ascii_case("linear-gradient") => {
            let g = p.parse_nested_block(parse_linear_gradient)?;
            Ok(DeclValue::BackgroundImage(BackgroundImage::Gradient(g)))
        }
        Token::Function(name) if name.eq_ignore_ascii_case("radial-gradient") => {
            let g = p.parse_nested_block(parse_radial_gradient)?;
            Ok(DeclValue::BackgroundImage(BackgroundImage::Gradient(g)))
        }
        _ => Err(p.new_error_for_next_token()),
    }
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
    let stops = parse_gradient_stops(p)?;
    Ok(Gradient { kind, stops })
}

/// Dimension 数值+单位 → 度。
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
    // MVP：仅支持无 shape/size 段的形式（radial-gradient(red, blue)）；
    // 带 circle/ellipse 等前缀的形式按容错拒绝（偏差记录）。
    let stops = parse_gradient_stops(p)?;
    Ok(Gradient {
        kind: GradientKind::Radial,
        stops,
    })
}

fn parse_gradient_stops(p: &mut Parser<'_>) -> ValResult<Vec<ColorStop>> {
    let mut stops = Vec::new();
    loop {
        let color = parse_color_value(p)?;
        let position = p.try_parse(|p| parse_length_percentage(p)).ok();
        stops.push(ColorStop { color, position });
        let more = p.try_parse(|p| p.expect_comma());
        if more.is_err() {
            break;
        }
    }
    if stops.len() < 2 {
        return Err(p.new_error_for_next_token());
    }
    Ok(stops)
}

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
        loop {
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
        });
        let more = p.try_parse(|p| p.expect_comma());
        if more.is_err() {
            break;
        }
    }
    Ok(DeclValue::BoxShadows(shadows))
}

// ---------- 属性分派 ----------

/// 单条声明值解析入口：PropertyId → 值族解析器。
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
        | P::MarginLeft => {
            // letter-spacing 的 normal → LenAuto(None)（0 尺寸）
            if matches!(id, P::LetterSpacing) {
                len_auto_with(p, &["auto", "normal"]).map(DeclValue::LenAuto)
            } else {
                parse_len_auto(p)
            }
        }
        P::PaddingTop
        | P::PaddingRight
        | P::PaddingBottom
        | P::PaddingLeft
        | P::Gap
        | P::RowGap
        | P::ColumnGap
        | P::BorderTopLeftRadius
        | P::BorderTopRightRadius
        | P::BorderBottomRightRadius
        | P::BorderBottomLeftRadius => parse_len(p),
        P::Opacity | P::FlexGrow | P::FlexShrink | P::ZIndex => parse_number_value(p),
        P::FontWeight => parse_font_weight(p),
        P::Color
        | P::BackgroundColor
        | P::BorderTopColor
        | P::BorderRightColor
        | P::BorderBottomColor
        | P::BorderLeftColor => parse_color(p),
        P::Display => parse_display(p),
        P::Position => parse_position(p),
        P::OverflowX | P::OverflowY => parse_overflow(p),
        P::JustifyContent | P::AlignItems | P::AlignSelf | P::AlignContent => parse_align(p),
        P::FlexDirection => parse_flex_direction(p),
        P::FlexWrap => parse_flex_wrap(p),
        P::BorderTopStyle | P::BorderRightStyle | P::BorderBottomStyle | P::BorderLeftStyle => {
            parse_border_style(p)
        }
        P::TextAlign => parse_text_align(p),
        P::WhiteSpace => parse_white_space(p),
        P::FontStyle => parse_font_style(p),
        P::LineHeight => parse_line_height(p),
        P::FontFamily => parse_font_family(p),
        P::FontSize => parse_font_size(p),
        P::GridTemplateColumns | P::GridTemplateRows | P::GridAutoRows | P::GridAutoColumns => {
            parse_grid_tracks(p)
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
        P::BoxShadow => parse_box_shadow(p),
        P::BorderTopWidth | P::BorderRightWidth | P::BorderBottomWidth | P::BorderLeftWidth => {
            parse_border_width(p)
        }
    }
}
