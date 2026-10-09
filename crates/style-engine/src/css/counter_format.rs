//! E 计数器样式格式化（css-counter-styles-3 §2 生成算法 + §3 描述符 +
//! §6 内置样式子集）。
//!
//! 入口 [`format_counter_with`]：name + 计数值 + 文档级 @counter-style
//! 登记表（ua → user → 主表 → 附加表合并序，同名后写胜）→ 表示串。
//! 登记表可整体覆盖内置样式（css-counter-styles-3 §3：predefined 在
//! UA 表中，任何 author 定义后写胜）。
//!
//! spec 对齐要点（2021-07-27 CR）：
//! - §2 注：**prefix/suffix 不进入 counter()/counters() 输出**——它们仅由
//!   ::marker 内容算法追加。本函数因此不拼接二者（decimal 缺省 suffix
//!   ". " 不渲染，counter(x) = 纯数字）。
//! - §2 生成算法次序：未知名 → decimal；范围外 → fallback；负值且使用
//!   负号 → 按绝对值生成；pad 补齐（差值再减负号簇数）；负号包裹。
//! - 负号使用条件（§3.2）：system ∈ {symbolic, alphabetic, numeric,
//!   additive}（extends 继承基样式）；cyclic/fixed 不使用——负值按原值
//!   进核心算法（cyclic 取模回绕；fixed 出窗 → fallback）。`negative`
//!   未写时初始值 "-"。
//! - auto range（§3.5）：cyclic/numeric/fixed 全域；alphabetic/symbolic
//!   1..∞；additive 0..∞。
//! - system 符号数不足（§3.1 前言）：规则不定义计数样式 → 视为未知名 →
//!   decimal。最低要求 cyclic/fixed/symbolic ≥1、alphabetic/numeric ≥2、
//!   additive ≥1 组。
//! - fallback（§3.7）：未写/未知名 → decimal；成环 → decimal；`none`
//!   宽容按空输出（解析层在案宽容的延续，【B】级）。
//! - extends（§3.1.7）：未指定描述符继承基样式；环/未知基名 → decimal。
//!   覆盖检测 = 字段 ≠ 缺省值（无法区分"恰好等于缺省的显式值"，【B】级
//!   近似）；extends 规则带 symbols/additive-symbols 本应整规则无效，
//!   此处宽容并入（【B】级）。
//! - 表示长度护栏（§2 注：UA 必须支持 ≥60 码点、可以更长即回退）：表示
//!   超过 60 码点 → fallback（防 symbolic/additive 线性膨胀 OOM，pad 差
//!   值同步钳制）。
//!
//! 内置子集（§6）：decimal、decimal-leading-zero、lower/upper-roman、
//! lower/upper-alpha、lower/upper-latin、lower-greek、disc、circle、
//! square。其余预定义样式未收——未知名统一回退 decimal。

use crate::css::stylesheet::counter_style::{
    CounterStyleRange, CounterStyleRule, CounterStyleSystem,
};

/// 表示长度护栏（css-counter-styles-3 §2 注的 UA 允许项下限）。
const MAX_REP_CODEPOINTS: usize = 60;

/// P9-3（css-lists-3 §3.2 ③）：::marker 内容 = list-item 计数表示 +
/// prefix + suffix（css-counter-styles-3 §2 注：affixes 仅由 ::marker
/// 内容算法追加——counter()/counters() 不含，故 [`format_counter_with`]
/// 不拼接）。`none` → 空串（调用方据以抑制成盒）；未知名按 decimal
/// 生成（缺省 suffix ". "）。
pub fn marker_text(name: &str, value: i64, registry: &[CounterStyleRule]) -> String {
    if name.eq_ignore_ascii_case("none") {
        return String::new();
    }
    match resolve_rule(name, registry) {
        Some(rule) => {
            let rep = format_rule(&rule, value, registry, &mut vec![name.to_string()]);
            let prefix = rule.prefix.unwrap_or_default();
            format!("{prefix}{rep}{}", rule.suffix)
        }
        // css-counter-styles-3 §2 步骤 1：未知样式按 decimal 生成
        // （decimal = Numeric 系统 + suffix ". "）。
        None => format!("{value}. "),
    }
}

/// 以指定计数样式格式化一个计数值。
///
/// `registry` 为文档级 @counter-style 登记表（合并序，同名后写胜）；
/// `name` 未在登记表命中时回退内置样式，再未命中 → decimal。
/// `none` 样式名（list-style/counter() 的关键字）→ 空输出。
pub fn format_counter_with(name: &str, value: i64, registry: &[CounterStyleRule]) -> String {
    if name.eq_ignore_ascii_case("none") {
        return String::new();
    }
    let mut visited: Vec<String> = vec![name.to_string()];
    format_resolving(name, value, registry, &mut visited)
}

/// 按名解析（登记表后写胜 → 内置 → 未知名）并格式化。
fn format_resolving(
    name: &str,
    value: i64,
    registry: &[CounterStyleRule],
    visited: &mut Vec<String>,
) -> String {
    match resolve_rule(name, registry) {
        Some(rule) => format_rule(&rule, value, registry, visited),
        // css-counter-styles-3 §2 步骤 1：未知样式按 decimal 生成。
        None => value.to_string(),
    }
}

/// 名字 → 规则：登记表精确匹配（spec 计数样式名大小写敏感）优先，未
/// 命中回退内置（内置按 spec「解析期一律小写」的语义做 ASCII 不敏感
/// 匹配，容忍 UPPER-ROMAN 式写法）。
fn resolve_rule(name: &str, registry: &[CounterStyleRule]) -> Option<CounterStyleRule> {
    if let Some(r) = registry.iter().rev().find(|r| r.name == name) {
        return Some(r.clone());
    }
    builtin_rule(name)
}

/// css-counter-styles-3 §2 生成算法（extends 已在此前置展开）。
fn format_rule(
    rule: &CounterStyleRule,
    value: i64,
    registry: &[CounterStyleRule],
    visited: &mut Vec<String>,
) -> String {
    // §3.1.7 extends：先合并基样式再走统一管线。基名未知/成环 → 按
    // extends decimal 处理（spec：参与环的样式一律视为 extends decimal）。
    if let CounterStyleSystem::Extends(base) = &rule.system {
        if base.eq_ignore_ascii_case("none") || visited.iter().any(|v| v == base) {
            return value.to_string();
        }
        let Some(base_rule) = resolve_rule(base, registry) else {
            return value.to_string();
        };
        visited.push(base.clone());
        let mut eff = base_rule;
        eff.name = rule.name.clone();
        // 覆盖检测 = 字段 ≠ 缺省（【B】级近似，见模块注）。
        if !rule.symbols.is_empty() {
            eff.symbols = rule.symbols.clone();
        }
        if !rule.additive_symbols.is_empty() {
            eff.additive_symbols = rule.additive_symbols.clone();
        }
        if rule.prefix.is_some() {
            eff.prefix = rule.prefix.clone();
        }
        if rule.suffix != ". " {
            eff.suffix = rule.suffix.clone();
        }
        if rule.range != CounterStyleRange::Auto {
            eff.range = rule.range.clone();
        }
        if rule.pad.is_some() {
            eff.pad = rule.pad.clone();
        }
        if !rule.negative.is_empty() {
            eff.negative = rule.negative.clone();
        }
        if rule.fallback.is_some() {
            eff.fallback = rule.fallback.clone();
        }
        return format_rule(&eff, value, registry, visited);
    }

    // §3.1 前言：符号数不足 = 未定义计数样式 → decimal。
    if !system_requirements_met(&rule.system, rule) {
        return value.to_string();
    }

    // §2 步骤 2：范围外 → fallback。
    let in_range = match &rule.range {
        CounterStyleRange::Auto => auto_range_allows(&rule.system, value),
        CounterStyleRange::Ranges(list) => list.iter().any(|(lo, hi)| {
            lo.is_none_or(|l| value >= l as i64) && hi.is_none_or(|h| value <= h as i64)
        }),
    };
    if !in_range {
        return fallback_of(rule, value, registry, visited);
    }

    let uses_neg = uses_negative_sign(&rule.system);
    // §2 步骤 3：负值且使用负号 → 按绝对值生成。
    let mag: u64 = value.unsigned_abs();

    // §2 步骤 3：初始表示。无法表示 → fallback。
    let Some(mut rep) = format_core(&rule.system, value, mag, rule) else {
        return fallback_of(rule, value, registry, visited);
    };

    // §3.6 pad：差值 = min_len − 表示簇数，负值再减负号簇数；差值 > 0
    // 前插 pad 符号。差值钳 60（护栏内的迭代上限；超长由下方总护栏
    // 统一回退）。负值 pad 计入负号——"-5" + pad 3 "0" → "-05"。
    if let Some((min_len, pad_sym)) = &rule.pad {
        let mut diff = *min_len as i64 - rep.chars().count() as i64;
        if value < 0 && uses_neg {
            diff -= negative_marker_chars(rule) as i64;
        }
        if diff > 0 {
            let mut padded = String::new();
            for _ in 0..diff.min(MAX_REP_CODEPOINTS as i64) {
                padded.push_str(pad_sym);
            }
            padded.push_str(&rep);
            rep = padded;
        }
    }

    // §2 步骤 5：负号包裹（负值且使用负号）。
    if value < 0 && uses_neg {
        let (pre, post) = effective_negative(rule);
        rep = format!("{pre}{rep}{post}");
    }

    // §2 注：UA 可对超过 60 码点的表示改用 fallback（防膨胀总护栏）。
    if rep.chars().count() > MAX_REP_CODEPOINTS {
        return fallback_of(rule, value, registry, visited);
    }
    rep
}

/// system 最低符号数（css-counter-styles-3 §3.1 前言）。
fn system_requirements_met(system: &CounterStyleSystem, rule: &CounterStyleRule) -> bool {
    match system {
        CounterStyleSystem::Cyclic | CounterStyleSystem::Symbolic => !rule.symbols.is_empty(),
        CounterStyleSystem::Fixed(_) => !rule.symbols.is_empty(),
        CounterStyleSystem::Alphabetic | CounterStyleSystem::Numeric => rule.symbols.len() >= 2,
        CounterStyleSystem::Additive => !rule.additive_symbols.is_empty(),
        // extends 本身无 symbols 要求（继承基样式；带 symbols 的宽容见模块注）。
        CounterStyleSystem::Extends(_) => true,
    }
}

/// auto range 域（css-counter-styles-3 §3.5）。
fn auto_range_allows(system: &CounterStyleSystem, value: i64) -> bool {
    match system {
        CounterStyleSystem::Cyclic | CounterStyleSystem::Numeric | CounterStyleSystem::Fixed(_) => {
            true
        }
        CounterStyleSystem::Alphabetic | CounterStyleSystem::Symbolic => value >= 1,
        CounterStyleSystem::Additive => value >= 0,
        // extends 已前置展开；合并后 eff.system 为具体系统。
        CounterStyleSystem::Extends(_) => true,
    }
}

/// 负号使用条件（css-counter-styles-3 §3.2）：symbolic/alphabetic/
/// numeric/additive 使用；cyclic/fixed 不使用。
fn uses_negative_sign(system: &CounterStyleSystem) -> bool {
    matches!(
        system,
        CounterStyleSystem::Symbolic
            | CounterStyleSystem::Alphabetic
            | CounterStyleSystem::Numeric
            | CounterStyleSystem::Additive
    )
}

/// 有效负号标记：描述符未写时初始值 "-"（css-counter-styles-3 §3.2）。
fn effective_negative(rule: &CounterStyleRule) -> (&str, &str) {
    match rule.negative.as_slice() {
        [] => ("-", ""),
        [a] => (a.as_str(), ""),
        [a, b, ..] => (a.as_str(), b.as_str()),
    }
}

/// 有效负号标记的码点簇总数（pad 差值扣减用）。
fn negative_marker_chars(rule: &CounterStyleRule) -> usize {
    match rule.negative.as_slice() {
        [] => 1,
        [a] => a.chars().count(),
        [a, b, ..] => a.chars().count() + b.chars().count(),
    }
}

/// §3.1 各 system 的核心表示算法。
///
/// `value` = 原始有符号值（cyclic/fixed 不使用负号，负值原样进入）；
/// `mag` = 绝对值（使用负号的系统在 §2 步骤 3 已换算）。返回 None =
/// 该值不可表示 → fallback。
fn format_core(
    system: &CounterStyleSystem,
    value: i64,
    mag: u64,
    rule: &CounterStyleRule,
) -> Option<String> {
    match system {
        // §3.1.1：symbol((value-1) mod N)，全域（i128 防溢出取模）。
        CounterStyleSystem::Cyclic => {
            let n = rule.symbols.len() as i128;
            let idx = ((value as i128 - 1).rem_euclid(n)) as usize;
            Some(rule.symbols[idx].clone())
        }
        // §3.1.2：symbols[value − first_symbol_value]，出窗 → None。
        CounterStyleSystem::Fixed(start) => {
            let n = rule.symbols.len() as i128;
            let idx = value as i128 - *start as i128;
            if idx >= 0 && idx < n {
                Some(rule.symbols[idx as usize].clone())
            } else {
                None
            }
        }
        // §3.1.3：symbol((value-1) mod N) 重复 ceil(value/N) 次；仅正域。
        CounterStyleSystem::Symbolic => {
            if mag == 0 {
                return None;
            }
            let n = rule.symbols.len() as u64;
            let sym = &rule.symbols[((mag - 1) % n) as usize];
            let reps = mag.div_ceil(n);
            // 护栏预检：膨胀前中止。
            if reps.saturating_mul(sym.chars().count() as u64) > MAX_REP_CODEPOINTS as u64 {
                return None;
            }
            Some(sym.repeat(reps as usize))
        }
        // §3.1.4：bijective base-N（1→a … 26→z … 27→aa）；仅正域。
        CounterStyleSystem::Alphabetic => {
            if mag == 0 {
                return None;
            }
            let n = rule.symbols.len() as u64;
            let mut v = mag;
            let mut out = Vec::new();
            while v != 0 {
                v -= 1;
                out.push(rule.symbols[(v % n) as usize].clone());
                v /= n;
            }
            let rep: String = out.into_iter().rev().collect();
            if rep.chars().count() > MAX_REP_CODEPOINTS {
                return None;
            }
            Some(rep)
        }
        // §3.1.5：positional base-N（0 → symbol(0)）；全域（按绝对值）。
        CounterStyleSystem::Numeric => {
            let n = rule.symbols.len() as u64;
            if mag == 0 {
                return Some(rule.symbols[0].clone());
            }
            let mut v = mag;
            let mut out = Vec::new();
            while v != 0 {
                out.push(rule.symbols[(v % n) as usize].clone());
                v /= n;
            }
            let rep: String = out.into_iter().rev().collect();
            if rep.chars().count() > MAX_REP_CODEPOINTS {
                return None;
            }
            Some(rep)
        }
        // §3.1.6：对递减 weight 组做贪婪组合；0 → 零权组或 None；
        // 剩余不可消 → None。
        CounterStyleSystem::Additive => {
            if mag == 0 {
                return rule
                    .additive_symbols
                    .iter()
                    .find(|(w, _)| *w == 0)
                    .map(|(_, s)| s.clone());
            }
            let mut v = mag;
            let mut out = String::new();
            for (w, sym) in &rule.additive_symbols {
                if *w <= 0 {
                    continue;
                }
                let weight = *w as u64;
                if weight > v {
                    continue;
                }
                let reps = v / weight;
                // 护栏预检：本轮追加即超限 → 不可表示（防 v/weight 巨量循环）。
                if out.chars().count() as u64 + reps.saturating_mul(sym.chars().count() as u64)
                    > MAX_REP_CODEPOINTS as u64
                {
                    return None;
                }
                for _ in 0..reps {
                    out.push_str(sym);
                }
                v -= weight * reps;
                if v == 0 {
                    return Some(out);
                }
            }
            None
        }
        // extends 已前置展开；不可达。
        CounterStyleSystem::Extends(_) => None,
    }
}

/// §3.7 fallback：未写/未知名 → decimal；`none` 宽容 → 空输出；成环 →
/// decimal；否则以 fallback 样式重走完整生成算法。
fn fallback_of(
    rule: &CounterStyleRule,
    value: i64,
    registry: &[CounterStyleRule],
    visited: &mut Vec<String>,
) -> String {
    let Some(fb) = &rule.fallback else {
        return value.to_string();
    };
    if fb.eq_ignore_ascii_case("none") {
        return String::new();
    }
    if visited.iter().any(|v| v == fb) {
        return value.to_string();
    }
    visited.push(fb.clone());
    format_resolving(fb, value, registry, visited)
}

/// §6 内置样式子集。ASCII 不敏感匹配（spec：预定义名解析期一律小写）。
fn builtin_rule(name: &str) -> Option<CounterStyleRule> {
    let lower = name.to_ascii_lowercase();
    let mut rule = CounterStyleRule {
        name: lower.clone(),
        ..CounterStyleRule::default()
    };
    match lower.as_str() {
        "decimal" => {
            rule.system = CounterStyleSystem::Numeric;
            rule.symbols = vec!["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]
                .into_iter()
                .map(str::to_string)
                .collect();
        }
        "decimal-leading-zero" => {
            rule.system = CounterStyleSystem::Numeric;
            rule.symbols = vec!["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]
                .into_iter()
                .map(str::to_string)
                .collect();
            rule.pad = Some((2, "0".to_string()));
        }
        "lower-roman" | "upper-roman" => {
            rule.system = CounterStyleSystem::Additive;
            let digits = [
                (1000, "m"),
                (900, "cm"),
                (500, "d"),
                (400, "cd"),
                (100, "c"),
                (90, "xc"),
                (50, "l"),
                (40, "xl"),
                (10, "x"),
                (9, "ix"),
                (5, "v"),
                (4, "iv"),
                (1, "i"),
            ];
            let upper = lower == "upper-roman";
            rule.additive_symbols = digits
                .into_iter()
                .map(|(w, s)| {
                    (
                        w,
                        if upper {
                            s.to_ascii_uppercase()
                        } else {
                            s.to_string()
                        },
                    )
                })
                .collect();
            // spec §6.1：内置 roman 定义即 range: 1 3999——0/负值/超 3999
            // 域外 → fallback decimal（负号路径对内置 roman 不可达）。
            rule.range = CounterStyleRange::Ranges(vec![(Some(1), Some(3999))]);
        }
        "lower-alpha" | "lower-latin" => {
            rule.system = CounterStyleSystem::Alphabetic;
            rule.symbols = ('a'..='z').map(String::from).collect();
        }
        "upper-alpha" | "upper-latin" => {
            rule.system = CounterStyleSystem::Alphabetic;
            rule.symbols = ('A'..='Z').map(String::from).collect();
        }
        "lower-greek" => {
            rule.system = CounterStyleSystem::Alphabetic;
            rule.symbols = [
                "α", "β", "γ", "δ", "ε", "ζ", "η", "θ", "ι", "κ", "λ", "μ", "ν", "ξ", "ο", "π",
                "ρ", "σ", "τ", "υ", "φ", "χ", "ψ", "ω",
            ]
            .into_iter()
            .map(str::to_string)
            .collect();
        }
        // §6.3：cyclic 单符号（UA 可换绘；counter() 文本按规范符号）。
        "disc" => {
            rule.system = CounterStyleSystem::Cyclic;
            rule.symbols = vec!["•".to_string()];
            rule.suffix = " ".to_string();
        }
        "circle" => {
            rule.system = CounterStyleSystem::Cyclic;
            rule.symbols = vec!["◦".to_string()];
            rule.suffix = " ".to_string();
        }
        "square" => {
            rule.system = CounterStyleSystem::Cyclic;
            rule.symbols = vec!["▪".to_string()];
            rule.suffix = " ".to_string();
        }
        _ => return None,
    }
    Some(rule)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(name: &str, system: CounterStyleSystem, symbols: &[&str]) -> CounterStyleRule {
        CounterStyleRule {
            name: name.to_string(),
            system,
            symbols: symbols.iter().map(|s| s.to_string()).collect(),
            ..CounterStyleRule::default()
        }
    }

    fn fmt(name: &str, value: i64, registry: &[CounterStyleRule]) -> String {
        format_counter_with(name, value, registry)
    }

    #[test]
    fn builtin_decimal_and_leading_zero() {
        assert_eq!(fmt("decimal", 5, &[]), "5");
        assert_eq!(fmt("decimal", 0, &[]), "0");
        assert_eq!(fmt("decimal", -5, &[]), "-5");
        // pad 计入负号（css-counter-styles-3 §3.6 示例）。
        assert_eq!(fmt("decimal-leading-zero", 1, &[]), "01");
        assert_eq!(fmt("decimal-leading-zero", 20, &[]), "20");
        assert_eq!(fmt("decimal-leading-zero", 4000, &[]), "4000");
        // spec §3.6：pad 差值按负号簇数折减——"5" 差 1 扣 "-" 1 → 0 不补
        // 零；-5 恰 2 簇满足 min 2（即 "-05" 需 pad: 3）。
        assert_eq!(fmt("decimal-leading-zero", -5, &[]), "-5");
    }

    #[test]
    fn builtin_roman_all_phases() {
        assert_eq!(fmt("upper-roman", 1, &[]), "I");
        assert_eq!(fmt("upper-roman", 4, &[]), "IV");
        assert_eq!(fmt("upper-roman", 9, &[]), "IX");
        assert_eq!(fmt("upper-roman", 14, &[]), "XIV");
        assert_eq!(fmt("upper-roman", 40, &[]), "XL");
        assert_eq!(fmt("upper-roman", 90, &[]), "XC");
        assert_eq!(fmt("upper-roman", 400, &[]), "CD");
        assert_eq!(fmt("upper-roman", 900, &[]), "CM");
        assert_eq!(fmt("upper-roman", 1999, &[]), "MCMXCIX");
        assert_eq!(fmt("lower-roman", 1999, &[]), "mcmxcix");
        // spec §6.1：内置 roman 带 range 1 3999——0/负值/超 3999 域外 →
        // fallback decimal（负号路径对内置 roman 不可达）。
        assert_eq!(fmt("upper-roman", 0, &[]), "0");
        assert_eq!(fmt("upper-roman", 4000, &[]), "4000");
        assert_eq!(fmt("upper-roman", -5, &[]), "-5");
    }

    #[test]
    fn builtin_alpha_and_greek() {
        assert_eq!(fmt("lower-alpha", 1, &[]), "a");
        assert_eq!(fmt("lower-alpha", 26, &[]), "z");
        assert_eq!(fmt("lower-alpha", 27, &[]), "aa");
        assert_eq!(fmt("lower-alpha", 28, &[]), "ab");
        assert_eq!(fmt("lower-alpha", 52, &[]), "az");
        assert_eq!(fmt("lower-alpha", 53, &[]), "ba");
        assert_eq!(fmt("lower-alpha", 703, &[]), "aaa");
        assert_eq!(fmt("upper-alpha", 1, &[]), "A");
        assert_eq!(fmt("upper-alpha", 27, &[]), "AA");
        assert_eq!(fmt("lower-latin", 27, &[]), "aa");
        assert_eq!(fmt("upper-latin", 2, &[]), "B");
        // 0 在 alphabetic auto-range（1..∞）外 → decimal。
        assert_eq!(fmt("lower-alpha", 0, &[]), "0");
        assert_eq!(fmt("lower-greek", 1, &[]), "α");
        assert_eq!(fmt("lower-greek", 24, &[]), "ω");
        assert_eq!(fmt("lower-greek", 25, &[]), "αα");
    }

    #[test]
    fn builtin_symbolic_bullets() {
        assert_eq!(fmt("disc", 1, &[]), "•");
        assert_eq!(fmt("disc", 100, &[]), "•");
        assert_eq!(fmt("circle", 1, &[]), "◦");
        assert_eq!(fmt("square", 1, &[]), "▪");
        // cyclic 不使用负号：负值取模回绕（单符号恒 symbols[0]）。
        assert_eq!(fmt("disc", -7, &[]), "•");
    }

    #[test]
    fn symbolic_repeats_per_pass() {
        // css-counter-styles-3 §3.1.3 footnote 示例形态。
        let r = rule(
            "footnote",
            CounterStyleSystem::Symbolic,
            &["*", "⁑", "†", "‡"],
        );
        let reg = [r];
        assert_eq!(fmt("footnote", 1, &reg), "*");
        assert_eq!(fmt("footnote", 2, &reg), "⁑");
        assert_eq!(fmt("footnote", 4, &reg), "‡");
        assert_eq!(fmt("footnote", 5, &reg), "**");
        assert_eq!(fmt("footnote", 6, &reg), "⁑⁑");
        // 0 在 symbolic auto-range（1..∞）外 → decimal。
        assert_eq!(fmt("footnote", 0, &reg), "0");
    }

    #[test]
    fn numeric_custom_base() {
        let r = rule("trinary", CounterStyleSystem::Numeric, &["0", "1", "2"]);
        let reg = [r];
        assert_eq!(fmt("trinary", 0, &reg), "0");
        assert_eq!(fmt("trinary", 2, &reg), "2");
        assert_eq!(fmt("trinary", 3, &reg), "10");
        assert_eq!(fmt("trinary", 5, &reg), "12");
        assert_eq!(fmt("trinary", 8, &reg), "22");
        assert_eq!(fmt("trinary", 9, &reg), "100");
        // numeric 使用负号：按绝对值 + "-"。
        assert_eq!(fmt("trinary", -3, &reg), "-10");
    }

    #[test]
    fn fixed_first_symbol_value_and_exhaustion() {
        let r = rule("box", CounterStyleSystem::Fixed(3), &["a", "b"]);
        let reg = [r];
        assert_eq!(fmt("box", 3, &reg), "a");
        assert_eq!(fmt("box", 4, &reg), "b");
        // 出窗 → fallback decimal（缺省）。
        assert_eq!(fmt("box", 2, &reg), "2");
        assert_eq!(fmt("box", 5, &reg), "5");
        // fixed 不使用负号：负值出窗 → fallback。
        assert_eq!(fmt("box", -1, &reg), "-1");
    }

    #[test]
    fn custom_explicit_range_and_fallback_chain() {
        let mut a = rule("caps", CounterStyleSystem::Fixed(1), &["A", "B", "C"]);
        a.fallback = Some("upper-roman".to_string());
        a.range = CounterStyleRange::Ranges(vec![(Some(1), Some(3))]);
        let reg = [a];
        assert_eq!(fmt("caps", 1, &reg), "A");
        assert_eq!(fmt("caps", 3, &reg), "C");
        // 范围外 → fallback 链 → upper-roman 完整算法。
        assert_eq!(fmt("caps", 4, &reg), "IV");
        assert_eq!(fmt("caps", 0, &reg), "0");
    }

    #[test]
    fn fallback_loop_guards_to_decimal() {
        let mut a = rule("a", CounterStyleSystem::Fixed(1), &["x"]);
        a.range = CounterStyleRange::Ranges(vec![(Some(1), Some(1))]);
        a.fallback = Some("b".to_string());
        let mut b = rule("b", CounterStyleSystem::Fixed(3), &["y", "z"]);
        b.range = CounterStyleRange::Ranges(vec![(Some(3), Some(4))]);
        b.fallback = Some("a".to_string());
        let reg = [a, b];
        // A 范围外 → B → B 范围外 → A（成环）→ decimal。
        assert_eq!(fmt("a", 2, &reg), "2");
        assert_eq!(fmt("a", 1, &reg), "x");
        // B 域内 fixed 窗：3 → "y"、4 → "z"。
        assert_eq!(fmt("a", 4, &reg), "z");
        assert_eq!(fmt("b", 3, &reg), "y");
    }

    #[test]
    fn extends_merges_base_and_overrides() {
        let mut paren = CounterStyleRule {
            name: "decimal-paren".to_string(),
            system: CounterStyleSystem::Extends("decimal".to_string()),
            pad: Some((3, "0".to_string())),
            ..CounterStyleRule::default()
        };
        paren.negative = vec!["(".to_string(), ")".to_string()];
        let reg = [paren];
        assert_eq!(fmt("decimal-paren", 5, &reg), "005");
        // 负值：pad 差值先扣负号簇数（1+2=3 − 剩 0）→ 不补零。
        assert_eq!(fmt("decimal-paren", -5, &reg), "(5)");
        // extends roman：继承 additive 算法与显式 range（1 3999）——0/负值
        // 域外 → fallback decimal，负号路径对内置 roman 不可达。
        let mut er = CounterStyleRule {
            name: "er".to_string(),
            system: CounterStyleSystem::Extends("upper-roman".to_string()),
            ..CounterStyleRule::default()
        };
        er.negative = vec!["(".to_string(), ")".to_string()];
        let reg = [er];
        assert_eq!(fmt("er", 4, &reg), "IV");
        assert_eq!(fmt("er", -4, &reg), "-4");
        assert_eq!(fmt("er", 0, &reg), "0");
        // extends numeric 基样式（auto 范围含负）→ 负号生效：-1 → rep
        // "n1" 包裹 "(n1)"。
        let base = rule("neg-base", CounterStyleSystem::Numeric, &["n0", "n1"]);
        let mut neg = CounterStyleRule {
            name: "neg-over".to_string(),
            system: CounterStyleSystem::Extends("neg-base".to_string()),
            ..CounterStyleRule::default()
        };
        neg.negative = vec!["(".to_string(), ")".to_string()];
        let reg = [base, neg];
        assert_eq!(fmt("neg-over", 1, &reg), "n1");
        assert_eq!(fmt("neg-over", -1, &reg), "(n1)");
    }

    #[test]
    fn extends_unknown_base_and_cycles_fall_to_decimal() {
        let a = CounterStyleRule {
            name: "a".to_string(),
            system: CounterStyleSystem::Extends("nosuch".to_string()),
            ..CounterStyleRule::default()
        };
        assert_eq!(fmt("a", 7, &[a]), "7");
        let mut b = CounterStyleRule {
            name: "b".to_string(),
            system: CounterStyleSystem::Extends("c".to_string()),
            ..CounterStyleRule::default()
        };
        b.symbols = vec!["b".to_string()];
        let mut c = CounterStyleRule {
            name: "c".to_string(),
            system: CounterStyleSystem::Extends("b".to_string()),
            ..CounterStyleRule::default()
        };
        c.symbols = vec!["c".to_string()];
        // extends 链成环 → 双方均按 extends decimal。
        assert_eq!(fmt("b", 7, &[b, c]), "7");
    }

    #[test]
    fn registry_overrides_builtin_and_last_write_wins() {
        let r1 = rule("decimal", CounterStyleSystem::Alphabetic, &["x", "y"]);
        let r2 = rule("decimal", CounterStyleSystem::Alphabetic, &["p", "q"]);
        let reg = [r1, r2];
        // 同名后写胜 + 覆盖内置。
        assert_eq!(fmt("decimal", 1, &reg), "p");
        assert_eq!(fmt("decimal", 2, &reg), "q");
        assert_eq!(fmt("decimal", 3, &reg), "pp");
    }

    #[test]
    fn unknown_name_and_none_style() {
        assert_eq!(fmt("nosuch", 42, &[]), "42");
        assert_eq!(fmt("none", 42, &[]), "");
        assert_eq!(fmt("NONE", 42, &[]), "");
        // 大小写敏感登记表：用户 "DECIMAL" 不覆盖内置 decimal（内置
        // ASCII 不敏感回退）。
        assert_eq!(fmt("DECIMAL", 5, &[]), "5");
    }

    #[test]
    fn system_symbol_requirements_gate_to_decimal() {
        // alphabetic 需 ≥2 符号；cyclic 需 ≥1。
        let bad_alpha = rule("bad-alpha", CounterStyleSystem::Alphabetic, &["a"]);
        assert_eq!(fmt("bad-alpha", 3, &[bad_alpha]), "3");
        let bad_cyclic = rule("bad-cyclic", CounterStyleSystem::Cyclic, &[]);
        assert_eq!(fmt("bad-cyclic", 3, &[bad_cyclic]), "3");
    }

    #[test]
    fn negative_wrapped_parens_and_pad() {
        let mut r = rule("fin", CounterStyleSystem::Numeric, &["0", "1"]);
        r.negative = vec!["(".to_string(), ")".to_string()];
        r.pad = Some((3, "0".to_string()));
        let reg = [r];
        assert_eq!(fmt("fin", 2, &reg), "010");
        // 负值 pad 差值扣负号簇数：3 − 2(digit) − 2(paren) < 0 → 不补零。
        assert_eq!(fmt("fin", -2, &reg), "(10)");
        assert_eq!(fmt("fin", -2, &reg).chars().count(), 4);
    }

    #[test]
    fn oversized_representation_falls_back() {
        // symbolic 线性膨胀：> 60 码点 → UA 允许项按 fallback 回退。
        let r = rule("stars", CounterStyleSystem::Symbolic, &["*"]);
        let reg = [r];
        assert_eq!(fmt("stars", 60, &reg), "*".repeat(60));
        assert_eq!(fmt("stars", 61, &reg), "61");
        // additive 同护栏：roman 超 60 码点 → decimal。
        assert_eq!(fmt("upper-roman", 100000, &[]), "100000");
        assert_eq!(fmt("upper-roman", 3888, &[]), "MMMDCCCLXXXVIII");
    }

    #[test]
    fn fallback_none_renders_empty() {
        let mut r = rule("ghost", CounterStyleSystem::Fixed(1), &["g"]);
        r.range = CounterStyleRange::Ranges(vec![(Some(1), Some(1))]);
        r.fallback = Some("none".to_string());
        let reg = [r];
        // 解析层宽容登记的 fallback: none → 渲染空串（【B】级）。
        assert_eq!(fmt("ghost", 2, &reg), "");
        assert_eq!(fmt("ghost", 1, &reg), "g");
    }
}
