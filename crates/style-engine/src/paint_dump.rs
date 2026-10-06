//! F3a2（ADR-0023）：DisplayList 可序列化投影（feature = "serde" 门控）。
//!
//! PaintOp 全 16 变体的 typed tagged-enum 镜像（serde derive；色=[f32;4]
//! sRGBA 分量、ImageRes 像素=Vec<u8> 直序列）；枚举值以 canonical 名字符
//! 串承载（重建=名匹配+缺省回退——non_exhaustive 值族演进的诚实边界：
//! 未知 op/单位/枚举名降级或跳过，不 panic）。
//!
//! 入口：[`DisplayList::to_dump`]（序列化源）/ [`DisplayListDump::
//! to_display_list`]（重建）。

use serde::{Deserialize, Serialize};

use crate::css::property::{
    BorderStyle, ConicSpec, FamilyName, Gradient, GradientKind, OverflowWrapKind, RadialShape,
    RadialSize, RadialSpec, TextAlign, TextDecoStyleKind, WordBreakKind,
};
use crate::css::value::{ColorValue, LengthPercentage};
use crate::paint::{
    ConicGeom, DisplayList, ImageRes, LinearGeom, PaintOp, RadialGeom, TextDecorationPaint,
    TextShadowPaint, TextSpanPaint,
};

/// 颜色投影 = sRGBA 分量（AlphaColor\<Srgb\>.components 直序列）。
type ColorDump = [f32; 4];

// ===== 值族镜像 =====

/// `<length-percentage>` 投影：单位名+数值。non_exhaustive 值族——未知
/// 变体以 unit="unknown" 记录并置零（重建丢弃=诚实边界）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LPDump {
    /// 单位名：px/em/rem/%/vw/vh/cqw/cqh/cqi（未知="unknown"）。
    pub unit: String,
    /// 数值。
    pub value: f32,
}

fn lp_dump(lp: &LengthPercentage) -> LPDump {
    let (unit, value) = match lp {
        LengthPercentage::Px(v) => ("px", *v),
        LengthPercentage::Em(v) => ("em", *v),
        LengthPercentage::Rem(v) => ("rem", *v),
        LengthPercentage::Percent(v) => ("%", *v),
        LengthPercentage::Vw(v) => ("vw", *v),
        LengthPercentage::Vh(v) => ("vh", *v),
        LengthPercentage::Cqw(v) => ("cqw", *v),
        LengthPercentage::Cqh(v) => ("cqh", *v),
        LengthPercentage::Cqi(v) => ("cqi", *v),
        LengthPercentage::Cqb(v) => ("cqb", *v),
        LengthPercentage::Ch(v) => ("ch", *v),
        LengthPercentage::Ex(v) => ("ex", *v),
        LengthPercentage::Ic(v) => ("ic", *v),
        // calc() 结构不可序列化——诚实降级（load 丢弃）
        LengthPercentage::Calc(_) => ("unknown", 0.0),
    };
    LPDump {
        unit: unit.to_string(),
        value,
    }
}

fn lp_load(d: &LPDump) -> Option<LengthPercentage> {
    Some(match d.unit.as_str() {
        "px" => LengthPercentage::Px(d.value),
        "em" => LengthPercentage::Em(d.value),
        "rem" => LengthPercentage::Rem(d.value),
        "%" => LengthPercentage::Percent(d.value),
        "vw" => LengthPercentage::Vw(d.value),
        "vh" => LengthPercentage::Vh(d.value),
        "cqw" => LengthPercentage::Cqw(d.value),
        "cqh" => LengthPercentage::Cqh(d.value),
        "cqi" => LengthPercentage::Cqi(d.value),
        "cqb" => LengthPercentage::Cqb(d.value),
        "ch" => LengthPercentage::Ch(d.value),
        "ex" => LengthPercentage::Ex(d.value),
        "ic" => LengthPercentage::Ic(d.value),
        _ => return None, // dump 侧已降级的未知单位
    })
}

/// ColorValue 投影（non_exhaustive——未知变体降级 CurrentColor）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "v")]
pub enum ColorValueDump {
    /// currentcolor。
    CurrentColor,
    /// 绝对 sRGBA。
    Absolute(ColorDump),
    /// light-dark() 两侧。
    LightDark(ColorDump, ColorDump),
}

fn color_dump(cv: &ColorValue) -> ColorValueDump {
    match cv {
        ColorValue::CurrentColor => ColorValueDump::CurrentColor,
        ColorValue::Absolute(c) => ColorValueDump::Absolute(c.components),
        ColorValue::LightDark(a, b) => ColorValueDump::LightDark(a.components, b.components),
    }
}

fn color_load(d: &ColorValueDump) -> ColorValue {
    match *d {
        ColorValueDump::CurrentColor => ColorValue::CurrentColor,
        ColorValueDump::Absolute(c) => ColorValue::Absolute(::peniko::color::AlphaColor::new(c)),
        ColorValueDump::LightDark(a, b) => ColorValue::LightDark(
            ::peniko::color::AlphaColor::new(a),
            ::peniko::color::AlphaColor::new(b),
        ),
    }
}

/// 渐变投影。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GradientDump {
    /// 类型+几何。
    pub kind: GradientKindDump,
    /// repeating（css-images-3，P1-3；缺省 false 兼容旧 dump）。
    #[serde(default)]
    pub repeating: bool,
    /// 停靠点。
    pub stops: Vec<ColorStopDump>,
}

/// 渐变类型投影（non_exhaustive——未知变体降级 Linear 180deg）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GradientKindDump {
    /// 线性（角度°，Angle 度语义）。
    Linear {
        /// 角度（°）。
        deg: f32,
    },
    /// 径向。
    Radial {
        /// circle/ellipse（未知="unknown"）。
        shape: String,
        /// 尺寸。
        size: RadialSizeDump,
        /// 圆心 [x, y]。
        position: [LPDump; 2],
    },
    /// 锥形。
    Conic {
        /// 起始角（°）。
        from_deg: f32,
        /// 圆心 [x, y]。
        position: [LPDump; 2],
    },
}

/// 径向尺寸投影（non_exhaustive——未知变体降级 Named("unknown")）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RadialSizeDump {
    /// 关键字：closest-side/closest-corner/farthest-side/farthest-corner。
    Named(String),
    /// 显式半径。
    Explicit {
        /// 水平半径。
        rx: LPDump,
        /// 垂直半径（椭圆第二个；缺省 None）。
        ry: Option<LPDump>,
    },
}

/// 混合模式投影（P1-2；kebab-case 序列化，与 CSS 关键字同名）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlendModeDump {
    /// normal。
    Normal,
    /// multiply。
    Multiply,
    /// screen。
    Screen,
    /// overlay。
    Overlay,
    /// darken。
    Darken,
    /// lighten。
    Lighten,
    /// color-dodge。
    ColorDodge,
    /// color-burn。
    ColorBurn,
    /// hard-light。
    HardLight,
    /// soft-light。
    SoftLight,
    /// difference。
    Difference,
    /// exclusion。
    Exclusion,
    /// hue。
    Hue,
    /// saturation。
    Saturation,
    /// color。
    Color,
    /// luminosity。
    Luminosity,
    /// plus-lighter。
    PlusLighter,
    /// plus-darker。
    PlusDarker,
}

impl BlendModeDump {
    /// 核心 BlendMode → 投影。
    pub(crate) fn from_core(m: &crate::css::property::BlendMode) -> Self {
        use crate::css::property::BlendMode as B;
        match m {
            B::Normal => Self::Normal,
            B::Multiply => Self::Multiply,
            B::Screen => Self::Screen,
            B::Overlay => Self::Overlay,
            B::Darken => Self::Darken,
            B::Lighten => Self::Lighten,
            B::ColorDodge => Self::ColorDodge,
            B::ColorBurn => Self::ColorBurn,
            B::HardLight => Self::HardLight,
            B::SoftLight => Self::SoftLight,
            B::Difference => Self::Difference,
            B::Exclusion => Self::Exclusion,
            B::Hue => Self::Hue,
            B::Saturation => Self::Saturation,
            B::Color => Self::Color,
            B::Luminosity => Self::Luminosity,
            B::PlusLighter => Self::PlusLighter,
            B::PlusDarker => Self::PlusDarker,
        }
    }

    /// 投影 → 核心 BlendMode。
    pub(crate) fn to_core(self) -> crate::css::property::BlendMode {
        use crate::css::property::BlendMode as B;
        match self {
            Self::Normal => B::Normal,
            Self::Multiply => B::Multiply,
            Self::Screen => B::Screen,
            Self::Overlay => B::Overlay,
            Self::Darken => B::Darken,
            Self::Lighten => B::Lighten,
            Self::ColorDodge => B::ColorDodge,
            Self::ColorBurn => B::ColorBurn,
            Self::HardLight => B::HardLight,
            Self::SoftLight => B::SoftLight,
            Self::Difference => B::Difference,
            Self::Exclusion => B::Exclusion,
            Self::Hue => B::Hue,
            Self::Saturation => B::Saturation,
            Self::Color => B::Color,
            Self::Luminosity => B::Luminosity,
            Self::PlusLighter => B::PlusLighter,
            Self::PlusDarker => B::PlusDarker,
        }
    }
}

/// 停靠点投影。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorStopDump {
    /// 颜色。
    pub color: ColorValueDump,
    /// 位置（None=沿轴自动均布）。
    pub position: Option<LPDump>,
}

fn gradient_dump(g: &Gradient) -> GradientDump {
    let kind = match &g.kind {
        GradientKind::Linear(a) => GradientKindDump::Linear { deg: a.0 },
        GradientKind::Radial(r) => GradientKindDump::Radial {
            shape: match r.shape {
                RadialShape::Circle => "circle",
                RadialShape::Ellipse => "ellipse",
            }
            .to_string(),
            size: match &r.size {
                RadialSize::ClosestSide => RadialSizeDump::Named("closest-side".into()),
                RadialSize::ClosestCorner => RadialSizeDump::Named("closest-corner".into()),
                RadialSize::FarthestSide => RadialSizeDump::Named("farthest-side".into()),
                RadialSize::FarthestCorner => RadialSizeDump::Named("farthest-corner".into()),
                RadialSize::Explicit { rx, ry } => RadialSizeDump::Explicit {
                    rx: lp_dump(rx),
                    ry: ry.as_ref().map(lp_dump),
                },
            },
            position: [lp_dump(&r.position.0), lp_dump(&r.position.1)],
        },
        GradientKind::Conic(c) => GradientKindDump::Conic {
            from_deg: c.from.0,
            position: [lp_dump(&c.position.0), lp_dump(&c.position.1)],
        },
    };
    GradientDump {
        kind,
        repeating: g.repeating,
        stops: g
            .stops
            .iter()
            .map(|s| ColorStopDump {
                color: color_dump(&s.color),
                position: s.position.as_ref().map(lp_dump),
            })
            .collect(),
    }
}

fn gradient_load(d: &GradientDump) -> Gradient {
    let kind = match &d.kind {
        GradientKindDump::Linear { deg } => GradientKind::Linear(crate::css::value::Angle(*deg)),
        GradientKindDump::Radial {
            shape,
            size,
            position,
        } => GradientKind::Radial(RadialSpec {
            shape: match shape.as_str() {
                "circle" => RadialShape::Circle,
                _ => RadialShape::Ellipse, // 缺省（doc 语义）
            },
            size: match size {
                RadialSizeDump::Named(n) => match n.as_str() {
                    "closest-side" => RadialSize::ClosestSide,
                    "closest-corner" => RadialSize::ClosestCorner,
                    "farthest-side" => RadialSize::FarthestSide,
                    _ => RadialSize::FarthestCorner, // 缺省
                },
                RadialSizeDump::Explicit { rx, ry } => RadialSize::Explicit {
                    rx: lp_load(rx).unwrap_or(LengthPercentage::Px(0.0)),
                    ry: ry.as_ref().and_then(lp_load),
                },
            },
            position: (
                lp_load(&position[0]).unwrap_or(LengthPercentage::Percent(0.5)),
                lp_load(&position[1]).unwrap_or(LengthPercentage::Percent(0.5)),
            ),
        }),
        GradientKindDump::Conic { from_deg, position } => GradientKind::Conic(ConicSpec {
            from: crate::css::value::Angle(*from_deg),
            position: (
                lp_load(&position[0]).unwrap_or(LengthPercentage::Percent(0.5)),
                lp_load(&position[1]).unwrap_or(LengthPercentage::Percent(0.5)),
            ),
        }),
    };
    Gradient {
        kind,
        repeating: d.repeating,
        stops: d
            .stops
            .iter()
            .map(|s| crate::css::property::ColorStop {
                color: color_load(&s.color),
                position: s.position.as_ref().and_then(lp_load),
            })
            .collect(),
    }
}

// ===== 绘制面镜像 =====

/// 单边边框投影。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BorderSideDump {
    /// 边宽 px。
    pub width: f32,
    /// 边框样式名（none/solid/dashed/dotted；未知="none"）。
    pub style: String,
    /// 边框色。
    pub color: ColorDump,
}

fn border_style_name(s: BorderStyle) -> &'static str {
    match s {
        BorderStyle::None => "none",
        BorderStyle::Solid => "solid",
        BorderStyle::Dashed => "dashed",
        BorderStyle::Dotted => "dotted",
    }
}

fn border_style_load(s: &str) -> BorderStyle {
    match s {
        "solid" => BorderStyle::Solid,
        "dashed" => BorderStyle::Dashed,
        "dotted" => BorderStyle::Dotted,
        _ => BorderStyle::None,
    }
}

/// 径向几何投影（paint 层已解析绝对 px）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RadialGeomDump {
    /// 圆心 x。
    pub cx: f32,
    /// 圆心 y。
    pub cy: f32,
    /// 水平半径。
    pub rx: f32,
    /// 垂直半径。
    pub ry: f32,
}

/// 锥形几何投影（paint 层已解析）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ConicGeomDump {
    /// 圆心 x。
    pub cx: f32,
    /// 圆心 y。
    pub cy: f32,
    /// 起始角（弧度）。
    pub start: f32,
}

/// 线性几何投影（F3d，ADR-0026；paint 层已解析渐变线绝对端点）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LinearGeomDump {
    /// 渐变线起点 (x, y)。
    pub start: [f32; 2],
    /// 渐变线终点 (x, y)。
    pub end: [f32; 2],
}

/// 富文本 span 投影。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextSpanDump {
    /// span 起始字节偏移（含）。
    pub start: u32,
    /// span 结束字节偏移（不含）。
    pub end: u32,
    /// 文本色。
    pub color: ColorDump,
    /// 字号 px。
    pub font_size: f32,
    /// 字重。
    pub font_weight: f32,
    /// 斜体。
    pub italic: bool,
    /// 字体族列表。
    pub font_family: Vec<String>,
}

/// 文本装饰投影。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextDecorationDump {
    /// 行位集（1=underline 2=overline 4=line-through）。
    pub line: u8,
    /// 线型名（solid/double/dotted/dashed/wavy；未知="solid"）。
    pub style: String,
    /// 装饰色。
    pub color: ColorDump,
    /// 厚度 px。
    pub thickness_px: f32,
}

/// 文本阴影投影。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextShadowDump {
    /// 水平偏移 px。
    pub dx: f32,
    /// 垂直偏移 px。
    pub dy: f32,
    /// 模糊半径 px。
    pub blur: f32,
    /// 阴影色。
    pub color: ColorDump,
}

fn family_name_dump(f: &FamilyName) -> String {
    match f {
        FamilyName::Named(n) => n.clone(),
        FamilyName::Serif => "serif".into(),
        FamilyName::SansSerif => "sans-serif".into(),
        FamilyName::Monospace => "monospace".into(),
        FamilyName::Cursive => "cursive".into(),
        FamilyName::Fantasy => "fantasy".into(),
        FamilyName::SystemUi => "system-ui".into(),
    }
}

fn family_name_load(s: &str) -> FamilyName {
    match s {
        "serif" => FamilyName::Serif,
        "sans-serif" => FamilyName::SansSerif,
        "monospace" => FamilyName::Monospace,
        "cursive" => FamilyName::Cursive,
        "fantasy" => FamilyName::Fantasy,
        "system-ui" => FamilyName::SystemUi,
        other => FamilyName::Named(other.to_string()),
    }
}

fn families_dump(fl: &crate::css::property::FontFamilyList) -> Vec<String> {
    fl.0.iter().map(family_name_dump).collect()
}

fn align_name(a: TextAlign) -> &'static str {
    match a {
        TextAlign::Start => "start",
        TextAlign::End => "end",
        TextAlign::Center => "center",
        TextAlign::Left => "left",
        TextAlign::Right => "right",
        TextAlign::Justify => "justify",
    }
}

fn align_load(s: &str) -> TextAlign {
    match s {
        "end" => TextAlign::End,
        "center" => TextAlign::Center,
        "left" => TextAlign::Left,
        "right" => TextAlign::Right,
        "justify" => TextAlign::Justify,
        _ => TextAlign::Start,
    }
}

fn wb_name(w: WordBreakKind) -> &'static str {
    match w {
        WordBreakKind::Normal => "normal",
        WordBreakKind::BreakAll => "break-all",
        WordBreakKind::KeepAll => "keep-all",
    }
}

fn wb_load(s: &str) -> WordBreakKind {
    match s {
        "break-all" => WordBreakKind::BreakAll,
        "keep-all" => WordBreakKind::KeepAll,
        _ => WordBreakKind::Normal,
    }
}

fn ow_name(w: OverflowWrapKind) -> &'static str {
    match w {
        OverflowWrapKind::Normal => "normal",
        OverflowWrapKind::BreakWord => "break-word",
        OverflowWrapKind::Anywhere => "anywhere",
    }
}

fn ow_load(s: &str) -> OverflowWrapKind {
    match s {
        "break-word" => OverflowWrapKind::BreakWord,
        "anywhere" => OverflowWrapKind::Anywhere,
        _ => OverflowWrapKind::Normal,
    }
}

fn deco_name(s: TextDecoStyleKind) -> &'static str {
    match s {
        TextDecoStyleKind::Solid => "solid",
        TextDecoStyleKind::Double => "double",
        TextDecoStyleKind::Dotted => "dotted",
        TextDecoStyleKind::Dashed => "dashed",
        TextDecoStyleKind::Wavy => "wavy",
    }
}

fn deco_load(s: &str) -> TextDecoStyleKind {
    match s {
        "double" => TextDecoStyleKind::Double,
        "dotted" => TextDecoStyleKind::Dotted,
        "dashed" => TextDecoStyleKind::Dashed,
        "wavy" => TextDecoStyleKind::Wavy,
        _ => TextDecoStyleKind::Solid,
    }
}

fn deco_dump(d: &TextDecorationPaint) -> TextDecorationDump {
    TextDecorationDump {
        line: d.line,
        style: deco_name(d.style).to_string(),
        color: d.color.components,
        thickness_px: d.thickness_px,
    }
}

fn shadow_dump(s: &TextShadowPaint) -> TextShadowDump {
    TextShadowDump {
        dx: s.dx,
        dy: s.dy,
        blur: s.blur,
        color: s.color.components,
    }
}

// ===== op 投影 =====

/// 单 op 投影（typed tagged enum——与 PaintOp 变体一一对应；PaintOp
/// non_exhaustive，未来变体降级 [`OpDump::Unknown`]，重建跳过）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum OpDump {
    /// FillRect。
    #[serde(rename = "fill_rect")]
    FillRect {
        /// 盒 x。
        x: f32,
        /// 盒 y。
        y: f32,
        /// 盒宽。
        width: f32,
        /// 盒高。
        height: f32,
        /// 圆角 [f32; 8]。
        radius: [f32; 8],
        /// 填充色。
        color: ColorDump,
    },
    /// Gradient。
    #[serde(rename = "gradient")]
    Gradient {
        /// 盒 x。
        x: f32,
        /// 盒 y。
        y: f32,
        /// 盒宽。
        width: f32,
        /// 盒高。
        height: f32,
        /// 圆角 [f32; 8]。
        radius: [f32; 8],
        /// 渐变。
        gradient: GradientDump,
        /// 径向几何。
        radial: Option<RadialGeomDump>,
        /// 锥形几何。
        conic: Option<ConicGeomDump>,
        /// 线性几何（F3d，ADR-0026）：渐变线绝对端点（非线性为 None）。
        linear: Option<LinearGeomDump>,
    },
    /// Shadow。
    #[serde(rename = "shadow")]
    Shadow {
        /// 盒 x。
        x: f32,
        /// 盒 y。
        y: f32,
        /// 盒宽。
        width: f32,
        /// 盒高。
        height: f32,
        /// 圆角 [f32; 8]。
        radius: [f32; 8],
        /// 阴影色。
        color: ColorDump,
        /// 水平偏移。
        offset_x: f32,
        /// 垂直偏移。
        offset_y: f32,
        /// 模糊。
        blur: f32,
        /// 外扩/内缩。
        spread: f32,
        /// 内阴影。
        inset: bool,
    },
    /// Image（像素直序列 Vec<u8>）。
    #[serde(rename = "image")]
    Image {
        /// 盒 x。
        x: f32,
        /// 盒 y。
        y: f32,
        /// 盒宽。
        width: f32,
        /// 盒高。
        height: f32,
        /// 圆角 [f32; 8]。
        radius: [f32; 8],
        /// 源宽。
        source_w: u32,
        /// 源高。
        source_h: u32,
        /// 源子域左缘 px（F3d 9-slice；全图 = 0）。
        src_x: f32,
        /// 源子域顶缘 px（全图 = 0）。
        src_y: f32,
        /// 源子域宽 px（全图 = source_w）。
        src_w: f32,
        /// 源子域高 px（全图 = source_h）。
        src_h: f32,
        /// 预解码 RGBA 字节。
        pixels: Vec<u8>,
    },
    /// Border。
    #[serde(rename = "border")]
    Border {
        /// 盒 x。
        x: f32,
        /// 盒 y。
        y: f32,
        /// 盒宽。
        width: f32,
        /// 盒高。
        height: f32,
        /// 圆角 [f32; 8]。
        radius: [f32; 8],
        /// 四边 [top, right, bottom, left]。
        sides: [BorderSideDump; 4],
    },
    /// Text。
    #[serde(rename = "text")]
    Text {
        /// 起点 x。
        x: f32,
        /// 起点 y。
        y: f32,
        /// 全文。
        text: String,
        /// 基础色。
        color: ColorDump,
        /// span 覆盖。
        spans: Vec<TextSpanDump>,
        /// 字号。
        font_size: f32,
        /// 字体族列表。
        font_family: Vec<String>,
        /// 字重。
        font_weight: f32,
        /// 斜体。
        italic: bool,
        /// 折行约束。
        max_advance: Option<f32>,
        /// 行高。
        line_height: Option<f32>,
        /// 字距。
        letter_spacing: f32,
        /// 对齐名。
        text_align: String,
        /// word-break 名。
        word_break: String,
        /// overflow-wrap 名。
        overflow_wrap: String,
        /// 装饰。
        decorations: Vec<TextDecorationDump>,
        /// 阴影。
        shadows: Vec<TextShadowDump>,
        /// font-stretch 百分比（F3d；100 = normal）。
        font_stretch: f32,
        /// 词距 px（F3d；None = normal）。
        word_spacing: Option<f32>,
        /// OpenType 特性对（F3d；tag + value）。
        font_features: Vec<([u8; 4], u16)>,
        /// 变体轴对（F3d；tag + value）。
        font_variations: Vec<([u8; 4], f32)>,
    },
    /// PushClip。
    #[serde(rename = "push_clip")]
    PushClip {
        /// 盒 x。
        x: f32,
        /// 盒 y。
        y: f32,
        /// 盒宽。
        width: f32,
        /// 盒高。
        height: f32,
        /// 圆角 [f32; 8]。
        radius: [f32; 8],
    },
    /// PushClipPath（F3c，ADR-0025）。
    #[serde(rename = "push_clip_path")]
    PushClipPath {
        /// 多边形顶点 [x, y]（视口坐标 px；圆/椭圆 64 段折线近似）。
        points: Vec<[f32; 2]>,
        /// 填充规则：true = nonzero，false = evenodd。
        nonzero: bool,
    },
    /// PopClip。
    #[serde(rename = "pop_clip")]
    PopClip,
    /// PushOpacity。
    #[serde(rename = "push_opacity")]
    PushOpacity {
        /// 透明度。
        alpha: f32,
        /// 盒 x。
        x: f32,
        /// 盒 y。
        y: f32,
        /// 盒宽。
        width: f32,
        /// 盒高。
        height: f32,
    },
    /// PopOpacity。
    #[serde(rename = "pop_opacity")]
    PopOpacity,
    /// PushBlend。
    #[serde(rename = "push_blend")]
    PushBlend {
        /// 混合模式。
        mode: BlendModeDump,
        /// 盒 x。
        x: f32,
        /// 盒 y。
        y: f32,
        /// 盒宽。
        width: f32,
        /// 盒高。
        height: f32,
    },
    /// PopBlend。
    #[serde(rename = "pop_blend")]
    PopBlend,
    /// PushTransform。
    #[serde(rename = "push_transform")]
    PushTransform {
        /// [a, b, c, d, e, f]。
        affine: [f32; 6],
    },
    /// PopTransform。
    #[serde(rename = "pop_transform")]
    PopTransform,
    /// PushScroll。
    #[serde(rename = "push_scroll")]
    PushScroll {
        /// 水平偏移。
        dx: f32,
        /// 垂直偏移。
        dy: f32,
    },
    /// PopScroll。
    #[serde(rename = "pop_scroll")]
    PopScroll,
    /// 未来变体兜底（PaintOp non_exhaustive；重建跳过）。
    #[serde(rename = "unknown")]
    Unknown {
        /// 变体提示名。
        name: String,
    },
}

fn span_dump(s: &TextSpanPaint) -> TextSpanDump {
    TextSpanDump {
        start: s.start,
        end: s.end,
        color: s.color.components,
        font_size: s.font_size,
        font_weight: s.font_weight,
        italic: s.italic,
        font_family: families_dump(&s.font_family),
    }
}

fn op_dump(op: &PaintOp) -> OpDump {
    match op {
        PaintOp::FillRect {
            x,
            y,
            width,
            height,
            radius,
            color,
        } => OpDump::FillRect {
            x: *x,
            y: *y,
            width: *width,
            height: *height,
            radius: *radius,
            color: color.components,
        },
        PaintOp::Gradient {
            x,
            y,
            width,
            height,
            radius,
            gradient,
            radial,
            conic,
            linear,
        } => OpDump::Gradient {
            x: *x,
            y: *y,
            width: *width,
            height: *height,
            radius: *radius,
            gradient: gradient_dump(gradient),
            radial: radial.map(|g| RadialGeomDump {
                cx: g.cx,
                cy: g.cy,
                rx: g.rx,
                ry: g.ry,
            }),
            conic: conic.map(|g| ConicGeomDump {
                cx: g.cx,
                cy: g.cy,
                start: g.start,
            }),
            linear: linear.map(|g| LinearGeomDump {
                start: g.start,
                end: g.end,
            }),
        },
        PaintOp::Shadow {
            x,
            y,
            width,
            height,
            radius,
            color,
            offset_x,
            offset_y,
            blur,
            spread,
            inset,
        } => OpDump::Shadow {
            x: *x,
            y: *y,
            width: *width,
            height: *height,
            radius: *radius,
            color: color.components,
            offset_x: *offset_x,
            offset_y: *offset_y,
            blur: *blur,
            spread: *spread,
            inset: *inset,
        },
        PaintOp::Image {
            x,
            y,
            width,
            height,
            radius,
            source_w,
            source_h,
            src_x,
            src_y,
            src_w,
            src_h,
            pixels,
        } => OpDump::Image {
            x: *x,
            y: *y,
            width: *width,
            height: *height,
            radius: *radius,
            source_w: *source_w,
            source_h: *source_h,
            src_x: *src_x,
            src_y: *src_y,
            src_w: *src_w,
            src_h: *src_h,
            pixels: pixels.rgba.as_ref().as_ref().to_vec(),
        },
        PaintOp::Border {
            x,
            y,
            width,
            height,
            radius,
            sides,
        } => OpDump::Border {
            x: *x,
            y: *y,
            width: *width,
            height: *height,
            radius: *radius,
            sides: [
                BorderSideDump {
                    width: sides[0].width,
                    style: border_style_name(sides[0].style).to_string(),
                    color: sides[0].color.components,
                },
                BorderSideDump {
                    width: sides[1].width,
                    style: border_style_name(sides[1].style).to_string(),
                    color: sides[1].color.components,
                },
                BorderSideDump {
                    width: sides[2].width,
                    style: border_style_name(sides[2].style).to_string(),
                    color: sides[2].color.components,
                },
                BorderSideDump {
                    width: sides[3].width,
                    style: border_style_name(sides[3].style).to_string(),
                    color: sides[3].color.components,
                },
            ],
        },
        PaintOp::Text {
            x,
            y,
            text,
            color,
            spans,
            font_size,
            font_family,
            font_weight,
            italic,
            max_advance,
            line_height,
            letter_spacing,
            text_align,
            word_break,
            overflow_wrap,
            decorations,
            shadows,
            font_stretch,
            word_spacing,
            font_features,
            font_variations,
        } => OpDump::Text {
            x: *x,
            y: *y,
            text: text.clone(),
            color: color.components,
            spans: spans.iter().map(span_dump).collect(),
            font_size: *font_size,
            font_family: families_dump(font_family),
            font_weight: *font_weight,
            italic: *italic,
            max_advance: *max_advance,
            line_height: *line_height,
            letter_spacing: *letter_spacing,
            text_align: align_name(*text_align).to_string(),
            word_break: wb_name(*word_break).to_string(),
            overflow_wrap: ow_name(*overflow_wrap).to_string(),
            decorations: decorations.iter().map(deco_dump).collect(),
            shadows: shadows.iter().map(shadow_dump).collect(),
            font_stretch: *font_stretch,
            word_spacing: *word_spacing,
            font_features: font_features.clone(),
            font_variations: font_variations.clone(),
        },
        PaintOp::PushClip {
            x,
            y,
            width,
            height,
            radius,
        } => OpDump::PushClip {
            x: *x,
            y: *y,
            width: *width,
            height: *height,
            radius: *radius,
        },
        PaintOp::PushClipPath { points, nonzero } => OpDump::PushClipPath {
            points: points.clone(),
            nonzero: *nonzero,
        },
        PaintOp::PopClip => OpDump::PopClip,
        PaintOp::PushOpacity {
            alpha,
            x,
            y,
            width,
            height,
        } => OpDump::PushOpacity {
            alpha: *alpha,
            x: *x,
            y: *y,
            width: *width,
            height: *height,
        },
        PaintOp::PopOpacity => OpDump::PopOpacity,
        PaintOp::PushBlend {
            mode,
            x,
            y,
            width,
            height,
        } => OpDump::PushBlend {
            mode: BlendModeDump::from_core(mode),
            x: *x,
            y: *y,
            width: *width,
            height: *height,
        },
        PaintOp::PopBlend => OpDump::PopBlend,
        PaintOp::PushTransform { affine } => OpDump::PushTransform { affine: *affine },
        PaintOp::PopTransform => OpDump::PopTransform,
        PaintOp::PushScroll { dx, dy } => OpDump::PushScroll { dx: *dx, dy: *dy },
        PaintOp::PopScroll => OpDump::PopScroll,
    }
}

fn op_load(d: &OpDump) -> Option<PaintOp> {
    Some(match d {
        OpDump::FillRect {
            x,
            y,
            width,
            height,
            radius,
            color,
        } => PaintOp::FillRect {
            x: *x,
            y: *y,
            width: *width,
            height: *height,
            radius: *radius,
            color: ::peniko::color::AlphaColor::new(*color),
        },
        OpDump::Gradient {
            x,
            y,
            width,
            height,
            radius,
            gradient,
            radial,
            conic,
            linear,
        } => PaintOp::Gradient {
            x: *x,
            y: *y,
            width: *width,
            height: *height,
            radius: *radius,
            gradient: gradient_load(gradient),
            radial: radial.map(|g| RadialGeom {
                cx: g.cx,
                cy: g.cy,
                rx: g.rx,
                ry: g.ry,
            }),
            conic: conic.map(|g| ConicGeom {
                cx: g.cx,
                cy: g.cy,
                start: g.start,
            }),
            linear: linear.map(|g| LinearGeom {
                start: g.start,
                end: g.end,
            }),
        },
        OpDump::Shadow {
            x,
            y,
            width,
            height,
            radius,
            color,
            offset_x,
            offset_y,
            blur,
            spread,
            inset,
        } => PaintOp::Shadow {
            x: *x,
            y: *y,
            width: *width,
            height: *height,
            radius: *radius,
            color: ::peniko::color::AlphaColor::new(*color),
            offset_x: *offset_x,
            offset_y: *offset_y,
            blur: *blur,
            spread: *spread,
            inset: *inset,
        },
        OpDump::Image {
            x,
            y,
            width,
            height,
            radius,
            source_w,
            source_h,
            src_x,
            src_y,
            src_w,
            src_h,
            pixels,
        } => PaintOp::Image {
            x: *x,
            y: *y,
            width: *width,
            height: *height,
            radius: *radius,
            source_w: *source_w,
            source_h: *source_h,
            src_x: *src_x,
            src_y: *src_y,
            src_w: *src_w,
            src_h: *src_h,
            pixels: ImageRes {
                width: *source_w,
                height: *source_h,
                rgba: std::sync::Arc::new(pixels.clone()),
            },
        },
        OpDump::Border {
            x,
            y,
            width,
            height,
            radius,
            sides,
        } => PaintOp::Border {
            x: *x,
            y: *y,
            width: *width,
            height: *height,
            radius: *radius,
            sides: [
                crate::paint::BorderSide {
                    width: sides[0].width,
                    style: border_style_load(&sides[0].style),
                    color: ::peniko::color::AlphaColor::new(sides[0].color),
                },
                crate::paint::BorderSide {
                    width: sides[1].width,
                    style: border_style_load(&sides[1].style),
                    color: ::peniko::color::AlphaColor::new(sides[1].color),
                },
                crate::paint::BorderSide {
                    width: sides[2].width,
                    style: border_style_load(&sides[2].style),
                    color: ::peniko::color::AlphaColor::new(sides[2].color),
                },
                crate::paint::BorderSide {
                    width: sides[3].width,
                    style: border_style_load(&sides[3].style),
                    color: ::peniko::color::AlphaColor::new(sides[3].color),
                },
            ],
        },
        OpDump::Text {
            x,
            y,
            text,
            color,
            spans,
            font_size,
            font_family,
            font_weight,
            italic,
            max_advance,
            line_height,
            letter_spacing,
            text_align,
            word_break,
            overflow_wrap,
            decorations,
            shadows,
            font_stretch,
            word_spacing,
            font_features,
            font_variations,
        } => PaintOp::Text {
            x: *x,
            y: *y,
            text: text.clone(),
            color: ::peniko::color::AlphaColor::new(*color),
            spans: spans
                .iter()
                .map(|s| TextSpanPaint {
                    start: s.start,
                    end: s.end,
                    color: ::peniko::color::AlphaColor::new(s.color),
                    font_size: s.font_size,
                    font_weight: s.font_weight,
                    italic: s.italic,
                    font_family: crate::css::property::FontFamilyList(
                        smallvec::SmallVec::from_vec(
                            s.font_family.iter().map(|f| family_name_load(f)).collect(),
                        ),
                    ),
                })
                .collect(),
            font_size: *font_size,
            font_family: crate::css::property::FontFamilyList(smallvec::SmallVec::from_vec(
                font_family.iter().map(|f| family_name_load(f)).collect(),
            )),
            font_weight: *font_weight,
            italic: *italic,
            max_advance: *max_advance,
            line_height: *line_height,
            letter_spacing: *letter_spacing,
            text_align: align_load(text_align),
            word_break: wb_load(word_break),
            overflow_wrap: ow_load(overflow_wrap),
            decorations: decorations
                .iter()
                .map(|d| TextDecorationPaint {
                    line: d.line,
                    style: deco_load(&d.style),
                    color: ::peniko::color::AlphaColor::new(d.color),
                    thickness_px: d.thickness_px,
                })
                .collect(),
            shadows: shadows
                .iter()
                .map(|s| TextShadowPaint {
                    dx: s.dx,
                    dy: s.dy,
                    blur: s.blur,
                    color: ::peniko::color::AlphaColor::new(s.color),
                })
                .collect(),
            font_stretch: *font_stretch,
            word_spacing: *word_spacing,
            font_features: font_features.clone(),
            font_variations: font_variations.clone(),
        },
        OpDump::PushClip {
            x,
            y,
            width,
            height,
            radius,
        } => PaintOp::PushClip {
            x: *x,
            y: *y,
            width: *width,
            height: *height,
            radius: *radius,
        },
        OpDump::PushClipPath { points, nonzero } => PaintOp::PushClipPath {
            points: points.clone(),
            nonzero: *nonzero,
        },
        OpDump::PopClip => PaintOp::PopClip,
        OpDump::PushOpacity {
            alpha,
            x,
            y,
            width,
            height,
        } => PaintOp::PushOpacity {
            alpha: *alpha,
            x: *x,
            y: *y,
            width: *width,
            height: *height,
        },
        OpDump::PopOpacity => PaintOp::PopOpacity,
        OpDump::PushBlend {
            mode,
            x,
            y,
            width,
            height,
        } => PaintOp::PushBlend {
            mode: mode.to_core(),
            x: *x,
            y: *y,
            width: *width,
            height: *height,
        },
        OpDump::PopBlend => PaintOp::PopBlend,
        OpDump::PushTransform { affine } => PaintOp::PushTransform { affine: *affine },
        OpDump::PopTransform => PaintOp::PopTransform,
        OpDump::PushScroll { dx, dy } => PaintOp::PushScroll { dx: *dx, dy: *dy },
        OpDump::PopScroll => PaintOp::PopScroll,
        OpDump::Unknown { .. } => return None, // 未知 op 丢弃（诚实边界）
    })
}

/// 一帧 DisplayList 的可序列化投影。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayListDump {
    /// 帧生成号。
    pub generation: u64,
    /// op 序列。
    pub ops: Vec<OpDump>,
}

impl DisplayList {
    /// F3a2（ADR-0023）：投影为可序列化转储（serde JSON/CBOR 等宿主自选
    /// 格式；未来 PaintOp 变体降级 Unknown）。
    pub fn to_dump(&self) -> DisplayListDump {
        DisplayListDump {
            generation: self.generation,
            ops: self.ops.iter().map(op_dump).collect(),
        }
    }
}

impl DisplayListDump {
    /// 重建 DisplayList（未知 op 丢弃=诚实边界；对应 to_dump 已知变体
    /// 无损往返）。
    pub fn to_display_list(&self) -> DisplayList {
        DisplayList {
            ops: self.ops.iter().filter_map(op_load).collect(),
            generation: self.generation,
        }
    }
}
