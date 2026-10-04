//! 绘制清单（L3）：布局 + 计算样式 → 绘制基元序列。
//!
//! 中立交换格式（ADR-0001/0003）：颜色为已解析的绝对 sRGBA
//! （currentColor / light-dark 在此层终结；线性化与混合由 vello sink
//! 承担，ADR-0002）。绘制顺序 = 树序（父先于子；z-index 排序属后续）。
//! 单节点内：阴影 → 背景 → 边框 → 文本。

use crate::computed::ComputedStyle;
use crate::css::property::{
    BackgroundImage, BorderStyle, DeclValue, FontStyle, Gradient, Overflow, PropertyId, TransformFn,
};
use crate::css::property::{FontFamilyList, TextAlign};
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
        /// 盒左缘 x（视口坐标，px，border-box）。
        x: f32,
        /// 盒顶缘 y（视口坐标，px，border-box）。
        y: f32,
        /// 盒宽 px（border-box）。
        width: f32,
        /// 盒高 px（border-box）。
        height: f32,
        /// 每角 (横, 纵) 圆角 px（第五批⑪椭圆圆角；序 tl.x tl.y tr.x tr.y
        /// br.x br.y bl.x bl.y）。
        radius: [f32; 8],
        /// 填充色（绝对 sRGBA）。
        color: AlphaColor<Srgb>,
    },
    /// 渐变背景（linear/radial，语义同 CSS）。
    Gradient {
        /// 盒左缘 x（视口坐标，px，border-box）。
        x: f32,
        /// 盒顶缘 y（视口坐标，px，border-box）。
        y: f32,
        /// 盒宽 px（border-box）。
        width: f32,
        /// 盒高 px（border-box）。
        height: f32,
        /// 每角 (横, 纵) 圆角 px（序同 FillRect.radius）。
        radius: [f32; 8],
        /// 渐变参数（CSS linear-gradient/radial-gradient）。
        gradient: Gradient,
        /// 径向几何（T4c）：圆心与半径已按盒子解析为绝对 px（线性渐变为 None）。
        radial: Option<RadialGeom>,
    },
    /// 阴影（第五批⑩：模糊=sink 多环近似；inset=盒内反转填充）。
    Shadow {
        /// 盒左缘 x（视口坐标，px，border-box）。
        x: f32,
        /// 盒顶缘 y（视口坐标，px，border-box）。
        y: f32,
        /// 盒宽 px（border-box）。
        width: f32,
        /// 盒高 px（border-box）。
        height: f32,
        /// 每角 (横, 纵) 圆角 px（序同 FillRect.radius）。
        radius: [f32; 8],
        /// 阴影颜色（box-shadow <color>，绝对 sRGBA）。
        color: AlphaColor<Srgb>,
        /// 水平偏移 px（box-shadow <offset-x>，右为正）。
        offset_x: f32,
        /// 垂直偏移 px（box-shadow <offset-y>，下为正）。
        offset_y: f32,
        /// 模糊半径 px（box-shadow <blur-radius>）。
        blur: f32,
        /// 外扩/内缩（px）。
        spread: f32,
        /// 内阴影（盒内反转填充）。
        inset: bool,
    },
    /// 背景图（第五批⑨）：宿主预解码 RGBA（零副作用——引擎不取 URL，
    /// 引用经 add_image 注册）；源尺寸与像素自带（DisplayList 自足）。
    Image {
        /// 绘制盒左缘 x（视口坐标，px，border-box）。
        x: f32,
        /// 绘制盒顶缘 y（视口坐标，px，border-box）。
        y: f32,
        /// 绘制盒宽 px（border-box）。
        width: f32,
        /// 绘制盒高 px（border-box）。
        height: f32,
        /// 每角 (横, 纵) 圆角（第五批⑪序）——sink 据此决定是否裁剪。
        radius: [f32; 8],
        /// 源图像素宽（内在尺寸）。
        source_w: u32,
        /// 源图像素高（内在尺寸）。
        source_h: u32,
        /// 预解码 RGBA 像素（宿主注册）。
        pixels: ImageRes,
    },
    /// 边框（四边独立：top/right/bottom/left；style none 或 width 0 的边由 sink 忽略）。
    Border {
        /// 盒左缘 x（视口坐标，px，border-box）。
        x: f32,
        /// 盒顶缘 y（视口坐标，px，border-box）。
        y: f32,
        /// 盒宽 px（border-box）。
        width: f32,
        /// 盒高 px（border-box）。
        height: f32,
        /// 每角 (横, 纵) 圆角 px（序同 FillRect.radius）。
        radius: [f32; 8],
        /// 四边边框，序 [top, right, bottom, left]。
        sides: [BorderSide; 4],
    },
    /// 文本（T5 转换为字形 run；spans 为 T5c 富文本覆盖，可为空）。
    Text {
        /// 文本起点 x（视口坐标，px，内容盒左上）。
        x: f32,
        /// 文本起点 y（视口坐标，px，内容盒左上）。
        y: f32,
        /// 待排版绘制的 UTF-8 文本（叶节点全文）。
        text: String,
        /// 基础文本色（color，绝对 sRGBA；span 可覆盖）。
        color: AlphaColor<Srgb>,
        /// span 覆盖样式（T5c）：绘制期已终结；空 = 无富文本。
        spans: Vec<TextSpanPaint>,
        /// 字号 px（font-size）。
        font_size: f32,
        /// 字体族列表（font-family，按序回退）。
        font_family: FontFamilyList,
        /// 字重（font-weight 数值，400 = normal）。
        font_weight: f32,
        /// 是否斜体（font-style: italic）。
        italic: bool,
        /// 换行约束（T5c-2）：测量与绘制共用同一 max_advance 保证折行一致；None = 无界。
        max_advance: Option<f32>,
        /// 行高（盘点修复）：Some = 绝对 px；None = normal（排版器默认字体度量）。
        line_height: Option<f32>,
        /// 字距 px（0 = 默认）。span 级行高/字距为已知近似（仅基样式生效）。
        letter_spacing: f32,
        /// 行内对齐（第五批⑳）：测量不变宽（折行与盒宽与对齐无关），仅
        /// sink 排版后 align 消费；Start = 排版器默认。
        text_align: TextAlign,
    },
    /// 裁剪层开始（overflow 非 visible）。
    PushClip {
        /// 裁剪盒左缘 x（视口坐标，px，border-box）。
        x: f32,
        /// 裁剪盒顶缘 y（视口坐标，px，border-box）。
        y: f32,
        /// 裁剪盒宽 px（border-box）。
        width: f32,
        /// 裁剪盒高 px（border-box）。
        height: f32,
        /// 每角 (横, 纵) 圆角 px（序同 FillRect.radius）。
        radius: [f32; 8],
    },
    /// 裁剪层结束（对应最近的 PushClip）。
    PopClip,
    /// 透明度层开始（opacity < 1，ADR-0008）：整节点子树以 alpha 合成。
    PushOpacity {
        /// 整层不透明度（0.0–1.0，CSS opacity）。
        alpha: f32,
        /// 受影响节点盒左缘 x（视口坐标，px，border-box）。
        x: f32,
        /// 受影响节点盒顶缘 y（视口坐标，px，border-box）。
        y: f32,
        /// 受影响节点盒宽 px（border-box）。
        width: f32,
        /// 受影响节点盒高 px（border-box）。
        height: f32,
    },
    /// 透明度层结束（对应最近的 PushOpacity）。
    PopOpacity,
    /// 2D 仿射变换层开始（ADR-0009）：本节点子树全部绘制经矩阵变换；
    /// 布局盒保持未变换坐标（taffy 不可见 transform）。
    PushTransform {
        /// [a, b, c, d, e, f]：x' = a·x + c·y + e，y' = b·x + d·y + f。
        affine: [f32; 6],
    },
    /// 变换层结束（对应最近的 PushTransform）。
    PopTransform,
    /// 滚动偏移层开始。
    PushScroll {
        /// 水平滚动偏移 px（等价 scrollLeft，子树内容随之平移）。
        dx: f32,
        /// 垂直滚动偏移 px（等价 scrollTop，子树内容随之平移）。
        dy: f32,
    },
    /// 滚动偏移层结束（对应最近的 PushScroll）。
    PopScroll,
}

/// 单边边框（T4b：宽度/样式/颜色已在 paint 层终结为绝对值）。
#[derive(Debug, Clone, PartialEq)]
pub struct BorderSide {
    /// 边宽 px（border-width，已终结为绝对值）。
    pub width: f32,
    /// 边框样式（border-style；none 或宽 0 由 sink 忽略）。
    pub style: BorderStyle,
    /// 边框色（border-color，绝对 sRGBA）。
    pub color: AlphaColor<Srgb>,
}

/// 已解析的径向几何（绝对 px；椭圆分别给 rx/ry，圆时相等）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadialGeom {
    /// 圆心 x（px，盒子坐标）。
    pub cx: f32,
    /// 圆心 y（px，盒子坐标）。
    pub cy: f32,
    /// 水平半径 px。
    pub rx: f32,
    /// 垂直半径 px。
    pub ry: f32,
}

/// 富文本 span（T5c）：绘制期已终结的覆盖样式；区间 [start, end) 为文本字节偏移。
#[derive(Debug, Clone, PartialEq)]
pub struct TextSpanPaint {
    /// span 起始字节偏移（含）。
    pub start: u32,
    /// span 结束字节偏移（不含）。
    pub end: u32,
    /// 文本色（color，绝对 sRGBA）。
    pub color: AlphaColor<Srgb>,
    /// 字号 px（font-size）。
    pub font_size: f32,
    /// 字重（font-weight 数值，400 = normal）。
    pub font_weight: f32,
    /// 是否斜体（font-style: italic）。
    pub italic: bool,
    /// 字体族列表（font-family，按序回退）。
    pub font_family: crate::css::property::FontFamilyList,
}

/// 一帧的绘制清单。
#[derive(Debug, Clone, Default, PartialEq)]
#[must_use = "绘制清单被丢弃则该帧无法渲染"]
pub struct DisplayList {
    /// 树序基元序列。
    pub ops: Vec<PaintOp>,
    /// 对应帧的生成号。
    pub generation: u64,
}

/// 宿主注册的背景图（第五批⑨）：预解码 RGBA（引擎不取 URL、不解码位图
/// 格式——零副作用）；DisplayList 自足携带像素。rgba 以
/// `Arc<dyn AsRef<[u8]>>` 承载（peniko Blob 同型，免去去size化转换）。
pub struct ImageRes {
    /// 图像像素宽（内在尺寸）。
    pub width: u32,
    /// 图像像素高（内在尺寸）。
    pub height: u32,
    /// 预解码 RGBA 字节（每像素 4 字节，sRGB）。
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

/// 多列列规条带（三期⑤c）：settle_column_rules 结算的几何，坐标相对
/// multicol 容器 border-box 原点（paint 层加容器原点）；引擎逐帧全量
/// 重建（列平衡几何随内容漂移，无稳态缓存）。
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnRuleSeg {
    /// 段左缘 x（px，multicol 容器 border-box 坐标）。
    pub x: f32,
    /// 段顶缘 y（px，multicol 容器 border-box 坐标）。
    pub y: f32,
    /// 段宽 px。
    pub width: f32,
    /// 段高 px。
    pub height: f32,
    /// 规条颜色（column-rule-color，绝对 sRGBA）。
    pub color: AlphaColor<Srgb>,
}

/// 绘制输入上下文（树镜像 + 布局 + 滚动 + 环境）。
pub struct PaintCtx<'a> {
    /// 样式树镜像（提供节点结构与叶文本）。
    pub tree: &'a StyleTree,
    /// 节点 → 计算样式。
    pub styles: &'a HashMap<NodeId, ComputedStyle>,
    /// 节点 → 布局盒 (x, y, w, h)（视口坐标，px，border-box）。
    pub layout: &'a HashMap<NodeId, (f32, f32, f32, f32)>,
    /// 节点 → 滚动偏移 (dx, dy)（px，等价 scrollLeft/scrollTop）。
    pub scroll: &'a HashMap<NodeId, (f32, f32)>,
    /// 媒体环境（媒体查询求值输入）。
    pub env: &'a MediaEnv,
    /// span 级样式（T5c）：已按节点基样式级联求解。
    pub spans: &'a HashMap<NodeId, Vec<(u32, u32, ComputedStyle)>>,
    /// 文本叶测量所用换行约束（T5c-2）：缺席 = 无界 / 宿主测量。
    pub wrap_widths: &'a HashMap<NodeId, Option<f32>>,
    /// 背景图注册表（第五批⑨）：url() 引用 → 宿主预解码 RGBA。
    pub images: &'a HashMap<String, ImageRes>,
    /// 多列列规条带（三期⑤c）：NodeId → 段列表（引擎逐帧重建）。
    pub column_rules: &'a HashMap<NodeId, Vec<ColumnRuleSeg>>,
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

    // transform ≠ none（ADR-0009）：L3 绘制期仿射终结——translate 百分比基 =
    // 自身 border-box，transform-origin 默认 50% 50%（T(o)·M·T(−o)）；层栈序
    // transform → clip → filter → opacity（transform 最外）。布局盒保持未变换
    // 坐标（taffy 不可见 transform）；绘制期终结见 ADR-0009 双时机契约。
    let transformed = style.has_transform();
    if transformed {
        out.ops.push(PaintOp::PushTransform {
            affine: resolve_transform_affine(style, x, y, w, h, env),
        });
    }

    // 0) opacity < 1（ADR-0008）：整节点（背景/边框/文本/子树）包 alpha 合成层；
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
        Some(DeclValue::BackgroundImage(BackgroundImage::Url(reference))) => {
            // 第五批⑨背景图：url() 引用 → 宿主 add_image 注册表解析；
            // 未注册引用告警跳过（零副作用——引擎不取 URL 不解码）。
            // MVP 语义：拉伸至 padding box、无 repeat/size 语义。
            match ctx.images.get(reference) {
                Some(img) => {
                    let clipped = radius.iter().any(|r| *r > 0.0);
                    if clipped {
                        out.ops.push(PaintOp::PushClip {
                            x,
                            y,
                            width: w,
                            height: h,
                            radius,
                        });
                    }
                    out.ops.push(PaintOp::Image {
                        x,
                        y,
                        width: w,
                        height: h,
                        radius,
                        source_w: img.width,
                        source_h: img.height,
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
                        "background-image url not registered; skipped"
                    );
                }
            }
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
            line_height: style.resolved_line_height_px(env),
            letter_spacing: style.resolved_letter_spacing_px(env),
            text_align: style.text_align(),
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
        // isolation: isolate / mix-blend-mode ≠ normal（第五批㉒）——均仅
        // 触发、无效果实现、不产生任何 PaintOp；非定位触发者进 Pos 带键 0
        let sc = cstyle.has_transform()
            || cstyle.has_filter()
            || cstyle.has_clip_path()
            || cstyle.has_will_change_sc()
            || cstyle.has_isolation()
            || cstyle.has_mix_blend();
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
    }
    if faded {
        out.ops.push(PaintOp::PopOpacity);
    }
    if transformed {
        out.ops.push(PaintOp::PopTransform);
    }
}

/// 2D 仿射复合 [a, b, c, d, e, f]（列向量约定：x' = a·x + c·y + e）。
/// 返回 m∘n（先 n 后 m）：M = M·N 的块乘。三期⑥：滚动量程结算复用。
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

/// 绘制期仿射终结（ADR-0009）：函数列表按书写顺序连乘（最右先应用），
/// translate 百分比基 = 自身 border-box 宽/高，最后包 transform-origin
/// 默认 50% 50%：A = T(o)·M·T(−o)。rotate 顺时针（y-down 屏幕坐标）。
/// op 坐标为视口系（盒左上角在 (x,y)），origin = (x,y) + 盒内百分比基点
/// ——二期⑦修复：旧实现漏加盒偏移，offset 盒绕错中心旋转/整体错位。
/// 三期⑥：滚动量程结算复用同一终结（pub(crate)）。
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
        rem: 16.0,
        viewport_w: env.viewport_w,
        viewport_h: env.viewport_h,
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

/// currentColor / light-dark 终结为绝对 sRGBA。
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
            images: &images,
            column_rules: &HashMap::new(),
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
            images: &images,
            column_rules: &HashMap::new(),
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
            images: &images,
            column_rules: &HashMap::new(),
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
            images: &images,
            column_rules: &HashMap::new(),
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
        // 偏心 circle farthest-corner（第四批⑤锁公式）：at 25% 25% →
        // 圆心 (35, 32.5)；fx = max(25, 75) = 75、fy = max(12.5, 37.5) = 37.5，
        // r = √(75² + 37.5²) ≈ 83.8526（最远角 = 右下）
        let (tree, id, style) = setup(
            "background-image: radial-gradient(circle farthest-corner at 25% 25%, red, blue)",
            None,
        );
        let out = run(&tree, id, style, &HashMap::new());
        match &out.ops[0] {
            PaintOp::Gradient { radial, .. } => {
                let g = radial.expect("radial geometry");
                let r = (75.0f32 * 75.0 + 37.5 * 37.5).sqrt();
                assert_eq!((g.cx, g.cy), (35.0, 32.5));
                assert!((g.rx - r).abs() < 0.01 && (g.ry - r).abs() < 0.01);
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
            images: &images,
            column_rules: &HashMap::new(),
        };
        build_display_list(&ctx, id, 1, &mut out);
        match &out.ops[0] {
            PaintOp::Image {
                x,
                y,
                width,
                height,
                source_w,
                source_h,
                pixels,
                ..
            } => {
                assert_eq!((*x, *y, *width, *height), (10.0, 20.0, 100.0, 50.0));
                assert_eq!((*source_w, *source_h), (2, 2));
                assert_eq!(pixels.width, 2);
            }
            other => panic!("{other:?}"),
        }
        // 未注册引用 → 无 Image op（背景纯色臂接管→透明→零 op）
        let (tree, id, style) = setup("background-image: url(res://missing)", None);
        let out = run(&tree, id, style, &HashMap::new());
        assert!(out.ops.is_empty(), "{:?}", out.ops);
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
            images: &HashMap::new(),
            column_rules: &rules,
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
        assert_eq!(out.ops.len(), 1);
        match &out.ops[0] {
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
}
