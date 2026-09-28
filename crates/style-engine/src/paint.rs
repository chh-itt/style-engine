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
    /// 文本（T5 转换为字形 run；MVP 记录排版输入）。
    Text {
        x: f32,
        y: f32,
        text: String,
        color: AlphaColor<Srgb>,
        font_size: f32,
        font_family: FontFamilyList,
        font_weight: f32,
        italic: bool,
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
            out.ops.push(PaintOp::Gradient {
                x,
                y,
                width: w,
                height: h,
                radius,
                gradient: resolved,
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
            out.ops.push(PaintOp::Text {
                x: x + pad_l,
                y: y + pad_t,
                text: text.clone(),
                color,
                font_size: style.font_size_px(),
                font_family: style.font_family().clone(),
                font_weight: style.font_weight(),
                italic: style.font_style() == FontStyle::Italic,
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
    for c in tree.children(id) {
        paint_node(ctx, *c, out);
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
        };
        build_display_list(&ctx, id, 1, &mut out);
        out
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
