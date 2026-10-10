//! B4 @property (css-properties-values-api): registration rule parsing plus
//! syntax grammar parsing and validation. Design: docs/adr/0014-property-registry.md.
//!
//! MVP surface: 14 type families + single-level `+`/`#` combinators;
//! `||`/`&&`/nested combinations make the registration invalid (Dropped).
//! Type probing is string-level (color reuses the real grammar parse_color;
//! everything else is a token-level check).

use crate::css::decl::{TokenBuf, capture_tokens, token_buf_to_string};
use cssparser::{Delimiter, ParseError, Parser, Token};

/// Type families supported by the syntax descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxType {
    /// `<length>` (number + length unit; unitless 0 is valid).
    Length,
    /// `<percentage>`.
    Percentage,
    /// `<length-percentage>` (length or percentage).
    LengthPercentage,
    /// `<number>`.
    Number,
    /// `<integer>`.
    Integer,
    /// `<color>` (probed with the real grammar: parse_color).
    Color,
    /// `<image>` (approximated by a set of gradient/image function names).
    Image,
    /// `<url>` (url token / string / url() function).
    Url,
    /// `<angle>` (deg/grad/rad/turn).
    Angle,
    /// `<time>` (s/ms).
    Time,
    /// `<resolution>` (dpi/dpcm/dppx/x).
    Resolution,
    /// `<transform-function>` (set of known transform function names).
    TransformFunction,
    /// `<custom-ident>` (an Ident that is not a CSS-wide keyword).
    CustomIdent,
    /// `<string>` (QuotedString).
    String,
}

/// Multi-value combinators (MVP subset of the css-properties-values-api §syntax
/// grammar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Multi {
    /// Single value.
    Single,
    /// `+`: whitespace-separated multi-value (≥1).
    Space,
    /// `#`: comma-separated multi-value (≥1).
    Comma,
}

/// Parsed syntax descriptor.
#[derive(Debug, Clone, PartialEq)]
pub enum PropertySyntax {
    /// `"*"`: any value, unchecked (default).
    Universal,
    /// `<type>[+|#]?`.
    Named {
        /// Type family.
        ty: SyntaxType,
        /// Multi-value combinator.
        multi: Multi,
    },
}

/// An @property rule (registration surface: the parse product before the engine
/// merges it into its registered_props).
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyRule {
    /// Registered name (a `--`-prefixed custom property name).
    pub name: String,
    /// The syntax descriptor (defaults to universal).
    pub syntax: PropertySyntax,
    /// The inherits descriptor (defaults to false).
    pub inherits: bool,
    /// Raw initial-value tokens (validated against the syntax at registration time).
    pub initial_value: Option<TokenBuf>,
}

/// CSS-wide keywords (css-cascade): a registered property value containing one is
/// invalid. Single source: `CSS_WIDE_KEYWORDS` in css/mod.rs.
use super::CSS_WIDE_KEYWORDS;

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

/// syntax string → grammar tree; an unsupported surface (nested combinators etc.)
/// → None = invalid registration.
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

/// Parses an @property rule: name (prelude ident) + descriptor loop over the
/// block body. None = invalid rule (the caller emits a Dropped warning and
/// consumes the whole block).
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

/// Syntax matching (shared by initial-value registration and declaration-value
/// computation).
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

/// Splits on top-level commas at paren depth 0 (comma multi-value combinator).
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

/// Type probing (string-level; first-token check + exhaustion where possible).
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

/// First-token check with an optional exhausted requirement.
fn first_token_is(s: &str, f: impl Fn(&Token<'_>) -> bool, exhaust: bool) -> bool {
    let mut p = Parser::new(s);
    let ok = match p.next_including_whitespace() {
        Ok(t) => f(t),
        Err(_) => false,
    };
    ok && (!exhaust || p.expect_exhausted().is_ok())
}

/// Numeric probe (must be exhausted after parsing; generic over
/// expect_number/expect_integer and their BasicParseError error type).
fn exhausted<T, E>(s: &str, f: impl FnOnce(&mut Parser<'_>) -> Result<T, E>) -> bool {
    let mut p = Parser::new(s);
    f(&mut p).is_ok() && p.expect_exhausted().is_ok()
}

/// `<length>`: 0 (unitless) or a Dimension with a unit; `%` is not a length.
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

/// Dimension-with-unit check (angle/time/resolution).
fn dimension_unit_in(s: &str, units: &[&str]) -> bool {
    first_token_is(
        s,
        |t| matches!(t, Token::Dimension { unit, .. } if units.iter().any(|k| unit.eq_ignore_ascii_case(k))),
        true,
    )
}

/// Trims leading/trailing whitespace tokens from the token stream (same
/// treatment as custom properties).
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
