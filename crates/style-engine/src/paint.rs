//! 绘制清单（L3）：布局 + 计算样式 → 绘制基元序列。
//!
//! 中立交换格式（ADR-0001/0003）：颜色为已解析的绝对 sRGBA
//! （currentColor / light-dark 在此层终结；线性化与混合由 vello sink
//! 承担，ADR-0002）。绘制顺序 = 树序（父先于子；z-index 排序属后续）。
//! 单节点内：阴影 → 背景 → 边框 → 文本。

use crate::computed::ComputedStyle;
use crate::css::property::{
    Attachment, BackgroundBox, BackgroundClip, BackgroundImage, BgSize, BorderImageOutsetComp,
    BorderImageRepeatKind, BorderImageSliceComp, BorderImageWidthComp, BorderStyle, ClipRadius,
    ClipShape, DeclValue, FontStyle, Gradient, LPorAuto, OutlineStyle, Overflow, Position2D,
    PositionComp, PropertyId, RepeatAxis, RepeatXY, TransformFn,
};
use crate::css::property::{FontFamilyList, TextAlign};
use crate::css::stylesheet::MediaEnv;
use crate::css::value::{ColorValue, LengthPercentage, ResolveCtx};
use crate::tree::{NodeId, StyleTree};
use peniko::color::{AlphaColor, Srgb};
use std::collections::HashMap;

/// F2（ADR-0022 D4）：文本装饰绘制单元（叶级；绘制期解析终结）。
#[derive(Debug, Clone, PartialEq)]
pub struct TextDecorationPaint {
    /// 行位集（1=underline 2=overline 4=line-through）。
    pub line: u8,
    /// 线型。
    pub style: crate::css::property::TextDecoStyleKind,
    /// 装饰色（绝对 sRGBA）。
    pub color: AlphaColor<Srgb>,
    /// 厚度 px（声明 LP 解析；auto/from-font 退化 font_size/12）。
    pub thickness_px: f32,
}

/// F2（ADR-0022 D5）：文本阴影绘制单元（列表序即绘制序，影先于字）。
#[derive(Debug, Clone, PartialEq)]
pub struct TextShadowPaint {
    /// 水平偏移 px（右为正）。
    pub dx: f32,
    /// 垂直偏移 px（下为正）。
    pub dy: f32,
    /// 模糊半径 px（0=锐利；sink 多环近似——box-shadow 先例同源）。
    pub blur: f32,
    /// 阴影色（绝对 sRGBA）。
    pub color: AlphaColor<Srgb>,
}

/// 命中几何单元（F3a，ADR-0023）：绘制序收集，后绘=更顶；border-box
/// 视口坐标 + 收集时的活跃 clip 矩形链快照。变换节点=未旋盒（B 级在
/// 案——op 坐标为视口系、PushTransform 由 sink 终结）。
#[derive(Debug, Clone, PartialEq)]
pub struct HitRect {
    /// 所属节点。
    pub node_id: NodeId,
    /// border-box x（视口）。
    pub x: f32,
    /// border-box y（视口）。
    pub y: f32,
    /// border-box 宽。
    pub w: f32,
    /// border-box 高。
    pub h: f32,
    /// 祖先 clip 矩形链（视口，外→内；[x, y, w, h]）。
    pub clips: Vec<[f32; 4]>,
}

/// 命中收集器（F3a，ADR-0023）：paint 期经 `PaintCtx.hit` 透传收集。
#[derive(Default)]
pub struct HitCollector {
    /// 收集序=绘制序（后=顶）。
    pub rects: Vec<HitRect>,
    /// 活跃 clip 矩形链（子树 PushClip 登记 / PopClip 弹出）。
    pub clips: Vec<[f32; 4]>,
}

/// 命中结果（F3a，ADR-0023）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HitTestHit {
    /// 命中节点。
    pub node_id: NodeId,
}

/// F2（ADR-0022 D4）：叶级装饰解析——line 位集非零才产出；thickness=
/// 声明 LP 经 px() 解析，auto/from-font 退化 font_size/12（近似在案）。
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

/// F2（ADR-0022 D5）：叶级阴影解析——声明列表 → 绘制单元（LP→px、
/// color 解析、blur 缺省 0、color 缺省 currentColor）。
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
        /// 锥形几何（C3）：圆心绝对 px + 起始角弧度（非锥形为 None）。
        conic: Option<ConicGeom>,
        /// 线性几何（F3d，ADR-0026）：渐变线绝对端点（非线性为 None）。
        linear: Option<LinearGeom>,
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
        /// 源子域左缘 px（F3d 9-slice；全图 = 0）。
        src_x: f32,
        /// 源子域顶缘 px（全图 = 0）。
        src_y: f32,
        /// 源子域宽 px（全图 = source_w）。
        src_w: f32,
        /// 源子域高 px（全图 = source_h）。
        src_h: f32,
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
        /// C2（ADR-0016）：词内断行强度（sink 重建排版同参——测量与绘制
        /// 断行一致性契约，同 max_advance 通道）。
        word_break: crate::css::property::WordBreakKind,
        /// C2（ADR-0016）：长词溢出断行（sink 重建排版同参）。
        overflow_wrap: crate::css::property::OverflowWrapKind,
        /// F2（ADR-0022 D4）：叶级文本装饰（line 位集非零才非空；绘制期
        /// 已解析颜色/厚度 px）。
        decorations: Vec<TextDecorationPaint>,
        /// F2（ADR-0022 D5）：文本阴影（逗号列表序；sink 影字先绘）。
        shadows: Vec<TextShadowPaint>,
        /// F3d（ADR-0026 D5）：font-stretch 百分比（100 = normal；100 不推
        /// = sink 默认）。基样式级，span 级为已知近似（行高/字距先例）。
        font_stretch: f32,
        /// F3d（ADR-0026 D5）：词距 px（None = normal = sink 默认 0）。
        word_spacing: Option<f32>,
        /// F3d（ADR-0026 D5）：生效 OpenType 特性对（font-feature-settings
        /// ∪ font-variant-caps 派生 titl/unic；空 = sink 默认）。
        font_features: Vec<([u8; 4], u16)>,
        /// F3d（ADR-0026 D5）：变体轴对（font-variation-settings；空 = sink 默认）。
        font_variations: Vec<([u8; 4], f32)>,
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
    /// 多边形裁剪层开始（F3c，ADR-0025：clip-path 非 inset 形状）。
    /// 视口坐标顶点序列（≥3；圆/椭圆绘制期 64 段折线近似，B 级在案）。
    PushClipPath {
        /// 视口坐标顶点 [x, y] px 序列（顺序即多边形周界序，隐式闭合）。
        points: Vec<[f32; 2]>,
        /// 填充规则：true = nonzero（圆/椭圆/polygon 缺省），false =
        /// evenodd（polygon(evenodd, …)）。
        nonzero: bool,
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
    /// 混合层开始（P1-2，css-compositing-1）：mix-blend-mode ≠ normal 或
    /// isolation: isolate 时包住整节点子树——层内容以 `mode` 与背后画布
    /// 合成（Normal = 纯隔离组边界）。混合层必须最外（opacity 层之内）：
    /// 合成序 = blend(背后画布, opacity(子树))。
    PushBlend {
        /// 混合模式（16 标准模式 + plus-lighter/darker）。
        mode: crate::css::property::BlendMode,
        /// 受影响节点盒左缘 x（视口坐标，px，border-box）。
        x: f32,
        /// 受影响节点盒顶缘 y（视口坐标，px，border-box）。
        y: f32,
        /// 受影响节点盒宽 px（border-box）。
        width: f32,
        /// 受影响节点盒高 px（border-box）。
        height: f32,
    },
    /// 混合层结束（对应最近的 PushBlend）。
    PopBlend,
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

/// 已解析的锥形几何（C3，css-images-3；ADR-0017）。绝对 px 圆心；起始角
/// 弧度、自正 X 轴起、顺时针（peniko SweepGradientPosition 语义直接对齐——
/// CSS 0deg=12 点方向经 (deg−90°)·π/180 平移）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConicGeom {
    /// 圆心 x（px，盒子坐标）。
    pub cx: f32,
    /// 圆心 y（px，盒子坐标）。
    pub cy: f32,
    /// 起始角（弧度，正 X 轴起，顺时针）。
    pub start: f32,
}

/// 已解析的线性渐变几何（F3d，ADR-0026）：CSS 渐变线两端点已按绘制盒
/// 解析为绝对 px（css-images-3 §3.2；radial/conic 绝对几何先例的补齐——
/// 9-slice 切片要求九区域共享同一绝对渐变）。paint 层终结；sink 优先
/// 消费，缺省 None = 盒推导回退（既有语义不变）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinearGeom {
    /// 渐变线起点 (x, y)（px，盒子坐标；stop 0 端）。
    pub start: [f32; 2],
    /// 渐变线终点 (x, y)（px，盒子坐标；末 stop 端）。
    pub end: [f32; 2],
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
    /// F2（ADR-0022 D2）：文本截断 override（ellipsis/line-clamp）——
    /// apply_text_truncation 生成；Text op 文本替换消费。
    pub text_overrides: &'a HashMap<NodeId, String>,
    /// 命中收集通道（F3a，ADR-0023；None=不收集零成本）。
    pub hit: Option<&'a std::cell::RefCell<HitCollector>>,
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

/// ADR-0010：向既有 DisplayList 追加一棵子树的绘制基元（不清空、不改
/// generation）——frame 按 root_order 逐根追加（超根无样式不可作为走查
/// 起点；用户根各有样式/布局条目）。
pub(crate) fn append_display_list(ctx: &PaintCtx<'_>, root: NodeId, out: &mut DisplayList) {
    paint_node(ctx, root, out);
}

/// 线性渐变几何解析（F3d，ADR-0026）：CSS 角（0deg=12 点方向顺时针）→
/// 渐变线端点（css-images-3 §3.2：方向 d=(sin θ, −cos θ)（屏幕 y 向下），
/// 线长 L=|w·sin θ|+|h·cos θ|，端点=中心±(L/2)·d）。to-corner 形式解析期
/// 容错拒绝（在案），此处仅需角度式。
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

/// border-image 九片发射（F3d，ADR-0026；css-backgrounds-3 §6.3-6.6）：
/// source ≠ none 时解析切片/带宽/外扩并发九区域基元（4 角拉伸 + 4 边
/// stretch/repeat/round/space + fill 中心），每区域独立 op（ADR-0001
/// 中立性，sink 零逻辑重复）。渐变源九区域共享全盒绝对几何
///（linear/radial/conic）= 切片精确；位图源经 Image 源子域字段裁切
///（vello=仿射子域+裁剪层、soft=采样偏移）。outset 仅位移绘制域
///（ink overflow，不入 scrollable/hit）。返回 true = 已取代 Border op；
/// false = 源缺席或 URL 未注册（调用方回退边框，零副作用契约）。
///
/// B 级在案：渐变源无内在片尺寸 → 全部平铺模式按 stretch；边框圆角不
/// 裁切 9-slice（浏览器按圆角裁边框图，此处直角区域）。
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
        /// 位图源（宿主注册 RGBA）。
        Image(ImageRes),
        /// 渐变源：停止点已终结绝对色；几何按全盒解析（九区域共享）。
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
                stops: g
                    .stops
                    .iter()
                    .map(|s| crate::css::property::ColorStop {
                        color: ColorValue::Absolute(resolve_color(&s.color, style, env)),
                        position: s.position.clone(),
                    })
                    .collect(),
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

    /// 边区平铺发射（css-backgrounds-3 §6.5）：stretch=单片拉伸；repeat=
    /// 源片尺寸顺排末片截断（区域 PushClip）；round=整数片等分拉伸；
    /// space=整数片均布（首尾贴边、间隙均摊），不足一片→单片拉伸。
    /// `tile` = 源片沿轴尺寸（0 = 渐变无内在片尺寸 → stretch，B 级）。
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
        // 单片拉伸（沿轴铺满 len）。
        let stretch = |out: &mut DisplayList| {
            if horizontal {
                emit_region(out, paint, origin, cross_pos, len, thick, sx, sy, swd, shd);
            } else {
                emit_region(out, paint, cross_pos, origin, thick, len, sx, sy, swd, shd);
            }
        };
        if tile <= 0.0 {
            stretch(out);
            return;
        }
        match mode {
            BorderImageRepeatKind::Stretch => stretch(out),
            BorderImageRepeatKind::Repeat => {
                // 顺排、末片截断：区域 PushClip 兜裁。
                out.ops.push(PaintOp::PushClip {
                    x: if horizontal { origin } else { cross_pos },
                    y: if horizontal { cross_pos } else { origin },
                    width: if horizontal { len } else { thick },
                    height: if horizontal { thick } else { len },
                    radius: [0.0; 8],
                });
                let mut p = origin;
                let end = origin + len;
                while p < end {
                    if horizontal {
                        emit_region(
                            out,
                            paint,
                            p,
                            cross_pos,
                            tile.min(end - p),
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
                            p,
                            thick,
                            tile.min(end - p),
                            sx,
                            sy,
                            swd,
                            shd,
                        );
                    }
                    p += tile;
                }
                out.ops.push(PaintOp::PopClip);
            }
            BorderImageRepeatKind::Round => {
                // 整数片等分拉伸：无截断、无裁剪。
                let n = ((len / tile).round() as usize).max(1);
                let tw = len / n as f32;
                for i in 0..n {
                    let p = origin + tw * i as f32;
                    if horizontal {
                        emit_region(out, paint, p, cross_pos, tw, thick, sx, sy, swd, shd);
                    } else {
                        emit_region(out, paint, cross_pos, p, thick, tw, sx, sy, swd, shd);
                    }
                }
            }
            BorderImageRepeatKind::Space => {
                // 整数片均布：首尾贴边、间隙均摊（css-backgrounds-3 space）。
                let n = (len / tile) as usize;
                if n == 0 {
                    stretch(out);
                    return;
                }
                let gap = if n > 1 {
                    (len - tile * n as f32) / (n - 1) as f32
                } else {
                    0.0
                };
                for i in 0..n {
                    let p = origin + (tile + gap) * i as f32;
                    if horizontal {
                        emit_region(out, paint, p, cross_pos, tile, thick, sx, sy, swd, shd);
                    } else {
                        emit_region(out, paint, cross_pos, p, thick, tile, sx, sy, swd, shd);
                    }
                }
            }
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
    // 中心（fill 才绘制；恒拉伸，css-backgrounds-3 middle 无平铺）。
    if fill {
        emit_region(
            out,
            &paint,
            bx + wl,
            by + wt,
            (bwid - wl - wr).max(0.0),
            (bhei - wt - wb).max(0.0),
            sl,
            st,
            mid_w,
            mid_h,
        );
    }
    true
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

/// 锥形几何解析（C3，ADR-0017）：语义值 → 绝对圆心 + 起始角。
/// 角度映射（D1）：CSS 0deg=12 点方向顺时针 → peniko/ConicGeom 0=正 X 轴
/// 顺时针，故 start = (from_deg − 90°)·π/180。
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
                hc.borrow_mut().clips.push([rect.0, rect.1, rect.2, rect.3]);
            }
        }
        ClipEmit::Path { points, nonzero } => {
            let mut min_x = f32::INFINITY;
            let mut min_y = f32::INFINITY;
            let mut max_x = f32::NEG_INFINITY;
            let mut max_y = f32::NEG_INFINITY;
            for p in points {
                min_x = min_x.min(p[0]);
                min_y = min_y.min(p[1]);
                max_x = max_x.max(p[0]);
                max_y = max_y.max(p[1]);
            }
            out.ops.push(PaintOp::PushClipPath {
                points: points.clone(),
                nonzero: *nonzero,
            });
            if let Some(hc) = ctx.hit {
                hc.borrow_mut()
                    .clips
                    .push([min_x, min_y, max_x - min_x, max_y - min_y]);
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
            hc.borrow_mut().rects.push(HitRect {
                node_id: id,
                x,
                y,
                w,
                h,
                clips,
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
    let blend_isolated =
        (blend != crate::css::property::BlendMode::Normal || style.has_isolation())
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
        let tiles_x = tile_positions(geom.dx, geom.dw, geom.tile_x, clip.0, clip.2);
        let tiles_y = tile_positions(geom.dy, geom.dh, geom.tile_y, clip.1, clip.3);
        out.ops.push(PaintOp::PushClip {
            x: clip.0,
            y: clip.1,
            width: clip.2,
            height: clip.3,
            radius: clip_radius,
        });
        for ty in &tiles_y {
            for tx in &tiles_x {
                if let Some(g) = gradient {
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
                            Some(resolve_radial(spec, *tx, *ty, geom.dw, geom.dh, style, env))
                        }
                        _ => None,
                    };
                    let conic = match &g.kind {
                        crate::css::property::GradientKind::Conic(spec) => {
                            Some(resolve_conic(spec, *tx, *ty, geom.dw, geom.dh, style, env))
                        }
                        _ => None,
                    };
                    // 线性几何（F3d，ADR-0026）：渐变线按 tile 盒解析为绝对
                    // 端点（radial/conic 同款 paint 层终结；sink 优先消费）。
                    let linear = match &g.kind {
                        crate::css::property::GradientKind::Linear(angle) => {
                            Some(resolve_linear(angle.0, *tx, *ty, geom.dw, geom.dh))
                        }
                        _ => None,
                    };
                    out.ops.push(PaintOp::Gradient {
                        x: *tx,
                        y: *ty,
                        width: geom.dw,
                        height: geom.dh,
                        radius: [0.0; 8],
                        gradient: resolved,
                        radial,
                        conic,
                        linear,
                    });
                } else if let Some(img) = img {
                    out.ops.push(PaintOp::Image {
                        x: *tx,
                        y: *ty,
                        width: geom.dw,
                        height: geom.dh,
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
    if !paint_border_image(ctx, style, env, x, y, w, h, &sides, out)
        && sides
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
            // 花式线型（double/groove/ridge/inset/outset）与 auto（宿主
            // focus ring 语义位）按 Solid 近似=B 级在案（BorderStyle 实际
            // 仅 None/Hidden/Solid/Dashed/Dotted 五变体）
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
        let mut o_radius = radius;
        for r in &mut o_radius {
            *r = (*r + d).max(0.0);
        }
        out.ops.push(PaintOp::Border {
            x: x - d,
            y: y - d,
            width: w + 2.0 * d,
            height: h + 2.0 * d,
            radius: o_radius,
            sides: std::array::from_fn(|_| BorderSide {
                width: o_width,
                style: o_style,
                color: o_color,
            }),
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
                });
            }
        }
        out.ops.push(PaintOp::Text {
            x: x + pad_l,
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
        // F3a（ADR-0023）：子树裁剪 → 命中 clip 链登记（PopClip 同步弹出）。
        if let Some(hc) = ctx.hit {
            hc.borrow_mut().clips.push([x, y, w, h]);
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
    if faded {
        out.ops.push(PaintOp::PopOpacity);
    }
    if blend_isolated {
        out.ops.push(PaintOp::PopBlend);
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

/// F3b（ADR-0024）：border/padding 四边 px 内嵌。
struct BoxInsets {
    /// 左。
    l: f32,
    /// 右。
    r: f32,
    /// 上。
    t: f32,
    /// 下。
    b: f32,
}

/// 解析 border 与 padding 四边 px（缺失声明 = 0）。
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

/// 背景盒关键字 → 视口矩形（F3b ADR-0024：origin 定位区 / clip 绘制区）。
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

/// LP 语义分量：Percent → 占比×语义基准（定位 = area−img；尺寸 = area），
/// 其余单位 → px()（em/rem/vw 绝对解析）。
fn lp_semantic(lp: &LengthPercentage, basis: f32, style: &ComputedStyle, env: &MediaEnv) -> f32 {
    match lp {
        LengthPercentage::Percent(v) => v * basis,
        other => px(other, style, env),
    }
}

/// 单层绘制几何（F3b ADR-0024）。
struct LayerGeom {
    /// 首 tile 原点 x。
    dx: f32,
    /// 首 tile 原点 y。
    dy: f32,
    /// 绘制宽。
    dw: f32,
    /// 绘制高。
    dh: f32,
    /// 水平平铺。
    tile_x: bool,
    /// 垂直平铺。
    tile_y: bool,
}

/// 单层几何解析（css-backgrounds-3 §3.9 尺寸 / §3.10 定位 / §3.4 平铺；
/// space/round → repeat B 级近似在案）。
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
        tile_x: !matches!(repeat.x, RepeatAxis::NoRepeat),
        tile_y: !matches!(repeat.y, RepeatAxis::NoRepeat),
    })
}

/// 平铺起点枚举：与窗口 [win, win+len) 相交的全部 tile 原点
/// （不平铺 = 单点；size<=0 单点防御）。
fn tile_positions(origin: f32, size: f32, tile: bool, win: f32, len: f32) -> Vec<f32> {
    if !tile || size <= 0.0 || len <= 0.0 {
        return vec![origin];
    }
    let mut out = Vec::new();
    let mut p = origin - ((origin - win) / size).ceil() * size;
    let end = win + len;
    while p < end {
        out.push(p);
        p += size;
    }
    out
}

/// clip-path 发射形（F3c，ADR-0025）。
enum ClipEmit {
    /// 不裁剪（none / 宽容收容 Other）。
    None,
    /// 矩形裁剪（inset：复用 PushClip 逐角圆角精确能力）。
    Rect {
        /// (x, y, w, h) px（参考盒内缩后；宽高收 0）。
        rect: (f32, f32, f32, f32),
        /// 每角 (横, 纵) 圆角 px（序同 FillRect.radius；§5.5 收束后）。
        radius: [f32; 8],
    },
    /// 多边形裁剪（circle/ellipse 64 段折线 / polygon 顶点直传）。
    Path {
        /// 视口坐标顶点。
        points: Vec<[f32; 2]>,
        /// 填充规则（true = nonzero）。
        nonzero: bool,
    },
}

/// 圆/椭圆折线段数（64 段；面积误差 ≈0.15%，B 级在案——锯齿 vs
/// 顶点量折衷，ADR-0025）。
const CLIP_POLY_SEGMENTS: usize = 64;

/// 圆/椭圆内接折线顶点（起始角 0 逆时针；css-shapes-1 §3）。
fn ellipse_points(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<[f32; 2]> {
    (0..CLIP_POLY_SEGMENTS)
        .map(|i| {
            let a = (i as f32) * std::f32::consts::TAU / CLIP_POLY_SEGMENTS as f32;
            [cx + rx * a.cos(), cy + ry * a.sin()]
        })
        .collect()
}

/// clip-path 形状 → 视口坐标发射形（css-shapes-1 §3 + css-masking-1 §5，
/// F3c，ADR-0025）。百分比语义：inset 边距左右基准参考盒宽、上下基准高；
/// 圆角水平集基准宽、垂直集基准高（css-backgrounds-3 §5.5 收束同构）；
/// position 同 background-position 点位语义（right/bottom 解析期负偏移
/// 归一）；circle/ellipse 角关键字 = 圆心到角欧氏距离（圆）/逐轴距离
/// （椭圆）；polygon 顶点 x 基准宽 y 基准高。
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
    fn clip_path_circle_sixty_four_segments() {
        // F3c：circle → PushClipPath 64 段折线（起点 0°、逆时针、首点 =
        // 右极点）；AABB 命中链不在此测（soft 像素锁负责几何正确性）
        let (tree, id, style) = setup("clip-path: circle(20px at 50% 50%)", None);
        let out = run(&tree, id, style, &HashMap::new());
        assert_eq!(out.ops.len(), 2, "{:?}", out.ops);
        match &out.ops[0] {
            PaintOp::PushClipPath { points, nonzero } => {
                assert_eq!(points.len(), 64);
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
            PaintOp::PushClipPath { points, .. } if points.len() == 64
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

    // ===== F3d（ADR-0026）：border-image 九片 + 渐变绝对几何 =====

    /// run 变体：注入宿主已注册位图（border-image 源解析用）。
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
        // repeat 模式：源片尺寸（10px）顺排 80px 上边 → 8 整片 + 区域 PushClip；
        // 角仍单片
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
        // 4 角 + 上 8 + 下 8 + 左 3（30/10） + 右 3 + 中心 = 27
        assert_eq!(img_count, 27, "{:?}", out.ops.len());
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

    #[cfg(feature = "serde")] // paint_dump 模块随 serde 门控（默认 feature 集下编译不过的既有耦合，P1-2 顺手修正）
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
                stops: vec![],
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

    #[test]
    fn text_op_carries_font_deep_fields() {
        // F3d（ADR-0026 D5）：深化字体四字段进入 PaintOp::Text（基样式级）；
        // font-variant-caps: titling-caps 派生 'titl' 特性并入生效表
        let (tree, id, style) = setup(
            "font-stretch: 125%; word-spacing: 4px; font-feature-settings: \"kern\" 0; font-variation-settings: \"wght\" 350; font-variant-caps: titling-caps",
            Some("hi"),
        );
        let out = run(&tree, id, style, &HashMap::new());
        match out.ops.iter().find_map(|op| match op {
            PaintOp::Text { .. } => Some(op),
            _ => None,
        }) {
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
