//! 级联：规则匹配与胜出判定（css-cascade-5 序键）。
//!
//! 来源阶梯（低→高，`rank(important)`）：normal
//! `Default < UserAgent < User < Author(Stylesheet+Inline)`；
//! important 反转 `Author < User < UserAgent < Default`（引擎默认
//! !important 反转为最强，等价 UA !important 语义）。Inline 归入
//! Author 档（element-attached 以 u32::MAX 特异性在同档内取胜——
//! 与 B1 前独立 Inline 档逐位等价：胜者恒同）。层序（@layer，B1）
//! 在 origin-importance 之后、specificity 之前：normal 轴未分层胜
//! 一切分层（晚者胜同层内）；important 轴反转（未分层 important 输
//! 给一切分层 important，早层胜晚层）。同键内 (specificity, order,
//! decl_index) 升序、后者胜。每属性保留全部候选——revert/revert-layer
//! 赛后回滚（resolve_revert）。custom properties 同阶梯参与。

use crate::css::decl::TokenBuf;
use crate::css::property::ContainerType;
use crate::css::property::PropertyId;
use crate::css::property::WideKeyword;
use crate::css::stylesheet::{MediaEnv, Rule, Stylesheet};
use crate::selector::{PseudoElement, match_specificity};
use crate::tree::{NodeId, StyleTree};

/// 可查询容器的一帧快照（阶段2③）：restyle DFS 自祖先向内压栈，
/// @container 求值自栈顶向外查找。
#[derive(Debug, Clone, Default)]
pub struct ContainerCtx {
    /// container-name 名单（空 = 无名容器）。
    pub names: Vec<String>,
    /// container-type（normal 不入栈）。
    pub ctype: ContainerType,
    /// 内容盒尺寸（布局上一 pass 记录；None = 尺寸未就绪 → 特性 unknown）。
    pub size: Option<[f32; 2]>,
}

/// 级联来源层。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Origin {
    /// 引擎内置默认（在 ComputedStyle 初值层落地）。
    Default,
    /// UA 起源（B1：类型与阶梯就位；UA 声明的产生钩子留待宿主需求）。
    UserAgent,
    /// 用户起源（B1：`set_user_stylesheet` 注入的第二样式表）。
    User,
    /// 样式表规则（author）。
    Stylesheet,
    /// 节点内联声明。
    Inline,
}

impl Origin {
    /// 级联优先级阶梯（css-cascade-5 §6.4：origin+importance 反转）。
    /// Inline 归入 Author 档（element-attached 以 u32::MAX 特异性同档
    /// 取胜；与旧独立档逐位等价）。
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

/// 一条候选声明（含级联键）。
#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    /// 声明来源层（枚举序即优先级）。
    pub origin: Origin,
    /// 是否带 !important。
    pub important: bool,
    /// 命中选择器特异性；内联取 u32::MAX。
    pub specificity: u32,
    /// 规则源顺序；内联取 u32::MAX（同键后者胜）。
    pub order: u32,
    /// 规则内声明序（A8：同一规则块内逻辑/物理对按声明先后定夺）。
    pub decl_index: usize,
    /// B1 @layer：层序数（未分层 = u32::MAX）。
    pub layer_rank: u32,
    /// 胜出声明值（已解析或 var() 挂起 token）。
    pub value: &'a crate::css::decl::DeclSource,
}

/// 候选 custom property（B1：携带层序数；important 标记仍 MVP 忽略）。
#[derive(Debug, Clone, Copy)]
pub struct CustomCandidate<'a> {
    /// 声明来源层（枚举序即优先级）。
    pub origin: Origin,
    /// 命中选择器特异性。
    pub specificity: u32,
    /// 规则源顺序。
    pub order: u32,
    /// B1 @layer：层序数（未分层 = u32::MAX）。
    pub layer_rank: u32,
    /// 胜出的 custom property 原始 token（计算值期代换）。
    pub tokens: &'a TokenBuf,
}

/// 级联输出：每属性/custom property 的全部候选（按 PropertyId/名升序；
/// 列表序 = 压入序 = 源序，冠军取 beats 序最大、并列后者胜——B1 revert
/// 族需要次名候选回滚）。
#[derive(Debug, Default)]
pub struct CascadeOutput<'a> {
    /// 每属性候选列表（末位不保证是冠军——冠军经 `cascade_winner`）。
    pub winners: Vec<(PropertyId, Vec<Candidate<'a>>)>,
    /// 胜出自定义属性候选列表（按名升序）。
    pub custom_winners: Vec<(String, Vec<CustomCandidate<'a>>)>,
}

/// 层比较键：normal = layer_rank（未分层 u32::MAX 最高 = 胜一切分层）；
/// important 轴反转（u32::MAX - rank：未分层 important 最低 = 输给一切
/// 分层 important，早层胜晚层）——css-cascade-5 层序反转。
fn layer_key(c: &Candidate<'_>) -> u32 {
    if c.important {
        u32::MAX - c.layer_rank
    } else {
        c.layer_rank
    }
}

/// 级联键比较（css-cascade-5 序：origin-importance > 层 > specificity >
/// order > decl_index）；`>=` 语义：同键后者（源序更晚）胜。
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

/// 候选列表的级联冠军（beats 序最大、并列后者胜——列表序 = 压入序 =
/// 源序）。空列表返回 None。
pub(crate) fn cascade_winner<'c, 'a>(cands: &'c [Candidate<'a>]) -> Option<&'c Candidate<'a>> {
    let mut best = None;
    for (i, c) in cands.iter().enumerate() {
        if best.is_none_or(|b: usize| beats(c, &cands[b])) {
            best = Some(i);
        }
    }
    best.map(|i| &cands[i])
}

/// custom property 候选列表的级联冠军。
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

/// 宽关键字冠军值识别（revert 族回滚判定用）。
fn wide_keyword_of(c: &Candidate<'_>) -> Option<WideKeyword> {
    match c.value {
        crate::css::decl::DeclSource::Parsed(crate::css::property::DeclValue::WideKeyword(k)) => {
            Some(*k)
        }
        _ => None,
    }
}

/// custom property 冠军 token 是否恰为宽关键字单 ident（revert 族）。
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

/// B1：revert / revert-layer 赛后回滚（css-cascade-5）。冠军为宽关键字
/// 时取回滚目标：**revert** = 严格更低 origin-importance 档的冠军候选；
/// **revert-layer** = 同档内层键严格更小的冠军候选（无 → 按 revert 继续
/// 回滚）。无任何更低候选 → 剔除该属性（回落初值物化）。
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

/// 逻辑长hand → 物理槽映射（A8，css-logical-1）。ltr：inline-start=left、
/// inline-end=right、block-start=top、block-end=bottom；rtl：inline 轴
/// 翻转（块轴与纵向书写不支持——在案 FEATURES.md）；radius 四角随行内
/// 轴翻转（start-start↔start-end、end-start↔end-end）。
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

/// A8 逻辑属性解析（css-logical-1）：逻辑槽冠军与映射物理槽冠军按级联
/// 全序键 (origin-importance, layer, specificity, order, decl_index) 定夺
/// ——晚者填物理槽（规范正确：物理/逻辑同池比先后；同规则内按声明序），
/// 逻辑槽自身从输出剔除（无读者；ComputedStyle 仅暴露物理槽 + direction）。
/// 调用方（computed.rs）以节点 direction 传入 rtl。
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

/// C4（ADR-0018）：规则选择器表是否含通道伪元素分量。`want = None` 判
/// 任意通道变体（::selection/::placeholder），`Some(w)` 精确比对。
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

/// 单条命中的规则。
#[derive(Debug)]
pub struct MatchedRule<'a> {
    /// 命中的样式表规则。
    pub rule: &'a Rule,
    /// 命中选择器的特异性。
    pub specificity: u32,
}

/// 收集单节点命中的规则（@media 先行求值过滤；@container 按祖先容器
/// 快照求值过滤）。
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

/// 单表规则收集（B2：author 多表与 user 表共用；压入序 = 表序 × 源序）。
/// C4（ADR-0018）：`channel = None` 为主级联——排除 ::selection/
/// ::placeholder 规则（防样式泄漏，通道规则只经 cascade_channel 解析）；
/// `Some(w)` 为通道级联——仅收含对应通道伪元素分量的规则。
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

/// 匹配并级联单节点声明（B1 管线：User 表 + Author 表组 + Inline 全候选
/// 收集 → resolve_revert 回滚 → 排序；resolve_logical 由调用方在取得
/// 节点 direction 后另行调用；Default 层在 computed 落地）。B2：author
/// 改为表组按值（主表在前、附加表按登记序 = 文档序，后者胜平手）。
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

/// C4（ADR-0018）：通道级联（::selection/::placeholder）——UA → user →
/// author 序逐表收集对应通道规则（origin 直配由 match_pseudo_element 承担：
/// pseudo == None 即命中）；无内联段（style 属性无法指向伪元素，spec
/// 一致）；尾部与 cascade_declarations 同形（resolve_revert → retain →
/// 排序）。
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

    /// 读 margin 长手冠军的 px 值（None = 该长手无冠军 = 已回滚剔除或未写）。
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
