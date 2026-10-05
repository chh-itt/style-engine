//! 布局映射：ComputedStyle → taffy::Style（L2 适配）。
//!
//! 偏差（记录 FEATURES.md）：calc 内含百分比在布局映射期按 0 折算
//! （taffy 无 calc）；grid 轨道映射（repeat/minmax）T3b 落地，当前
//! grid 容器仅映射 display 与 gap。

use crate::computed::ComputedStyle;
use crate::css::property::{Align, DeclValue, PropertyId};
use crate::css::stylesheet::MediaEnv;
use crate::css::value::{CalcNode, CalcUnit, LengthPercentage, ResolveCtx};

/// em 以节点自身字号为基准，rem 以根 16px 为基准。
fn resolve_px(lp: &LengthPercentage, cs: &ComputedStyle, env: &MediaEnv) -> Option<f32> {
    let ctx = map_ctx(cs, env);
    lp.resolve(&ctx, 0.0)
}

/// A9：映射期统一 ResolveCtx——容器查询单位按 small viewport 回落
///（无容器祖先的规范缺省），字体度量取节点 ComputedStyle（restyle 期
/// 由引擎按 font-family 补写真实值；未注册族=近似缺省，B 级在案）。
fn map_ctx(cs: &ComputedStyle, env: &MediaEnv) -> ResolveCtx {
    let m = cs.font_metrics();
    ResolveCtx {
        cq_w: env.viewport_w,
        cq_h: env.viewport_h,
        ch_per_em: m.ch_per_em,
        ex_per_em: m.ex_per_em,
        ic_per_em: m.ic_per_em,
        ..ResolveCtx::base(cs.font_size_px(), 16.0, env.viewport_w, env.viewport_h)
    }
}

/// A9：直变体 → calc 表达式（延迟结算队列统一以 CalcNode 承载）。
fn lp_to_calc(lp: &LengthPercentage) -> CalcNode {
    match lp {
        LengthPercentage::Px(v) => CalcNode::Value(*v, CalcUnit::Px),
        LengthPercentage::Em(v) => CalcNode::Value(*v, CalcUnit::Em),
        LengthPercentage::Rem(v) => CalcNode::Value(*v, CalcUnit::Rem),
        LengthPercentage::Percent(v) => CalcNode::Value(*v, CalcUnit::Percent),
        LengthPercentage::Vw(v) => CalcNode::Value(*v, CalcUnit::Vw),
        LengthPercentage::Vh(v) => CalcNode::Value(*v, CalcUnit::Vh),
        LengthPercentage::Cqw(v) => CalcNode::Value(*v, CalcUnit::Cqw),
        LengthPercentage::Cqh(v) => CalcNode::Value(*v, CalcUnit::Cqh),
        LengthPercentage::Cqi(v) => CalcNode::Value(*v, CalcUnit::Cqi),
        LengthPercentage::Cqb(v) => CalcNode::Value(*v, CalcUnit::Cqb),
        LengthPercentage::Ch(v) => CalcNode::Value(*v, CalcUnit::Ch),
        LengthPercentage::Ex(v) => CalcNode::Value(*v, CalcUnit::Ex),
        LengthPercentage::Ic(v) => CalcNode::Value(*v, CalcUnit::Ic),
        LengthPercentage::Calc(e) => (**e).clone(),
    }
}

// —— ①calc 直通：延迟结算 ——
// taffy 0.14 原生 calc 指针传输层的公共接入点被 pub(crate) 内部阻断
// （LayoutPartialTree 包装需访问 TaffyView 的 nodes/cache/unrounded
// 私有字段）——引擎侧以「结算式直通」替代：映射期捕获含百分比的
// calc（px 部分先行折叠供首遍布局），布局后以父内容尺寸为基准解析
// 百分比并回写固定值（engine.rs settle_calc，上限 3 遍）。收集走
// thread_local（引擎帧路径单线程；map_style 每次调用即清空）。

/// 延迟 calc 的结算槽位（三期③扩展：width/height 之外新增 flex-basis、
/// min/max、margin/padding 三族——此前含百分比 calc 在这些槽位按 0 折算）。
/// 结算基准：width 族 = 包含块内容宽；height 族 = 包含块内容高；
/// margin/padding 百分比按 CSS 2.1 §8.3/§8.4 恒以包含块 WIDTH 为基
/// （含 margin-top/bottom、padding-top/bottom）；flex-basis 按父容器
/// 主轴（settle 期读父 flex_direction 决定，basis_axis 返回 None）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CalcAxis {
    Width,
    Height,
    MinWidth,
    MinHeight,
    MaxWidth,
    MaxHeight,
    FlexBasis,
    MarginTop,
    MarginRight,
    MarginBottom,
    MarginLeft,
    PaddingTop,
    PaddingRight,
    PaddingBottom,
    PaddingLeft,
    ColumnGap,
    RowGap,
}

impl CalcAxis {
    /// 结算基准轴（对应父内容盒的宽/高；FlexBasis 动态判定）。
    pub(crate) fn basis_axis(self) -> Option<CalcAxis> {
        match self {
            CalcAxis::Height | CalcAxis::MinHeight | CalcAxis::MaxHeight => Some(CalcAxis::Height),
            CalcAxis::RowGap => Some(CalcAxis::Height),
            CalcAxis::FlexBasis => None,
            // width 族 + margin/padding 全族（百分比基恒为包含块宽度）+ 列隙
            _ => Some(CalcAxis::Width),
        }
    }

    /// 结算值回写 taffy 样式（padding 负值按 CSS §8.4 钳 0）。
    pub(crate) fn write(self, style: &mut taffy::prelude::Style, px: f32) {
        use taffy::prelude::{Dimension, LengthPercentage as LP, LengthPercentageAuto as LPA};
        let px = match self {
            CalcAxis::PaddingTop
            | CalcAxis::PaddingRight
            | CalcAxis::PaddingBottom
            | CalcAxis::PaddingLeft => px.max(0.0),
            _ => px,
        };
        match self {
            CalcAxis::Width => style.size.width = Dimension::length(px),
            CalcAxis::Height => style.size.height = Dimension::length(px),
            CalcAxis::MinWidth => style.min_size.width = LPA::length(px),
            CalcAxis::MinHeight => style.min_size.height = LPA::length(px),
            CalcAxis::MaxWidth => style.max_size.width = LPA::length(px),
            CalcAxis::MaxHeight => style.max_size.height = LPA::length(px),
            CalcAxis::FlexBasis => style.flex_basis = Dimension::length(px),
            CalcAxis::MarginTop => style.margin.top = LPA::length(px),
            CalcAxis::MarginRight => style.margin.right = LPA::length(px),
            CalcAxis::MarginBottom => style.margin.bottom = LPA::length(px),
            CalcAxis::MarginLeft => style.margin.left = LPA::length(px),
            CalcAxis::PaddingTop => style.padding.top = LP::length(px),
            CalcAxis::PaddingRight => style.padding.right = LP::length(px),
            CalcAxis::PaddingBottom => style.padding.bottom = LP::length(px),
            CalcAxis::PaddingLeft => style.padding.left = LP::length(px),
            CalcAxis::ColumnGap => style.gap.width = LP::length(px.max(0.0)),
            CalcAxis::RowGap => style.gap.height = LP::length(px.max(0.0)),
        }
    }
}

/// 映射期捕获的延迟 calc（expr + 解析上下文快照；A9 + 字体度量与容器
/// 基值——cq 基值结算期现查 cq_basis，度量快照自节点 ComputedStyle）。
pub(crate) struct DeferredRaw {
    pub axis: CalcAxis,
    pub expr: CalcNode,
    pub em: f32,
    pub rem: f32,
    pub vw: f32,
    pub vh: f32,
    /// A9：ch 基准（每 em；defer 时自 cs 捕获）。
    pub ch_per_em: f32,
    /// A9：ex 基准（每 em）。
    pub ex_per_em: f32,
    /// A9：ic 基准（每 em）。
    pub ic_per_em: f32,
}

/// 挂接 taffy 节点后的结算条目。
pub(crate) struct DeferredCalc {
    pub node: taffy::NodeId,
    pub raw: DeferredRaw,
}

thread_local! {
    static CALC_DEFERRED: std::cell::RefCell<Vec<DeferredRaw>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// 取走本次 map_style 捕获的延迟 calc（调用方挂接 taffy 节点 id）。
pub(crate) fn take_calc_deferred() -> Vec<DeferredRaw> {
    CALC_DEFERRED.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

fn defer_calc(expr: &CalcNode, cs: &ComputedStyle, env: &MediaEnv, axis: CalcAxis) {
    let m = cs.font_metrics();
    CALC_DEFERRED.with(|c| {
        c.borrow_mut().push(DeferredRaw {
            axis,
            expr: expr.clone(),
            em: cs.font_size_px(),
            rem: 16.0,
            vw: env.viewport_w,
            vh: env.viewport_h,
            ch_per_em: m.ch_per_em,
            ex_per_em: m.ex_per_em,
            ic_per_em: m.ic_per_em,
        });
    });
}

fn dimension(
    lp: Option<&LengthPercentage>,
    cs: &ComputedStyle,
    env: &MediaEnv,
    axis: CalcAxis,
) -> taffy::prelude::Dimension {
    use taffy::prelude::Dimension;
    match lp {
        None => Dimension::auto(),
        Some(LengthPercentage::Percent(f)) => Dimension::percent(*f),
        // ①calc 直通：含百分比 calc 延迟结算（px 部分先行折叠供首遍布局）。
        Some(LengthPercentage::Calc(e)) if e.has_percent() || e.has_cq() => {
            defer_calc(e, cs, env, axis);
            // 首遍折叠：cq 叶按 small viewport 回落（settle 期以真实容器基精化）
            Dimension::length(e.resolve(&map_ctx(cs, env), 0.0).unwrap_or(0.0))
        }
        // A9：cq 直变体（width: 50cqw 等）同样延迟结算
        Some(lp) if lp.has_cq() => {
            let node = lp_to_calc(lp);
            defer_calc(&node, cs, env, axis);
            Dimension::length(node.resolve(&map_ctx(cs, env), 0.0).unwrap_or(0.0))
        }
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
    slot: CalcAxis,
) -> taffy::prelude::LengthPercentageAuto {
    use taffy::prelude::LengthPercentageAuto;
    match cs.get(id) {
        None => LengthPercentageAuto::length(0.0),
        Some(DeclValue::LenAuto(None)) => LengthPercentageAuto::auto(),
        Some(DeclValue::LenAuto(Some(lp))) => lp_auto_defer(Some(lp), cs, env, slot),
        Some(_) => lp_auto_defer(cs.len_auto(id), cs, env, slot),
    }
}

/// 三期③：LengthPercentageAuto 槽位（min/max/margin）的延迟结算映射——
/// 含百分比 calc 捕获进结算队列，首遍以 px 部分折叠。
fn lp_auto_defer(
    lp: Option<&LengthPercentage>,
    cs: &ComputedStyle,
    env: &MediaEnv,
    slot: CalcAxis,
) -> taffy::prelude::LengthPercentageAuto {
    use taffy::prelude::LengthPercentageAuto as T;
    match lp {
        None => T::auto(),
        Some(LengthPercentage::Percent(f)) => T::percent(*f),
        Some(LengthPercentage::Calc(e)) if e.has_percent() || e.has_cq() => {
            defer_calc(e, cs, env, slot);
            // 首遍折叠：cq 叶按 small viewport 回落（settle 期以真实容器基精化）
            T::length(e.resolve(&map_ctx(cs, env), 0.0).unwrap_or(0.0))
        }
        // A9：cq 直变体同样延迟结算
        Some(lp) if lp.has_cq() => {
            let node = lp_to_calc(lp);
            defer_calc(&node, cs, env, slot);
            T::length(node.resolve(&map_ctx(cs, env), 0.0).unwrap_or(0.0))
        }
        Some(other) => match resolve_px(other, cs, env) {
            Some(px) => T::length(px),
            None => T::auto(),
        },
    }
}

/// 三期③：LengthPercentage 槽位（padding/gap）的延迟结算映射；clamp_neg
/// 对 padding/gap 负值按 CSS 钳 0（负值非法）。
fn lp_defer(
    lp: &LengthPercentage,
    cs: &ComputedStyle,
    env: &MediaEnv,
    slot: CalcAxis,
    clamp_neg: bool,
) -> taffy::prelude::LengthPercentage {
    use taffy::prelude::LengthPercentage as T;
    let fold = |px: f32| T::length(if clamp_neg { px.max(0.0) } else { px });
    match lp {
        LengthPercentage::Percent(f) => T::percent(*f),
        LengthPercentage::Calc(e) if e.has_percent() || e.has_cq() => {
            defer_calc(e, cs, env, slot);
            // 首遍折叠：cq 叶按 small viewport 回落（settle 期以真实容器基精化）
            fold(e.resolve(&map_ctx(cs, env), 0.0).unwrap_or(0.0))
        }
        // A9：cq 直变体同样延迟结算
        LengthPercentage::Cqw(_)
        | LengthPercentage::Cqh(_)
        | LengthPercentage::Cqi(_)
        | LengthPercentage::Cqb(_) => {
            let node = lp_to_calc(lp);
            defer_calc(&node, cs, env, slot);
            fold(node.resolve(&map_ctx(cs, env), 0.0).unwrap_or(0.0))
        }
        other => fold(resolve_px(other, cs, env).unwrap_or(0.0)),
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

    let mut ts = Style {
        display: if cs.pseudo().is_some()
            && !matches!(cs.content(), crate::css::property::ContentValue::Str(_))
        {
            // C1（ADR-0015）：伪节点 content none/normal → 无盒（spec：
            // content 仅作用于伪元素；宿主节点恒 Normal 不受影响）
            taffy::prelude::Display::None
        } else {
            match cs.display() {
                crate::css::property::Display::Block => taffy::prelude::Display::Block,
                // F1（ADR-0021）：inline/inline-block 均映射 taffy Block
                //（零布局差异）；行内参与由引擎 settle_lines 行打包处理。
                crate::css::property::Display::Inline
                | crate::css::property::Display::InlineBlock => taffy::prelude::Display::Block,
                crate::css::property::Display::Flex => taffy::prelude::Display::Flex,
                crate::css::property::Display::Grid => taffy::prelude::Display::Grid,
                crate::css::property::Display::None => taffy::prelude::Display::None,
                // 二期②表格三值：表=块容器（行级 Grid 纵向堆叠）、行=单行 Grid
                // （列模板由引擎 settle_tables 结算回写）、单元格=Grid 项。
                crate::css::property::Display::Table => taffy::prelude::Display::Block,
                crate::css::property::Display::TableRow => taffy::prelude::Display::Grid,
                crate::css::property::Display::TableCell => taffy::prelude::Display::Block,
                // 三期④：行组=纵向透明块包装、caption=普通块（置于行区上方）。
                crate::css::property::Display::TableRowGroup => taffy::prelude::Display::Block,
                crate::css::property::Display::TableCaption => taffy::prelude::Display::Block,
            }
        },
        position: match cs.position() {
            crate::css::property::Position::Static
            | crate::css::property::Position::Relative
            | crate::css::property::Position::Sticky => taffy::prelude::Position::Relative,
            crate::css::property::Position::Absolute => taffy::prelude::Position::Absolute,
            // A4：taffy 无 fixed——映射为 Absolute；包含块（transformed
            // 祖先或 ICB 视口）由 settle_absolute_anchors 的 cb 判定。
            crate::css::property::Position::Fixed => taffy::prelude::Position::Absolute,
        },
        // box-sizing 直通：taffy 的 size 语义随 ContentBox/BorderBox 换算
        // （block.rs 布局期以 padding_border_size 调整），CSS 默认 content-box
        box_sizing: match cs.box_sizing() {
            crate::css::property::BoxSizing::ContentBox => taffy::style::BoxSizing::ContentBox,
            crate::css::property::BoxSizing::BorderBox => taffy::style::BoxSizing::BorderBox,
        },
        size: Size {
            width: dimension(lp_auto(PropertyId::Width), cs, env, CalcAxis::Width),
            height: dimension(lp_auto(PropertyId::Height), cs, env, CalcAxis::Height),
        },
        min_size: Size {
            // 三期③修正：min-* 解析入 LenAuto 族（parse_len_auto），旧代码误用
            // Len 族读取（cs.len）恒得 None——min/max 尺寸整体失效（calc 与
            // 普通值皆然），本槽位扩展顺带修复；auto → taffy Auto（flex
            // automatic minimum size 语义）。
            width: lp_auto_defer(
                cs.len_auto(PropertyId::MinWidth),
                cs,
                env,
                CalcAxis::MinWidth,
            ),
            height: lp_auto_defer(
                cs.len_auto(PropertyId::MinHeight),
                cs,
                env,
                CalcAxis::MinHeight,
            ),
        },
        max_size: Size {
            width: lp_auto_defer(lp_auto(PropertyId::MaxWidth), cs, env, CalcAxis::MaxWidth),
            height: lp_auto_defer(lp_auto(PropertyId::MaxHeight), cs, env, CalcAxis::MaxHeight),
        },
        margin: Rect {
            top: margin_side(cs, PropertyId::MarginTop, env, CalcAxis::MarginTop),
            right: margin_side(cs, PropertyId::MarginRight, env, CalcAxis::MarginRight),
            bottom: margin_side(cs, PropertyId::MarginBottom, env, CalcAxis::MarginBottom),
            left: margin_side(cs, PropertyId::MarginLeft, env, CalcAxis::MarginLeft),
        },
        padding: Rect {
            top: padding[0]
                .map(|lp| lp_defer(lp, cs, env, CalcAxis::PaddingTop, true))
                .unwrap_or_else(|| taffy::prelude::LengthPercentage::length(0.0)),
            right: padding[1]
                .map(|lp| lp_defer(lp, cs, env, CalcAxis::PaddingRight, true))
                .unwrap_or_else(|| taffy::prelude::LengthPercentage::length(0.0)),
            bottom: padding[2]
                .map(|lp| lp_defer(lp, cs, env, CalcAxis::PaddingBottom, true))
                .unwrap_or_else(|| taffy::prelude::LengthPercentage::length(0.0)),
            left: padding[3]
                .map(|lp| lp_defer(lp, cs, env, CalcAxis::PaddingLeft, true))
                .unwrap_or_else(|| taffy::prelude::LengthPercentage::length(0.0)),
        },
        // A4 sticky：in-flow 布局不施加 inset 偏移（top/right/bottom/left
        // 是粘滞约束语义、宿主消费；taffy 相对定位会把 inset 当偏移用）。
        inset: if matches!(cs.position(), crate::css::property::Position::Sticky) {
            Rect {
                top: taffy::prelude::LengthPercentageAuto::auto(),
                right: taffy::prelude::LengthPercentageAuto::auto(),
                bottom: taffy::prelude::LengthPercentageAuto::auto(),
                left: taffy::prelude::LengthPercentageAuto::auto(),
            }
        } else {
            Rect {
                top: length_percentage_auto(lp_auto(PropertyId::Top), cs, env),
                right: length_percentage_auto(lp_auto(PropertyId::Right), cs, env),
                bottom: length_percentage_auto(lp_auto(PropertyId::Bottom), cs, env),
                left: length_percentage_auto(lp_auto(PropertyId::Left), cs, env),
            }
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
            // 三期③：gap 含百分比 calc 延迟结算（列隙基=内容宽、行隙基=内容高）。
            width: lp_defer(
                cs.len(PropertyId::ColumnGap).unwrap_or(
                    cs.len(PropertyId::Gap)
                        .unwrap_or(&LengthPercentage::Px(0.0)),
                ),
                cs,
                env,
                CalcAxis::ColumnGap,
                true,
            ),
            height: lp_defer(
                cs.len(PropertyId::RowGap).unwrap_or(
                    cs.len(PropertyId::Gap)
                        .unwrap_or(&LengthPercentage::Px(0.0)),
                ),
                cs,
                env,
                CalcAxis::RowGap,
                true,
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
        // 三期③：flex-basis 含百分比 calc 延迟结算（基=父容器主轴内容尺寸，
        // settle 期按父 flex_direction 判定；行向=宽、列向=高）。
        flex_basis: dimension(lp_auto(PropertyId::FlexBasis), cs, env, CalcAxis::FlexBasis),
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
        // grid 显式轨道（第五批③ grid-columns 用例驱动）：此前解析入库但
        // 零消费——taffy 落入隐式单列、行高 0；接通后轨道语义直达 taffy。
        // 百分比保留原生（taffy 按容器解析）；calc/em/rem 沿用 resolve_px 扁平化。
        grid_template_columns: cs
            .get(PropertyId::GridTemplateColumns)
            .and_then(|v| match v {
                DeclValue::GridTracks(t) => Some(
                    t.tracks
                        .iter()
                        .map(|ts| grid_component(ts, cs, env))
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default(),
        grid_template_rows: cs
            .get(PropertyId::GridTemplateRows)
            .and_then(|v| match v {
                DeclValue::GridTracks(t) => Some(
                    t.tracks
                        .iter()
                        .map(|ts| grid_component(ts, cs, env))
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default(),
        // grid-auto-flow（解析子集 Row|Column）；auto 行/列轨道为单长度子集
        grid_auto_flow: match cs.get(PropertyId::GridAutoFlow) {
            Some(DeclValue::GridAutoFlow(k)) => match k {
                crate::css::property::GridAutoFlowKind::Row => taffy::style::GridAutoFlow::Row,
                crate::css::property::GridAutoFlowKind::Column => {
                    taffy::style::GridAutoFlow::Column
                }
            },
            _ => taffy::style::GridAutoFlow::Row,
        },
        grid_auto_rows: auto_tracks(cs.get(PropertyId::GridAutoRows), cs, env),
        grid_auto_columns: auto_tracks(cs.get(PropertyId::GridAutoColumns), cs, env),
        ..Style::default()
    };
    // ③multi-column：多列容器映射 taffy Flex Row——幻影列节点由
    // settle_columns 布局期创建并承接真实子节点；column-gap 沿用上方
    // gap 映射，缺席时按 multicol 语义 normal = 1em（flex/grid normal=0）。
    if multicol_requested(cs) {
        ts.display = taffy::prelude::Display::Flex;
        ts.flex_direction = taffy::prelude::FlexDirection::Row;
        if cs.get(PropertyId::ColumnGap).is_none() && cs.get(PropertyId::Gap).is_none() {
            ts.gap.width = taffy::prelude::LengthPercentage::length(cs.font_size_px());
        }
    }
    ts
}

/// ③multi-column 请求判定（map_style 与引擎登记共用）：column-count ≥2
/// 显式多列；count 缺席/auto 时 column-width 声明即请求（列数布局期
/// 结算，可能回退 1 列——引擎 deactivate 路径还原块流）。
pub fn multicol_requested(cs: &ComputedStyle) -> bool {
    match cs.get(PropertyId::ColumnCount) {
        Some(DeclValue::ColumnCount(Some(n))) => *n >= 2,
        Some(DeclValue::ColumnCount(None)) | None => matches!(
            cs.get(PropertyId::ColumnWidth),
            Some(DeclValue::LenAuto(Some(_)))
        ),
        _ => false,
    }
}

/// ②table v1 列宽分配（像素列宽序列，settle_tables 消费）：
/// 定宽照抄、百分比按表内容宽解析、auto 均分剩余（CSS 规范按内容
/// max-content 比例分配——v1 偏差：等分；单 auto 列时与规范一致）。
/// 剩余为负时 auto 取 0；声明列数不足 n_cols 以 auto 补齐。
/// 浏览器语义（Chromium 153 实测对齐）：Length 声明 = content-box，
/// 列贡献 = px + 单元格水平内缩（padding+border）；Percent 声明 =
/// border-box（列宽 = p×表内容宽，不追加内缩——单元格百分比宽的
/// 已知非对称行为）。Auto 列取剩余 border-box 宽。
/// 注：taffy 0.14 Dimension 为 CompactLength 编码结构体（非枚举），
/// 分类用 is_auto/into_option/value。
pub fn table_column_template(
    table_content_width: f32,
    declared: &[(taffy::prelude::Dimension, f32)],
    n_cols: usize,
) -> Vec<f32> {
    let mut cols: Vec<(taffy::prelude::Dimension, f32)> =
        vec![(taffy::prelude::Dimension::auto(), 0.0); n_cols.max(declared.len())];
    for (slot, d) in cols.iter_mut().zip(declared.iter()) {
        *slot = *d;
    }
    let mut used = 0.0f32;
    let mut autos = 0usize;
    for (d, inset) in &cols {
        if let Some(px) = d.into_option() {
            used += px + inset;
        } else if d.is_auto() {
            autos += 1;
        } else {
            used += d.value() * table_content_width;
        }
    }
    let share = ((table_content_width - used) / autos as f32).max(0.0);
    cols.into_iter()
        .map(|(d, inset)| match d.into_option() {
            // Length 列 = 声明 px + 内缩（单元格 border-box，Chromium 语义）。
            Some(px) => px + inset,
            None if d.is_auto() => share,
            None => d.value() * table_content_width,
        })
        .collect()
}

/// 轨道项 → taffy GridTemplateComponent（repeat 展开为重复计数）。
fn grid_component<S: taffy::style::CheapCloneStr>(
    ts: &crate::css::property::TrackSize,
    cs: &ComputedStyle,
    env: &MediaEnv,
) -> taffy::style::GridTemplateComponent<S> {
    match ts {
        crate::css::property::TrackSize::Repeat(n, list) => taffy::style_helpers::repeat(
            *n,
            list.iter()
                .map(|t| track_sizing(t, cs, env))
                .collect::<Vec<_>>(),
        ),
        // 阶段2①：auto-fill / auto-fit —— 计数由 taffy 布局期按可用空间定
        //（RepetitionCount::AutoFill / AutoFit 原生支持；auto-fit 空轨折叠）。
        crate::css::property::TrackSize::RepeatAuto(fit, list) => taffy::style_helpers::repeat(
            if *fit { "auto-fit" } else { "auto-fill" },
            list.iter()
                .map(|t| track_sizing(t, cs, env))
                .collect::<Vec<_>>(),
        ),
        other => taffy::style::GridTemplateComponent::Single(track_sizing(other, cs, env)),
    }
}

/// 轨道尺寸 → taffy TrackSizingFunction。极小侧 fr / 嵌套 minmax / 嵌套
/// repeat 属解析容错场景（CSS 禁止），防御性归 auto。
fn track_sizing(
    ts: &crate::css::property::TrackSize,
    cs: &ComputedStyle,
    env: &MediaEnv,
) -> taffy::prelude::TrackSizingFunction {
    use taffy::style_helpers;
    match ts {
        crate::css::property::TrackSize::Len(lp) => track_lp(lp, cs, env),
        crate::css::property::TrackSize::Fr(v) => style_helpers::fr(*v),
        crate::css::property::TrackSize::Auto => style_helpers::auto(),
        crate::css::property::TrackSize::MaxContent => style_helpers::max_content(),
        crate::css::property::TrackSize::MinContent => style_helpers::min_content(),
        crate::css::property::TrackSize::MinMax(min, max) => {
            style_helpers::minmax(min_side(min, cs, env), max_side(max, cs, env))
        }
        crate::css::property::TrackSize::Repeat(_, _) => style_helpers::auto(),
        // auto 重复嵌套于轨道内属解析容错场景（CSS 禁止），防御性归 auto。
        crate::css::property::TrackSize::RepeatAuto(_, _) => style_helpers::auto(),
    }
}

fn track_lp(
    lp: &LengthPercentage,
    cs: &ComputedStyle,
    env: &MediaEnv,
) -> taffy::prelude::TrackSizingFunction {
    use taffy::style_helpers;
    match lp {
        LengthPercentage::Percent(f) => style_helpers::percent(*f),
        other => style_helpers::length(resolve_px(other, cs, env).unwrap_or(0.0)),
    }
}

fn min_side(
    ts: &crate::css::property::TrackSize,
    cs: &ComputedStyle,
    env: &MediaEnv,
) -> taffy::prelude::MinTrackSizingFunction {
    use taffy::style_helpers;
    match ts {
        crate::css::property::TrackSize::Len(lp) => match lp {
            LengthPercentage::Percent(f) => style_helpers::percent(*f),
            other => style_helpers::length(resolve_px(other, cs, env).unwrap_or(0.0)),
        },
        crate::css::property::TrackSize::MinContent => style_helpers::min_content(),
        crate::css::property::TrackSize::MaxContent => style_helpers::max_content(),
        // 极小侧无 fr（CSS 禁止）；嵌套 minmax/repeat 容错归 auto
        _ => style_helpers::auto(),
    }
}

fn max_side(
    ts: &crate::css::property::TrackSize,
    cs: &ComputedStyle,
    env: &MediaEnv,
) -> taffy::prelude::MaxTrackSizingFunction {
    use taffy::style_helpers;
    match ts {
        crate::css::property::TrackSize::Len(lp) => match lp {
            LengthPercentage::Percent(f) => style_helpers::percent(*f),
            other => style_helpers::length(resolve_px(other, cs, env).unwrap_or(0.0)),
        },
        crate::css::property::TrackSize::Fr(v) => style_helpers::fr(*v),
        crate::css::property::TrackSize::MinContent => style_helpers::min_content(),
        crate::css::property::TrackSize::MaxContent => style_helpers::max_content(),
        _ => style_helpers::auto(),
    }
}

/// grid-auto-rows/columns（单长度解析子集）→ taffy 自动轨道列表；
/// 缺席 → 空（taffy 默认 = auto 行为）。
fn auto_tracks(
    v: Option<&crate::css::property::DeclValue>,
    cs: &ComputedStyle,
    env: &MediaEnv,
) -> Vec<taffy::prelude::TrackSizingFunction> {
    match v {
        // E5（ADR-0020）：grid-auto-* 与模板共用轨道列表解析（GridTracks，
        // line_names 空）→ 全列表映射（多条目按奇偶隐式轨交替）。
        Some(DeclValue::GridTracks(t)) => t
            .tracks
            .iter()
            .map(|ts| track_sizing(ts, cs, env))
            .collect(),
        Some(DeclValue::LenAuto(Some(lp))) => {
            vec![track_lp(lp, cs, env)]
        }
        _ => Vec::new(),
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

#[cfg(test)]
mod table_template_tests {
    use super::table_column_template;
    use taffy::prelude::Dimension;

    #[test]
    fn fixed_percent_auto_mix() {
        // Chromium 153 实测语义（conformance table-basic 同构）：Length
        // 声明 content-box——贡献 = px + 内缩（150+16=166）；Percent
        // 声明 border-box——列 = p×表宽（300，不追加内缩）；auto 取剩余
        // border-box（134）。
        let cols = table_column_template(
            600.0,
            &[
                (Dimension::length(150.0), 16.0),
                (Dimension::percent(0.5), 0.0),
                (Dimension::auto(), 0.0),
            ],
            3,
        );
        assert_eq!(cols, vec![166.0, 300.0, 134.0]);
    }

    #[test]
    fn auto_only_splits_equally() {
        let cols = table_column_template(
            600.0,
            &[(Dimension::auto(), 0.0), (Dimension::auto(), 0.0)],
            2,
        );
        assert_eq!(cols, vec![300.0, 300.0]);
    }

    #[test]
    fn negative_remainder_clamps_auto_to_zero() {
        let cols = table_column_template(
            600.0,
            &[(Dimension::length(700.0), 0.0), (Dimension::auto(), 0.0)],
            2,
        );
        assert_eq!(cols, vec![700.0, 0.0]);
    }

    #[test]
    fn missing_declarations_pad_as_auto() {
        // 首行 1 列声明、后续行共 3 列：补齐 auto 均分。
        let cols = table_column_template(600.0, &[(Dimension::length(150.0), 0.0)], 3);
        assert_eq!(cols, vec![150.0, 225.0, 225.0]);
    }
}
