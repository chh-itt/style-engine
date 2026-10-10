//! E @counter-style (css-counter-styles-3 subset): rule parsing and
//! registration.
//!
//! This slice only parses and registers (source-ordered `Vec` +
//! last-write-wins lookup on equal names); rendering lives in the
//! counter_format slice (consumed by engine.rs eval_pseudo_content). The
//! lenient semantics align with @font-face: registration proceeds normally
//! inside conditional groups/nested blocks; an invalid value for a known
//! descriptor = that descriptor is ignored and the rule survives; an unknown
//! descriptor = a ParseReport warning (per task requirements, deliberately
//! asymmetric with @font-face's silent skip).
//!
//! Documented simplifications (Tier B, intentional):
//! - Cross-descriptor constraints are not validated (e.g. `system: additive`
//!   without `additive-symbols`, or bound ordering within a `range` group) —
//!   the parse/storage layer performs no semantic completeness checks;
//! - `<symbol>` is `<string> | <ident>` (the spec also has composite forms;
//!   rare, not included);
//! - `fallback` does not reject `none` (accepted leniently, registered as-is).

use crate::error::{ParseReport, ParseSeverity};
use cssparser::{Delimiter, ParseError, Parser, Token};

/// `system` descriptor value (css-counter-styles-3 §3.2).
#[derive(Debug, Clone, PartialEq)]
pub enum CounterStyleSystem {
    /// `cyclic`.
    Cyclic,
    /// `numeric`.
    Numeric,
    /// `alphabetic`.
    Alphabetic,
    /// `symbolic` (initial value when the descriptor is absent —
    /// css-counter-styles-3 §3.2 Initial: symbolic).
    Symbolic,
    /// `additive`.
    Additive,
    /// `fixed <integer>?` (`<integer>` defaults to 1 — spec: fixed
    /// `<integer>`? counts from 1 by default).
    Fixed(i32),
    /// `extends <counter-style-name>` (the name is registered verbatim from
    /// the source text; lookup matching does not expand it here).
    Extends(String),
}

/// `range` descriptor value (css-counter-styles-3 §3.7): `auto` or
/// `[ <integer> | infinite ]{2}#` (`infinite` = open bound, None).
#[derive(Debug, Clone, PartialEq, Default)]
pub enum CounterStyleRange {
    /// `auto` (default).
    #[default]
    Auto,
    /// List of closed interval groups (exactly two bounds per group;
    /// None = infinite).
    Ranges(Vec<(Option<i32>, Option<i32>)>),
}

/// A registered E @counter-style rule (css-counter-styles-3 subset).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct CounterStyleRule {
    /// Counter style name (prelude custom-ident; `none` is reserved and
    /// invalid). Matching is case-sensitive (the spec's counter-style-name is
    /// case-sensitive).
    pub name: String,
    /// `system` (defaults to Symbolic).
    pub system: CounterStyleSystem,
    /// `symbols`: `<symbol>#` (`<symbol>` = `<string> | <ident>`).
    pub symbols: Vec<String>,
    /// `additive-symbols`: `[ <integer [0,∞]> && <symbol> ]#` (strictly
    /// decreasing enforced at parse time — css-counter-styles-3 §3.6).
    pub additive_symbols: Vec<(i32, String)>,
    /// `prefix`: `<symbol>` (None = not written).
    pub prefix: Option<String>,
    /// `suffix`: `<symbol>` (initial value `". "` — css-counter-styles-3 §3.4).
    pub suffix: String,
    /// `range`: `auto | [ <integer> | infinite ]{2}#` (defaults to Auto).
    pub range: CounterStyleRange,
    /// `pad`: `<integer [0,∞]> && <symbol>` (None = not written).
    pub pad: Option<(u32, String)>,
    /// `negative`: `<symbol>{1,2}`.
    pub negative: Vec<String>,
    /// `fallback`: `<counter-style-name>` (None = not written; `none` is
    /// accepted leniently).
    pub fallback: Option<String>,
}

impl Default for CounterStyleRule {
    fn default() -> Self {
        Self {
            name: String::new(),
            system: CounterStyleSystem::Symbolic,
            symbols: Vec::new(),
            additive_symbols: Vec::new(),
            prefix: None,
            // css-counter-styles-3 §3.4：suffix 初始值 `". "`。
            suffix: ". ".to_string(),
            range: CounterStyleRange::Auto,
            pad: None,
            negative: Vec::new(),
            fallback: None,
        }
    }
}

/// CSS-wide keywords (css-cascade): a descriptor value containing one
/// invalidates that descriptor. Single source: `CSS_WIDE_KEYWORDS` in
/// css/mod.rs.
use crate::css::CSS_WIDE_KEYWORDS;

/// Parses an @counter-style rule: name (prelude already validated as
/// custom-ident) + descriptor loop over the block body (template =
/// parse_font_face_block + report threading). None = invalid rule (the caller
/// emits a Dropped warning and consumes the whole block).
pub(crate) fn parse_counter_style_rule(
    name: &str,
    input: &mut Parser<'_>,
    report: &mut ParseReport,
) -> Option<CounterStyleRule> {
    let mut rule = CounterStyleRule {
        name: name.to_string(),
        ..CounterStyleRule::default()
    };
    loop {
        input.skip_whitespace();
        let (desc, loc) = match input.next() {
            Ok(Token::Ident(id)) => (id.to_string(), input.current_source_location()),
            // 块内视图：结束 = EOF；防御性收掉闭合括号（同 @property/@font-face）。
            Ok(Token::CloseCurlyBracket) | Err(_) => break,
            Ok(_) => continue, // 杂散 token（畸形片段）宽容跳过
        };
        match input.next() {
            Ok(Token::Colon) => {}
            Ok(_) => {
                // 无冒号 = 畸形描述符——值段消费至 ';'，后续描述符存活
                //（同 @font-face）。
                drain_value_segment(input);
                continue;
            }
            Err(_) => break,
        }
        // 自定义属性描述符（`--x:`）：at-rule 块内合法噪声，静默忽略
        //（css-syntax：自定义属性在任何块内都按声明语法吞掉）。
        if desc.starts_with("--") {
            drain_value_segment(input);
            continue;
        }
        let lower = desc.to_ascii_lowercase();
        let known = matches!(
            lower.as_str(),
            "system"
                | "symbols"
                | "additive-symbols"
                | "prefix"
                | "suffix"
                | "range"
                | "pad"
                | "negative"
                | "fallback"
                | "speak-as"
        );
        if !known {
            // 未知描述符：告警 + 值段照常消费（任务要求；与 @font-face
            // 的静默跳过刻意不对称，见模块文档）。
            report.push(
                loc.line + 1,
                loc.column + 1,
                ParseSeverity::Dropped,
                format!("unknown @counter-style descriptor '{desc}'"),
            );
        }
        // CSS 全局关键字（initial/inherit/…）：描述符值含之 = 该描述符
        // 无效——忽略、规则存活（css-cascade 全局关键字语义）。
        let wide = input
            .try_parse(|p| -> Result<bool, ParseError<()>> {
                p.skip_whitespace();
                match p.next()? {
                    Token::Ident(id)
                        if CSS_WIDE_KEYWORDS.iter().any(|k| id.eq_ignore_ascii_case(k)) =>
                    {
                        Ok(true)
                    }
                    _ => Err(ParseError::unexpected_token()),
                }
            })
            .unwrap_or(false);
        if wide {
            drain_value_segment(input);
            continue;
        }
        // 同 @font-face：忽略外层 Err——闭包 bool 已裁决该描述符有效性，
        // 外层 Err 仅意味子解析器未吃尽值段（残留噪声），由
        // drain_value_segment 排干；规则恒存活（css-counter-styles-3：
        // 非法描述符忽略、规则照常登记）。
        let _ = input.parse_until_before::<_, _, ()>(Delimiter::Semicolon, |v| {
            Ok(parse_counter_style_descriptor(&lower, v, &mut rule))
        });
        // 排干值段残留至 ';'（子解析器未消费尽的噪声不影响后续描述符）。
        drain_value_segment(input);
    }
    Some(rule)
}

/// Parses one descriptor value segment (sub-parser scope = up to ';' or the
/// end of the block). false = the value is syntactically invalid (that
/// descriptor is ignored; the rule survives).
fn parse_counter_style_descriptor(
    name: &str,
    v: &mut Parser<'_>,
    rule: &mut CounterStyleRule,
) -> bool {
    match name {
        "system" => parse_system(v, &mut rule.system),
        "symbols" => parse_symbols(v, &mut rule.symbols),
        "additive-symbols" => parse_additive_symbols(v, &mut rule.additive_symbols),
        "prefix" => match parse_one_symbol(v) {
            Some(s) => {
                rule.prefix = Some(s);
                true
            }
            None => false,
        },
        "suffix" => match parse_one_symbol(v) {
            Some(s) => {
                rule.suffix = s;
                true
            }
            None => false,
        },
        "range" => parse_range(v, &mut rule.range),
        "pad" => parse_pad(v, &mut rule.pad),
        "negative" => parse_negative(v, &mut rule.negative),
        "fallback" => parse_fallback(v, &mut rule.fallback),
        // speak-as：解析并忽略（css-counter-styles-3 §5；无 TTS 语义，
        // 值段由调用方排干）。
        "speak-as" => true,
        // 未知描述符已在调用方告警；值段照常消费。
        _ => true,
    }
}

/// `<symbol>` = `<string> | <ident>` (subset; see module docs). None = no match.
fn parse_one_symbol(v: &mut Parser<'_>) -> Option<String> {
    v.skip_whitespace();
    match v.next() {
        Ok(Token::QuotedString(s)) => Some(s.to_string()),
        Ok(Token::Ident(id)) if !id.starts_with("--") => Some(id.to_string()),
        _ => None,
    }
}

/// `system`: `cyclic | numeric | alphabetic | symbolic | additive |
/// [fixed `<integer>`?] | [extends `<counter-style-name>`]`.
fn parse_system(v: &mut Parser<'_>, out: &mut CounterStyleSystem) -> bool {
    v.skip_whitespace();
    let id = match v.next() {
        Ok(Token::Ident(id)) => id,
        _ => return false,
    };
    match id.to_ascii_lowercase().as_str() {
        "cyclic" => {
            *out = CounterStyleSystem::Cyclic;
            true
        }
        "numeric" => {
            *out = CounterStyleSystem::Numeric;
            true
        }
        "alphabetic" => {
            *out = CounterStyleSystem::Alphabetic;
            true
        }
        "symbolic" => {
            *out = CounterStyleSystem::Symbolic;
            true
        }
        "additive" => {
            *out = CounterStyleSystem::Additive;
            true
        }
        "fixed" => {
            // fixed <integer>?（缺省 1——css-counter-styles-3 §3.2）。
            let save = v.state();
            let n = match v.next() {
                Ok(Token::Number {
                    int_value: Some(i), ..
                }) => *i,
                _ => {
                    v.reset(&save);
                    1
                }
            };
            *out = CounterStyleSystem::Fixed(n);
            true
        }
        "extends" => {
            v.skip_whitespace();
            match v.next() {
                Ok(Token::Ident(sub))
                    if !sub.starts_with("--") && !sub.eq_ignore_ascii_case("none") =>
                {
                    *out = CounterStyleSystem::Extends(sub.to_string());
                    true
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// `symbols`: `<symbol>+` (**space**-separated, css-counter-styles-3 §3.5 —
/// unlike additive-symbols' `#` (comma)).
fn parse_symbols(v: &mut Parser<'_>, out: &mut Vec<String>) -> bool {
    let mut got: Vec<String> = Vec::new();
    while let Some(s) = parse_one_symbol(v) {
        got.push(s);
    }
    if got.is_empty() {
        return false;
    }
    *out = got;
    true
}

/// `additive-symbols`: `[ <integer [0,∞]> && <symbol> ]#` with strictly
/// decreasing weights (css-counter-styles-3 §3.6: `&&` accepts both orders —
/// integer first or symbol first).
fn parse_additive_symbols(v: &mut Parser<'_>, out: &mut Vec<(i32, String)>) -> bool {
    let mut got: Vec<(i32, String)> = Vec::new();
    loop {
        v.skip_whitespace();
        let save = v.state();
        // 序 1：<integer> && <symbol>（整数在前）。
        let mut pair = (|| -> Option<(i32, String)> {
            let i = next_nonneg_int(v)?;
            let s = parse_one_symbol(v)?;
            Some((i, s))
        })();
        // 序 2：<symbol> && <integer>（符号在前）。
        if pair.is_none() {
            v.reset(&save);
            pair = (|| -> Option<(i32, String)> {
                let s = parse_one_symbol(v)?;
                let i = next_nonneg_int(v)?;
                Some((i, s))
            })();
        }
        match pair {
            Some(p) => got.push(p),
            None => return false,
        }
        let save = v.state();
        match v.next() {
            Ok(Token::Comma) => continue,
            _ => {
                v.reset(&save);
                break;
            }
        }
    }
    // 严格递减（相等亦无效）。
    if got.windows(2).any(|w| w[0].0 <= w[1].0) {
        return false;
    }
    *out = got;
    true
}

/// `<integer [0,∞]>` (u32 semantics via a non-negative i32 channel).
fn next_nonneg_int(v: &mut Parser<'_>) -> Option<i32> {
    match v.next() {
        Ok(Token::Number {
            int_value: Some(i), ..
        }) if *i >= 0 => Some(*i),
        _ => None,
    }
}

/// `range`: `auto | [ <integer> | infinite ]{2}#`.
fn parse_range(v: &mut Parser<'_>, out: &mut CounterStyleRange) -> bool {
    v.skip_whitespace();
    // `auto` 短路（先试，try_parse 失败自动回退）。
    if v.try_parse(|p| -> Result<(), ParseError<()>> {
        p.skip_whitespace();
        match p.next()? {
            Token::Ident(id) if id.eq_ignore_ascii_case("auto") => Ok(()),
            _ => Err(ParseError::unexpected_token()),
        }
    })
    .is_ok()
    {
        *out = CounterStyleRange::Auto;
        return true;
    }
    let mut groups: Vec<(Option<i32>, Option<i32>)> = Vec::new();
    loop {
        let lo = parse_range_bound(v);
        let hi = if lo.is_some() {
            parse_range_bound(v)
        } else {
            None
        };
        if lo.is_none() || hi.is_none() {
            return false;
        }
        groups.push((lo.unwrap(), hi.unwrap()));
        let save = v.state();
        match v.next() {
            Ok(Token::Comma) => continue,
            _ => {
                v.reset(&save);
                break;
            }
        }
    }
    *out = CounterStyleRange::Ranges(groups);
    true
}

/// One range bound: `<integer> | infinite` (inner None = infinite).
fn parse_range_bound(v: &mut Parser<'_>) -> Option<Option<i32>> {
    v.skip_whitespace();
    match v.next() {
        Ok(Token::Number {
            int_value: Some(i), ..
        }) => Some(Some(*i)),
        Ok(Token::Ident(id)) if id.eq_ignore_ascii_case("infinite") => Some(None),
        _ => None,
    }
}

/// `pad`: `<integer [0,∞]> && <symbol>` (both orders accepted, like
/// additive-symbols).
fn parse_pad(v: &mut Parser<'_>, out: &mut Option<(u32, String)>) -> bool {
    v.skip_whitespace();
    let save = v.state();
    // 序 1：整数在前。
    let mut pair = (|| -> Option<(u32, String)> {
        let i = u32::try_from(next_nonneg_int(v)?).ok()?;
        let s = parse_one_symbol(v)?;
        Some((i, s))
    })();
    // 序 2：符号在前。
    if pair.is_none() {
        v.reset(&save);
        pair = (|| -> Option<(u32, String)> {
            let s = parse_one_symbol(v)?;
            let i = u32::try_from(next_nonneg_int(v)?).ok()?;
            Some((i, s))
        })();
    }
    match pair {
        Some(p) => {
            *out = Some(p);
            true
        }
        None => false,
    }
}

/// `negative`: `<symbol>{1,2}`.
fn parse_negative(v: &mut Parser<'_>, out: &mut Vec<String>) -> bool {
    let Some(first) = parse_one_symbol(v) else {
        return false;
    };
    let mut list = vec![first];
    let save = v.state();
    match parse_one_symbol(v) {
        Some(s) => list.push(s),
        None => {
            v.reset(&save);
        }
    }
    *out = list;
    true
}

/// `fallback`: `<counter-style-name>` (`none` accepted leniently; see module
/// docs).
fn parse_fallback(v: &mut Parser<'_>, out: &mut Option<String>) -> bool {
    v.skip_whitespace();
    match v.next() {
        Ok(Token::Ident(id)) if !id.starts_with("--") => {
            *out = Some(id.to_string());
            true
        }
        _ => false,
    }
}

/// Drains the remainder of the value segment up to ';' (consuming the ';';
/// end of block/EOF terminates).
fn drain_value_segment(input: &mut Parser<'_>) {
    loop {
        match input.next() {
            Ok(Token::Semicolon) | Err(_) => break,
            Ok(_) => {}
        }
    }
}
