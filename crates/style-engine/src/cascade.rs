//! The cascade: rule matching and winner determination (the css-cascade-5
//! order keys).
//!
//! Origin ladder (low→high, `rank(important)`): normal
//! `Default < UserAgent < User < Author(Stylesheet+Inline)`;
//! important inverts to `Author < User < UserAgent < Default` (the engine
//! defaults to !important inverted as the strongest, equivalent to UA
//! !important semantics). Inline is folded into the Author band
//! (element-attached wins within the band with u32::MAX specificity —
//! bitwise-equivalent to the pre-B1 separate Inline band: the winner is
//! always the same). Layer order (@layer, B1) sits after origin-importance
//! and before specificity: on the normal axis, unlayered beats all layered
//! (within a layer, the later one wins); the important axis inverts
//! (unlayered important loses to all layered important, and earlier layers
//! beat later ones). Within the same key, (specificity, order, decl_index)
//! sort ascending and the later one wins. All candidates are kept per
//! property — revert/revert-layer roll back after the contest
//! (resolve_revert). Custom properties participate on the same ladder.

use crate::css::decl::TokenBuf;
use crate::css::property::ContainerType;
use crate::css::property::PropertyId;
use crate::css::property::WideKeyword;
use crate::css::stylesheet::{MediaEnv, Rule, Stylesheet};
use crate::selector::{PseudoElement, match_specificity};
use crate::tree::{NodeId, StyleTree};

/// A one-frame snapshot of queryable containers (phase 2 ③): the restyle DFS
/// pushes containers from ancestors inward, and @container evaluation
/// searches outward from the top of the stack.
#[derive(Debug, Clone, Default)]
pub struct ContainerCtx {
    /// container-name list (empty = unnamed container).
    pub names: Vec<String>,
    /// container-type (normal is not pushed onto the stack).
    pub ctype: ContainerType,
    /// Content box size (recorded by the previous layout pass; None = size
    /// not ready → features unknown).
    pub size: Option<[f32; 2]>,
}

/// Cascade origin layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Origin {
    /// Engine built-in defaults (materialized at the ComputedStyle
    /// initial-value layer).
    Default,
    /// UA origin (B1: the type and ladder are in place; the hook that
    /// produces UA declarations awaits host demand).
    UserAgent,
    /// User origin (B1: the second stylesheet injected via
    /// `set_user_stylesheet`).
    User,
    /// Stylesheet rules (author).
    Stylesheet,
    /// The node's inline declarations.
    Inline,
}

impl Origin {
    /// The cascade priority ladder (css-cascade-5 §6.4: origin+importance
    /// inversion). Inline is folded into the Author band (element-attached
    /// wins within the band with u32::MAX specificity; bitwise-equivalent
    /// to the old separate band).
    pub fn rank(self, important: bool) -> u8 {
        match (self, important) {
            (Self::Default, false) => 0,
            (Self::UserAgent, false) => 1,
            (Self::User, false) => 2,
            (Self::Stylesheet, false) | (Self::Inline, false) => 3,
            (Self::Stylesheet, true) | (Self::Inline, true) => 4,
            (Self::User, true) => 5,
            (Self::UserAgent, true) => 6,
            (Self::Default, true) => 7,
        }
    }
}

/// One candidate declaration (with its cascade keys).
#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    /// Declaration origin layer (enum order is priority).
    pub origin: Origin,
    /// Whether it carries !important.
    pub important: bool,
    /// Matched selector specificity; u32::MAX for inline.
    pub specificity: u32,
    /// Rule source order; u32::MAX for inline (within the same key, the
    /// later one wins).
    pub order: u32,
    /// Declaration order within the rule (A8: logical/physical pairs within
    /// one rule block are decided by declaration order).
    pub decl_index: usize,
    /// B1 @layer: layer rank (unlayered = u32::MAX).
    pub layer_rank: u32,
    /// The winning declaration value (parsed, or suspended var() tokens).
    pub value: &'a crate::css::decl::DeclSource,
}

/// A candidate custom property (B1: carries the layer rank; the important
/// flag is still ignored, MVP).
#[derive(Debug, Clone, Copy)]
pub struct CustomCandidate<'a> {
    /// Declaration origin layer (enum order is priority).
    pub origin: Origin,
    /// Matched selector specificity.
    pub specificity: u32,
    /// Rule source order.
    pub order: u32,
    /// B1 @layer: layer rank (unlayered = u32::MAX).
    pub layer_rank: u32,
    /// The winning custom property's raw tokens (substituted during
    /// computed-value time).
    pub tokens: &'a TokenBuf,
}

/// Cascade output: all candidates per property/custom property (ascending by
/// PropertyId/name; list order = push order = source order; the champion is
/// the candidate with the largest beats order, ties broken by the later one
/// — the B1 revert family needs the runner-up candidate to roll back).
#[derive(Debug, Default)]
pub struct CascadeOutput<'a> {
    /// Per-property candidate lists (the last item is not guaranteed to be
    /// the champion — the champion goes through `cascade_winner`).
    pub winners: Vec<(PropertyId, Vec<Candidate<'a>>)>,
    /// Winning custom-property candidate lists (ascending by name).
    pub custom_winners: Vec<(String, Vec<CustomCandidate<'a>>)>,
}

/// Layer comparison key: normal = layer_rank (unlayered u32::MAX is highest
/// = beats all layered); the important axis inverts (u32::MAX - rank:
/// unlayered important is lowest = loses to all layered important, earlier
/// layers beat later ones) — the css-cascade-5 layer-order inversion.
fn layer_key(c: &Candidate<'_>) -> u32 {
    if c.important {
        u32::MAX - c.layer_rank
    } else {
        c.layer_rank
    }
}

/// Cascade-key comparison (css-cascade-5 order: origin-importance > layer >
/// specificity > order > decl_index); `>=` semantics: with equal keys, the
/// later one (later in source order) wins.
fn beats(new: &Candidate<'_>, cur: &Candidate<'_>) -> bool {
    (
        new.origin.rank(new.important),
        layer_key(new),
        new.specificity,
        new.order,
        new.decl_index,
    ) >= (
        cur.origin.rank(cur.important),
        layer_key(cur),
        cur.specificity,
        cur.order,
        cur.decl_index,
    )
}

fn beats_custom(new: &CustomCandidate<'_>, cur: &CustomCandidate<'_>) -> bool {
    (
        new.origin.rank(false),
        new.layer_rank,
        new.specificity,
        new.order,
    ) >= (
        cur.origin.rank(false),
        cur.layer_rank,
        cur.specificity,
        cur.order,
    )
}

/// The cascade champion of a candidate list (largest beats order, ties
/// broken by the later one — list order = push order = source order). An
/// empty list returns None.
pub(crate) fn cascade_winner<'c, 'a>(cands: &'c [Candidate<'a>]) -> Option<&'c Candidate<'a>> {
    let mut best = None;
    for (i, c) in cands.iter().enumerate() {
        if best.is_none_or(|b: usize| beats(c, &cands[b])) {
            best = Some(i);
        }
    }
    best.map(|i| &cands[i])
}

/// The cascade champion of a custom-property candidate list.
pub(crate) fn cascade_winner_custom<'c, 'a>(
    cands: &'c [CustomCandidate<'a>],
) -> Option<&'c CustomCandidate<'a>> {
    let mut best = None;
    for (i, c) in cands.iter().enumerate() {
        if best.is_none_or(|b: usize| beats_custom(c, &cands[b])) {
            best = Some(i);
        }
    }
    best.map(|i| &cands[i])
}

fn push_decl<'a>(out: &mut CascadeOutput<'a>, pid: PropertyId, cand: Candidate<'a>) {
    if let Some(entry) = out.winners.iter_mut().find(|(p, _)| *p == pid) {
        entry.1.push(cand);
    } else {
        out.winners.push((pid, vec![cand]));
    }
}

fn push_custom<'a>(out: &mut CascadeOutput<'a>, name: String, cand: CustomCandidate<'a>) {
    if let Some(entry) = out.custom_winners.iter_mut().find(|(n, _)| *n == name) {
        entry.1.push(cand);
    } else {
        out.custom_winners.push((name, vec![cand]));
    }
}

/// Wide-keyword champion value detection (used for revert-family rollback
/// decisions).
fn wide_keyword_of(c: &Candidate<'_>) -> Option<WideKeyword> {
    match c.value {
        crate::css::decl::DeclSource::Parsed(crate::css::property::DeclValue::WideKeyword(k)) => {
            Some(*k)
        }
        _ => None,
    }
}

/// Whether a custom-property champion's tokens are exactly a wide-keyword
/// single ident (revert family).
fn custom_wide_keyword_of(c: &CustomCandidate<'_>) -> Option<WideKeyword> {
    if c.tokens.len() != 1 {
        return None;
    }
    match c.tokens[0].text.to_ascii_lowercase().as_str() {
        "revert" => Some(WideKeyword::Revert),
        "revert-layer" => Some(WideKeyword::RevertLayer),
        _ => None,
    }
}

/// B1: revert / revert-layer post-contest rollback (css-cascade-5). When the
/// champion is a wide keyword, the rollback target is picked: **revert** =
/// the champion candidate from a strictly lower origin-importance band;
/// **revert-layer** = the champion candidate with a strictly smaller layer
/// key within the same band (none → continue rolling back per revert). With
/// no lower candidate at all → the property is dropped (falling back to
/// initial-value materialization).
fn resolve_revert<'a>(mut out: CascadeOutput<'a>) -> CascadeOutput<'a> {
    for (_, cands) in out.winners.iter_mut() {
        let (kind, wrank, wlayer) = match cascade_winner(cands) {
            Some(w) => match wide_keyword_of(w) {
                // 仅 revert 族触发回滚；initial/inherit/unset 留给计算值期物化
                Some(k @ (WideKeyword::Revert | WideKeyword::RevertLayer)) => {
                    (k, w.origin.rank(w.important), layer_key(w))
                }
                _ => continue,
            },
            None => continue,
        };
        // revert-layer 首选：同档内层键严格更小的最优候选
        let mut target: Option<&Candidate<'a>> = None;
        if kind == WideKeyword::RevertLayer {
            target = cands
                .iter()
                .filter(|c| c.origin.rank(c.important) == wrank && layer_key(c) < wlayer)
                .fold(None, |best: Option<&Candidate<'a>>, c| {
                    Some(match best {
                        Some(b) if !beats(c, b) => b,
                        _ => c,
                    })
                });
        }
        // revert（或 revert-layer 无同档更早层）：严格更低档的最优候选
        if target.is_none() {
            target = cands
                .iter()
                .filter(|c| c.origin.rank(c.important) < wrank)
                .fold(None, |best: Option<&Candidate<'a>>, c| {
                    Some(match best {
                        Some(b) if !beats(c, b) => b,
                        _ => c,
                    })
                });
        }
        *cands = match target {
            Some(t) => vec![*t],
            None => Vec::new(),
        };
    }
    // custom properties：同机制（token 恰为宽关键字单 ident）
    for (_, cands) in out.custom_winners.iter_mut() {
        let w = match cascade_winner_custom(cands) {
            Some(w) => w,
            None => continue,
        };
        let Some(kind) = custom_wide_keyword_of(w) else {
            continue;
        };
        let wrank = w.origin.rank(false);
        let wlayer = w.layer_rank;
        let pick = |filter: &dyn Fn(&CustomCandidate<'a>) -> bool| -> Option<CustomCandidate<'a>> {
            cands.iter().filter(|c| filter(c)).fold(None, |best, c| {
                Some(match best {
                    Some(b) if !beats_custom(c, &b) => b,
                    _ => *c,
                })
            })
        };
        let mut target = None;
        if kind == WideKeyword::RevertLayer {
            target = pick(&|c: &CustomCandidate<'a>| {
                c.origin.rank(false) == wrank && c.layer_rank < wlayer
            });
        }
        if target.is_none() {
            target = pick(&|c: &CustomCandidate<'a>| c.origin.rank(false) < wrank);
        }
        *cands = match target {
            Some(t) => vec![t],
            None => Vec::new(),
        };
    }
    out
}

/// Logical longhand → physical slot mapping (A8, css-logical-1). ltr:
/// inline-start=left, inline-end=right, block-start=top, block-end=bottom;
/// rtl: the inline axis flips (block axis and vertical writing are not
/// supported — documented in FEATURES.md); the four radius corners flip
/// along the inline axis (start-start↔start-end, end-start↔end-end).
fn logical_map(p: PropertyId, rtl: bool) -> Option<PropertyId> {
    use PropertyId as P;
    Some(match (p, rtl) {
        (P::MarginInlineStart, false) => P::MarginLeft,
        (P::MarginInlineStart, true) => P::MarginRight,
        (P::MarginInlineEnd, false) => P::MarginRight,
        (P::MarginInlineEnd, true) => P::MarginLeft,
        (P::MarginBlockStart, _) => P::MarginTop,
        (P::MarginBlockEnd, _) => P::MarginBottom,
        (P::PaddingInlineStart, false) => P::PaddingLeft,
        (P::PaddingInlineStart, true) => P::PaddingRight,
        (P::PaddingInlineEnd, false) => P::PaddingRight,
        (P::PaddingInlineEnd, true) => P::PaddingLeft,
        (P::PaddingBlockStart, _) => P::PaddingTop,
        (P::PaddingBlockEnd, _) => P::PaddingBottom,
        (P::InsetInlineStart, false) => P::Left,
        (P::InsetInlineStart, true) => P::Right,
        (P::InsetInlineEnd, false) => P::Right,
        (P::InsetInlineEnd, true) => P::Left,
        (P::InsetBlockStart, _) => P::Top,
        (P::InsetBlockEnd, _) => P::Bottom,
        (P::BorderInlineStartWidth, false) => P::BorderLeftWidth,
        (P::BorderInlineStartWidth, true) => P::BorderRightWidth,
        (P::BorderInlineEndWidth, false) => P::BorderRightWidth,
        (P::BorderInlineEndWidth, true) => P::BorderLeftWidth,
        (P::BorderBlockStartWidth, _) => P::BorderTopWidth,
        (P::BorderBlockEndWidth, _) => P::BorderBottomWidth,
        (P::BorderInlineStartStyle, false) => P::BorderLeftStyle,
        (P::BorderInlineStartStyle, true) => P::BorderRightStyle,
        (P::BorderInlineEndStyle, false) => P::BorderRightStyle,
        (P::BorderInlineEndStyle, true) => P::BorderLeftStyle,
        (P::BorderBlockStartStyle, _) => P::BorderTopStyle,
        (P::BorderBlockEndStyle, _) => P::BorderBottomStyle,
        (P::BorderInlineStartColor, false) => P::BorderLeftColor,
        (P::BorderInlineStartColor, true) => P::BorderRightColor,
        (P::BorderInlineEndColor, false) => P::BorderRightColor,
        (P::BorderInlineEndColor, true) => P::BorderLeftColor,
        (P::BorderBlockStartColor, _) => P::BorderTopColor,
        (P::BorderBlockEndColor, _) => P::BorderBottomColor,
        (P::BorderStartStartRadius, false) => P::BorderTopLeftRadius,
        (P::BorderStartStartRadius, true) => P::BorderTopRightRadius,
        (P::BorderStartEndRadius, false) => P::BorderTopRightRadius,
        (P::BorderStartEndRadius, true) => P::BorderTopLeftRadius,
        (P::BorderEndStartRadius, false) => P::BorderBottomLeftRadius,
        (P::BorderEndStartRadius, true) => P::BorderBottomRightRadius,
        (P::BorderEndEndRadius, false) => P::BorderBottomRightRadius,
        (P::BorderEndEndRadius, true) => P::BorderBottomLeftRadius,
        _ => return None,
    })
}

/// A8 logical property resolution (css-logical-1): the logical-slot champion
/// and the mapped physical-slot champion are decided by the cascade total
/// order key (origin-importance, layer, specificity, order, decl_index) —
/// the later one fills the physical slot (spec-correct: physical/logical
/// compete in one pool by recency; within one rule, by declaration order),
/// and the logical slot itself is removed from the output (no readers;
/// ComputedStyle exposes only physical slots + direction). The caller
/// (computed.rs) passes rtl from the node's direction.
pub fn resolve_logical<'a>(mut out: CascadeOutput<'a>, rtl: bool) -> CascadeOutput<'a> {
    let key = |c: &Candidate<'_>| {
        (
            c.origin.rank(c.important),
            layer_key(c),
            c.specificity,
            c.order,
            c.decl_index,
        )
    };
    let all = std::mem::take(&mut out.winners);
    let mut winners: Vec<(PropertyId, Vec<Candidate<'a>>)> = Vec::with_capacity(all.len());
    // 先剥出全部逻辑槽（物理槽位暂存），再按序键逐对定夺。
    let mut logical: Vec<(PropertyId, Vec<Candidate<'a>>)> = Vec::new();
    for (pid, cands) in all {
        if logical_map(pid, rtl).is_some() {
            logical.push((pid, cands));
        } else {
            winners.push((pid, cands));
        }
    }
    for (pid, cands) in logical {
        let Some(phys) = logical_map(pid, rtl) else {
            continue;
        };
        // 逻辑槽无候选（revert 已剔除）→ 不参战
        let Some(logical_champ) = cascade_winner(&cands).copied() else {
            continue;
        };
        match winners.iter_mut().find(|(p, _)| *p == phys) {
            Some((_, phys_cands)) => {
                let phys_wins = match cascade_winner(phys_cands).copied() {
                    Some(pc) => key(&pc) >= key(&logical_champ),
                    None => false,
                };
                if !phys_wins {
                    *phys_cands = vec![logical_champ];
                }
            }
            // 物理槽无声明：逻辑值直接胜出（初始值走缺省物化路径）
            None => winners.push((phys, vec![logical_champ])),
        }
    }
    out.winners = winners;
    out.winners.sort_by_key(|(pid, _)| *pid);
    out
}

/// C4 (ADR-0018): whether a rule's selector list contains a channel
/// pseudo-element component. `want = None` matches any channel variant
/// (::selection/::placeholder); `Some(w)` compares exactly.
fn rule_has_channel_pseudo(rule: &Rule, want: Option<PseudoElement>) -> bool {
    rule.selectors.slice().iter().any(|sel| {
        sel.iter_raw_match_order().any(|c| match c {
            selectors::parser::Component::PseudoElement(pe) => match want {
                None => matches!(pe, PseudoElement::Selection | PseudoElement::Placeholder),
                Some(w) => *pe == w,
            },
            _ => false,
        })
    })
}

/// One matched rule.
#[derive(Debug)]
pub struct MatchedRule<'a> {
    /// The matched stylesheet rule.
    pub rule: &'a Rule,
    /// Specificity of the matched selector.
    pub specificity: u32,
}

/// Collects the rules matching a single node (@media is evaluated and
/// filtered first; @container is evaluated against the ancestor container
/// snapshots).
pub fn match_rules<'a>(
    tree: &StyleTree,
    id: NodeId,
    sheet: &'a Stylesheet,
    env: &MediaEnv,
    container_ctx: &[ContainerCtx],
) -> Vec<MatchedRule<'a>> {
    sheet
        .rules
        .iter()
        .filter_map(|rule| {
            if let Some(q) = &rule.media
                && !q.eval(env)
            {
                return None;
            }
            if let Some(conds) = &rule.container
                && !conds.iter().all(|c| c.eval(container_ctx))
            {
                return None;
            }
            let specificity = match_specificity(tree, id, &rule.selectors)?;
            Some(MatchedRule { rule, specificity })
        })
        .collect()
}

/// Single-sheet rule collection (B2: shared by the author multi-sheet group
/// and the user sheet; push order = sheet order × source order).
/// C4 (ADR-0018): `channel = None` is the main cascade — ::selection/
/// ::placeholder rules are excluded (to prevent style leakage, channel rules
/// resolve only through cascade_channel); `Some(w)` is the channel cascade —
/// only rules containing the corresponding channel pseudo-element component
/// are collected.
#[allow(clippy::too_many_arguments)] // 单表收集直传（out/tree/sheet/origin/env/通道）
fn collect_sheet<'a>(
    out: &mut CascadeOutput<'a>,
    tree: &'a StyleTree,
    id: NodeId,
    sheet: &'a Stylesheet,
    origin: Origin,
    env: &MediaEnv,
    container_ctx: &[ContainerCtx],
    channel: Option<PseudoElement>,
) {
    for matched in match_rules(tree, id, sheet, env, container_ctx) {
        let keep = match channel {
            None => !rule_has_channel_pseudo(matched.rule, None),
            Some(w) => rule_has_channel_pseudo(matched.rule, Some(w)),
        };
        if !keep {
            continue;
        }
        let (rule, specificity) = (matched.rule, matched.specificity);
        for (di, d) in rule.declarations.decls.iter().enumerate() {
            push_decl(
                out,
                d.id,
                Candidate {
                    origin,
                    important: d.important,
                    specificity,
                    order: rule.order,
                    decl_index: di,
                    layer_rank: rule.layer_rank,
                    value: &d.value,
                },
            );
        }
        for (name, tokens) in &rule.declarations.custom {
            push_custom(
                out,
                name.clone(),
                CustomCandidate {
                    origin,
                    specificity,
                    order: rule.order,
                    layer_rank: rule.layer_rank,
                    tokens,
                },
            );
        }
    }
}

/// Matches and cascades a single node's declarations (the B1 pipeline: User
/// sheet + Author sheet group + Inline all-collect → resolve_revert rollback
/// → sort; resolve_logical is called separately by the caller after it
/// obtains the node's direction; the Default layer lands in computed).
/// B2: author becomes a sheet group passed by value (main sheet first, extra
/// sheets in registration order = document order, the later one wins ties).
pub fn cascade_declarations<'a>(
    tree: &'a StyleTree,
    id: NodeId,
    sheets: Vec<&'a Stylesheet>,
    user_sheet: Option<&'a Stylesheet>,
    env: &MediaEnv,
    container_ctx: &[ContainerCtx],
    ua_sheet: Option<&'a Stylesheet>,
) -> CascadeOutput<'a> {
    let node = tree.node(id);
    let mut out = CascadeOutput::default();

    // UA → User → Author 依次收集（压入序 = 源序；同键后者胜仅在同层内成立，
    // 跨 origin/层由 beats 序键定夺）。B2：sheets 按值收取——输出只借表
    // 内容（'a），不借 Vec 本身，调用方行内临时构造即可。
    // P5（ADR-0033）：UA 表挂点——Origin::UserAgent 层序 (1,6) 既有，
    // important 反转自动生效。
    if let Some(ua) = ua_sheet {
        collect_sheet(
            &mut out,
            tree,
            id,
            ua,
            Origin::UserAgent,
            env,
            container_ctx,
            None,
        );
    }
    if let Some(user) = user_sheet {
        collect_sheet(
            &mut out,
            tree,
            id,
            user,
            Origin::User,
            env,
            container_ctx,
            None,
        );
    }
    for sheet in &sheets {
        collect_sheet(
            &mut out,
            tree,
            id,
            sheet,
            Origin::Stylesheet,
            env,
            container_ctx,
            None,
        );
    }

    // 内联最后压入：Author 档内 u32::MAX 特异性取胜（element-attached）；
    // 未分层（u32::MAX）
    for (di, d) in node.declarations.decls.iter().enumerate() {
        push_decl(
            &mut out,
            d.id,
            Candidate {
                origin: Origin::Inline,
                important: d.important,
                specificity: u32::MAX,
                order: u32::MAX,
                decl_index: di,
                layer_rank: u32::MAX,
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
                layer_rank: u32::MAX,
                tokens,
            },
        );
    }

    let mut out = resolve_revert(out);
    out.winners.retain(|(_, cands)| !cands.is_empty());
    out.custom_winners.retain(|(_, cands)| !cands.is_empty());
    out.winners.sort_by_key(|(pid, _)| *pid);
    out.custom_winners.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// C4 (ADR-0018): the channel cascade (::selection/::placeholder) — collects
/// the corresponding channel rules sheet by sheet in UA → user → author
/// order (direct origin matching is handled by match_pseudo_element:
/// pseudo == None matches); no inline segment (the style attribute cannot
/// target pseudo-elements, spec-consistent); the tail is shaped like
/// cascade_declarations (resolve_revert → retain → sort).
#[allow(clippy::too_many_arguments)]
pub fn cascade_channel<'a>(
    tree: &'a StyleTree,
    id: NodeId,
    sheets: Vec<&'a Stylesheet>,
    user_sheet: Option<&'a Stylesheet>,
    env: &MediaEnv,
    container_ctx: &[ContainerCtx],
    channel: PseudoElement,
    ua_sheet: Option<&'a Stylesheet>,
) -> CascadeOutput<'a> {
    let mut out = CascadeOutput::default();
    if let Some(ua) = ua_sheet {
        collect_sheet(
            &mut out,
            tree,
            id,
            ua,
            Origin::UserAgent,
            env,
            container_ctx,
            Some(channel),
        );
    }
    if let Some(user) = user_sheet {
        collect_sheet(
            &mut out,
            tree,
            id,
            user,
            Origin::User,
            env,
            container_ctx,
            Some(channel),
        );
    }
    for sheet in &sheets {
        collect_sheet(
            &mut out,
            tree,
            id,
            sheet,
            Origin::Stylesheet,
            env,
            container_ctx,
            Some(channel),
        );
    }
    let mut out = resolve_revert(out);
    out.winners.retain(|(_, cands)| !cands.is_empty());
    out.custom_winners.retain(|(_, cands)| !cands.is_empty());
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
        let (_, cands) = out.winners.iter().find(|(p, _)| *p == PropertyId::Color)?;
        let cand = cascade_winner(cands)?;
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
        let out = cascade_declarations(
            &tree,
            n,
            vec![&sheet],
            None,
            &MediaEnv::default(),
            &[],
            None,
        );
        assert_eq!(color_of(&out).unwrap()[0], 1.0); // red 胜
    }

    #[test]
    fn important_beats_specificity_and_inline() {
        // important 样式表胜过高特异性 normal
        let sheet = parse_stylesheet("div { color: blue !important } .a { color: red }");
        let (tree, n) = tree_one("color: green");
        let out = cascade_declarations(
            &tree,
            n,
            vec![&sheet],
            None,
            &MediaEnv::default(),
            &[],
            None,
        );
        assert_eq!(color_of(&out).unwrap()[2], 1.0); // blue 胜
    }

    #[test]
    fn inline_normal_beats_stylesheet_normal() {
        let sheet = parse_stylesheet("div { color: red }");
        let (tree, n) = tree_one("color: blue");
        let out = cascade_declarations(
            &tree,
            n,
            vec![&sheet],
            None,
            &MediaEnv::default(),
            &[],
            None,
        );
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
        let out = cascade_declarations(
            &tree,
            n,
            vec![&sheet],
            None,
            &MediaEnv::default(),
            &[],
            None,
        );
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
            cascade_declarations(&tree, n, vec![&sheet], None, &wide, &[], None)
                .winners
                .iter()
                .any(|(p, _)| *p == PropertyId::Color)
        );
        assert!(
            cascade_declarations(&tree, n, vec![&sheet], None, &narrow, &[], None)
                .winners
                .is_empty()
        );
    }

    #[test]
    fn custom_inline_beats_stylesheet() {
        let sheet = parse_stylesheet("div { --x: 10px }");
        let (tree, n) = tree_one("--x: 20px");
        let out = cascade_declarations(
            &tree,
            n,
            vec![&sheet],
            None,
            &MediaEnv::default(),
            &[],
            None,
        );
        assert_eq!(out.custom_winners.len(), 1);
        assert_eq!(
            crate::css::decl::token_buf_to_string(
                cascade_winner_custom(&out.custom_winners[0].1)
                    .unwrap()
                    .tokens
            ),
            "20px"
        );
    }

    fn ctx(names: &[&str], ctype: ContainerType, w: f32) -> ContainerCtx {
        ContainerCtx {
            names: names.iter().map(|s| s.to_string()).collect(),
            ctype,
            size: Some([w, 100.0]),
        }
    }

    #[test]
    fn container_rule_matches_named_and_unnamed() {
        // 有名段按名自最近祖先向外查（跳过无名/异名）；无名段取最近容器。
        let sheet = parse_stylesheet(
            "@container panel (min-width: 300px) { .a { color: red } } \
             @container (min-width: 350px) { .a { color: blue } }",
        );
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
        // 最近容器无名 400px；外层有名 panel 320px。
        let stack = [
            ctx(&["panel"], ContainerType::InlineSize, 320.0),
            ctx(&[], ContainerType::InlineSize, 400.0),
        ];
        let out = cascade_declarations(
            &tree,
            n,
            vec![&sheet],
            None,
            &MediaEnv::default(),
            &stack,
            None,
        );
        // 有名段查到 panel=320 ≥ 300 命中；无名段取最近 400 ≥ 350 命中——
        // 同特异性后者胜 → blue。
        assert_eq!(color_of(&out).unwrap()[2], 1.0);
        // 有名段：panel 只匹配名为 panel 的容器——400 的无名容器不参与。
        let stack2 = [ctx(&[], ContainerType::InlineSize, 400.0)];
        let out2 = cascade_declarations(
            &tree,
            n,
            vec![&sheet],
            None,
            &MediaEnv::default(),
            &stack2,
            None,
        );
        assert!(out2.winners.iter().any(|(p, _)| *p == PropertyId::Color));
        let only_named =
            parse_stylesheet("@container panel (min-width: 300px) { .a { color: red } }");
        assert!(
            cascade_declarations(
                &tree,
                n,
                vec![&only_named],
                None,
                &MediaEnv::default(),
                &stack2,
                None,
            )
            .winners
            .is_empty()
        );
        // 尺寸不达标 → 不命中。
        let small = [ctx(&["panel"], ContainerType::InlineSize, 200.0)];
        assert!(
            cascade_declarations(
                &tree,
                n,
                vec![&sheet],
                None,
                &MediaEnv::default(),
                &small,
                None
            )
            .winners
            .is_empty()
        );
    }

    #[test]
    fn container_inline_size_blocks_block_axis_features() {
        // inline-size 容器上块轴/双轴特性 = unknown → 不匹配。
        let sheet = parse_stylesheet(
            "@container (min-height: 50px) { .a { color: red } } \
             @container (orientation: portrait) { .a { color: green } } \
             @container (min-width: 300px) { .a { color: blue } }",
        );
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
        let stack = [ctx(&[], ContainerType::InlineSize, 400.0)];
        let out = cascade_declarations(
            &tree,
            n,
            vec![&sheet],
            None,
            &MediaEnv::default(),
            &stack,
            None,
        );
        // 高度特性与 orientation 均被门控，仅 min-width 命中 → blue。
        assert_eq!(color_of(&out).unwrap()[2], 1.0);
        // size 容器：块轴与 orientation 正常参与（400×100 → 横向）。
        let sheet2 = parse_stylesheet("@container (min-height: 50px) { .a { color: red } }");
        let stack2 = [ctx(&[], ContainerType::Size, 400.0)];
        let out2 = cascade_declarations(
            &tree,
            n,
            vec![&sheet2],
            None,
            &MediaEnv::default(),
            &stack2,
            None,
        );
        assert_eq!(color_of(&out2).unwrap()[0], 1.0);
    }

    #[test]
    fn container_no_available_container_no_match() {
        // 无可用容器（未入栈）→ 不匹配。
        let sheet = parse_stylesheet("@container (min-width: 100px) { .a { color: red } }");
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
        assert!(
            cascade_declarations(
                &tree,
                n,
                vec![&sheet],
                None,
                &MediaEnv::default(),
                &[],
                None
            )
            .winners
            .is_empty()
        );
    }

    // ---------- B1 任务3：简写宽关键字 → 逐长手还原（css-cascade-5） ----------
    //
    // 简写 `margin: revert` 经 decl.rs 宽关键字整值拦截展开为 margin 四长手
    // 各自一条 WideKeyword(Revert) 声明（B1 逐长手物化语义）；resolve_revert
    // 赛后回滚 = 严格更低 origin 档冠军。以下三个场景固定该语义。

    /// Reads the px value of a margin longhand's champion (None = that
    /// longhand has no champion = rolled back and dropped, or never written).
    fn margin_px(out: &CascadeOutput<'_>, pid: PropertyId) -> Option<f32> {
        let (_, cands) = out.winners.iter().find(|(p, _)| *p == pid)?;
        let cand = cascade_winner(cands)?;
        match cand.value {
            DeclSource::Parsed(DeclValue::LenAuto(Some(
                crate::css::value::LengthPercentage::Px(v),
            ))) => Some(*v),
            _ => None,
        }
    }

    #[test]
    fn margin_shorthand_revert_rolls_all_longhands_to_lower_origin() {
        // 简写 revert：四长手整体回滚到 User 档（5px）——简写展开必须覆盖
        // 长手全集（漏一条则该长手留 10px）。
        let sheet = parse_stylesheet("div { margin: 10px } div { margin: revert }");
        assert!(sheet.report.is_clean());
        let user = parse_stylesheet("div { margin: 5px }");
        let (tree, n) = tree_one("");
        let out = cascade_declarations(
            &tree,
            n,
            vec![&sheet],
            Some(&user),
            &MediaEnv::default(),
            &[],
            None,
        );
        for pid in [
            PropertyId::MarginTop,
            PropertyId::MarginRight,
            PropertyId::MarginBottom,
            PropertyId::MarginLeft,
        ] {
            assert_eq!(margin_px(&out, pid), Some(5.0), "{pid:?}");
        }
    }

    #[test]
    fn margin_shorthand_revert_without_lower_tier_drops_all_longhands() {
        // 无更低起源档：revert 回滚目标不存在 → 四长手冠军全部剔除
        //（计算值期落初值 0，级联输出不出现 margin-* 冠军）。
        let sheet = parse_stylesheet("div { margin: 10px } div { margin: revert }");
        assert!(sheet.report.is_clean());
        let (tree, n) = tree_one("");
        let out = cascade_declarations(
            &tree,
            n,
            vec![&sheet],
            None,
            &MediaEnv::default(),
            &[],
            None,
        );
        assert!(out.winners.iter().all(|(p, _)| !matches!(
            p,
            PropertyId::MarginTop
                | PropertyId::MarginRight
                | PropertyId::MarginBottom
                | PropertyId::MarginLeft
        )));
    }

    #[test]
    fn margin_longhand_revert_only_affects_that_longhand() {
        // 长手 revert 仅还原该长手：margin-top 回滚到 User 档 5px，同简写
        // 写入的兄弟长手（10px）不受牵连——逐长手独立回滚（B1 在案语义）。
        let sheet = parse_stylesheet("div { margin: 10px } div { margin-top: revert }");
        assert!(sheet.report.is_clean());
        let user = parse_stylesheet("div { margin-top: 5px }");
        let (tree, n) = tree_one("");
        let out = cascade_declarations(
            &tree,
            n,
            vec![&sheet],
            Some(&user),
            &MediaEnv::default(),
            &[],
            None,
        );
        assert_eq!(margin_px(&out, PropertyId::MarginTop), Some(5.0));
        assert_eq!(margin_px(&out, PropertyId::MarginRight), Some(10.0));
        assert_eq!(margin_px(&out, PropertyId::MarginBottom), Some(10.0));
        assert_eq!(margin_px(&out, PropertyId::MarginLeft), Some(10.0));
    }
}
