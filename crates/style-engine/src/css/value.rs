//! CSS value types and grammars: value parsing for the T0 feature set
//! (FEATURES.md).
//!
//! Conventions: `Percent` always stores a fraction (50% → 0.5, matching
//! cssparser's `unit_value`); angles are always normalized to degrees (deg).
//! `calc()` allows px/em/rem/%/vw/vh mixed with numbers, where numbers may
//! only appear as multiplication/division factors (validated at parse time).
//! Per the MVP lenient policy, spaces around `+`/`-` are not enforced
//! (accepted superset; conformance cases use the canonical form).

use cssparser::{BasicParseError, ParseError, Parser, ToCss, Token, TokenSerializationType};
use peniko::color::{self, AlphaColor, Srgb};

/// Value parsing error (cssparser 0.38's `ParseError` no longer carries an
/// input lifetime parameter).
pub type ValError = ParseError<BasicParseError>;
/// `Result` alias carrying the value parsing error.
pub type ValResult<T> = Result<T, ValError>;

/// Value resolution context: font size, root font size, viewport, and
/// container/font metrics (all supplied by the engine/host; the crate has no
/// side effects).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolveCtx {
    /// Current node font size (em basis).
    pub em: f32,
    /// Root node font size (rem basis).
    pub rem: f32,
    /// Viewport width (vw basis).
    pub viewport_w: f32,
    /// Viewport height (vh basis).
    pub viewport_h: f32,
    /// Container query basis width (cqw/cqi; falls back to the small
    /// viewport when there is no container ancestor).
    pub cq_w: f32,
    /// Container query basis height (cqh/cqb; falls back to the small
    /// viewport when there is no container ancestor).
    pub cq_h: f32,
    /// ch basis: advance of the digit 0 glyph (per em; 0.5 approximation
    /// when the font is unregistered).
    pub ch_per_em: f32,
    /// ex basis: yMax of the x glyph (per em; 0.5 approximation when the
    /// font is unregistered).
    pub ex_per_em: f32,
    /// ic basis: advance of the ideograph U+6C34 (per em; 1.0 when the glyph
    /// is missing).
    pub ic_per_em: f32,
}

impl ResolveCtx {
    /// Base context (container queries fall back to the viewport; font
    /// metrics use approximation defaults) — a convenience constructor for
    /// existing call sites; the engine overrides fields where it has
    /// container/font information.
    pub fn base(em: f32, rem: f32, viewport_w: f32, viewport_h: f32) -> Self {
        Self {
            em,
            rem,
            viewport_w,
            viewport_h,
            cq_w: viewport_w,
            cq_h: viewport_h,
            ch_per_em: 0.5,
            ex_per_em: 0.5,
            ic_per_em: 1.0,
        }
    }
}

/// Font-relative unit metrics (normalized per em; A9). Real values are
/// probed by the engine when a font is registered (css::fontprobe);
/// unregistered families fall back to the CSS approximation conventions
/// (ch=0.5em, ex=0.5em, ic=1em; deviation documented in FEATURES.md).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FontMetrics {
    /// Advance of the digit 0 glyph (ch basis).
    pub ch_per_em: f32,
    /// yMax of the x glyph (ex basis).
    pub ex_per_em: f32,
    /// Advance of the ideograph U+6C34 (ic basis; 1.0 when the glyph is
    /// missing).
    pub ic_per_em: f32,
    /// hhea ascender (P3, ADR-0034 D3: strut metric for vertical-align
    /// text-top/bottom and middle; 0.8em fallback when unregistered).
    pub ascent_per_em: f32,
    /// hhea descender (always positive; 0.2em fallback when unregistered).
    pub descent_per_em: f32,
}

impl Default for FontMetrics {
    fn default() -> Self {
        Self {
            ch_per_em: 0.5,
            ex_per_em: 0.5,
            ic_per_em: 1.0,
            ascent_per_em: 0.8,
            descent_per_em: 0.2,
        }
    }
}

/// `<length-percentage>`: px/em/rem/%/vw/vh/calc().
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum LengthPercentage {
    /// Absolute pixel value (px).
    Px(f32),
    /// Relative to the current node font size (em).
    Em(f32),
    /// Relative to the root node font size (rem).
    Rem(f32),
    /// Fraction (50% → 0.5).
    Percent(f32),
    /// Viewport width fraction (50vw → 0.5).
    Vw(f32),
    /// Viewport height fraction (50vh → 0.5).
    Vh(f32),
    /// Container query width fraction (50cqw → 0.5; A9).
    Cqw(f32),
    /// Container query height fraction (50cqh → 0.5; A9).
    Cqh(f32),
    /// Container inline-axis fraction (50cqi → 0.5; = cqw in horizontal
    /// writing mode; A9).
    Cqi(f32),
    /// Container block-axis fraction (50cqb → 0.5; = cqh in horizontal
    /// writing mode; A9).
    Cqb(f32),
    /// Multiple of the digit 0 glyph advance (2ch; A9, font-relative).
    Ch(f32),
    /// Multiple of the x glyph yMax / x-height (2ex; A9, font-relative).
    Ex(f32),
    /// Multiple of the ideograph U+6C34 advance (2ic; A9, font-relative;
    /// 1em when the glyph is missing).
    Ic(f32),
    /// calc() expression (CSS Values 4 subset).
    Calc(Box<CalcNode>),
}

impl LengthPercentage {
    /// Zero-length convenience value (0px).
    pub fn zero() -> Self {
        Self::Px(0.0)
    }

    /// Resolves the value. `percent_basis` is the length that percentages
    /// refer to (width/height/font size, chosen by the caller).
    pub fn resolve(&self, ctx: &ResolveCtx, percent_basis: f32) -> Option<f32> {
        match self {
            Self::Px(v) => Some(*v),
            Self::Em(v) => Some(v * ctx.em),
            Self::Rem(v) => Some(v * ctx.rem),
            Self::Percent(v) => Some(v * percent_basis),
            Self::Vw(v) => Some(v * ctx.viewport_w),
            Self::Vh(v) => Some(v * ctx.viewport_h),
            // A9：容器查询单位（cqi/cqb 水平书写=cqw/cqh；纵向书写不支持在案）
            Self::Cqw(v) | Self::Cqi(v) => Some(v * ctx.cq_w),
            Self::Cqh(v) | Self::Cqb(v) => Some(v * ctx.cq_h),
            // A9：字体相对单位（度量按节点字号缩放）
            Self::Ch(v) => Some(v * ctx.ch_per_em * ctx.em),
            Self::Ex(v) => Some(v * ctx.ex_per_em * ctx.em),
            Self::Ic(v) => Some(v * ctx.ic_per_em * ctx.em),
            Self::Calc(node) => node.resolve(ctx, percent_basis),
        }
    }

    /// A9: whether the value contains a container-query unit leaf (used at
    /// mapping time to decide deferred resolution — container basis values
    /// only stabilize at layout time, handled exactly like percentage
    /// calc()).
    pub fn has_cq(&self) -> bool {
        match self {
            Self::Cqw(_) | Self::Cqh(_) | Self::Cqi(_) | Self::Cqb(_) => true,
            Self::Calc(node) => node.has_cq(),
            _ => false,
        }
    }
}

/// Unit dimension of a calc() value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CalcUnit {
    /// Unitless number (only usable as a multiplication/division factor).
    Number,
    /// Pixels (px).
    Px,
    /// Relative to the current node font size (em).
    Em,
    /// Relative to the root node font size (rem).
    Rem,
    /// Fraction (50% → 0.5).
    Percent,
    /// Viewport width fraction (50vw → 0.5).
    Vw,
    /// Viewport height fraction (50vh → 0.5).
    Vh,
    /// Container query width fraction (50cqw → 0.5; A9).
    Cqw,
    /// Container query height fraction (50cqh → 0.5; A9).
    Cqh,
    /// Container inline-axis fraction (50cqi → 0.5; = cqw in horizontal
    /// writing mode; A9).
    Cqi,
    /// Container block-axis fraction (50cqb → 0.5; = cqh in horizontal
    /// writing mode; A9).
    Cqb,
    /// Multiple of the digit 0 glyph advance (A9, font-relative).
    Ch,
    /// Multiple of the x glyph yMax / x-height (A9, font-relative).
    Ex,
    /// Multiple of the ideograph U+6C34 advance (A9, font-relative; 1em
    /// when the glyph is missing).
    Ic,
}

/// `calc()` expression tree (CSS Values 4 subset: the four arithmetic
/// operations, nested calc, and parentheses).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum CalcNode {
    /// A value with a unit; `Number` is only allowed as a
    /// multiplication/division factor (validated at parse time).
    Value(f32, CalcUnit),
    /// Addition (a + b).
    Sum(Box<CalcNode>, Box<CalcNode>),
    /// Subtraction (a - b).
    Sub(Box<CalcNode>, Box<CalcNode>),
    /// Multiplication (a × b; one side must be a pure number factor).
    Product(Box<CalcNode>, Box<CalcNode>),
    /// Division by a non-zero number (CSS constraint).
    Divide(Box<CalcNode>, f32),
    /// min(a, b, …) (A6: n-ary arguments folded pairwise; resolvable as a
    /// whole if any argument is resolvable).
    Min(Box<CalcNode>, Box<CalcNode>),
    /// max(a, b, …) (A6, pairwise folding).
    Max(Box<CalcNode>, Box<CalcNode>),
    /// clamp(min, val, max) (A6) = max(min, min(val, max)).
    Clamp(Box<CalcNode>, Box<CalcNode>, Box<CalcNode>),
}

impl CalcNode {
    /// Whether the tree contains a percentage leaf (①calc pass-through:
    /// used at mapping time to decide deferred resolution).
    pub fn has_percent(&self) -> bool {
        match self {
            Self::Value(_, CalcUnit::Percent) => true,
            // 非百分比定值叶
            Self::Value(_, _) => false,
            Self::Sum(a, b)
            | Self::Sub(a, b)
            | Self::Product(a, b)
            | Self::Min(a, b)
            | Self::Max(a, b) => a.has_percent() || b.has_percent(),
            Self::Divide(a, _) => a.has_percent(),
            Self::Clamp(mn, v, mx) => mn.has_percent() || v.has_percent() || mx.has_percent(),
        }
    }

    /// Resolution: evaluates the expression to a px length using `ctx` and
    /// the percentage basis (`None` when it cannot be resolved).
    pub fn resolve(&self, ctx: &ResolveCtx, percent_basis: f32) -> Option<f32> {
        match self {
            Self::Value(v, u) => Some(match u {
                // 解析期已拒绝；防御性处理
                CalcUnit::Number => return None,
                CalcUnit::Px => *v,
                CalcUnit::Em => v * ctx.em,
                CalcUnit::Rem => v * ctx.rem,
                CalcUnit::Percent => v * percent_basis,
                CalcUnit::Vw => v * ctx.viewport_w,
                CalcUnit::Vh => v * ctx.viewport_h,
                // A9：容器查询/字体相对单位（与 LengthPercentage 直变体同式）
                CalcUnit::Cqw | CalcUnit::Cqi => v * ctx.cq_w,
                CalcUnit::Cqh | CalcUnit::Cqb => v * ctx.cq_h,
                CalcUnit::Ch => v * ctx.ch_per_em * ctx.em,
                CalcUnit::Ex => v * ctx.ex_per_em * ctx.em,
                CalcUnit::Ic => v * ctx.ic_per_em * ctx.em,
            }),
            Self::Sum(a, b) => {
                Some(a.resolve(ctx, percent_basis)? + b.resolve(ctx, percent_basis)?)
            }
            Self::Sub(a, b) => {
                Some(a.resolve(ctx, percent_basis)? - b.resolve(ctx, percent_basis)?)
            }
            Self::Product(a, b) => {
                // 数字系数取原始标量，另一侧按量纲解析（长度×长度解析期已拒绝）
                let v = match (a.is_pure_number(), b.is_pure_number()) {
                    (true, false) => a.raw_number()? * b.resolve(ctx, percent_basis)?,
                    (false, true) => a.resolve(ctx, percent_basis)? * b.raw_number()?,
                    _ => return None,
                };
                Some(v)
            }
            Self::Divide(a, n) => Some(a.resolve(ctx, percent_basis)? / n),
            Self::Min(a, b) => Some(
                a.resolve(ctx, percent_basis)?
                    .min(b.resolve(ctx, percent_basis)?),
            ),
            Self::Max(a, b) => Some(
                a.resolve(ctx, percent_basis)?
                    .max(b.resolve(ctx, percent_basis)?),
            ),
            // CSS：clamp(MIN, VAL, MAX) = max(MIN, min(VAL, MAX))
            Self::Clamp(mn, v, mx) => Some(
                mn.resolve(ctx, percent_basis)?.max(
                    v.resolve(ctx, percent_basis)?
                        .min(mx.resolve(ctx, percent_basis)?),
                ),
            ),
        }
    }

    /// Raw scalar of a pure-number leaf (only used in the Product factor
    /// context).
    fn raw_number(&self) -> Option<f32> {
        match self {
            Self::Value(v, CalcUnit::Number) => Some(*v),
            _ => None,
        }
    }

    fn is_pure_number(&self) -> bool {
        match self {
            Self::Value(_, CalcUnit::Number) => true,
            Self::Sum(a, b)
            | Self::Sub(a, b)
            | Self::Product(a, b)
            | Self::Min(a, b)
            | Self::Max(a, b) => a.is_pure_number() && b.is_pure_number(),
            Self::Divide(a, _) => a.is_pure_number(),
            Self::Clamp(mn, v, mx) => {
                mn.is_pure_number() && v.is_pure_number() && mx.is_pure_number()
            }
            Self::Value(..) => false,
        }
    }

    /// A9: whether the tree contains a container-query unit leaf (recursive;
    /// calc body of `LengthPercentage::has_cq`).
    pub fn has_cq(&self) -> bool {
        match self {
            Self::Value(_, u) => matches!(
                u,
                CalcUnit::Cqw | CalcUnit::Cqh | CalcUnit::Cqi | CalcUnit::Cqb
            ),
            Self::Sum(a, b)
            | Self::Sub(a, b)
            | Self::Product(a, b)
            | Self::Min(a, b)
            | Self::Max(a, b) => a.has_cq() || b.has_cq(),
            Self::Divide(a, _) => a.has_cq(),
            Self::Clamp(mn, v, mx) => mn.has_cq() || v.has_cq() || mx.has_cq(),
        }
    }
}

/// An angle, normalized to degrees (deg).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Angle(pub f32);

/// A CSS color: an absolute color, currentColor, or light-dark().
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum ColorValue {
    /// The currentcolor keyword (inherits the computed value of the color
    /// property).
    CurrentColor,
    /// Absolute color (already converted to sRGB; components are encoded
    /// values in \[0,1\]).
    Absolute(AlphaColor<Srgb>),
    /// MVP deviation: the parameters only store absolute colors
    /// (currentcolor / nested light-dark goes through fault-tolerant
    /// dropping).
    LightDark(AlphaColor<Srgb>, AlphaColor<Srgb>),
}

impl ColorValue {
    /// Picks one side of light-dark() based on the environment color scheme;
    /// everything else is returned as-is (part of Environment evaluation).
    pub fn pick_scheme(self, dark: bool) -> ColorValue {
        match self {
            Self::LightDark(a, b) => Self::Absolute(if dark { b } else { a }),
            other => other,
        }
    }
}

/// Converts to sRGB and clamps to [0,1] (MVP stand-in for gamut mapping:
/// clipping, a known deviation).
fn to_srgb_clamped(c: color::DynamicColor) -> AlphaColor<Srgb> {
    let mut a = c.to_alpha_color::<Srgb>();
    a.components = a.components.map(|v| v.clamp(0.0, 1.0));
    a
}

/// Parses `<length-percentage>`: px/em/rem/%/vw/vh/calc().
pub fn parse_length_percentage(p: &mut Parser<'_>) -> ValResult<LengthPercentage> {
    match p.next()?.clone() {
        Token::Dimension {
            value, ref unit, ..
        } => {
            if unit.eq_ignore_ascii_case("px") {
                Ok(LengthPercentage::Px(value))
            } else if unit.eq_ignore_ascii_case("em") {
                Ok(LengthPercentage::Em(value))
            } else if unit.eq_ignore_ascii_case("rem") {
                Ok(LengthPercentage::Rem(value))
            } else if unit.eq_ignore_ascii_case("vw") {
                Ok(LengthPercentage::Vw(value / 100.0))
            } else if unit.eq_ignore_ascii_case("vh") {
                Ok(LengthPercentage::Vh(value / 100.0))
            // A9：容器查询单位（按小数存，同 vw/vh 惯例）
            } else if unit.eq_ignore_ascii_case("cqw") {
                Ok(LengthPercentage::Cqw(value / 100.0))
            } else if unit.eq_ignore_ascii_case("cqh") {
                Ok(LengthPercentage::Cqh(value / 100.0))
            } else if unit.eq_ignore_ascii_case("cqi") {
                Ok(LengthPercentage::Cqi(value / 100.0))
            } else if unit.eq_ignore_ascii_case("cqb") {
                Ok(LengthPercentage::Cqb(value / 100.0))
            // A9：字体相对单位（按倍数存，同 em 惯例）
            } else if unit.eq_ignore_ascii_case("ch") {
                Ok(LengthPercentage::Ch(value))
            } else if unit.eq_ignore_ascii_case("ex") {
                Ok(LengthPercentage::Ex(value))
            } else if unit.eq_ignore_ascii_case("ic") {
                Ok(LengthPercentage::Ic(value))
            } else {
                Err(p.new_error_for_next_token())
            }
        }
        Token::Percentage { unit_value, .. } => Ok(LengthPercentage::Percent(unit_value)),
        // 无单位数字仅 0 可作长度
        Token::Number { value: 0.0, .. } => Ok(LengthPercentage::Px(0.0)),
        Token::Function(ref name) if name.eq_ignore_ascii_case("calc") => {
            Ok(LengthPercentage::Calc(Box::new(parse_calc_body(p)?)))
        }
        // A6：min()/max()/clamp() 与 calc() 同为长度语境可定值数学函数
        //（结果须量纲值——纯数字拒绝同 calc 体）。
        Token::Function(ref name)
            if name.eq_ignore_ascii_case("min")
                || name.eq_ignore_ascii_case("max")
                || name.eq_ignore_ascii_case("clamp") =>
        {
            let name = name.clone();
            let node = parse_math_body(p, &name)?;
            if node.is_pure_number() {
                return Err(p.new_error_for_next_token());
            }
            Ok(LengthPercentage::Calc(Box::new(node)))
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

fn parse_calc_body(p: &mut Parser<'_>) -> ValResult<CalcNode> {
    let node = p.parse_nested_block(parse_calc_sum)?;
    if node.is_pure_number() {
        // 长度上下文的 calc 结果必须是量纲值
        return Err(p.new_error_for_next_token());
    }
    Ok(node)
}

fn parse_calc_sum(p: &mut Parser<'_>) -> ValResult<CalcNode> {
    let mut left = parse_calc_product(p)?;
    loop {
        let op = p.try_parse(|p| -> ValResult<u8> {
            Ok(match p.next()?.clone() {
                Token::Delim('+') => b'+',
                Token::Delim('-') => b'-',
                _ => return Err(p.new_error_for_next_token()),
            })
        });
        let Ok(op) = op else { break };
        let right = parse_calc_product(p)?;
        left = match op {
            b'+' => CalcNode::Sum(Box::new(left), Box::new(right)),
            _ => CalcNode::Sub(Box::new(left), Box::new(right)),
        };
    }
    Ok(left)
}

fn parse_calc_product(p: &mut Parser<'_>) -> ValResult<CalcNode> {
    let mut left = parse_calc_value(p)?;
    loop {
        let op = p.try_parse(|p| -> ValResult<u8> {
            Ok(match p.next()?.clone() {
                Token::Delim('*') => b'*',
                Token::Delim('/') => b'/',
                _ => return Err(p.new_error_for_next_token()),
            })
        });
        let Ok(op) = op else { break };
        let right = parse_calc_value(p)?;
        left = match op {
            b'*' => {
                // CSS 禁止长度×长度：一侧必须是纯数字
                if !left.is_pure_number() && !right.is_pure_number() {
                    return Err(p.new_error_for_next_token());
                }
                CalcNode::Product(Box::new(left), Box::new(right))
            }
            _ => match right {
                CalcNode::Value(n, CalcUnit::Number) if n != 0.0 => {
                    CalcNode::Divide(Box::new(left), n)
                }
                _ => return Err(p.new_error_for_next_token()),
            },
        };
    }
    Ok(left)
}

fn parse_calc_value(p: &mut Parser<'_>) -> ValResult<CalcNode> {
    match p.next()?.clone() {
        Token::Number { value, .. } => Ok(CalcNode::Value(value, CalcUnit::Number)),
        Token::Percentage { unit_value, .. } => Ok(CalcNode::Value(unit_value, CalcUnit::Percent)),
        Token::Dimension {
            value, ref unit, ..
        } => {
            let u = if unit.eq_ignore_ascii_case("px") {
                CalcUnit::Px
            } else if unit.eq_ignore_ascii_case("em") {
                CalcUnit::Em
            } else if unit.eq_ignore_ascii_case("rem") {
                CalcUnit::Rem
            } else if unit.eq_ignore_ascii_case("vw") {
                CalcUnit::Vw
            } else if unit.eq_ignore_ascii_case("vh") {
                CalcUnit::Vh
            // A9：容器查询/字体相对单位（calc 体同链路）
            } else if unit.eq_ignore_ascii_case("cqw") {
                CalcUnit::Cqw
            } else if unit.eq_ignore_ascii_case("cqh") {
                CalcUnit::Cqh
            } else if unit.eq_ignore_ascii_case("cqi") {
                CalcUnit::Cqi
            } else if unit.eq_ignore_ascii_case("cqb") {
                CalcUnit::Cqb
            } else if unit.eq_ignore_ascii_case("ch") {
                CalcUnit::Ch
            } else if unit.eq_ignore_ascii_case("ex") {
                CalcUnit::Ex
            } else if unit.eq_ignore_ascii_case("ic") {
                CalcUnit::Ic
            } else {
                return Err(p.new_error_for_next_token());
            };
            // vw/vh/cqw/cqh/cqi/cqb 与百分比一样按小数存（50vw → 0.5）
            let v = if matches!(
                u,
                CalcUnit::Vw
                    | CalcUnit::Vh
                    | CalcUnit::Cqw
                    | CalcUnit::Cqh
                    | CalcUnit::Cqi
                    | CalcUnit::Cqb
            ) {
                value / 100.0
            } else {
                value
            };
            Ok(CalcNode::Value(v, u))
        }
        Token::ParenthesisBlock => p.parse_nested_block(parse_calc_sum),
        Token::Function(ref name) if name.eq_ignore_ascii_case("calc") => {
            p.parse_nested_block(parse_calc_sum)
        }
        // A6 数学函数：min/max 逗号参数二元折叠；clamp 恰三参；嵌套数学
        // 函数经 parse_calc_value 递归支持。
        Token::Function(ref name)
            if name.eq_ignore_ascii_case("min")
                || name.eq_ignore_ascii_case("max")
                || name.eq_ignore_ascii_case("clamp") =>
        {
            parse_math_body(p, name)
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// Argument body of min()/max()/clamp() (the Function name has already been
/// consumed by the caller).
fn parse_math_body(p: &mut Parser<'_>, name: &str) -> ValResult<CalcNode> {
    if name.eq_ignore_ascii_case("clamp") {
        let args = p.parse_nested_block(|p| p.parse_comma_separated(parse_calc_sum))?;
        if args.len() != 3 {
            return Err(p.new_error_for_next_token());
        }
        let mut it = args.into_iter();
        let mn = it.next().unwrap();
        let v = it.next().unwrap();
        let mx = it.next().unwrap();
        return Ok(CalcNode::Clamp(Box::new(mn), Box::new(v), Box::new(mx)));
    }
    let is_min = name.eq_ignore_ascii_case("min");
    let args = p.parse_nested_block(|p| p.parse_comma_separated(parse_calc_sum))?;
    let mut it = args.into_iter();
    let first = it.next().ok_or_else(|| p.new_error_for_next_token())?;
    let mut acc = first;
    for b in it {
        acc = if is_min {
            CalcNode::Min(Box::new(acc), Box::new(b))
        } else {
            CalcNode::Max(Box::new(acc), Box::new(b))
        };
    }
    Ok(acc)
}

/// Parses `<angle>` (deg/grad/rad/turn, normalized to degrees; a unitless
/// number is treated as deg).
pub fn parse_angle(p: &mut Parser<'_>) -> ValResult<Angle> {
    match p.next()?.clone() {
        Token::Dimension {
            value, ref unit, ..
        } => {
            let deg = if unit.eq_ignore_ascii_case("deg") {
                value
            } else if unit.eq_ignore_ascii_case("grad") {
                value * 0.9
            } else if unit.eq_ignore_ascii_case("rad") {
                value.to_degrees()
            } else if unit.eq_ignore_ascii_case("turn") {
                value * 360.0
            } else {
                return Err(p.new_error_for_next_token());
            };
            Ok(Angle(deg))
        }
        // 部分上下文允许无单位数字（视为 deg）
        Token::Number { value, .. } => Ok(Angle(value)),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// Parses `<number>` (unitless number).
pub fn parse_number(p: &mut Parser<'_>) -> ValResult<f32> {
    match p.next()?.clone() {
        Token::Number { value, .. } => Ok(value),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// Parses `<color>`: keywords, #hex, or color functions (light-dark() and
/// the CSS Color 4 grammar).
pub fn parse_color_value(p: &mut Parser<'_>) -> ValResult<ColorValue> {
    match p.next()?.clone() {
        Token::Ident(ref name) => parse_color_keyword(name, p),
        Token::Hash(value) | Token::IDHash(value) => delegate_parse_color(&format!("#{value}"), p),
        Token::Function(ref name) if name.eq_ignore_ascii_case("light-dark") => {
            let (a, b) = p.parse_nested_block(|p| {
                let a = require_absolute(parse_color_value(p)?, p)?;
                p.expect_comma()?;
                let b = require_absolute(parse_color_value(p)?, p)?;
                Ok((a, b))
            })?;
            Ok(ColorValue::LightDark(a, b))
        }
        // 其余颜色函数整段 token 流重构后交给 color crate 的 CSS Color 4 文法
        Token::Function(ref name) => {
            let name = name.to_string();
            let body = serialize_nested_tokens(p)?;
            delegate_parse_color(&format!("{name}({body})"), p)
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// MVP deviation: light-dark() arguments only support absolute colors;
/// anything else goes through fault-tolerant dropping.
fn require_absolute(v: ColorValue, p: &mut Parser<'_>) -> ValResult<AlphaColor<Srgb>> {
    match v {
        ColorValue::Absolute(c) => Ok(c),
        _ => Err(p.new_error_for_next_token()),
    }
}

fn parse_color_keyword(name: &str, p: &mut Parser<'_>) -> ValResult<ColorValue> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "currentcolor" => return Ok(ColorValue::CurrentColor),
        "transparent" => return Ok(ColorValue::Absolute(AlphaColor::new([0.0, 0.0, 0.0, 0.0]))),
        _ => {}
    }
    delegate_parse_color(&lower, p)
}

fn delegate_parse_color(s: &str, p: &mut Parser<'_>) -> ValResult<ColorValue> {
    match color::parse_color(s) {
        Ok(c) => Ok(ColorValue::Absolute(to_srgb_clamped(c))),
        Err(_) => Err(p.new_error_for_next_token()),
    }
}

/// Deserializes the token stream of the current function/block into a
/// string (preserving whitespace and separator semantics).
fn serialize_nested_tokens(p: &mut Parser<'_>) -> ValResult<String> {
    p.parse_nested_block(|p| {
        let mut out = String::new();
        let mut prev: Option<TokenSerializationType> = None;
        while let Ok(t) = p.next_including_whitespace() {
            if matches!(t, Token::Comment(_)) {
                continue;
            }
            let ser = t.serialization_type();
            if prev.is_some_and(|prev| prev.needs_separator_when_before(ser)) {
                out.push(' ');
            }
            out.push_str(&t.to_css_string());
            prev = Some(ser);
        }
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lp(s: &str) -> LengthPercentage {
        let mut p = Parser::new(s);
        let v = parse_length_percentage(&mut p).unwrap();
        p.expect_exhausted().unwrap();
        v
    }

    fn color(s: &str) -> ColorValue {
        let mut p = Parser::new(s);
        let v = parse_color_value(&mut p).unwrap();
        p.expect_exhausted().unwrap();
        v
    }

    fn ctx() -> ResolveCtx {
        ResolveCtx {
            em: 16.0,
            rem: 16.0,
            viewport_w: 800.0,
            viewport_h: 600.0,
            ..ResolveCtx::base(16.0, 16.0, 800.0, 600.0)
        }
    }

    #[test]
    fn lengths() {
        assert_eq!(lp("10px"), LengthPercentage::Px(10.0));
        assert_eq!(lp("50%"), LengthPercentage::Percent(0.5));
        assert_eq!(lp("1.5em"), LengthPercentage::Em(1.5));
        assert_eq!(lp("0"), LengthPercentage::Px(0.0));
        assert_eq!(lp("2rem"), LengthPercentage::Rem(2.0));
        assert!(parse_length_percentage(&mut Parser::new("10pt")).is_err());
        assert!(parse_length_percentage(&mut Parser::new("10")).is_err());
    }

    #[test]
    fn resolve_units() {
        let c = ctx();
        assert_eq!(lp("2em").resolve(&c, 0.0), Some(32.0));
        assert_eq!(lp("50vw").resolve(&c, 0.0), Some(400.0));
        assert_eq!(lp("25%").resolve(&c, 200.0), Some(50.0));
    }

    #[test]
    fn calc() {
        let c = ctx();
        assert_eq!(lp("calc(100% - 32px)").resolve(&c, 200.0), Some(168.0));
        assert_eq!(lp("calc(2 * (1em + 4px))").resolve(&c, 0.0), Some(40.0));
        assert_eq!(lp("calc(10px / 4)").resolve(&c, 0.0), Some(2.5));
        assert_eq!(lp("calc(50vw + 10px)").resolve(&c, 0.0), Some(410.0));
        assert_eq!(lp("calc(50% + 10px)").resolve(&c, 200.0), Some(110.0));
        // 长度×长度与除零被拒；纯数字结果被拒
        assert!(parse_length_percentage(&mut Parser::new("calc(10px * 10px)")).is_err());
        assert!(parse_length_percentage(&mut Parser::new("calc(10px / 0)")).is_err());
        assert!(parse_length_percentage(&mut Parser::new("calc(2 * 3)")).is_err());
    }

    #[test]
    fn colors() {
        let ColorValue::Absolute(a) = color("#ff0000") else {
            panic!("hex should parse");
        };
        assert!((a.components[0] - 1.0).abs() < 1e-4);
        assert!((a.components[3] - 1.0).abs() < 1e-4);
        assert_eq!(color("currentcolor"), ColorValue::CurrentColor);
        assert!(matches!(
            color("light-dark(red, blue)"),
            ColorValue::LightDark(..)
        ));
        assert!(matches!(color("rebeccapurple"), ColorValue::Absolute(_)));
        assert!(matches!(
            color("rgb(255 0 0 / 50%)"),
            ColorValue::Absolute(_)
        ));
        assert!(matches!(color("rgb(255, 0, 0)"), ColorValue::Absolute(_)));
        assert!(matches!(
            color("hsl(120deg 50% 50%)"),
            ColorValue::Absolute(_)
        ));
        assert!(matches!(
            color("oklch(70% 0.1 200)"),
            ColorValue::Absolute(_)
        ));
        assert!(matches!(
            color("color(srgb 1 0 0)"),
            ColorValue::Absolute(_)
        ));
        assert!(parse_color_value(&mut Parser::new("nosuchcolor")).is_err());
    }
}
