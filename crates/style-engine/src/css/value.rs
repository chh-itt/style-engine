//! CSS 值类型与文法：T0 特性集（FEATURES.md）所需的值解析。
//!
//! 约定：`Percent` 一律存小数（50% → 0.5，与 cssparser 的 `unit_value`
//! 一致）；角度一律归一为度（deg）。`calc()` 允许 px/em/rem/%/vw/vh 与
//! 数字混合，数字仅可作为乘除系数出现（解析期校验）；按 MVP 宽容策略，
//! 不强制 `+`/`-` 两侧空格（超集接受；conformance 用例使用规范形式）。

use cssparser::{BasicParseError, ParseError, Parser, ToCss, Token, TokenSerializationType};
use peniko::color::{self, AlphaColor, Srgb};

/// 值解析错误（cssparser 0.38 的 `ParseError` 已无输入生命周期参数）。
pub type ValError = ParseError<BasicParseError>;
pub type ValResult<T> = Result<T, ValError>;

/// 值定值上下文：字号、根字号与视口（全部由引擎/宿主提供，crate 无副作用）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolveCtx {
    /// 当前节点字号（em 基准）。
    pub em: f32,
    /// 根节点字号（rem 基准）。
    pub rem: f32,
    /// 视口宽度（vw 基准）。
    pub viewport_w: f32,
    /// 视口高度（vh 基准）。
    pub viewport_h: f32,
}

/// `<length-percentage>`：px/em/rem/%/vw/vh/calc()。
#[derive(Debug, Clone, PartialEq)]
pub enum LengthPercentage {
    Px(f32),
    Em(f32),
    Rem(f32),
    /// 小数（50% → 0.5）。
    Percent(f32),
    /// 视口宽度小数（50vw → 0.5）。
    Vw(f32),
    /// 视口高度小数（50vh → 0.5）。
    Vh(f32),
    Calc(Box<CalcNode>),
}

impl LengthPercentage {
    pub fn zero() -> Self {
        Self::Px(0.0)
    }

    /// 定值。`percent_basis` 是百分比参照的长度（宽/高/字号，由调用方决定）。
    pub fn resolve(&self, ctx: &ResolveCtx, percent_basis: f32) -> Option<f32> {
        match self {
            Self::Px(v) => Some(*v),
            Self::Em(v) => Some(v * ctx.em),
            Self::Rem(v) => Some(v * ctx.rem),
            Self::Percent(v) => Some(v * percent_basis),
            Self::Vw(v) => Some(v * ctx.viewport_w),
            Self::Vh(v) => Some(v * ctx.viewport_h),
            Self::Calc(node) => node.resolve(ctx, percent_basis),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalcUnit {
    Number,
    Px,
    Em,
    Rem,
    /// 小数（50% → 0.5）。
    Percent,
    Vw,
    Vh,
}

/// `calc()` 表达式树（CSS Values 4 子集：四则运算、嵌套 calc、括号）。
#[derive(Debug, Clone, PartialEq)]
pub enum CalcNode {
    /// 带单位数值；`Number` 仅允许作为乘除系数（解析期校验）。
    Value(f32, CalcUnit),
    Sum(Box<CalcNode>, Box<CalcNode>),
    Sub(Box<CalcNode>, Box<CalcNode>),
    Product(Box<CalcNode>, Box<CalcNode>),
    /// 除以非零数字（CSS 约束）。
    Divide(Box<CalcNode>, f32),
}

impl CalcNode {
    /// 是否含百分比叶子（①calc 直通：映射期判定是否延迟结算）。
    pub fn has_percent(&self) -> bool {
        match self {
            Self::Value(_, CalcUnit::Percent) => true,
            Self::Sum(a, b) | Self::Sub(a, b) | Self::Product(a, b) => {
                a.has_percent() || b.has_percent()
            }
            Self::Divide(a, _) => a.has_percent(),
            _ => false,
        }
    }

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
        }
    }

    /// 纯数字叶子的原始标量（仅 Product 系数语境使用）。
    fn raw_number(&self) -> Option<f32> {
        match self {
            Self::Value(v, CalcUnit::Number) => Some(*v),
            _ => None,
        }
    }

    fn is_pure_number(&self) -> bool {
        match self {
            Self::Value(_, CalcUnit::Number) => true,
            Self::Sum(a, b) | Self::Sub(a, b) | Self::Product(a, b) => {
                a.is_pure_number() && b.is_pure_number()
            }
            Self::Divide(a, _) => a.is_pure_number(),
            Self::Value(..) => false,
        }
    }
}

/// 角度，统一为度（deg）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Angle(pub f32);

/// CSS 颜色：绝对色、currentColor 或 light-dark()。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColorValue {
    CurrentColor,
    Absolute(AlphaColor<Srgb>),
    /// MVP 偏差：参数仅存绝对色（currentcolor/嵌套 light-dark 走容错丢弃）。
    LightDark(AlphaColor<Srgb>, AlphaColor<Srgb>),
}

impl ColorValue {
    /// light-dark() 按环境色 scheme 取一边，其余原样（Environment 求值的一部分）。
    pub fn pick_scheme(self, dark: bool) -> ColorValue {
        match self {
            Self::LightDark(a, b) => Self::Absolute(if dark { b } else { a }),
            other => other,
        }
    }
}

/// 转到 sRGB 并钳制到 [0,1]（gamut 映射的 MVP 代替：clip，deviation 已知）。
fn to_srgb_clamped(c: color::DynamicColor) -> AlphaColor<Srgb> {
    let mut a = c.to_alpha_color::<Srgb>();
    a.components = a.components.map(|v| v.clamp(0.0, 1.0));
    a
}

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
            } else {
                return Err(p.new_error_for_next_token());
            };
            // vw/vh 与百分比一样按小数存（50vw → 0.5）
            let v = if matches!(u, CalcUnit::Vw | CalcUnit::Vh) {
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
        _ => Err(p.new_error_for_next_token()),
    }
}

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

pub fn parse_number(p: &mut Parser<'_>) -> ValResult<f32> {
    match p.next()?.clone() {
        Token::Number { value, .. } => Ok(value),
        _ => Err(p.new_error_for_next_token()),
    }
}

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

/// MVP 偏差：light-dark() 参数仅支持绝对色，其余走容错丢弃。
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

/// 把当前函数/块的 token 流反序列化为字符串（保留空白与分隔语义）。
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
