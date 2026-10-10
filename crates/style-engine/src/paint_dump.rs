//! F3a2 (ADR-0023): serializable projection of a DisplayList (gated by
//! feature = "serde").
//!
//! A typed tagged-enum mirror of all 20 PaintOp variants (serde derive; colors =
//! \[f32;4\] sRGBA components, ImageRes pixels = `Vec<u8>` serialized directly);
//! enum values are carried as canonical name strings (reconstruction = name
//! matching with default fallback — the honest boundary of evolving
//! non_exhaustive value families: unknown ops/units/enum names degrade or are
//! skipped, never panic).
//!
//! Entry points: [`DisplayList::to_dump`] (serialization source) /
//! [`DisplayListDump::to_display_list`] (reconstruction).

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

/// Color projection = sRGBA components (serialized directly from
/// AlphaColor\<Srgb\>.components).
type ColorDump = [f32; 4];

// ===== 值族镜像 =====

/// `<length-percentage>` projection: unit name + value. A non_exhaustive value
/// family — unknown variants are recorded with unit="unknown" and a zero value
/// (dropped on reconstruction = the honest boundary).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LPDump {
    /// Unit name: px/em/rem/%/vw/vh/cqw/cqh/cqi (unknown = "unknown").
    pub unit: String,
    /// Value.
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

/// ColorValue projection (non_exhaustive — unknown variants degrade to
/// CurrentColor).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "v")]
pub enum ColorValueDump {
    /// currentcolor.
    CurrentColor,
    /// Absolute sRGBA.
    Absolute(ColorDump),
    /// Both sides of light-dark().
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

/// Gradient projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GradientDump {
    /// Type + geometry.
    pub kind: GradientKindDump,
    /// repeating (css-images-3, P1-3; defaults to false for old-dump
    /// compatibility).
    #[serde(default)]
    pub repeating: bool,
    /// Gradient stops.
    pub stops: Vec<ColorStopDump>,
    /// Color hints (css-images-3, P9-1a; defaults to empty for old-dump
    /// compatibility).
    #[serde(default)]
    pub hints: Vec<GradientHintDump>,
}

/// Color hint projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GradientHintDump {
    /// The hint sits before the stop at this index.
    pub after_stop: usize,
    /// Hint position.
    pub position: LPDump,
}

/// Gradient kind projection (non_exhaustive — unknown variants degrade to
/// Linear 180deg).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GradientKindDump {
    /// Linear (angle in degrees, Angle degree semantics).
    Linear {
        /// Angle (°).
        deg: f32,
    },
    /// Radial.
    Radial {
        /// circle/ellipse (unknown = "unknown").
        shape: String,
        /// Size.
        size: RadialSizeDump,
        /// Center [x, y].
        position: [LPDump; 2],
    },
    /// Conic.
    Conic {
        /// Start angle (°).
        from_deg: f32,
        /// Center [x, y].
        position: [LPDump; 2],
    },
}

/// Radial size projection (non_exhaustive — unknown variants degrade to
/// Named("unknown")).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RadialSizeDump {
    /// Keyword: closest-side/closest-corner/farthest-side/farthest-corner.
    Named(String),
    /// Explicit radii.
    Explicit {
        /// Horizontal radius.
        rx: LPDump,
        /// Vertical radius (the ellipse's second one; defaults to None).
        ry: Option<LPDump>,
    },
}

/// Blend mode projection (P1-2; serialized as kebab-case, matching the CSS
/// keyword names).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlendModeDump {
    /// normal.
    Normal,
    /// multiply.
    Multiply,
    /// screen.
    Screen,
    /// overlay.
    Overlay,
    /// darken.
    Darken,
    /// lighten.
    Lighten,
    /// color-dodge.
    ColorDodge,
    /// color-burn.
    ColorBurn,
    /// hard-light.
    HardLight,
    /// soft-light.
    SoftLight,
    /// difference.
    Difference,
    /// exclusion.
    Exclusion,
    /// hue.
    Hue,
    /// saturation.
    Saturation,
    /// color.
    Color,
    /// luminosity.
    Luminosity,
    /// plus-lighter.
    PlusLighter,
    /// plus-darker.
    PlusDarker,
}

impl BlendModeDump {
    /// Core BlendMode → projection.
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

    /// Projection → core BlendMode.
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

/// Filter effect projection (P2, ADR-0031 D6): serde form of the paint-domain
/// `FilterEffect` (tag = `fn`, kebab-case function names matching the CSS
/// grammar; numeric semantics identical to the core).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "fn", rename_all = "kebab-case")]
pub enum FilterEffectDump {
    /// blur (radius px; σ = radius/2 converted by the sink).
    Blur {
        /// Radius px.
        radius: f32,
    },
    /// brightness.
    Brightness {
        /// Multiplier.
        amount: f32,
    },
    /// contrast.
    Contrast {
        /// Coefficient.
        amount: f32,
    },
    /// grayscale.
    Grayscale {
        /// Interpolation ratio.
        amount: f32,
    },
    /// sepia.
    Sepia {
        /// Interpolation ratio.
        amount: f32,
    },
    /// saturate.
    Saturate {
        /// Coefficient.
        amount: f32,
    },
    /// invert.
    Invert {
        /// Interpolation ratio.
        amount: f32,
    },
    /// opacity.
    Opacity {
        /// Alpha multiplier.
        amount: f32,
    },
    /// hue-rotate (degrees).
    HueRotate {
        /// Angle.
        degrees: f32,
    },
    /// drop-shadow.
    DropShadow {
        /// x offset px.
        dx: f32,
        /// y offset px.
        dy: f32,
        /// Blur radius px.
        blur: f32,
        /// Resolved sRGBA.
        color: [f32; 4],
    },
}

impl FilterEffectDump {
    /// Core FilterEffect → projection (the unknown catch-all is unreachable
    /// within the same crate; forward-compatibility placeholder = the identity
    /// brightness 1.0).
    pub(crate) fn from_core(f: &crate::paint::FilterEffect) -> Self {
        use crate::paint::FilterEffect as F;
        #[allow(unreachable_patterns)]
        match f {
            F::Blur(r) => Self::Blur { radius: *r },
            F::Brightness(v) => Self::Brightness { amount: *v },
            F::Contrast(v) => Self::Contrast { amount: *v },
            F::Grayscale(v) => Self::Grayscale { amount: *v },
            F::Sepia(v) => Self::Sepia { amount: *v },
            F::Saturate(v) => Self::Saturate { amount: *v },
            F::Invert(v) => Self::Invert { amount: *v },
            F::Opacity(v) => Self::Opacity { amount: *v },
            F::HueRotate(d) => Self::HueRotate { degrees: *d },
            F::DropShadow {
                dx,
                dy,
                blur,
                color,
            } => Self::DropShadow {
                dx: *dx,
                dy: *dy,
                blur: *blur,
                color: color.components,
            },
            _ => Self::Brightness { amount: 1.0 },
        }
    }

    /// Projection → core FilterEffect; unknown variants → None (skipped on
    /// reconstruction).
    pub(crate) fn to_core(self) -> Option<crate::paint::FilterEffect> {
        use crate::paint::FilterEffect as F;
        match self {
            Self::Blur { radius } => Some(F::Blur(radius)),
            Self::Brightness { amount } => Some(F::Brightness(amount)),
            Self::Contrast { amount } => Some(F::Contrast(amount)),
            Self::Grayscale { amount } => Some(F::Grayscale(amount)),
            Self::Sepia { amount } => Some(F::Sepia(amount)),
            Self::Saturate { amount } => Some(F::Saturate(amount)),
            Self::Invert { amount } => Some(F::Invert(amount)),
            Self::Opacity { amount } => Some(F::Opacity(amount)),
            Self::HueRotate { degrees } => Some(F::HueRotate(degrees)),
            Self::DropShadow {
                dx,
                dy,
                blur,
                color,
            } => Some(F::DropShadow {
                dx,
                dy,
                blur,
                color: ::peniko::color::AlphaColor::new(color),
            }),
        }
    }
}

/// Gradient stop projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorStopDump {
    /// Color.
    pub color: ColorValueDump,
    /// Position (None = evenly spaced along the axis).
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
        hints: g
            .hints
            .iter()
            .map(|h| GradientHintDump {
                after_stop: h.after_stop,
                position: lp_dump(&h.position),
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
        hints: d
            .hints
            .iter()
            .filter_map(|h| {
                lp_load(&h.position).map(|position| crate::css::property::GradientHint {
                    after_stop: h.after_stop,
                    position,
                })
            })
            .collect(),
    }
}

// ===== 绘制面镜像 =====

/// Single border side projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BorderSideDump {
    /// Border width px.
    pub width: f32,
    /// Border style name (none/solid/dashed/dotted; unknown = "none").
    pub style: String,
    /// Border color.
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

/// Radial geometry projection (absolute px resolved by the paint layer).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RadialGeomDump {
    /// Center x.
    pub cx: f32,
    /// Center y.
    pub cy: f32,
    /// Horizontal radius.
    pub rx: f32,
    /// Vertical radius.
    pub ry: f32,
}

/// Conic geometry projection (resolved by the paint layer).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ConicGeomDump {
    /// Center x.
    pub cx: f32,
    /// Center y.
    pub cy: f32,
    /// Start angle (radians).
    pub start: f32,
}

/// Linear geometry projection (F3d, ADR-0026; gradient-line absolute endpoints
/// resolved by the paint layer).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LinearGeomDump {
    /// Gradient-line start (x, y).
    pub start: [f32; 2],
    /// Gradient-line end (x, y).
    pub end: [f32; 2],
}

/// Rich-text span projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextSpanDump {
    /// Span start byte offset (inclusive).
    pub start: u32,
    /// Span end byte offset (exclusive).
    pub end: u32,
    /// Text color.
    pub color: ColorDump,
    /// Font size px.
    pub font_size: f32,
    /// Font weight.
    pub font_weight: f32,
    /// Italic.
    pub italic: bool,
    /// Font family list.
    pub font_family: Vec<String>,
    /// Span letter spacing px (P9-5; defaults to 0 for old-dump compatibility).
    #[serde(default)]
    pub letter_spacing: f32,
    /// Span line height px (P9-5; defaults to None = normal, old-dump
    /// compatibility).
    #[serde(default)]
    pub line_height: Option<f32>,
}

/// Text decoration projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextDecorationDump {
    /// Line bit set (1=underline 2=overline 4=line-through).
    pub line: u8,
    /// Line style name (solid/double/dotted/dashed/wavy; unknown = "solid").
    pub style: String,
    /// Decoration color.
    pub color: ColorDump,
    /// Thickness px.
    pub thickness_px: f32,
}

/// Text shadow projection.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextShadowDump {
    /// Horizontal offset px.
    pub dx: f32,
    /// Vertical offset px.
    pub dy: f32,
    /// Blur radius px.
    pub blur: f32,
    /// Shadow color.
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

/// Single-op projection (typed tagged enum — one-to-one with the PaintOp
/// variants; PaintOp is non_exhaustive, so future variants degrade to
/// [`OpDump::Unknown`] and are skipped on reconstruction).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum OpDump {
    /// FillRect.
    #[serde(rename = "fill_rect")]
    FillRect {
        /// Box x.
        x: f32,
        /// Box y.
        y: f32,
        /// Box width.
        width: f32,
        /// Box height.
        height: f32,
        /// Corner radii [f32; 8].
        radius: [f32; 8],
        /// Fill color.
        color: ColorDump,
    },
    /// Gradient.
    #[serde(rename = "gradient")]
    Gradient {
        /// Box x.
        x: f32,
        /// Box y.
        y: f32,
        /// Box width.
        width: f32,
        /// Box height.
        height: f32,
        /// Corner radii [f32; 8].
        radius: [f32; 8],
        /// Gradient.
        gradient: GradientDump,
        /// Radial geometry.
        radial: Option<RadialGeomDump>,
        /// Conic geometry.
        conic: Option<ConicGeomDump>,
        /// Linear geometry (F3d, ADR-0026): gradient-line absolute endpoints
        /// (None for non-linear).
        linear: Option<LinearGeomDump>,
    },
    /// Shadow.
    #[serde(rename = "shadow")]
    Shadow {
        /// Box x.
        x: f32,
        /// Box y.
        y: f32,
        /// Box width.
        width: f32,
        /// Box height.
        height: f32,
        /// Corner radii [f32; 8].
        radius: [f32; 8],
        /// Shadow color.
        color: ColorDump,
        /// Horizontal offset.
        offset_x: f32,
        /// Vertical offset.
        offset_y: f32,
        /// Blur.
        blur: f32,
        /// Spread (outset/inset).
        spread: f32,
        /// Inset shadow.
        inset: bool,
    },
    /// Image (pixels serialized directly as `Vec<u8>`).
    #[serde(rename = "image")]
    Image {
        /// Box x.
        x: f32,
        /// Box y.
        y: f32,
        /// Box width.
        width: f32,
        /// Box height.
        height: f32,
        /// Corner radii [f32; 8].
        radius: [f32; 8],
        /// Source width.
        source_w: u32,
        /// Source height.
        source_h: u32,
        /// Source sub-region left edge px (F3d 9-slice; full image = 0).
        src_x: f32,
        /// Source sub-region top edge px (full image = 0).
        src_y: f32,
        /// Source sub-region width px (full image = source_w).
        src_w: f32,
        /// Source sub-region height px (full image = source_h).
        src_h: f32,
        /// Pre-decoded RGBA bytes.
        pixels: Vec<u8>,
    },
    /// Border.
    #[serde(rename = "border")]
    Border {
        /// Box x.
        x: f32,
        /// Box y.
        y: f32,
        /// Box width.
        width: f32,
        /// Box height.
        height: f32,
        /// Corner radii [f32; 8].
        radius: [f32; 8],
        /// Four sides [top, right, bottom, left].
        sides: [BorderSideDump; 4],
    },
    /// Text。
    #[serde(rename = "text")]
    Text {
        /// Start x.
        x: f32,
        /// Start y.
        y: f32,
        /// Full text.
        text: String,
        /// Base color.
        color: ColorDump,
        /// Span overrides.
        spans: Vec<TextSpanDump>,
        /// Font size.
        font_size: f32,
        /// Font family list.
        font_family: Vec<String>,
        /// Font weight.
        font_weight: f32,
        /// Italic.
        italic: bool,
        /// Line-wrapping constraint.
        max_advance: Option<f32>,
        /// Line height.
        line_height: Option<f32>,
        /// Letter spacing.
        letter_spacing: f32,
        /// Align name.
        text_align: String,
        /// word-break name.
        word_break: String,
        /// overflow-wrap name.
        overflow_wrap: String,
        /// Decorations.
        decorations: Vec<TextDecorationDump>,
        /// Shadows.
        shadows: Vec<TextShadowDump>,
        /// font-stretch percentage (F3d; 100 = normal).
        font_stretch: f32,
        /// Word spacing px (F3d; None = normal).
        word_spacing: Option<f32>,
        /// OpenType feature pairs (F3d; tag + value).
        font_features: Vec<([u8; 4], u16)>,
        /// Variation axis pairs (F3d; tag + value).
        font_variations: Vec<([u8; 4], f32)>,
    },
    /// PushClip.
    #[serde(rename = "push_clip")]
    PushClip {
        /// Box x.
        x: f32,
        /// Box y.
        y: f32,
        /// Box width.
        width: f32,
        /// Box height.
        height: f32,
        /// Corner radii [f32; 8].
        radius: [f32; 8],
    },
    /// PushClipPath (F3c, ADR-0025).
    #[serde(rename = "push_clip_path")]
    PushClipPath {
        /// Polygon vertices [x, y] (viewport px; circle/ellipse approximated by
        /// a 64-segment polyline).
        points: Vec<[f32; 2]>,
        /// Fill rule: true = nonzero, false = evenodd.
        nonzero: bool,
    },
    /// PopClip.
    #[serde(rename = "pop_clip")]
    PopClip,
    /// PushOpacity.
    #[serde(rename = "push_opacity")]
    PushOpacity {
        /// Opacity.
        alpha: f32,
        /// Box x.
        x: f32,
        /// Box y.
        y: f32,
        /// Box width.
        width: f32,
        /// Box height.
        height: f32,
    },
    /// PopOpacity.
    #[serde(rename = "pop_opacity")]
    PopOpacity,
    /// PushBlend.
    #[serde(rename = "push_blend")]
    PushBlend {
        /// Blend mode.
        mode: BlendModeDump,
        /// Box x.
        x: f32,
        /// Box y.
        y: f32,
        /// Box width.
        width: f32,
        /// Box height.
        height: f32,
    },
    /// PopBlend.
    #[serde(rename = "pop_blend")]
    PopBlend,
    /// PushFilter (P2).
    #[serde(rename = "push_filter")]
    PushFilter {
        /// Filter effect chain.
        filters: Vec<FilterEffectDump>,
        /// Box x.
        x: f32,
        /// Box y.
        y: f32,
        /// Box width.
        width: f32,
        /// Box height.
        height: f32,
    },
    /// PopFilter (P2).
    #[serde(rename = "pop_filter")]
    PopFilter,
    /// BackdropFilter (P2).
    #[serde(rename = "backdrop_filter")]
    BackdropFilter {
        /// Filter effect chain.
        filters: Vec<FilterEffectDump>,
        /// Box x.
        x: f32,
        /// Box y.
        y: f32,
        /// Box width.
        width: f32,
        /// Box height.
        height: f32,
    },
    /// PushTransform.
    #[serde(rename = "push_transform")]
    PushTransform {
        /// [a, b, c, d, e, f].
        affine: [f32; 6],
    },
    /// PopTransform.
    #[serde(rename = "pop_transform")]
    PopTransform,
    /// PushScroll.
    #[serde(rename = "push_scroll")]
    PushScroll {
        /// Horizontal offset.
        dx: f32,
        /// Vertical offset.
        dy: f32,
    },
    /// PopScroll.
    #[serde(rename = "pop_scroll")]
    PopScroll,
    /// Catch-all for future variants (PaintOp is non_exhaustive; skipped on
    /// reconstruction).
    #[serde(rename = "unknown")]
    Unknown {
        /// Variant hint name.
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
        letter_spacing: s.letter_spacing,
        line_height: s.line_height,
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
        PaintOp::PushFilter {
            filters,
            x,
            y,
            width,
            height,
        } => OpDump::PushFilter {
            filters: filters.iter().map(FilterEffectDump::from_core).collect(),
            x: *x,
            y: *y,
            width: *width,
            height: *height,
        },
        PaintOp::PopFilter => OpDump::PopFilter,
        PaintOp::BackdropFilter {
            filters,
            x,
            y,
            width,
            height,
        } => OpDump::BackdropFilter {
            filters: filters.iter().map(FilterEffectDump::from_core).collect(),
            x: *x,
            y: *y,
            width: *width,
            height: *height,
        },
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
                    letter_spacing: s.letter_spacing,
                    line_height: s.line_height,
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
        OpDump::PushFilter {
            filters,
            x,
            y,
            width,
            height,
        } => {
            let mut core = Vec::with_capacity(filters.len());
            for f in filters {
                core.extend(f.to_core());
            }
            // 全部未知效果 = 无内容层 → 仍重建恒等空链（层对平衡）
            PaintOp::PushFilter {
                filters: core,
                x: *x,
                y: *y,
                width: *width,
                height: *height,
            }
        }
        OpDump::PopFilter => PaintOp::PopFilter,
        OpDump::BackdropFilter {
            filters,
            x,
            y,
            width,
            height,
        } => {
            let mut core = Vec::with_capacity(filters.len());
            for f in filters {
                core.extend(f.to_core());
            }
            PaintOp::BackdropFilter {
                filters: core,
                x: *x,
                y: *y,
                width: *width,
                height: *height,
            }
        }
        OpDump::PushTransform { affine } => PaintOp::PushTransform { affine: *affine },
        OpDump::PopTransform => PaintOp::PopTransform,
        OpDump::PushScroll { dx, dy } => PaintOp::PushScroll { dx: *dx, dy: *dy },
        OpDump::PopScroll => PaintOp::PopScroll,
        OpDump::Unknown { .. } => return None, // 未知 op 丢弃（诚实边界）
    })
}

/// Serializable projection of one frame's DisplayList.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayListDump {
    /// Frame generation number.
    pub generation: u64,
    /// Op sequence.
    pub ops: Vec<OpDump>,
}

impl DisplayList {
    /// F3a2 (ADR-0023): projects into a serializable dump (serde JSON/CBOR or
    /// whatever wire format the host chooses; future PaintOp variants degrade to
    /// Unknown).
    pub fn to_dump(&self) -> DisplayListDump {
        DisplayListDump {
            generation: self.generation,
            ops: self.ops.iter().map(op_dump).collect(),
        }
    }
}

impl DisplayListDump {
    /// Rebuilds a DisplayList (unknown ops are dropped = the honest boundary;
    /// known variants round-trip losslessly against to_dump).
    pub fn to_display_list(&self) -> DisplayList {
        DisplayList {
            ops: self.ops.iter().filter_map(op_load).collect(),
            generation: self.generation,
        }
    }
}
