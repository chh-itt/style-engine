//! 绘制清单（L3）：布局 + 计算样式 → 绘制基元序列。
//!
//! 中立交换格式（ADR-0001/0003）：颜色为已解析的绝对 sRGBA
//! （currentColor / light-dark 在此层终结；线性化与混合由 vello sink
//! 承担，ADR-0002）。绘制顺序 = 树序（父先于子；z-index 排序属后续）。
//! 单节点内：阴影 → 背景 → 边框 → 文本。

use crate::computed::ComputedStyle;
use crate::css::property::FontFamilyList;
use crate::css::property::{
    BackgroundImage, BorderStyle, DeclValue, FontStyle, Gradient, Overflow, PropertyId,
};
use crate::css::stylesheet::MediaEnv;
use crate::css::value::{ColorValue, LengthPercentage, ResolveCtx};
use crate::tree::{NodeId, StyleTree};
use peniko::color::{AlphaColor, Srgb};
use std::collections::HashMap;

/// 单个绘制基元。坐标相对视口（滚动前）；尺寸为 border-box。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum PaintOp {
    /// 纯色矩形（背景）。
    FillRect {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        /// 四角圆角（左上、右上、右下、左下）。
        radius: [f32; 4],
        color: AlphaColor<Srgb>,
    },
    /// 渐变背景（linear/radial，语义同 CSS）。
    Gradient {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radius: [f32; 4],
        gradient: Gradient,
        /// 径向几何（T4c）：圆心与半径已按盒子解析为绝对 px（线性渐变为 None）。
        radial: Option<RadialGeom>,
    },
    /// 外阴影（MVP：矩形阴影；inset 阴影暂缺）。
    Shadow {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radius: [f32; 4],
        color: AlphaColor<Srgb>,
        offset_x: f32,
        offset_y: f32,
        blur: f32,
    },
    /// 边框（四边独立：top/right/bottom/left；style none 或 width 0 的边由 sink 忽略）。
    Border {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radius: [f32; 4],
        sides: [BorderSide; 4],
    },
    /// 文本（T5 转换为字形 run；spans 为 T5c 富文本覆盖，可为空）。
    Text {
        x: f32,
        y: f32,
        text: String,
        color: AlphaColor<Srgb>,
        /// span 覆盖样式（T5c）：绘制期已终结；空 = 无富文本。
        spans: Vec<TextSpanPaint>,
        font_size: f32,
        font_family: FontFamilyList,
        font_weight: f32,
        italic: bool,
        /// 换行约束（T5c-2）：测量与绘制共用同一 max_advance 保证折行一致；None = 无界。
        max_advance: Option<f32>,
    },
    /// 裁剪层开始（overflow 非 visible）。
    PushClip {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radius: [f32; 4],
    },
    PopClip,
    /// 滚动偏移层开始。
    PushScroll {
        dx: f32,
        dy: f32,
    },
    PopScroll,
}

/// 单边边框（T4b：宽度/样式/颜色已在 paint 层终结为绝对值）。
#[derive(Debug, Clone, PartialEq)]
pub struct BorderSide {
    pub width: f32,
    pub style: BorderStyle,
    pub color: AlphaColor<Srgb>,
}

/// 已解析的径向几何（绝对 px；椭圆分别给 rx/ry，圆时相等）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadialGeom {
    pub cx: f32,
    pub cy: f32,
    pub rx: f32,
    pub ry: f32,
}

/// 富文本 span（T5c）：绘制期已终结的覆盖样式；区间 [start, end) 为文本字节偏移。
#[derive(Debug, Clone, PartialEq)]
pub struct TextSpanPaint {
    pub start: u32,
    pub end: u32,
    pub color: AlphaColor<Srgb>,
    pub font_size: f32,
    pub font_weight: f32,
    pub italic: bool,
    pub font_family: crate::css::property::FontFamilyList,
}

/// 一帧的绘制清单。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DisplayList {
    /// 树序基元序列。
    pub ops: Vec<PaintOp>,
    /// 对应帧的生成号。
    pub generation: u64,
}

/// 绘制输入上下文（树镜像 + 布局 + 滚动 + 环境）。
pub struct PaintCtx<'a> {
    pub tree: &'a StyleTree,
    pub styles: &'a HashMap<NodeId, ComputedStyle>,
    pub layout: &'a HashMap<NodeId, (f32, f32, f32, f32)>,
    pub scroll: &'a HashMap<NodeId, (f32, f32)>,
    pub env: &'a MediaEnv,
    /// span 级样式（T5c）：已按节点基样式级联求解。
    pub spans: &'a HashMap<NodeId, Vec<(u32, u32, ComputedStyle)>>,
    /// 文本叶测量所用换行约束（T5c-2）：缺席 = 无界 / 宿主测量。
    pub wrap_widths: &'a HashMap<NodeId, Option<f32>>,
}

/// 构建绘制清单（树序遍历；布局按节点给出 border-box）。
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

/// 生效 z-index（T4d）：position != static 时取解析值，否则按 0（树序）。
/// 残余偏差：flex/grid 子项的 z-index（无 position）不生效。
fn effective_z(ctx: &PaintCtx<'_>, id: NodeId) -> f32 {
    let Some(style) = ctx.styles.get(&id) else {
        return 0.0;
    };
    let positioned = matches!(
        style.get(crate::css::property::PropertyId::Position),
        Some(DeclValue::Position(p)) if !matches!(p, crate::css::property::Position::Static)
    );
    if !positioned {
        return 0.0;
    }
    match style.get(crate::css::property::PropertyId::ZIndex) {
        Some(DeclValue::ZIndex(Some(n))) => *n,
        _ => 0.0,
    }
}

/// 径向几何解析（T4c）：语义值 → 绝对 center/半径（px）。
/// 圆公式取 CSS spec：circle farthest-corner = 到最远角距离；
/// ellipse farthest-corner = fx·√2, fy·√2（fx/fy 为圆心到最远边距离）。
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
        rem: 16.0,
        viewport_w: env.viewport_w,
        viewport_h: env.viewport_h,
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

fn paint_node(ctx: &PaintCtx<'_>, id: NodeId, out: &mut DisplayList) {
    let tree = ctx.tree;
    let styles = ctx.styles;
    let layout = ctx.layout;
    let scroll = ctx.scroll;
    let env = ctx.env;
    let Some(style) = styles.get(&id) else {
        return;
    };
    let Some(&(x, y, w, h)) = layout.get(&id) else {
        return;
    };
    let radius = resolve_radius(style, env);

    // 1) 外阴影（CSS 绘制顺序：先于背景）
    if let Some(DeclValue::BoxShadows(shadows)) = style.get(PropertyId::BoxShadow) {
        for sh in shadows.iter() {
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
            });
        }
    }

    // 2) 背景：渐变优先，否则纯色（透明跳过）
    match style.get(PropertyId::BackgroundImage) {
        Some(DeclValue::BackgroundImage(BackgroundImage::Gradient(g))) => {
            let resolved = Gradient {
                kind: g.kind.clone(),
                stops: g
                    .stops
                    .iter()
                    .map(|s| crate::css::property::ColorStop {
                        color: ColorValue::Absolute(resolve_color(&s.color, style, env)),
                        position: s.position.clone(),
                    })
                    .collect(),
            };
            let radial = match &g.kind {
                crate::css::property::GradientKind::Radial(spec) => {
                    Some(resolve_radial(spec, x, y, w, h, style, env))
                }
                _ => None,
            };
            out.ops.push(PaintOp::Gradient {
                x,
                y,
                width: w,
                height: h,
                radius,
                gradient: resolved,
                radial,
            });
        }
        _ => {
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
    if sides
        .iter()
        .any(|s| s.style != BorderStyle::None && s.width > 0.0)
    {
        out.ops.push(PaintOp::Border {
            x,
            y,
            width: w,
            height: h,
            radius,
            sides,
        });
    }

    // 4) 文本（叶内容；原点 = 内容盒左上）
    if let Some(text) = tree.node(id).text.as_ref() {
        if !text.is_empty() {
            let pad_l = style
                .len(PropertyId::PaddingLeft)
                .map(|lp| px(lp, style, env))
                .unwrap_or(0.0);
            let pad_t = style
                .len(PropertyId::PaddingTop)
                .map(|lp| px(lp, style, env))
                .unwrap_or(0.0);
            let color = resolve_color(&style.color(), style, env);
            // T5c：span 绘制期样式终结（与基样式同一条解析路径）
            let spans = ctx
                .spans
                .get(&id)
                .map(|list| {
                    list.iter()
                        .map(|(start, end, scs)| TextSpanPaint {
                            start: *start,
                            end: *end,
                            color: resolve_color(&scs.color(), scs, env),
                            font_size: scs.font_size_px(),
                            font_weight: scs.font_weight(),
                            italic: scs.font_style() == FontStyle::Italic,
                            font_family: scs.font_family().clone(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            out.ops.push(PaintOp::Text {
                x: x + pad_l,
                y: y + pad_t,
                text: text.clone(),
                color,
                spans,
                font_size: style.font_size_px(),
                font_family: style.font_family().clone(),
                font_weight: style.font_weight(),
                italic: style.font_style() == FontStyle::Italic,
                max_advance: ctx.wrap_widths.get(&id).copied().flatten(),
            });
        }
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
    }
    let mut scrolled = false;
    if let Some(&(dx, dy)) = scroll.get(&id) {
        if dx != 0.0 || dy != 0.0 {
            out.ops.push(PaintOp::PushScroll { dx, dy });
            scrolled = true;
        }
    }
    // 7) 子树：兄弟按生效 z-index 稳定排序（相等保持树序），PushClip/PopScroll 随序配对
    let mut ordered: Vec<(f32, NodeId)> = tree
        .children(id)
        .iter()
        .map(|c| (effective_z(ctx, *c), *c))
        .collect();
    ordered.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    for (_, c) in ordered {
        paint_node(ctx, c, out);
    }
    if scrolled {
        out.ops.push(PaintOp::PopScroll);
    }
    if clip {
        out.ops.push(PaintOp::PopClip);
    }
}

/// currentColor / light-dark 终结为绝对 sRGBA。
fn resolve_color(cv: &ColorValue, style: &ComputedStyle, env: &MediaEnv) -> AlphaColor<Srgb> {
    match (*cv).pick_scheme(env.dark) {
        ColorValue::Absolute(c) => c,
        ColorValue::CurrentColor => match style.color() {
            ColorValue::Absolute(c) => c,
            _ => AlphaColor::new([0.0, 0.0, 0.0, 1.0]),
        },
        ColorValue::LightDark(..) => AlphaColor::new([0.0, 0.0, 0.0, 1.0]), // pick_scheme 已消化
    }
}

fn px(lp: &LengthPercentage, style: &ComputedStyle, env: &MediaEnv) -> f32 {
    lp.resolve(
        &ResolveCtx {
            em: style.font_size_px(),
            rem: 16.0,
            viewport_w: env.viewport_w,
            viewport_h: env.viewport_h,
        },
        0.0,
    )
    .unwrap_or(0.0)
}

fn resolve_radius(style: &ComputedStyle, env: &MediaEnv) -> [f32; 4] {
    [
        PropertyId::BorderTopLeftRadius,
        PropertyId::BorderTopRightRadius,
        PropertyId::BorderBottomRightRadius,
        PropertyId::BorderBottomLeftRadius,
    ]
    .map(|pid| style.len(pid).map(|lp| px(lp, style, env)).unwrap_or(0.0))
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
        let ctx = PaintCtx {
            tree,
            styles: &styles,
            layout: &layout,
            scroll,
            env: &MediaEnv::default(),
            spans: &HashMap::new(),
            wrap_widths: &HashMap::new(),
        };
        build_display_list(&ctx, id, 1, &mut out);
        out
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
        let ctx = PaintCtx {
            tree: &tree,
            styles: &styles,
            layout: &layout,
            scroll: &HashMap::new(),
            env: &env,
            spans: &HashMap::new(),
            wrap_widths: &HashMap::new(),
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
    fn radial_gradient_geometry() {
        // 显式半径 + 关键字圆心：circle 20px at left top → (0,0,r=20)
        let (tree, id, style) = setup(
            "background-image: radial-gradient(circle 20px at left top, red, blue)",
            None,
        );
        let out = run(&tree, id, style, &HashMap::new());
        match &out.ops[0] {
            PaintOp::Gradient { radial, .. } => {
                let g = radial.expect("radial geometry");
                // 盒原点 (10,20)（run 脚手架）+ 圆心 left/top (0,0) + r=20
                assert_eq!((g.cx, g.cy, g.rx, g.ry), (10.0, 20.0, 20.0, 20.0));
            }
            other => panic!("{other:?}"),
        }
        // 默认（无前导段）：盒心 + ellipse farthest-corner → r = 50√2
        let (tree, id, style) = setup("background-image: radial-gradient(red, blue)", None);
        let out = run(&tree, id, style, &HashMap::new());
        match &out.ops[0] {
            PaintOp::Gradient { radial, .. } => {
                let g = radial.expect("radial geometry");
                // 盒 100×50 @ (10,20)：盒心 (60,45)；fx=50→rx=50√2，fy=25→ry=25√2
                let rx = 50.0 * std::f32::consts::SQRT_2;
                let ry = 25.0 * std::f32::consts::SQRT_2;
                assert_eq!((g.cx, g.cy), (60.0, 45.0));
                assert!((g.rx - rx).abs() < 0.01 && (g.ry - ry).abs() < 0.01);
            }
            other => panic!("{other:?}"),
        }
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
    fn gradient_op() {
        let (tree, id, style) = setup(
            "background-image: linear-gradient(to bottom, red, blue)",
            None,
        );
        let out = run(&tree, id, style, &HashMap::new());
        assert_eq!(out.ops.len(), 1);
        match &out.ops[0] {
            PaintOp::Gradient { gradient, .. } => assert_eq!(gradient.stops.len(), 2),
            other => panic!("{other:?}"),
        }
    }
}
