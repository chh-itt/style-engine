//! 布局映射：ComputedStyle → taffy::Style（L2 适配）。
//!
//! 偏差（记录 FEATURES.md）：calc 内含百分比在布局映射期按 0 折算
//! （taffy 无 calc）；grid 轨道映射（repeat/minmax）T3b 落地，当前
//! grid 容器仅映射 display 与 gap。

use crate::computed::ComputedStyle;
use crate::css::property::{Align, DeclValue, PropertyId};
use crate::css::stylesheet::MediaEnv;
use crate::css::value::{LengthPercentage, ResolveCtx};

/// em 以节点自身字号为基准，rem 以根 16px 为基准。
fn resolve_px(lp: &LengthPercentage, cs: &ComputedStyle, env: &MediaEnv) -> Option<f32> {
    let ctx = ResolveCtx {
        em: cs.font_size_px(),
        rem: 16.0,
        viewport_w: env.viewport_w,
        viewport_h: env.viewport_h,
    };
    lp.resolve(&ctx, 0.0)
}

fn dimension(
    lp: Option<&LengthPercentage>,
    cs: &ComputedStyle,
    env: &MediaEnv,
) -> taffy::prelude::Dimension {
    use taffy::prelude::Dimension;
    match lp {
        None => Dimension::auto(),
        Some(LengthPercentage::Percent(f)) => Dimension::percent(*f),
        Some(other) => match resolve_px(other, cs, env) {
            Some(px) => Dimension::length(px),
            None => Dimension::auto(),
        },
    }
}

fn length_percentage_auto(
    lp: Option<&LengthPercentage>,
    cs: &ComputedStyle,
    env: &MediaEnv,
) -> taffy::prelude::LengthPercentageAuto {
    use taffy::prelude::LengthPercentageAuto;
    match lp {
        None => LengthPercentageAuto::auto(),
        Some(LengthPercentage::Percent(f)) => LengthPercentageAuto::percent(*f),
        Some(other) => match resolve_px(other, cs, env) {
            Some(px) => LengthPercentageAuto::length(px),
            None => LengthPercentageAuto::auto(),
        },
    }
}

/// margin：CSS 初始值为 0；显式 auto 保留（auto 水平 margin 使定宽块级盒居中）。
/// 旧实现把缺席 margin 经 len_auto(None) 映射为 auto，导致无 margin 的
/// 定宽块级子盒被意外水平居中。
fn margin_side(
    cs: &ComputedStyle,
    id: PropertyId,
    env: &MediaEnv,
) -> taffy::prelude::LengthPercentageAuto {
    use taffy::prelude::LengthPercentageAuto;
    match cs.get(id) {
        None => LengthPercentageAuto::length(0.0),
        Some(DeclValue::LenAuto(None)) => LengthPercentageAuto::auto(),
        Some(DeclValue::LenAuto(Some(lp))) => length_percentage_auto(Some(lp), cs, env),
        Some(_) => length_percentage_auto(cs.len_auto(id), cs, env),
    }
}

fn length_percentage(
    lp: &LengthPercentage,
    cs: &ComputedStyle,
    env: &MediaEnv,
) -> taffy::prelude::LengthPercentage {
    use taffy::prelude::LengthPercentage as T;
    match lp {
        LengthPercentage::Percent(f) => T::percent(*f),
        other => T::length(resolve_px(other, cs, env).unwrap_or(0.0)),
    }
}

/// align 家族 → taffy AlignItems（normal → None = 引擎初始）。
fn align_items(a: Align) -> Option<taffy::prelude::AlignItems> {
    match a {
        Align::Normal => None,
        Align::Start | Align::FlexStart => Some(taffy::prelude::AlignItems::FLEX_START),
        Align::End | Align::FlexEnd => Some(taffy::prelude::AlignItems::FLEX_END),
        Align::Center => Some(taffy::prelude::AlignItems::CENTER),
        Align::Stretch => Some(taffy::prelude::AlignItems::STRETCH),
        Align::Baseline => Some(taffy::prelude::AlignItems::BASELINE),
        // align-items 不接受 space-*（容错：取初始）
        Align::SpaceBetween | Align::SpaceAround | Align::SpaceEvenly => None,
    }
}

/// justify-content 映射。
fn justify_content(a: Align) -> Option<taffy::prelude::JustifyContent> {
    match a {
        Align::Normal | Align::Stretch | Align::Baseline => None,
        Align::Start | Align::FlexStart => Some(taffy::prelude::JustifyContent::FLEX_START),
        Align::End | Align::FlexEnd => Some(taffy::prelude::JustifyContent::FLEX_END),
        Align::Center => Some(taffy::prelude::JustifyContent::CENTER),
        Align::SpaceBetween => Some(taffy::prelude::JustifyContent::SPACE_BETWEEN),
        Align::SpaceAround => Some(taffy::prelude::JustifyContent::SPACE_AROUND),
        Align::SpaceEvenly => Some(taffy::prelude::JustifyContent::SPACE_EVENLY),
    }
}

fn align_content(a: Align) -> Option<taffy::prelude::AlignContent> {
    match a {
        Align::Normal => None,
        Align::Start | Align::FlexStart => Some(taffy::prelude::AlignContent::FLEX_START),
        Align::End | Align::FlexEnd => Some(taffy::prelude::AlignContent::FLEX_END),
        Align::Center => Some(taffy::prelude::AlignContent::CENTER),
        Align::Stretch => Some(taffy::prelude::AlignContent::STRETCH),
        Align::Baseline => Some(taffy::prelude::AlignContent::FLEX_START),
        Align::SpaceBetween => Some(taffy::prelude::AlignContent::SPACE_BETWEEN),
        Align::SpaceAround => Some(taffy::prelude::AlignContent::SPACE_AROUND),
        Align::SpaceEvenly => Some(taffy::prelude::AlignContent::SPACE_EVENLY),
    }
}

/// 计算样式 → taffy 样式。
pub fn map_style(cs: &ComputedStyle, env: &MediaEnv) -> taffy::prelude::Style {
    use taffy::prelude::{Rect, Size, Style};

    let lp_auto = |id: PropertyId| cs.len_auto(id);
    let padding = cs.padding();

    Style {
        display: match cs.display() {
            crate::css::property::Display::Block => taffy::prelude::Display::Block,
            crate::css::property::Display::Flex => taffy::prelude::Display::Flex,
            crate::css::property::Display::Grid => taffy::prelude::Display::Grid,
            crate::css::property::Display::None => taffy::prelude::Display::None,
        },
        position: match cs.position() {
            crate::css::property::Position::Static | crate::css::property::Position::Relative => {
                taffy::prelude::Position::Relative
            }
            crate::css::property::Position::Absolute => taffy::prelude::Position::Absolute,
        },
        // box-sizing 直通：taffy 的 size 语义随 ContentBox/BorderBox 换算
        // （block.rs 布局期以 padding_border_size 调整），CSS 默认 content-box
        box_sizing: match cs.box_sizing() {
            crate::css::property::BoxSizing::ContentBox => taffy::style::BoxSizing::ContentBox,
            crate::css::property::BoxSizing::BorderBox => taffy::style::BoxSizing::BorderBox,
        },
        size: Size {
            width: dimension(lp_auto(PropertyId::Width), cs, env),
            height: dimension(lp_auto(PropertyId::Height), cs, env),
        },
        min_size: Size {
            width: length_percentage_auto(cs.len(PropertyId::MinWidth), cs, env),
            height: length_percentage_auto(cs.len(PropertyId::MinHeight), cs, env),
        },
        max_size: Size {
            width: length_percentage_auto(lp_auto(PropertyId::MaxWidth), cs, env),
            height: length_percentage_auto(lp_auto(PropertyId::MaxHeight), cs, env),
        },
        margin: Rect {
            top: margin_side(cs, PropertyId::MarginTop, env),
            right: margin_side(cs, PropertyId::MarginRight, env),
            bottom: margin_side(cs, PropertyId::MarginBottom, env),
            left: margin_side(cs, PropertyId::MarginLeft, env),
        },
        padding: Rect {
            top: padding[0]
                .map(|lp| length_percentage(lp, cs, env))
                .unwrap_or_else(|| taffy::prelude::LengthPercentage::length(0.0)),
            right: padding[1]
                .map(|lp| length_percentage(lp, cs, env))
                .unwrap_or_else(|| taffy::prelude::LengthPercentage::length(0.0)),
            bottom: padding[2]
                .map(|lp| length_percentage(lp, cs, env))
                .unwrap_or_else(|| taffy::prelude::LengthPercentage::length(0.0)),
            left: padding[3]
                .map(|lp| length_percentage(lp, cs, env))
                .unwrap_or_else(|| taffy::prelude::LengthPercentage::length(0.0)),
        },
        inset: Rect {
            top: length_percentage_auto(lp_auto(PropertyId::Top), cs, env),
            right: length_percentage_auto(lp_auto(PropertyId::Right), cs, env),
            bottom: length_percentage_auto(lp_auto(PropertyId::Bottom), cs, env),
            left: length_percentage_auto(lp_auto(PropertyId::Left), cs, env),
        },
        // 边框占位（Numeric Channel box-model 用例驱动，ADR-0003）：CSS 边框
        // 计入布局（content-box 调整式含 padding+border）；style none → 0，
        // 与 used_h_inset 的 used-width 语义一致
        border: Rect {
            top: border_side(
                cs,
                PropertyId::BorderTopWidth,
                PropertyId::BorderTopStyle,
                env,
            ),
            right: border_side(
                cs,
                PropertyId::BorderRightWidth,
                PropertyId::BorderRightStyle,
                env,
            ),
            bottom: border_side(
                cs,
                PropertyId::BorderBottomWidth,
                PropertyId::BorderBottomStyle,
                env,
            ),
            left: border_side(
                cs,
                PropertyId::BorderLeftWidth,
                PropertyId::BorderLeftStyle,
                env,
            ),
        },
        gap: Size {
            width: length_percentage(
                cs.len(PropertyId::ColumnGap).unwrap_or(
                    cs.len(PropertyId::Gap)
                        .unwrap_or(&LengthPercentage::Px(0.0)),
                ),
                cs,
                env,
            ),
            height: length_percentage(
                cs.len(PropertyId::RowGap).unwrap_or(
                    cs.len(PropertyId::Gap)
                        .unwrap_or(&LengthPercentage::Px(0.0)),
                ),
                cs,
                env,
            ),
        },
        flex_direction: match cs.flex_direction() {
            crate::css::property::FlexDirection::Row => taffy::prelude::FlexDirection::Row,
            crate::css::property::FlexDirection::RowReverse => {
                taffy::prelude::FlexDirection::RowReverse
            }
            crate::css::property::FlexDirection::Column => taffy::prelude::FlexDirection::Column,
            crate::css::property::FlexDirection::ColumnReverse => {
                taffy::prelude::FlexDirection::ColumnReverse
            }
        },
        flex_wrap: match cs.flex_wrap() {
            crate::css::property::FlexWrap::NoWrap => taffy::prelude::FlexWrap::NoWrap,
            crate::css::property::FlexWrap::Wrap => taffy::prelude::FlexWrap::Wrap,
            crate::css::property::FlexWrap::WrapReverse => taffy::prelude::FlexWrap::WrapReverse,
        },
        flex_grow: flex_number(cs, PropertyId::FlexGrow, 0.0),
        flex_shrink: flex_number(cs, PropertyId::FlexShrink, 1.0),
        flex_basis: dimension(lp_auto(PropertyId::FlexBasis), cs, env),
        aspect_ratio: match cs.get(PropertyId::AspectRatio) {
            Some(DeclValue::AspectRatio(Some(r))) => Some(*r),
            _ => None,
        },
        align_items: match cs.get(PropertyId::AlignItems) {
            Some(DeclValue::Align(a)) => align_items(*a),
            _ => None,
        },
        align_self: match cs.get(PropertyId::AlignSelf) {
            Some(DeclValue::Align(a)) => align_items(*a),
            _ => None,
        },
        align_content: match cs.get(PropertyId::AlignContent) {
            Some(DeclValue::Align(a)) => align_content(*a),
            _ => None,
        },
        justify_content: match cs.get(PropertyId::JustifyContent) {
            Some(DeclValue::Align(a)) => justify_content(*a),
            _ => None,
        },
        overflow: taffy::Point {
            x: map_overflow(cs.overflow_x()),
            y: map_overflow(cs.overflow_y()),
        },
        ..Style::default()
    }
}

fn flex_number(cs: &ComputedStyle, id: PropertyId, fallback: f32) -> f32 {
    match cs.get(id) {
        Some(DeclValue::Number(n)) => *n,
        _ => fallback,
    }
}

/// 有效边框宽（单侧）：style none → 0；width 缺席/none → 0；其余按 LP 解析。
/// 与 used_h_inset 的 used-width 语义一致（style none 时边框宽归零）。
fn border_side(
    cs: &ComputedStyle,
    width_id: PropertyId,
    style_id: PropertyId,
    env: &MediaEnv,
) -> taffy::prelude::LengthPercentage {
    let none = matches!(
        cs.get(style_id),
        Some(crate::css::property::DeclValue::BorderStyle(
            crate::css::property::BorderStyle::None
        ))
    );
    if none {
        return taffy::prelude::LengthPercentage::length(0.0);
    }
    match cs.get(width_id) {
        Some(crate::css::property::DeclValue::BorderWidth(Some(lp))) => {
            length_percentage(lp, cs, env)
        }
        _ => taffy::prelude::LengthPercentage::length(0.0),
    }
}

fn map_overflow(o: crate::css::property::Overflow) -> taffy::Overflow {
    use crate::css::property::Overflow as O;
    match o {
        O::Visible => taffy::Overflow::Visible,
        O::Hidden => taffy::Overflow::Hidden,
        O::Clip => taffy::Overflow::Clip,
        O::Scroll => taffy::Overflow::Scroll,
    }
}
