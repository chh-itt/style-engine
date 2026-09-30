//! 样式表：规则解析（选择器预lude + 声明块）与 @media 子集。
//!
//! 支持：顶层规则、`@media`（一层嵌套；screen/all 类型；min-/max-width/
//! height、prefers-color-scheme、prefers-reduced-motion）。其余 at-rule
//! 按容错跳过并告警。

use crate::css::decl::{
    DeclarationBlock, TokenBuf, capture_tokens, parse_declaration_block, token_buf_to_string,
};
use crate::css::value::{ResolveCtx, parse_length_percentage};
use crate::error::ParseReport;
use crate::selector::{StyleSelectorList, parse_selector_list};
use cssparser::{BasicParseError, ParseError, Parser, Token};

/// 单条样式规则。
#[derive(Debug, Clone)]
pub struct Rule {
    pub selectors: StyleSelectorList,
    /// 规则源顺序（级联排序的次级键）。
    pub order: u32,
    pub declarations: crate::css::decl::DeclarationBlock,
    /// 所属 @media 条件；None = 无条件。
    pub media: Option<MediaQuery>,
}

/// 解析完成的样式表。
#[derive(Debug, Clone, Default)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
    pub keyframes: Vec<KeyframesRule>,
    pub report: ParseReport,
}

// ---------- @keyframes（第五批⑰） ----------

/// @keyframes 规则：动画名 + 帧序表（offset 升序由采样端排序消费）。
#[derive(Debug, Clone, PartialEq)]
pub struct KeyframesRule {
    pub name: String,
    pub frames: Vec<Keyframe>,
}

/// 单帧：offset ∈ [0,1]（from=0、to=1、百分比/100）+ 声明块。
#[derive(Debug, Clone, PartialEq)]
pub struct Keyframe {
    pub offset: f32,
    pub declarations: DeclarationBlock,
}

// ---------- @media 子集 ----------

/// 配色方案偏好。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorScheme {
    Light,
    Dark,
}

/// 单个媒体特性（MVP 子集）。
#[derive(Debug, Clone, PartialEq)]
pub enum MediaFeature {
    Width(f32),
    MinWidth(f32),
    MaxWidth(f32),
    Height(f32),
    MinHeight(f32),
    MaxHeight(f32),
    PrefersColorScheme(ColorScheme),
    PrefersReducedMotion(bool),
    /// 指针精度（第五批⑱）：主输入设备。
    Pointer(PointerKind),
    /// 主输入设备是否支持悬停。
    Hover(bool),
    /// 任意输入设备的指针精度（与 pointer 独立评估）。
    AnyPointer(PointerKind),
    /// 任意输入设备是否支持悬停。
    AnyHover(bool),
}

/// 指针精度（第五批⑱媒体查询扩展：pointer/any-pointer）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerKind {
    None,
    Coarse,
    Fine,
}

/// 媒体查询：可选类型段 + AND 连接的特性列表，可整体取反。
#[derive(Debug, Clone, PartialEq)]
pub struct MediaQuery {
    /// `not` 前缀。
    pub negate: bool,
    /// 类型段求值结果（screen/all → true；print 等 → false）；None = 未写。
    pub media_type: Option<bool>,
    pub features: Vec<MediaFeature>,
}

impl MediaFeature {
    fn eval(&self, env: &MediaEnv) -> bool {
        match self {
            Self::Width(v) => env.viewport_w == *v,
            Self::MinWidth(v) => env.viewport_w >= *v,
            Self::MaxWidth(v) => env.viewport_w <= *v,
            Self::Height(v) => env.viewport_h == *v,
            Self::MinHeight(v) => env.viewport_h >= *v,
            Self::MaxHeight(v) => env.viewport_h <= *v,
            Self::PrefersColorScheme(cs) => match cs {
                ColorScheme::Dark => env.dark,
                ColorScheme::Light => !env.dark,
            },
            Self::PrefersReducedMotion(reduce) => env.reduced_motion == *reduce,
            Self::Pointer(k) => env.pointer == *k,
            Self::Hover(h) => env.hover == *h,
            Self::AnyPointer(k) => env.any_pointer == *k,
            Self::AnyHover(h) => env.any_hover == *h,
        }
    }
}

impl MediaQuery {
    /// 对环境求值。类型段为 false 或任一特性为假 → 整体不适用。
    pub fn eval(&self, env: &MediaEnv) -> bool {
        let applies = self.media_type.unwrap_or(true) && self.features.iter().all(|f| f.eval(env));
        applies != self.negate
    }
}

/// 媒体环境（宿主每帧推送）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MediaEnv {
    pub viewport_w: f32,
    pub viewport_h: f32,
    pub dark: bool,
    pub reduced_motion: bool,
    /// 主输入设备指针精度（第五批⑱）。
    pub pointer: PointerKind,
    /// 主输入设备是否支持悬停。
    pub hover: bool,
    /// 任意输入设备指针精度。
    pub any_pointer: PointerKind,
    /// 任意输入设备是否支持悬停。
    pub any_hover: bool,
}

impl Default for MediaEnv {
    fn default() -> Self {
        Self {
            viewport_w: 1280.0,
            viewport_h: 720.0,
            dark: false,
            reduced_motion: false,
            pointer: PointerKind::Fine,
            hover: true,
            any_pointer: PointerKind::Fine,
            any_hover: true,
        }
    }
}

/// 解析 @media 预lude（delimited 到块前）。内部错误统一走
/// `ParseError<BasicParseError>`（与值解析同一形态），在 at-rule 边界
/// 收敛为 trait 的 `ParseError<()>`。
fn parse_media_query(p: &mut Parser<'_>) -> Result<MediaQuery, ParseError<BasicParseError>> {
    let mut negate = false;
    let mut media_type: Option<bool> = None;
    let mut features = Vec::new();

    p.skip_whitespace();
    // [not | only]?
    if p.try_parse(|p| -> Result<(), ParseError<BasicParseError>> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name)
                if name.eq_ignore_ascii_case("not") || name.eq_ignore_ascii_case("only") =>
            {
                if name.eq_ignore_ascii_case("not") {
                    negate = true;
                }
                Ok(())
            }
            _ => Err(p.new_error_for_next_token()),
        }
    })
    .is_ok()
    {
        p.skip_whitespace();
    }

    // 类型段（可选；整段 try_parse 失败自动回滚）
    let type_parsed = p.try_parse(|p| -> Result<bool, ParseError<BasicParseError>> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) if !name.eq_ignore_ascii_case("and") => Ok(matches!(
                name.to_ascii_lowercase().as_str(),
                "screen" | "all"
            )),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if let Ok(applies) = type_parsed {
        media_type = Some(applies);
    }

    // 首个表达式（`(x)` 开头形态，无 and 前缀；`(` 已消费，直接进块体）
    if p.try_parse(|p| -> Result<(), ParseError<BasicParseError>> {
        let t = p.next()?.clone();
        match &t {
            Token::ParenthesisBlock => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    })
    .is_ok()
    {
        features.push(parse_feature_body(p)?);
    }

    // [and <feature>]*
    loop {
        let has_and = p.try_parse(|p| -> Result<(), ParseError<BasicParseError>> {
            let t = p.next()?.clone();
            match &t {
                Token::Ident(name) if name.eq_ignore_ascii_case("and") => Ok(()),
                _ => Err(p.new_error_for_next_token()),
            }
        });
        if has_and.is_err() {
            break;
        }
        features.push(parse_media_feature(p)?);
    }

    p.expect_exhausted().map_err(ParseError::from)?;
    if media_type.is_none() && features.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    Ok(MediaQuery {
        negate,
        media_type,
        features,
    })
}

/// 解析 `(feature: value)`（负责消费 '('）。
fn parse_media_feature(p: &mut Parser<'_>) -> Result<MediaFeature, ParseError<BasicParseError>> {
    // parse_nested_block 要求「刚消费块 token」——此处负责消费 '('
    p.expect_parenthesis_block()?;
    parse_feature_body(p)
}

/// 块体解析（'(' 已消费或由 try_parse 提交后进入）。
fn parse_feature_body(p: &mut Parser<'_>) -> Result<MediaFeature, ParseError<BasicParseError>> {
    p.parse_nested_block(|p| {
        p.skip_whitespace();
        let t = p.next()?.clone();
        let Token::Ident(name) = &t else {
            return Err(p.new_error_for_next_token());
        };
        let lname = name.to_ascii_lowercase();
        match lname.as_str() {
            "prefers-color-scheme" => {
                p.expect_colon()?;
                p.skip_whitespace();
                let v = p.next()?.clone();
                let Token::Ident(value) = &v else {
                    return Err(p.new_error_for_next_token());
                };
                match value.to_ascii_lowercase().as_str() {
                    "dark" => Ok(MediaFeature::PrefersColorScheme(ColorScheme::Dark)),
                    "light" => Ok(MediaFeature::PrefersColorScheme(ColorScheme::Light)),
                    _ => Err(p.new_error_for_next_token()),
                }
            }
            "prefers-reduced-motion" => {
                p.expect_colon()?;
                p.skip_whitespace();
                let v = p.next()?.clone();
                let Token::Ident(value) = &v else {
                    return Err(p.new_error_for_next_token());
                };
                match value.to_ascii_lowercase().as_str() {
                    "reduce" => Ok(MediaFeature::PrefersReducedMotion(true)),
                    "no-preference" => Ok(MediaFeature::PrefersReducedMotion(false)),
                    _ => Err(p.new_error_for_next_token()),
                }
            }
            "width" | "min-width" | "max-width" | "height" | "min-height" | "max-height" => {
                p.expect_colon()?;
                p.skip_whitespace();
                let lp = parse_length_percentage(p)?;
                // 媒体查询长度换算：em/rem 按 16px 初始字号；vw/vh 与百分比
                // 依赖视口（解析期未知）→ 按 0 处理（偏差记录）
                let px = lp
                    .resolve(
                        &ResolveCtx {
                            em: 16.0,
                            rem: 16.0,
                            viewport_w: 0.0,
                            viewport_h: 0.0,
                        },
                        0.0,
                    )
                    .ok_or_else(|| p.new_error_for_next_token())?;
                Ok(match lname.as_str() {
                    "width" => MediaFeature::Width(px),
                    "min-width" => MediaFeature::MinWidth(px),
                    "max-width" => MediaFeature::MaxWidth(px),
                    "height" => MediaFeature::Height(px),
                    "min-height" => MediaFeature::MinHeight(px),
                    _ => MediaFeature::MaxHeight(px),
                })
            }
            "pointer" | "any-pointer" | "hover" | "any-hover" => {
                // 第五批⑱媒体查询扩展：交互媒体特性（指针/悬停；any- 变体
                // 面向多输入设备）
                p.expect_colon()?;
                p.skip_whitespace();
                let v = p.next()?.clone();
                let Token::Ident(value) = &v else {
                    return Err(p.new_error_for_next_token());
                };
                let lval = value.to_ascii_lowercase();
                if lname == "pointer" || lname == "any-pointer" {
                    let kind = match lval.as_str() {
                        "none" => PointerKind::None,
                        "coarse" => PointerKind::Coarse,
                        "fine" => PointerKind::Fine,
                        _ => return Err(p.new_error_for_next_token()),
                    };
                    Ok(if lname == "pointer" {
                        MediaFeature::Pointer(kind)
                    } else {
                        MediaFeature::AnyPointer(kind)
                    })
                } else {
                    let h = match lval.as_str() {
                        "hover" => true,
                        "none" => false,
                        _ => return Err(p.new_error_for_next_token()),
                    };
                    Ok(if lname == "hover" {
                        MediaFeature::Hover(h)
                    } else {
                        MediaFeature::AnyHover(h)
                    })
                }
            }
            _ => Err(p.new_error_for_next_token()),
        }
    })
}

// ---------- 规则表解析 ----------

/// 顶层/媒体块共用的规则解析器。
#[derive(Default)]
struct StylesheetParser {
    report: ParseReport,
    rules: Vec<Rule>,
    keyframes: Vec<KeyframesRule>,
    media: Option<MediaQuery>,
    order: u32,
}

impl<'i> cssparser::QualifiedRuleParser<'i> for StylesheetParser {
    type Prelude = StyleSelectorList;
    type QualifiedRule = ();
    type Error = ();

    fn parse_prelude(&mut self, input: &mut Parser<'i>) -> Result<Self::Prelude, ParseError<()>> {
        let loc = input.current_source_location();
        let mut buf = TokenBuf::new();
        capture_tokens(input, &mut buf);
        let source = token_buf_to_string(&buf);
        match parse_selector_list(&source) {
            Ok(list) => Ok(list),
            Err(msg) => {
                self.report.push(
                    loc.line + 1,
                    loc.column + 1,
                    format!("invalid selector '{source}': {msg}"),
                );
                Err(ParseError::unexpected_token())
            }
        }
    }

    fn parse_block(
        &mut self,
        prelude: Self::Prelude,
        _start: &cssparser::ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<()>> {
        let (block, report) = parse_declaration_block(input);
        self.report.extend(report);
        self.order += 1;
        self.rules.push(Rule {
            selectors: prelude,
            order: self.order,
            declarations: block,
            media: self.media.clone(),
        });
        Ok(())
    }
}

impl<'i> cssparser::AtRuleParser<'i> for StylesheetParser {
    type Prelude = AtPrelude;
    type AtRule = ();
    type Error = ();

    fn parse_prelude(
        &mut self,
        name: cssparser::CowRcStr<'i>,
        input: &mut Parser<'i>,
    ) -> Result<Self::Prelude, ParseError<()>> {
        if name.eq_ignore_ascii_case("media") {
            let loc = input.current_source_location();
            match parse_media_query(input) {
                Ok(q) => Ok(AtPrelude::Media(q)),
                Err(_) => {
                    self.report.push(
                        loc.line + 1,
                        loc.column + 1,
                        format!("invalid @media condition '{name}'"),
                    );
                    Err(ParseError::unexpected_token())
                }
            }
        } else if name.eq_ignore_ascii_case("font-face") {
            // @font-face 静默跳过（第五批⑯契约）：字体资源=宿主经 add_font
            // 推送字节（零副作用，引擎不取 src() URL），家族名按字体内部名
            // 匹配；良性已知规则不产生报告警告（真实世界 CSS 常携带之，
            // 告警应留给影响渲染的事）。Skip → parse_block 消费整块。
            Ok(AtPrelude::Skip)
        } else if name.eq_ignore_ascii_case("keyframes")
            || name.eq_ignore_ascii_case("-webkit-keyframes")
        {
            // @keyframes（第五批⑰）：prelude = 动画名（ident 或字符串）
            let loc = input.current_source_location();
            let ok = input.try_parse(|p| -> Result<String, ParseError<()>> {
                p.skip_whitespace();
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(id) if !id.starts_with("--") => Ok(id.to_string()),
                    Token::QuotedString(s) => Ok(s.to_string()),
                    _ => Err(ParseError::unexpected_token()),
                }
            });
            match ok {
                Ok(n) => Ok(AtPrelude::Keyframes(n)),
                Err(_) => {
                    self.report.push(
                        loc.line + 1,
                        loc.column + 1,
                        format!("invalid @keyframes name '{name}'"),
                    );
                    Err(ParseError::unexpected_token())
                }
            }
        } else {
            // @import/@supports/@container…：MVP 跳过整条规则并告警
            let loc = input.current_source_location();
            self.report.push(
                loc.line + 1,
                loc.column + 1,
                format!("unsupported at-rule '@{name}' skipped"),
            );
            Err(ParseError::unexpected_token())
        }
    }

    fn parse_block(
        &mut self,
        prelude: Self::Prelude,
        _start: &cssparser::ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<()>> {
        match prelude {
            AtPrelude::Skip => {
                // @font-face（第五批⑯）：整块静默消费——无规则产出、无告警
                // （块 token 自包含，next() 逐 token 推进至块尾即整块耗尽）
                while input.next().is_ok() {}
                Ok(())
            }
            AtPrelude::Media(query) => {
                // 递归解析媒体块内规则（继承 media 上下文与源顺序）
                let mut sub = StylesheetParser {
                    report: ParseReport::new(),
                    rules: Vec::new(),
                    keyframes: Vec::new(),
                    media: Some(query),
                    order: self.order,
                };
                {
                    let iter = cssparser::RuleBodyParser::new(input, &mut sub);
                    for item in iter {
                        let _ = item; // 错误已在 sub 内报告
                    }
                }
                self.order = sub.order;
                self.rules.extend(sub.rules);
                self.report.extend(sub.report);
                Ok(())
            }
            AtPrelude::Keyframes(name) => {
                // 第五批⑰：帧体解析——帧选择器（from/to/百分比，逗号分组）
                // + 声明块
                let mut sub = KeyframesParser::default();
                {
                    let iter = cssparser::RuleBodyParser::new(input, &mut sub);
                    for item in iter {
                        let _ = item; // 错误已在 sub 内报告
                    }
                }
                self.report.extend(sub.report);
                self.keyframes.push(KeyframesRule {
                    name,
                    frames: sub.frames,
                });
                Ok(())
            }
        }
    }
}

/// at-rule prelude 分类（第五批⑰扩展：media / keyframes / 静默跳过）。
#[derive(Debug, Clone)]
enum AtPrelude {
    Media(MediaQuery),
    Keyframes(String),
    /// @font-face（第五批⑯）：整块静默消费。
    Skip,
}

/// @keyframes 块体解析器：帧选择器 → 声明块。
#[derive(Default)]
struct KeyframesParser {
    report: ParseReport,
    frames: Vec<Keyframe>,
}

impl<'i> cssparser::QualifiedRuleParser<'i> for KeyframesParser {
    type Prelude = Vec<f32>;
    type QualifiedRule = ();
    type Error = ();

    fn parse_prelude(&mut self, input: &mut Parser<'i>) -> Result<Self::Prelude, ParseError<()>> {
        let loc = input.current_source_location();
        let mut offsets = Vec::new();
        loop {
            let ok = input.try_parse(|p| -> Result<f32, ParseError<()>> {
                p.skip_whitespace();
                let t = p.next()?.clone();
                match &t {
                    // cssparser Percentage.unit_value = 值/100（50% → 0.5）
                    Token::Percentage { unit_value, .. } => Ok(unit_value.clamp(0.0, 1.0)),
                    Token::Ident(id) if id.eq_ignore_ascii_case("from") => Ok(0.0),
                    Token::Ident(id) if id.eq_ignore_ascii_case("to") => Ok(1.0),
                    _ => Err(ParseError::unexpected_token()),
                }
            });
            match ok {
                Ok(v) => offsets.push(v),
                Err(_) => break,
            }
            // 逗号分组（"0%, 50% { … }"）；无逗号则收束
            let has_comma = input
                .try_parse(|p| -> Result<(), ParseError<()>> {
                    p.skip_whitespace();
                    let t = p.next()?.clone();
                    match t {
                        Token::Comma => Ok(()),
                        _ => Err(ParseError::unexpected_token()),
                    }
                })
                .is_ok();
            if !has_comma {
                break;
            }
        }
        if offsets.is_empty() {
            self.report.push(
                loc.line + 1,
                loc.column + 1,
                "invalid keyframe selector".to_string(),
            );
            return Err(ParseError::unexpected_token());
        }
        Ok(offsets)
    }

    fn parse_block(
        &mut self,
        prelude: Self::Prelude,
        _start: &cssparser::ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<()>> {
        let (block, report) = parse_declaration_block(input);
        self.report.extend(report);
        for offset in prelude {
            self.frames.push(Keyframe {
                offset,
                declarations: block.clone(),
            });
        }
        Ok(())
    }
}

impl<'i> cssparser::AtRuleParser<'i> for KeyframesParser {
    type Prelude = ();
    type AtRule = ();
    type Error = ();
    // 帧体内不允许嵌套 at-rule（默认实现拒绝——RuleBodyItemParser 的
    // trait bound 要求本实现存在）
}

impl<'i> cssparser::DeclarationParser<'i> for KeyframesParser {
    type Declaration = ();
    type Error = ();
    // 帧块内不允许裸声明（漏写帧选择器）；默认实现容错拒绝
}

impl<'i> cssparser::RuleBodyItemParser<'i, (), ()> for KeyframesParser {
    fn parse_declarations(&self) -> bool {
        false
    }
    fn parse_qualified(&self) -> bool {
        true
    }
}

impl<'i> cssparser::DeclarationParser<'i> for StylesheetParser {
    type Declaration = ();
    type Error = ();
    // 顶层不允许裸声明（漏写选择器）；默认实现容错拒绝
}

impl<'i> cssparser::RuleBodyItemParser<'i, (), ()> for StylesheetParser {
    fn parse_declarations(&self) -> bool {
        false
    }
    fn parse_qualified(&self) -> bool {
        true
    }
}

/// 解析样式表源文本（容错：坏规则跳过并记入 report）。
pub fn parse_stylesheet(source: &str) -> Stylesheet {
    let mut input = Parser::new(source);
    let mut sp = StylesheetParser::default();
    let mut skipped: Vec<(u32, u32)> = Vec::new();
    {
        let iter = cssparser::StyleSheetParser::new(&mut input, &mut sp);
        for item in iter {
            if let Err((_, _, loc)) = item {
                skipped.push((loc.line + 1, loc.column + 1));
            }
        }
    }
    for (line, column) in skipped {
        sp.report
            .push(line, column, "invalid rule skipped".to_string());
    }
    Stylesheet {
        rules: sp.rules,
        keyframes: sp.keyframes,
        report: sp.report,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::decl::DeclSource;
    use crate::css::property::DeclValue;

    #[test]
    fn parses_rules_and_media() {
        let sheet = parse_stylesheet(
            "h1 { color: red } .card { width: 100px } \
             @media screen and (min-width: 600px) { .card { width: 200px } } \
             @media (prefers-color-scheme: dark) { :root { --bg: black } }",
        );
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.rules.len(), 4);
        assert!(sheet.rules[0].media.is_none());
        assert!(sheet.rules[2].media.is_some());
        assert!(sheet.rules[2].media.as_ref().unwrap().eval(&MediaEnv {
            viewport_w: 800.0,
            viewport_h: 600.0,
            dark: false,
            reduced_motion: false,
            ..Default::default()
        }));
        assert!(!sheet.rules[2].media.as_ref().unwrap().eval(&MediaEnv {
            viewport_w: 400.0,
            viewport_h: 600.0,
            dark: false,
            reduced_motion: false,
            ..Default::default()
        }));
        assert!(sheet.rules[3].media.as_ref().unwrap().eval(&MediaEnv {
            viewport_w: 800.0,
            viewport_h: 600.0,
            dark: true,
            reduced_motion: false,
            ..Default::default()
        }));
    }

    #[test]
    fn tolerates_bad_selector_and_unknown_at_rule() {
        let sheet = parse_stylesheet(
            "@import url(x.css); :::bad { color: red } .ok { color: blue } @media print { .p { color: black } }",
        );
        assert!(!sheet.report.is_clean());
        // 坏选择器丢弃；print 类型段 eval=false 但规则保留
        assert_eq!(sheet.rules.len(), 2);
        assert_eq!(sheet.rules[0].declarations.decls.len(), 1);
    }

    #[test]
    fn font_face_at_rule_silently_skipped() {
        // 第五批⑯契约：@font-face 跳过且不产生报告警告（字体=宿主 add_font
        // 契约，引擎不取 src() URL）——真实世界 CSS 携带 @font-face 不产生
        // 告警噪音，后续规则解析不受影响
        let sheet = parse_stylesheet(
            "@font-face { font-family: 'X'; src: url(x.woff2); font-display: swap; } .ok { color: blue }",
        );
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].declarations.decls.len(), 1);
    }

    #[test]
    fn pointer_and_hover_features() {
        // 第五批⑱媒体查询扩展：pointer/hover/any-pointer/any-hover——
        // 解析 + 环境求值（MediaEnv 扩展四字段，宿主每帧推送）
        let sheet = parse_stylesheet(
            "@media (pointer: coarse) and (hover: none) { .a { color: red } } \
             @media (any-pointer: fine) { .b { color: blue } }",
        );
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.rules.len(), 2);
        let touch = MediaEnv {
            pointer: PointerKind::Coarse,
            hover: false,
            any_pointer: PointerKind::Fine,
            any_hover: true,
            ..Default::default()
        };
        assert!(sheet.rules[0].media.as_ref().unwrap().eval(&touch));
        assert!(sheet.rules[1].media.as_ref().unwrap().eval(&touch));
        // 桌面默认环境（Fine/hover=true）：触屏查询不适用
        let desk = MediaEnv::default();
        assert!(!sheet.rules[0].media.as_ref().unwrap().eval(&desk));
        // any-hover 与 hover 独立：主设备无悬停但副设备有 → any-hover: hover
        // 命中
        let hybrid = MediaEnv {
            pointer: PointerKind::None,
            hover: false,
            any_pointer: PointerKind::Fine,
            any_hover: true,
            ..Default::default()
        };
        let sheet2 = parse_stylesheet("@media (any-hover: hover) { .c { color: green } }");
        assert!(sheet2.report.is_clean(), "{:?}", sheet2.report);
        assert!(sheet2.rules[0].media.as_ref().unwrap().eval(&hybrid));
        assert!(!sheet2.rules[0].media.as_ref().unwrap().eval(&MediaEnv {
            any_hover: false,
            ..Default::default()
        }));
    }

    #[test]
    fn keyframes_parse() {
        // 第五批⑰：@keyframes 解析——from/to/百分比帧、逗号分组帧选择、
        // -webkit-keyframes 别名、坏帧选择器整帧容错丢弃
        let sheet = parse_stylesheet(
            "@keyframes grow { from { width: 100px } 50% { width: 150px } \
             to { width: 200px } } @-webkit-keyframes fade { from { opacity: 1 } }",
        );
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.keyframes.len(), 2);
        let grow = &sheet.keyframes[0];
        assert_eq!(grow.name, "grow");
        assert_eq!(grow.frames.len(), 3);
        assert_eq!(grow.frames[0].offset, 0.0);
        assert_eq!(grow.frames[1].offset, 0.5);
        assert_eq!(grow.frames[2].offset, 1.0);
        // 逗号分组：0%, 50% { … } 产出两帧
        let sheet2 = parse_stylesheet("@keyframes pulse { 0%, 50% { opacity: 0.5 } }");
        assert!(sheet2.report.is_clean(), "{:?}", sheet2.report);
        assert_eq!(sheet2.keyframes[0].frames.len(), 2);
        // 坏帧选择器：整帧容错丢弃 + 告警
        let bad = parse_stylesheet("@keyframes bad { nope { width: 1px } }");
        assert!(!bad.report.is_clean());
        assert!(bad.keyframes[0].frames.is_empty());
    }

    #[test]
    fn media_not_and_height() {
        let sheet =
            parse_stylesheet("@media not screen and (max-height: 500px) { .a { color: red } }");
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        let q = sheet.rules[0].media.as_ref().unwrap();
        let env = MediaEnv {
            viewport_w: 1280.0,
            viewport_h: 400.0,
            dark: false,
            reduced_motion: false,
            ..Default::default()
        };
        // max-height 命中 + not 取反 → false
        assert!(!q.eval(&env));
        let env_tall = MediaEnv {
            viewport_h: 800.0,
            ..env
        };
        // max-height 不命中 → and 短路 false → not → true
        assert!(q.eval(&env_tall));
    }

    #[test]
    fn declaration_values_visible() {
        let sheet = parse_stylesheet("div { margin: 4px auto; display: flex }");
        assert!(sheet.report.is_clean());
        let decls = &sheet.rules[0].declarations.decls;
        // margin: 4px auto 展开为 4 条 + display
        assert_eq!(decls.len(), 5);
        assert!(matches!(
            &decls[0].value,
            DeclSource::Parsed(DeclValue::LenAuto(Some(_)))
        ));
        assert!(matches!(
            &decls[4].value,
            DeclSource::Parsed(DeclValue::Display(_))
        ));
    }
}
