//! 级联：规则匹配与胜出判定（双键模型）。
//!
//! 来源阶梯（低→高）：normal `Default < Stylesheet < Inline`；
//! important `Stylesheet < Inline < Default`（引擎默认 !important 反转为
//! 最强，等价 UA !important 语义）。同键内按 (specificity, order) 升序、
//! 后者胜（源顺序）。custom properties 同阶梯参与（MVP：忽略其
//! !important 标记，解析层即丢弃）。

use crate::css::decl::TokenBuf;
use crate::css::property::PropertyId;
use crate::css::stylesheet::{MediaEnv, Rule, Stylesheet};
use crate::selector::match_specificity;
use crate::tree::{NodeId, StyleTree};

/// 级联来源层。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    /// 引擎内置默认（在 ComputedStyle 初值层落地）。
    Default,
    /// 样式表规则。
    Stylesheet,
    /// 节点内联声明。
    Inline,
}

impl Origin {
    /// 级联优先级阶梯（见模块注释）。
    pub fn rank(self, important: bool) -> u8 {
        match (self, important) {
            (Self::Default, false) => 0,
            (Self::Stylesheet, false) => 1,
            (Self::Inline, false) => 2,
            (Self::Stylesheet, true) => 3,
            (Self::Inline, true) => 4,
            (Self::Default, true) => 5,
        }
    }
}

/// 一条候选声明（含级联键）。
#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    pub origin: Origin,
    pub important: bool,
    /// 命中选择器特异性；内联取 u32::MAX。
    pub specificity: u32,
    /// 规则源顺序；内联取 u32::MAX（同键后者胜）。
    pub order: u32,
    pub value: &'a crate::css::decl::DeclSource,
}

/// 候选 custom property（MVP：仅 normal 层级）。
#[derive(Debug, Clone, Copy)]
pub struct CustomCandidate<'a> {
    pub origin: Origin,
    pub specificity: u32,
    pub order: u32,
    pub tokens: &'a TokenBuf,
}

/// 级联输出：每属性/custom property 的胜出候选。
#[derive(Debug, Default)]
pub struct CascadeOutput<'a> {
    /// 每属性胜出候选（按 PropertyId 升序）。
    pub winners: Vec<(PropertyId, Candidate<'a>)>,
    /// 胜出自定义属性（按名升序）。
    pub custom_winners: Vec<(String, CustomCandidate<'a>)>,
}

/// 级联键比较：rank 主导，同 rank 内 (specificity, order)，相等者后者胜。
fn beats(new: &Candidate<'_>, cur: &Candidate<'_>) -> bool {
    (new.origin.rank(new.important), new.specificity, new.order)
        >= (cur.origin.rank(cur.important), cur.specificity, cur.order)
}

fn beats_custom(new: &CustomCandidate<'_>, cur: &CustomCandidate<'_>) -> bool {
    (new.origin.rank(false), new.specificity, new.order)
        >= (cur.origin.rank(false), cur.specificity, cur.order)
}

fn push_decl<'a>(out: &mut CascadeOutput<'a>, pid: PropertyId, cand: Candidate<'a>) {
    if let Some(entry) = out.winners.iter_mut().find(|(p, _)| *p == pid) {
        if beats(&cand, &entry.1) {
            entry.1 = cand;
        }
    } else {
        out.winners.push((pid, cand));
    }
}

fn push_custom<'a>(out: &mut CascadeOutput<'a>, name: String, cand: CustomCandidate<'a>) {
    if let Some(entry) = out.custom_winners.iter_mut().find(|(n, _)| *n == name) {
        if beats_custom(&cand, &entry.1) {
            entry.1 = cand;
        }
    } else {
        out.custom_winners.push((name, cand));
    }
}

/// 单条命中的规则。
#[derive(Debug)]
pub struct MatchedRule<'a> {
    pub rule: &'a Rule,
    pub specificity: u32,
}

/// 收集单节点命中的规则（@media 先行求值过滤）。
pub fn match_rules<'a>(
    tree: &StyleTree,
    id: NodeId,
    sheet: &'a Stylesheet,
    env: &MediaEnv,
) -> Vec<MatchedRule<'a>> {
    sheet
        .rules
        .iter()
        .filter_map(|rule| {
            if let Some(q) = &rule.media {
                if !q.eval(env) {
                    return None;
                }
            }
            let specificity = match_specificity(tree, id, &rule.selectors)?;
            Some(MatchedRule { rule, specificity })
        })
        .collect()
}

/// 匹配并级联单节点声明（Stylesheet + Inline；Default 层在 computed 落地）。
pub fn cascade_declarations<'a>(
    tree: &'a StyleTree,
    id: NodeId,
    sheet: &'a Stylesheet,
    env: &MediaEnv,
) -> CascadeOutput<'a> {
    let node = tree.node(id);
    let mut out = CascadeOutput::default();

    for matched in match_rules(tree, id, sheet, env) {
        let (rule, specificity) = (matched.rule, matched.specificity);
        for d in &rule.declarations.decls {
            push_decl(
                &mut out,
                d.id,
                Candidate {
                    origin: Origin::Stylesheet,
                    important: d.important,
                    specificity,
                    order: rule.order,
                    value: &d.value,
                },
            );
        }
        for (name, tokens) in &rule.declarations.custom {
            push_custom(
                &mut out,
                name.clone(),
                CustomCandidate {
                    origin: Origin::Stylesheet,
                    specificity,
                    order: rule.order,
                    tokens,
                },
            );
        }
    }

    // 内联最后压入：同键后者胜天然成立
    for d in &node.declarations.decls {
        push_decl(
            &mut out,
            d.id,
            Candidate {
                origin: Origin::Inline,
                important: d.important,
                specificity: u32::MAX,
                order: u32::MAX,
                value: &d.value,
            },
        );
    }
    for (name, tokens) in &node.declarations.custom {
        push_custom(
            &mut out,
            name.clone(),
            CustomCandidate {
                origin: Origin::Inline,
                specificity: u32::MAX,
                order: u32::MAX,
                tokens,
            },
        );
    }

    out.winners.sort_by_key(|(pid, _)| *pid);
    out.custom_winners.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::decl::DeclSource;
    use crate::css::property::DeclValue;
    use crate::css::stylesheet::parse_stylesheet;
    use crate::css::value::ColorValue;
    use crate::tree::{StyleNode, StyleTree};

    fn tree_one(inline: &str) -> (StyleTree, NodeId) {
        let mut tree = StyleTree::new();
        let root = tree.root();
        let node = tree.insert_child(
            root,
            StyleNode {
                name: Some("div".into()),
                declarations: crate::css::decl::parse_inline_declarations(inline).0,
                ..Default::default()
            },
        );
        (tree, node)
    }

    fn color_of(out: &CascadeOutput<'_>) -> Option<[f32; 4]> {
        let (_, cand) = out.winners.iter().find(|(p, _)| *p == PropertyId::Color)?;
        match cand.value {
            DeclSource::Parsed(DeclValue::Color(ColorValue::Absolute(c))) => Some(c.components),
            _ => None,
        }
    }

    #[test]
    fn specificity_beats_order() {
        let sheet = parse_stylesheet("div { color: blue } .a { color: red }");
        assert!(sheet.report.is_clean());
        let mut tree = StyleTree::new();
        let root = tree.root();
        let n = tree.insert_child(
            root,
            StyleNode {
                name: Some("div".into()),
                classes: ["a"].iter().map(|c| c.to_string()).collect(),
                ..Default::default()
            },
        );
        let out = cascade_declarations(&tree, n, &sheet, &MediaEnv::default());
        assert_eq!(color_of(&out).unwrap()[0], 1.0); // red 胜
    }

    #[test]
    fn important_beats_specificity_and_inline() {
        // important 样式表胜过高特异性 normal
        let sheet = parse_stylesheet("div { color: blue !important } .a { color: red }");
        let (tree, n) = tree_one("color: green");
        let out = cascade_declarations(&tree, n, &sheet, &MediaEnv::default());
        assert_eq!(color_of(&out).unwrap()[2], 1.0); // blue 胜
    }

    #[test]
    fn inline_normal_beats_stylesheet_normal() {
        let sheet = parse_stylesheet("div { color: red }");
        let (tree, n) = tree_one("color: blue");
        let out = cascade_declarations(&tree, n, &sheet, &MediaEnv::default());
        assert_eq!(color_of(&out).unwrap()[2], 1.0); // blue 胜
    }

    #[test]
    fn source_order_breaks_ties() {
        let sheet = parse_stylesheet(".a { color: red } .a { color: blue }");
        let mut tree = StyleTree::new();
        let root = tree.root();
        let n = tree.insert_child(
            root,
            StyleNode {
                name: Some("div".into()),
                classes: ["a"].iter().map(|c| c.to_string()).collect(),
                ..Default::default()
            },
        );
        let out = cascade_declarations(&tree, n, &sheet, &MediaEnv::default());
        assert_eq!(color_of(&out).unwrap()[2], 1.0); // 后者 blue 胜
    }

    #[test]
    fn media_filters_rules() {
        let sheet = parse_stylesheet("@media (min-width: 600px) { .a { color: red } }");
        let mut tree = StyleTree::new();
        let root = tree.root();
        let n = tree.insert_child(
            root,
            StyleNode {
                name: Some("div".into()),
                classes: ["a"].iter().map(|c| c.to_string()).collect(),
                ..Default::default()
            },
        );
        let wide = MediaEnv {
            viewport_w: 800.0,
            ..Default::default()
        };
        let narrow = MediaEnv {
            viewport_w: 400.0,
            ..Default::default()
        };
        assert!(
            cascade_declarations(&tree, n, &sheet, &wide)
                .winners
                .iter()
                .any(|(p, _)| *p == PropertyId::Color)
        );
        assert!(
            cascade_declarations(&tree, n, &sheet, &narrow)
                .winners
                .is_empty()
        );
    }

    #[test]
    fn custom_inline_beats_stylesheet() {
        let sheet = parse_stylesheet("div { --x: 10px }");
        let (tree, n) = tree_one("--x: 20px");
        let out = cascade_declarations(&tree, n, &sheet, &MediaEnv::default());
        assert_eq!(out.custom_winners.len(), 1);
        assert_eq!(
            crate::css::decl::token_buf_to_string(out.custom_winners[0].1.tokens),
            "20px"
        );
    }
}
