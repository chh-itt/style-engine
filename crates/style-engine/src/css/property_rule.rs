//! B4 @property（css-properties-values-api）：注册规则解析 + syntax 文法
//! 解析与校验。设计见 docs/adr/0014-property-registry.md。
//!
//! MVP 面：类型族 14 种 + `+`/`#` 单层组合子；`||`/`&&`/嵌套组合 → 注册
//! 无效（Dropped）。类型试探为字符串级（color 复用真文法 parse_color，
//! 其余 token 级判定）。

use crate::css::decl::{TokenBuf, capture_tokens, token_buf_to_string};
use cssparser::{Delimiter, ParseError, Parser, Token};

/// syntax 描述符支持的类型族。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxType {
    /// `<length>`（数值+长度单位；0 无单位合法）。
    Length,
    /// `<percentage>`。
    Percentage,
    /// `<length-percentage>`（length 或 percentage）。
    LengthPercentage,
    /// `<number>`。
    Number,
    /// `<integer>`。
    Integer,
    /// `<color>`（真文法试探：parse_color）。
    Color,
    /// `<image>`（渐变/图片函数名集合近似）。
    Image,
    /// `<url>`（url token / 字符串 / url() 函数）。
    Url,
    /// `<angle>`（deg/grad/rad/turn）。
    Angle,
    /// `<time>`（s/ms）。
    Time,
    /// `<resolution>`（dpi/dpcm/dppx/x）。
    Resolution,
    /// `<transform-function>`（已知变换函数名集合）。
    TransformFunction,
    /// `<custom-ident>`（Ident 且非 CSS 宽关键字）。
    CustomIdent,
    /// `<string>`（QuotedString）。
    String,
}

/// 多值组合子（css-properties-values-api §syntax 文法的 MVP 子集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Multi {
    /// 单值。
    Single,
    /// `+`：空白分隔多值（≥1）。
    Space,
    /// `#`：逗号分隔多值（≥1）。
    Comma,
}

/// syntax 描述符解析产物。
#[derive(Debug, Clone, PartialEq)]
pub enum PropertySyntax {
    /// `"*"`：任意值，不校验（缺省）。
    Universal,
    /// `<type>[+|#]?`。
    Named {
        /// 类型族。
        ty: SyntaxType,
        /// 多值组合子。
        multi: Multi,
    },
}

/// @property 规则（注册面 = 引擎 registered_props 合并前的解析产物）。
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyRule {
    /// 注册名（`--` 前缀自定义属性名）。
    pub name: String,
    /// syntax 描述符（缺省 universal）。
    pub syntax: PropertySyntax,
    /// inherits 描述符（缺省 false）。
    pub inherits: bool,
    /// initial-value 原始 token（注册期已按 syntax 校验）。
    pub initial_value: Option<TokenBuf>,
}

const CSS_WIDE_KEYWORDS: [&str; 5] = ["initial", "inherit", "unset", "revert", "revert-layer"];

const LENGTH_UNITS: [&str; 24] = [
    "px", "cm", "mm", "q", "in", "pt", "pc", "em", "rem", "ex", "ch", "vw", "vh", "vmin", "vmax",
    "cqw", "cqh", "cqi", "cqb", "vi", "vb", "rlh", "lh", "cap",
];
const ANGLE_UNITS: [&str; 4] = ["deg", "grad", "rad", "turn"];
const TIME_UNITS: [&str; 2] = ["s", "ms"];
const RESOLUTION_UNITS: [&str; 4] = ["dpi", "dpcm", "dppx", "x"];
const IMAGE_FUNCS: [&str; 10] = [
    "url",
    "linear-gradient",
    "repeating-linear-gradient",
    "radial-gradient",
    "repeating-radial-gradient",
    "conic-gradient",
    "repeating-conic-gradient",
    "image-set",
    "cross-fade",
    "element",
];
const TRANSFORM_FUNCS: [&str; 23] = [
    "matrix",
    "matrix3d",
    "translate",
    "translate3d",
    "translatex",
    "translatey",
    "translatez",
    "scale",
    "scale3d",
    "scalex",
    "scaley",
    "scalez",
    "rotate",
    "rotate3d",
    "rotatex",
    "rotatey",
    "rotatez",
    "skew",
    "skewx",
    "skewy",
    "perspective",
    "translateX",
    "translateY",
];

/// syntax 字符串 → 文法树；不支持面（组合子嵌套等）→ None = 注册无效。
pub fn parse_syntax(s: &str) -> Option<PropertySyntax> {
    let s = s.trim();
    if s == "*" {
        return Some(PropertySyntax::Universal);
    }
    let (ty_str, multi) = if let Some(rest) = s.strip_suffix('#') {
        (rest.trim(), Multi::Comma)
    } else if let Some(rest) = s.strip_suffix('+') {
        (rest.trim(), Multi::Space)
    } else {
        (s, Multi::Single)
    };
    let inner = ty_str.strip_prefix('<')?.strip_suffix('>')?;
    let ty = match inner.to_ascii_lowercase().as_str() {
        "length" => SyntaxType::Length,
        "number" => SyntaxType::Number,
        "percentage" => SyntaxType::Percentage,
        "length-percentage" => SyntaxType::LengthPercentage,
        "color" => SyntaxType::Color,
        "image" => SyntaxType::Image,
        "url" => SyntaxType::Url,
        "integer" => SyntaxType::Integer,
        "angle" => SyntaxType::Angle,
        "time" => SyntaxType::Time,
        "resolution" => SyntaxType::Resolution,
        "transform-function" => SyntaxType::TransformFunction,
        "custom-ident" => SyntaxType::CustomIdent,
        "string" => SyntaxType::String,
        _ => return None,
    };
    Some(PropertySyntax::Named { ty, multi })
}

/// @property 规则解析：name（prelude ident）+ 块体描述符循环。
/// None = 规则无效（调用方 Dropped 告警 + 整块消费）。
pub(crate) fn parse_property_rule(name: &str, input: &mut Parser<'_>) -> Option<PropertyRule> {
    // 名必须为自定义属性名（css-properties-values-api §1）。
    if !name.starts_with("--") || name.len() <= 2 {
        return None;
    }
    let mut syntax: Option<PropertySyntax> = None;
    let mut inherits: Option<bool> = None;
    let mut initial: Option<TokenBuf> = None;
    loop {
        input.skip_whitespace();
        let desc = match input.next() {
            Ok(Token::Ident(id)) => id.to_string(),
            // 块内视图：结束 = EOF；防御性收掉闭合括号。
            Ok(Token::CloseCurlyBracket) | Err(_) => break,
            _ => return None,
        };
        if input.expect_colon().is_err() {
            return None;
        }
        let mut buf = TokenBuf::new();
        let ok = input.parse_until_after(Delimiter::Semicolon, |p| {
            capture_tokens(p, &mut buf);
            Ok::<(), ParseError<()>>(())
        });
        if ok.is_err() {
            return None;
        }
        match desc.to_ascii_lowercase().as_str() {
            "syntax" => {
                let raw = token_buf_to_string(&buf);
                // 描述符值为带引号字符串（spec）；容忍裸串。
                let quoted = raw.trim().trim_matches('"');
                syntax = Some(parse_syntax(quoted)?);
            }
            "inherits" => match token_buf_to_string(&buf).trim() {
                "true" => inherits = Some(true),
                "false" => inherits = Some(false),
                _ => return None,
            },
            "initial-value" => {
                let mut b = buf;
                trim_ws_edges(&mut b);
                if b.is_empty() {
                    return None;
                }
                initial = Some(b);
            }
            _ => {} // 未知描述符忽略（浏览器一致）
        }
    }
    let syntax = syntax.unwrap_or(PropertySyntax::Universal);
    // 注册有效性：非 universal 且缺 initial-value → 无效；initial 与
    // syntax 不匹配 → 无效（universal 允许 initial，作为缺省填充）。
    if let PropertySyntax::Named { ty, multi } = &syntax {
        let initial = initial.as_ref()?;
        let text = token_buf_to_string(initial);
        if !syntax_matches(
            &PropertySyntax::Named {
                ty: *ty,
                multi: *multi,
            },
            &text,
        ) {
            return None;
        }
    }
    Some(PropertyRule {
        name: name.to_string(),
        syntax,
        inherits: inherits.unwrap_or(false),
        initial_value: initial,
    })
}

/// 语法匹配（initial-value 注册期 + 声明值计算期共用）。
pub fn syntax_matches(syn: &PropertySyntax, text: &str) -> bool {
    match syn {
        PropertySyntax::Universal => true,
        PropertySyntax::Named { ty, multi } => {
            let parts: Vec<String> = match multi {
                Multi::Single => vec![text.trim().to_string()],
                Multi::Space => text
                    .split_ascii_whitespace()
                    .map(|s| s.to_string())
                    .collect(),
                Multi::Comma => split_top_commas(text),
            };
            !parts.is_empty() && parts.iter().all(|p| type_matches(*ty, p))
        }
    }
}

/// 括号深度 0 的顶层逗号切分（逗号多值组合子）。
fn split_top_commas(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in text.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => {
                parts.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    parts.push(cur.trim().to_string());
    parts
}

/// 类型试探（字符串级；首 token 判定 + 尽量 exhausted）。
fn type_matches(ty: SyntaxType, s: &str) -> bool {
    match ty {
        SyntaxType::Number => exhausted(s, |p| p.expect_number()),
        SyntaxType::Integer => exhausted(s, |p| p.expect_integer()),
        SyntaxType::Length => unit_or_zero(s, &LENGTH_UNITS),
        SyntaxType::Percentage => {
            first_token_is(s, |t| matches!(t, Token::Percentage { .. }), true)
        }
        SyntaxType::LengthPercentage => {
            unit_or_zero(s, &LENGTH_UNITS)
                || first_token_is(s, |t| matches!(t, Token::Percentage { .. }), true)
        }
        SyntaxType::Angle => dimension_unit_in(s, &ANGLE_UNITS),
        SyntaxType::Time => dimension_unit_in(s, &TIME_UNITS),
        SyntaxType::Resolution => dimension_unit_in(s, &RESOLUTION_UNITS),
        SyntaxType::Color => {
            let mut p = Parser::new(s);
            let ok = matches!(
                crate::css::property::parse_color(&mut p),
                Ok(crate::css::property::DeclValue::Color(_))
            );
            ok && p.expect_exhausted().is_ok()
        }
        SyntaxType::String => first_token_is(s, |t| matches!(t, Token::QuotedString(_)), true),
        SyntaxType::CustomIdent => first_token_is(
            s,
            |t| matches!(t, Token::Ident(id) if !CSS_WIDE_KEYWORDS.iter().any(|k| id.eq_ignore_ascii_case(k))),
            true,
        ),
        SyntaxType::Url => first_token_is(
            s,
            |t| {
                matches!(t, Token::UnquotedUrl(_))
                    || matches!(t, Token::QuotedString(_))
                    || matches!(t, Token::Function(f) if f.eq_ignore_ascii_case("url"))
            },
            false, // url() 函数体跟随后续 token，不要求 exhausted
        ),
        SyntaxType::Image => first_token_is(
            s,
            |t| {
                matches!(t, Token::QuotedString(_))
                    || matches!(t, Token::UnquotedUrl(_))
                    || matches!(t, Token::Function(f) if IMAGE_FUNCS.iter().any(|k| f.eq_ignore_ascii_case(k)))
            },
            false,
        ),
        SyntaxType::TransformFunction => first_token_is(
            s,
            |t| matches!(t, Token::Function(f) if TRANSFORM_FUNCS.iter().any(|k| f.eq_ignore_ascii_case(k))),
            false,
        ),
    }
}

/// 首-token 判定 + 可选 exhausted 要求。
fn first_token_is(s: &str, f: impl Fn(&Token<'_>) -> bool, exhaust: bool) -> bool {
    let mut p = Parser::new(s);
    let ok = match p.next_including_whitespace() {
        Ok(t) => f(t),
        Err(_) => false,
    };
    ok && (!exhaust || p.expect_exhausted().is_ok())
}

/// 数值探针（解析后必须耗尽；泛型兼容 expect_number/expect_integer 及
/// 其 BasicParseError 错误类型）。
fn exhausted<T, E>(s: &str, f: impl FnOnce(&mut Parser<'_>) -> Result<T, E>) -> bool {
    let mut p = Parser::new(s);
    f(&mut p).is_ok() && p.expect_exhausted().is_ok()
}

/// `<length>`：0（无单位）或带单位的 Dimension；`%` 不属 length。
fn unit_or_zero(s: &str, units: &[&str]) -> bool {
    first_token_is(
        s,
        |t| match t {
            Token::Number { value, .. } => *value == 0.0,
            Token::Dimension { unit, .. } => units.iter().any(|k| unit.eq_ignore_ascii_case(k)),
            _ => false,
        },
        true,
    )
}

/// 带单位 Dimension 判定（angle/time/resolution）。
fn dimension_unit_in(s: &str, units: &[&str]) -> bool {
    first_token_is(
        s,
        |t| matches!(t, Token::Dimension { unit, .. } if units.iter().any(|k| unit.eq_ignore_ascii_case(k))),
        true,
    )
}

/// 去除 token 流首尾空白 token（custom property 同款处理）。
fn trim_ws_edges(buf: &mut TokenBuf) {
    while buf
        .first()
        .is_some_and(|t| t.text.chars().all(|c| c.is_ascii_whitespace()))
    {
        buf.remove(0);
    }
    while buf
        .last()
        .is_some_and(|t| t.text.chars().all(|c| c.is_ascii_whitespace()))
    {
        buf.pop();
    }
}
