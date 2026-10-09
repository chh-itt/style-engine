//! E @counter-style（css-counter-styles-3 子集）：规则解析与登记。
//!
//! 本切片仅解析与登记（`Vec` 来源序 + 同名后写胜查询）；渲染由
//! counter_format 切片承担（engine.rs eval_pseudo_content 消费）。宽容
//! 语义与 @font-face 对齐：条件组/嵌套体内
//! 照常登记；已知描述符值非法 = 该描述符忽略、规则存活；未知描述符 =
//! ParseReport 告警（任务要求，与 @font-face 的静默跳过刻意不对称）。
//!
//! 在案简化（B 级，有意为之）：
//! - 跨描述符约束不校验（如 `system: additive` 未给 `additive-symbols`、
//!   `range` 组内上下界次序）——解析存储层不做语义完备性检查；
//! - `<symbol>` 取 `<string> | <ident>`（spec 另含组合形，罕见，未收）；
//! - `fallback` 不拒绝 `none`（宽容接受，登记原样保留）。

use crate::error::{ParseReport, ParseSeverity};
use cssparser::{Delimiter, ParseError, Parser, Token};

/// `system` 描述符值（css-counter-styles-3 §3.2）。
#[derive(Debug, Clone, PartialEq)]
pub enum CounterStyleSystem {
    /// `cyclic`。
    Cyclic,
    /// `numeric`。
    Numeric,
    /// `alphabetic`。
    Alphabetic,
    /// `symbolic`（描述符缺省时的初始值——css-counter-styles-3 §3.2
    /// Initial: symbolic）。
    Symbolic,
    /// `additive`。
    Additive,
    /// `fixed <integer>?`（`<integer>` 缺省 = 1——spec: fixed <integer>?
    /// 计数起点缺省 1）。
    Fixed(i32),
    /// `extends <counter-style-name>`（名称按源文本原样登记；查询匹配
    /// 不在此处展开）。
    Extends(String),
}

/// `range` 描述符值（css-counter-styles-3 §3.7）：`auto` 或
/// `[ <integer> | infinite ]{2}#`（`infinite` = 界开、None）。
#[derive(Debug, Clone, PartialEq, Default)]
pub enum CounterStyleRange {
    /// `auto`（缺省）。
    #[default]
    Auto,
    /// 闭区间组列表（每组恰两界；None = infinite）。
    Ranges(Vec<(Option<i32>, Option<i32>)>),
}

/// E @counter-style 登记规则（css-counter-styles-3 子集）。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct CounterStyleRule {
    /// 计数样式名（prelude custom-ident；`none` 为保留字无效）。匹配
    /// 区分大小写（spec counter-style-name 大小写敏感）。
    pub name: String,
    /// `system`（缺省 Symbolic）。
    pub system: CounterStyleSystem,
    /// `symbols`：`<symbol>#`（`<symbol>` = `<string> | <ident>`）。
    pub symbols: Vec<String>,
    /// `additive-symbols`：`[ <integer [0,∞]> && <symbol> ]#`（解析期
    /// 强制严格递减——css-counter-styles-3 §3.6）。
    pub additive_symbols: Vec<(i32, String)>,
    /// `prefix`：`<symbol>`（None = 未写）。
    pub prefix: Option<String>,
    /// `suffix`：`<symbol>`（初始值 `". "`——css-counter-styles-3 §3.4）。
    pub suffix: String,
    /// `range`：`auto | [ <integer> | infinite ]{2}#`（缺省 Auto）。
    pub range: CounterStyleRange,
    /// `pad`：`<integer [0,∞]> && <symbol>`（None = 未写）。
    pub pad: Option<(u32, String)>,
    /// `negative`：`<symbol>{1,2}`。
    pub negative: Vec<String>,
    /// `fallback`：`<counter-style-name>`（None = 未写；`none` 宽容接受）。
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

/// CSS 全局关键字（css-cascade）：描述符值含之 = 该描述符无效。
/// 单源见 css/mod.rs `CSS_WIDE_KEYWORDS`。
use crate::css::CSS_WIDE_KEYWORDS;

/// @counter-style 规则解析：name（prelude 已验证 custom-ident）+ 块体
/// 描述符循环（模板 = parse_font_face_block + report 线程）。None =
/// 规则无效（调用方 Dropped 告警 + 整块消费）。
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

/// 单个描述符值段解析（子解析器域 = 至 ';' 或块尾）。false = 值语法
/// 无效（该描述符忽略、规则存活）。
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

/// `<symbol>` = `<string> | <ident>`（子集，见模块文档）。None = 不匹配。
fn parse_one_symbol(v: &mut Parser<'_>) -> Option<String> {
    v.skip_whitespace();
    match v.next() {
        Ok(Token::QuotedString(s)) => Some(s.to_string()),
        Ok(Token::Ident(id)) if !id.starts_with("--") => Some(id.to_string()),
        _ => None,
    }
}

/// `system`：`cyclic | numeric | alphabetic | symbolic | additive |
/// [fixed <integer>?] | [extends <counter-style-name>]`。
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

/// `symbols`：`<symbol>+`（**空格**分隔，css-counter-styles-3 §3.5——与
/// additive-symbols 的 `#`（逗号）不同）。
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

/// `additive-symbols`：`[ <integer [0,∞]> && <symbol> ]#`，权重严格递减
///（css-counter-styles-3 §3.6：&& 两序皆容——整数在前或符号在前）。
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

/// `<integer [0,∞]>`（u32 语义经 i32 非负通道）。
fn next_nonneg_int(v: &mut Parser<'_>) -> Option<i32> {
    match v.next() {
        Ok(Token::Number {
            int_value: Some(i), ..
        }) if *i >= 0 => Some(*i),
        _ => None,
    }
}

/// `range`：`auto | [ <integer> | infinite ]{2}#`。
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

/// range 单界：`<integer> | infinite`（None 内层 = infinite）。
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

/// `pad`：`<integer [0,∞]> && <symbol>`（两序皆容，同 additive-symbols）。
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

/// `negative`：`<symbol>{1,2}`。
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

/// `fallback`：`<counter-style-name>`（`none` 宽容接受，见模块文档）。
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

/// 值段残留排干至 ';'（含消费 ';'；块尾/EOF 终止）。
fn drain_value_segment(input: &mut Parser<'_>) {
    loop {
        match input.next() {
            Ok(Token::Semicolon) | Err(_) => break,
            Ok(_) => {}
        }
    }
}
