//! Selector adaptation: selectors 0.40 (SelectorImpl/Parser/Element) + tree
//! matching.
//!
//! selectors 0.40 is bound to its paired cssparser 0.37 (the `cssparser-sel`
//! renamed dependency); the parsing layer's cssparser 0.38 shares no types with
//! it — the selector prelude is handed to selectors for re-parsing as source
//! text (see docs/DEPENDENCIES.md).

use crate::tree::{NodeState, StyleTree};
use precomputed_hash::PrecomputedHash;
use selectors::attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint};
use selectors::bloom::BloomFilter;
use selectors::context::{
    MatchingContext, MatchingForInvalidation, MatchingMode, NeedsSelectorFlags, QuirksMode,
    SelectorCaches,
};
use selectors::matching::{ElementSelectorFlags, matches_selector};
use selectors::parser::{
    Component, NonTSPseudoClass, ParseRelative, Parser as SelectorParserTrait,
    PseudoElement as PseudoElementTrait, SelectorImpl as SelectorImplTrait, SelectorList,
    SelectorParseErrorKind,
};
use selectors::{Element as ElementTrait, OpaqueElement};
use std::fmt;

/// The cssparser selectors uses internally (0.37, renamed dependency).
use cssparser_sel as sel_css;

/// String wrapper for the selectors associated types: String does not implement
/// the 0.37 cssparser's `ToCss` and `PrecomputedHash`, so they are provided here
/// (hashing uses FNV-1a, affecting only fast screening).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct SelString(String);

impl SelString {
    /// Read-only access to the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for SelString {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl std::ops::Deref for SelString {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for SelString {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for SelString {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl sel_css::ToCss for SelString {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        dest.write_str(&self.0)
    }
}

impl PrecomputedHash for SelString {
    fn precomputed_hash(&self) -> u32 {
        let mut hash: u32 = 0x811c_9dc5;
        for byte in self.0.as_bytes() {
            hash ^= u32::from(*byte);
            hash = hash.wrapping_mul(0x0100_0193);
        }
        hash
    }
}

/// Selector vocabulary: type/id/class names/attribute values all use SelString
/// (the GUI tree has no namespaces).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyleSelectorImpl;

impl SelectorImplTrait for StyleSelectorImpl {
    type ExtraMatchingData<'a> = ();
    type AttrValue = SelString;
    type Identifier = SelString;
    type LocalName = SelString;
    type NamespaceUrl = SelString;
    type NamespacePrefix = SelString;
    type BorrowedNamespaceUrl = str;
    type BorrowedLocalName = str;
    type NonTSPseudoClass = PseudoClass;
    type PseudoElement = PseudoElement;
}

/// Non-tree-structural pseudo-classes (T0 subset, FEATURES.md).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PseudoClass {
    /// :hover (hovering).
    Hover,
    /// :active (pressed).
    Active,
    /// :focus (focused).
    Focus,
    /// :focus-visible (keyboard-focus visible).
    FocusVisible,
    /// :focus-within (self or a descendant focused).
    FocusWithin,
    /// :disabled (disabled).
    Disabled,
    /// :enabled (enabled).
    Enabled,
    /// :checked (checked).
    Checked,
}

impl PseudoClass {
    /// Pseudo-class name → PseudoClass (case-insensitive).
    pub fn from_name(name: &str) -> Option<Self> {
        Some(cssparser::match_ignore_ascii_case!(name,
            "hover" => Self::Hover,
            "active" => Self::Active,
            "focus" => Self::Focus,
            "focus-visible" => Self::FocusVisible,
            "focus-within" => Self::FocusWithin,
            "disabled" => Self::Disabled,
            "enabled" => Self::Enabled,
            "checked" => Self::Checked,
            _ => return None,
        ))
    }

    /// Evaluates against the node's state bits.
    pub fn matches_state(&self, state: NodeState) -> bool {
        match self {
            Self::Hover => state.contains(NodeState::HOVER),
            Self::Active => state.contains(NodeState::ACTIVE),
            Self::Focus => state.contains(NodeState::FOCUS),
            Self::FocusVisible => state.contains(NodeState::FOCUS_VISIBLE),
            Self::FocusWithin => state.contains(NodeState::FOCUS_WITHIN),
            Self::Disabled => state.contains(NodeState::DISABLED),
            Self::Enabled => !state.contains(NodeState::DISABLED),
            Self::Checked => state.contains(NodeState::CHECKED),
        }
    }
}

impl fmt::Display for PseudoClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Hover => "hover",
            Self::Active => "active",
            Self::Focus => "focus",
            Self::FocusVisible => "focus-visible",
            Self::FocusWithin => "focus-within",
            Self::Disabled => "disabled",
            Self::Enabled => "enabled",
            Self::Checked => "checked",
        };
        f.write_str(name)
    }
}

impl sel_css::ToCss for PseudoClass {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        write!(dest, ":{self}")
    }
}

impl NonTSPseudoClass for PseudoClass {
    type Impl = StyleSelectorImpl;

    fn is_active_or_hover(&self) -> bool {
        matches!(self, Self::Hover | Self::Active)
    }

    fn is_user_action_state(&self) -> bool {
        matches!(
            self,
            Self::Hover | Self::Active | Self::Focus | Self::FocusVisible | Self::FocusWithin
        )
    }
}

/// Pseudo-elements (T0 accepts them at parse time only; matching is always
/// false, generated content lands with T5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PseudoElement {
    /// ::before.
    Before,
    /// ::after.
    After,
    /// ::selection (C4/ADR-0018: non-box-generating — the selection text styling
    /// channel).
    Selection,
    /// ::placeholder (C4/ADR-0018: non-box-generating — the placeholder text
    /// styling channel).
    Placeholder,
    /// ::marker (P9-3/css-lists-3 §3.1: box-generating — a list-item marker
    /// pseudo-node whose text the engine synthesizes from the host's
    /// list-style-*).
    Marker,
}

impl sel_css::ToCss for PseudoElement {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        dest.write_str(match self {
            Self::Before => "::before",
            Self::After => "::after",
            Self::Selection => "::selection",
            Self::Placeholder => "::placeholder",
            Self::Marker => "::marker",
        })
    }
}

impl PseudoElementTrait for PseudoElement {
    type Impl = StyleSelectorImpl;
}

/// selectors parser hook: accepts state pseudo-classes, enables :is()/:where().
#[derive(Debug, Default, Clone, Copy)]
pub struct SelectorParser;

impl<'i> SelectorParserTrait<'i> for SelectorParser {
    type Impl = StyleSelectorImpl;
    type Error = SelectorParseErrorKind<'i>;

    fn parse_is_and_where(&self) -> bool {
        true
    }

    /// B3: enables :has() relative selectors (selectors 0.40 ships its own
    /// parsing and matching — the matching.rs relative_selector module walks the
    /// same Element trait; invalidation model = engine-side any_has_rules()
    /// full re-style escalation).
    fn parse_has(&self) -> bool {
        true
    }

    fn parse_non_ts_pseudo_class(
        &self,
        location: sel_css::SourceLocation,
        name: sel_css::CowRcStr<'i>,
    ) -> Result<PseudoClass, sel_css::ParseError<'i, Self::Error>> {
        PseudoClass::from_name(&name).ok_or_else(|| {
            location.new_custom_error(SelectorParseErrorKind::UnsupportedPseudoClassOrElement(
                name,
            ))
        })
    }

    /// C1 (ADR-0015): box-generating pseudo-elements ::before/::after; C4
    /// (ADR-0018): non-box-generating pseudo-elements ::selection/::placeholder.
    /// selectors 0.40's `is_css2_pseudo_element` legacy routing automatically
    /// sends the single-colon `:before` form down the same path (the CSS2 set
    /// contains no selection/placeholder → single-colon forms of those are
    /// naturally rejected, spec-conformant); other pseudo-elements such as
    /// first-line/first-letter are out of scope and rejected.
    fn parse_pseudo_element(
        &self,
        location: sel_css::SourceLocation,
        name: sel_css::CowRcStr<'i>,
    ) -> Result<PseudoElement, sel_css::ParseError<'i, Self::Error>> {
        if name.eq_ignore_ascii_case("before") {
            Ok(PseudoElement::Before)
        } else if name.eq_ignore_ascii_case("after") {
            Ok(PseudoElement::After)
        } else if name.eq_ignore_ascii_case("selection") {
            Ok(PseudoElement::Selection)
        } else if name.eq_ignore_ascii_case("placeholder") {
            Ok(PseudoElement::Placeholder)
        } else if name.eq_ignore_ascii_case("marker") {
            Ok(PseudoElement::Marker)
        } else {
            Err(
                location.new_custom_error(SelectorParseErrorKind::UnsupportedPseudoClassOrElement(
                    name,
                )),
            )
        }
    }
}

/// This engine's selector list type.
pub type StyleSelectorList = SelectorList<StyleSelectorImpl>;

/// Parses a selector list from prelude source text.
///
/// The failure message is Debug text (used for ParseReport warnings; on selector
/// parse failure the whole rule is dropped per the error-tolerance policy).
pub fn parse_selector_list(source: &str) -> Result<StyleSelectorList, String> {
    // cssparser 0.37：Parser 借用可变的 ParserInput
    let mut input = sel_css::ParserInput::new(source);
    let mut parser = sel_css::Parser::new(&mut input);
    SelectorList::parse(&SelectorParser, &mut parser, ParseRelative::No)
        .map_err(|e| format!("{e:?}"))
}

/// P6 (ADR-0035 D1): host compound fast-screen key for `:has()` rules — the
/// type/class/id constraints extracted from the single compound preceding `:has`.
/// An all-empty key (sentinel) = unconstrained; the pre-screen never vetoes it
/// (conservative direction: can only over-escalate, never miss an escalation).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HasHostKey {
    /// Type selector constraint (None = unconstrained or wildcard).
    pub(crate) tag: Option<String>,
    /// Class constraint set (empty = unconstrained; a hit requires the key's
    /// class set ⊆ the target's class set).
    pub(crate) classes: Vec<String>,
    /// id constraint (None = unconstrained).
    pub(crate) id: Option<String>,
}

impl HasHostKey {
    /// Fast-screens a tree node: tag exact equality (case-sensitive `==`, same
    /// as the matcher's `has_local_name`, so no false negatives) / key class set
    /// ⊆ node class set / id exact equality; a None field = unconstrained on that
    /// axis, always passes.
    pub(crate) fn matches_node(
        &self,
        name: Option<&str>,
        id: Option<&str>,
        classes: &[String],
    ) -> bool {
        self.tag.as_ref().is_none_or(|t| name == Some(t.as_str()))
            && self.id.as_ref().is_none_or(|i| id == Some(i.as_str()))
            && self
                .classes
                .iter()
                .all(|c| classes.iter().any(|nc| nc == c))
    }
}

/// P6 (ADR-0035): deep-scans a selector list for `:has()` (including nesting
/// inside `:is()`/`:where()`/`:not()` arguments — anything containing a relative
/// selector falls under fast-screen gating, preventing a `:has` hidden inside a
/// functional pseudo-class argument from bypassing the index and missing an
/// escalation).
pub(crate) fn list_contains_has(list: &StyleSelectorList) -> bool {
    list.slice().iter().any(selector_contains_has)
}

fn selector_contains_has(selector: &selectors::parser::Selector<StyleSelectorImpl>) -> bool {
    selector.iter_raw_match_order().any(|c| match c {
        Component::Has(_) => true,
        Component::Is(l) | Component::Where(l) | Component::Negation(l) => {
            l.slice().iter().any(selector_contains_has)
        }
        _ => false,
    })
}

/// P6 (ADR-0035 D1): extracts the host fast-screen key of a single selector.
///
/// Returns None = the selector does not qualify (no `:has` / `:has` with a
/// prefixed combinator `.a > .b:has(x)` / `:has` not in the top-level rightmost
/// compound) — the caller falls back to the full scan for non-qualifying
/// selectors. Qualification test: the rightmost compound (the first `iter()`
/// sequence) contains a top-level `Has` component and `next_sequence()` is None
/// (no prefixed combinator). Other components inside the compound (attribute
/// selectors/pseudo-classes/namespaces etc.) do not enter the key — the key only
/// weakens constraints, never strengthens them, so the veto direction is
/// unaffected (conservatively correct).
pub(crate) fn has_host_key(
    selector: &selectors::parser::Selector<StyleSelectorImpl>,
) -> Option<HasHostKey> {
    let mut key = HasHostKey::default();
    let mut has_present = false;
    let mut iter = selector.iter();
    for c in iter.by_ref() {
        match c {
            Component::Has(_) => has_present = true,
            Component::LocalName(ln) => key.tag = Some(ln.name.as_str().to_string()),
            Component::ID(id) => key.id = Some(id.as_str().to_string()),
            Component::Class(cl) => key.classes.push(cl.as_str().to_string()),
            // 通配/命名空间/属性/伪类/结构伪类/伪元素：不进键（约束弱化）。
            _ => {}
        }
    }
    if !has_present || iter.next_sequence().is_some() {
        return None;
    }
    Some(key)
}

/// Tree view: the mount point for the selectors Element trait.
#[derive(Clone, Debug)]
pub struct TreeNode<'a>(&'a StyleTree, crate::tree::NodeId);

impl<'a> TreeNode<'a> {
    /// Mounts the style tree and a node key.
    pub fn new(tree: &'a StyleTree, id: crate::tree::NodeId) -> Self {
        Self(tree, id)
    }

    /// The current node's key.
    pub fn id(&self) -> crate::tree::NodeId {
        self.1
    }

    fn eq_case(case: CaseSensitivity, a: &str, b: &str) -> bool {
        match case {
            CaseSensitivity::CaseSensitive => a == b,
            _ => a.eq_ignore_ascii_case(b),
        }
    }

    /// Structural traversal skips pseudo-nodes (ADR-0015): host
    /// :first-child/:nth-child/sibling-combinator counts are bit-identical
    /// before and after pseudo-element materialization.
    fn first_host_child(&self) -> Option<crate::tree::NodeId> {
        self.0
            .children(self.1)
            .iter()
            .find(|c| !self.0.is_pseudo(**c))
            .copied()
    }
}

impl<'a> ElementTrait for TreeNode<'a> {
    type Impl = StyleSelectorImpl;

    fn opaque(&self) -> OpaqueElement {
        // slotmap 存储地址稳定且每节点唯一
        OpaqueElement::new(self.0.node_ref(self.1))
    }

    fn parent_element(&self) -> Option<Self> {
        self.0.parent(self.1).map(|p| TreeNode(self.0, p))
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        false
    }

    fn containing_shadow_host(&self) -> Option<Self> {
        None
    }

    fn is_pseudo_element(&self) -> bool {
        self.0.node(self.1).pseudo.is_some()
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        let parent = self.0.parent(self.1)?;
        let list = self.0.children(parent);
        let pos = list.iter().position(|c| *c == self.1)?;
        list[..pos]
            .iter()
            .rev()
            .find(|c| !self.0.is_pseudo(**c))
            .map(|id| TreeNode(self.0, *id))
    }

    fn next_sibling_element(&self) -> Option<Self> {
        let parent = self.0.parent(self.1)?;
        let list = self.0.children(parent);
        let pos = list.iter().position(|c| *c == self.1)?;
        list[pos + 1..]
            .iter()
            .find(|c| !self.0.is_pseudo(**c))
            .map(|id| TreeNode(self.0, *id))
    }

    fn first_element_child(&self) -> Option<Self> {
        self.first_host_child().map(|id| TreeNode(self.0, id))
    }

    fn is_html_element_in_html_document(&self) -> bool {
        false
    }

    fn has_local_name(&self, name: &str) -> bool {
        self.0.node(self.1).name.as_deref() == Some(name)
    }

    fn has_namespace(&self, _ns: &str) -> bool {
        false
    }

    fn is_same_type(&self, other: &Self) -> bool {
        self.0.node(self.1).name == other.0.node(other.1).name
    }

    fn attr_matches(
        &self,
        _ns: &NamespaceConstraint<&SelString>,
        local_name: &SelString,
        operation: &AttrSelectorOperation<&SelString>,
    ) -> bool {
        // 第五批⑮属性选择器：宿主经 StyleNode.attrs 供属性（BTreeMap）。
        // [attr]（Exists）=存在即命中；带值操作（=、~=、^= 等）经
        // eval_str（大小写敏感性已随选择器文法封装在操作里）。GUI 树无
        // 命名空间。
        let attrs = &self.0.node(self.1).attrs;
        match operation {
            AttrSelectorOperation::Exists => attrs.contains_key(local_name.as_str()),
            op => attrs
                .get(local_name.as_str())
                .is_some_and(|v| op.eval_str(v)),
        }
    }

    fn match_non_ts_pseudo_class(
        &self,
        pc: &PseudoClass,
        _context: &mut MatchingContext<Self::Impl>,
    ) -> bool {
        pc.matches_state(self.0.node(self.1).state)
    }

    fn match_pseudo_element(
        &self,
        pe: &PseudoElement,
        _context: &mut MatchingContext<Self::Impl>,
    ) -> bool {
        // C1（ADR-0015）：伪元素节点命中——引擎实体化的伪节点携带
        // PseudoWhich 标记；宿主节点恒 None 恒不命中。
        // C4（ADR-0018）：非盒生成伪元素 ::selection/::placeholder ——
        // origin 节点直配（pseudo == None 即命中；通道级联专用），实体化
        // 伪节点 pseudo == Some 恒不命中。match_specificity/match_rules
        // 原样可用（尾伪元素无需截断匹配）。
        let which = match pe {
            PseudoElement::Before => crate::tree::PseudoWhich::Before,
            PseudoElement::After => crate::tree::PseudoWhich::After,
            PseudoElement::Marker => crate::tree::PseudoWhich::Marker,
            PseudoElement::Selection | PseudoElement::Placeholder => {
                return self.0.node(self.1).pseudo.is_none();
            }
        };
        self.0.node(self.1).pseudo == Some(which)
    }

    /// C4 (ADR-0018): the combinator back-tracking target for pseudo-elements.
    /// C1-materialized pseudo-nodes keep the default parent chain (the host
    /// node); ::selection/::placeholder match directly on the origin node
    /// (channel matching) — after the pseudo-element subject compound hits, the
    /// prefixed compound is still evaluated on the origin node itself
    /// (originating element = the node itself).
    fn pseudo_element_originating_element(&self) -> Option<Self> {
        if self.0.node(self.1).pseudo.is_some() {
            self.parent_element()
        } else {
            Some(self.clone())
        }
    }

    fn apply_selector_flags(&self, _flags: ElementSelectorFlags) {
        // 失效由引擎 Epoch 管控，这里无需按元素打标
    }

    fn is_link(&self) -> bool {
        false
    }

    fn is_html_slot_element(&self) -> bool {
        false
    }

    fn has_id(&self, id: &SelString, case_sensitivity: CaseSensitivity) -> bool {
        self.0
            .node(self.1)
            .id
            .as_deref()
            .is_some_and(|v| Self::eq_case(case_sensitivity, v, id.as_str()))
    }

    fn has_class(&self, name: &SelString, case_sensitivity: CaseSensitivity) -> bool {
        self.0
            .node(self.1)
            .classes
            .iter()
            .any(|c| Self::eq_case(case_sensitivity, c, name.as_str()))
    }

    fn has_custom_state(&self, _name: &SelString) -> bool {
        false
    }

    fn imported_part(&self, _name: &SelString) -> Option<SelString> {
        None
    }

    fn is_part(&self, _name: &SelString) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        // ADR-0015：伪节点不影响 :empty（spec——伪元素非元素子节点）
        self.0.node(self.1).is_empty()
            && !self
                .0
                .children(self.1)
                .iter()
                .any(|c| !self.0.is_pseudo(*c))
    }

    fn is_root(&self) -> bool {
        self.0.is_root(self.1)
    }

    fn add_element_unique_hashes(&self, _filter: &mut BloomFilter) -> bool {
        // 不参与 bloom 快筛（树规模有限）
        false
    }
}

/// Matches a selector list against a single node.
pub fn matches(tree: &StyleTree, id: crate::tree::NodeId, list: &StyleSelectorList) -> bool {
    match_specificity(tree, id, list).is_some()
}

/// Matches and returns the specificity of the hit selector (the cascade sort
/// key); no hit → None.
pub fn match_specificity(
    tree: &StyleTree,
    id: crate::tree::NodeId,
    list: &StyleSelectorList,
) -> Option<u32> {
    let element = TreeNode::new(tree, id);
    let mut caches = SelectorCaches::default();
    let mut context = MatchingContext::new(
        MatchingMode::Normal,
        None,
        &mut caches,
        QuirksMode::NoQuirks,
        NeedsSelectorFlags::No,
        MatchingForInvalidation::No,
    );
    list.slice()
        .iter()
        .find(|s| matches_selector(s, 0, None, &element, &mut context))
        .map(|s| s.specificity())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{StyleNode, StyleTree};

    fn node(name: &str, classes: &[&str]) -> StyleNode {
        StyleNode {
            name: Some(name.to_string()),
            classes: classes.iter().map(|c| c.to_string()).collect(),
            ..Default::default()
        }
    }

    fn matched(tree: &StyleTree, id: crate::tree::NodeId, selector: &str) -> bool {
        let list = parse_selector_list(selector).expect("selector should parse");
        matches(tree, id, &list)
    }

    #[test]
    fn probe_pseudo_element_origin_match() {
        // C4 探针：::selection 于树根与子节点的直配（区分匹配层/引擎层）。
        let mut tree = StyleTree::new();
        let root = tree.root();
        let a = tree.insert_child(root, node("div", &[]));
        tree.node_mut(a).id = Some("root".into());
        let b = tree.insert_child(a, node("div", &[]));
        tree.node_mut(b).id = Some("t".into());
        let l_root = parse_selector_list("#root::selection").expect("parse");
        let l_t = parse_selector_list("#t::selection").expect("parse");
        assert!(
            match_specificity(&tree, a, &l_root).is_some(),
            "根节点 #root::selection 应命中"
        );
        assert!(
            match_specificity(&tree, b, &l_t).is_some(),
            "子节点 #t::selection 应命中"
        );
    }

    #[test]
    fn class_type_and_id() {
        let mut tree = StyleTree::new();
        let root = tree.root();
        let card = tree.insert_child(root, node("div", &["card"]));
        tree.node_mut(card).id = Some("main-card".into());
        let title = tree.insert_child(card, node("h1", &["title"]));

        assert!(matched(&tree, card, ".card"));
        assert!(matched(&tree, card, "div.card"));
        assert!(matched(&tree, card, "#main-card"));
        assert!(!matched(&tree, card, "h1"));
        assert!(matched(&tree, title, "h1.title"));
        assert!(!matched(&tree, title, ".card"));
    }

    #[test]
    fn descendant_and_child_combinators() {
        let mut tree = StyleTree::new();
        let root = tree.root();
        let card = tree.insert_child(root, node("div", &["card"]));
        let inner = tree.insert_child(card, node("div", &["inner"]));
        let title = tree.insert_child(inner, node("h1", &["title"]));

        assert!(matched(&tree, title, ".card .title"));
        assert!(matched(&tree, title, ".card h1"));
        assert!(!matched(&tree, title, ".card > .title"));
        assert!(matched(&tree, inner, ".card > .inner"));
        assert!(matched(&tree, title, ".card .inner .title"));
        assert!(!matched(&tree, card, ".card .card"));
    }

    #[test]
    fn state_pseudo_classes() {
        use crate::tree::NodeState;
        let mut tree = StyleTree::new();
        let root = tree.root();
        let btn = tree.insert_child(root, node("button", &["btn"]));
        tree.node_mut(btn).state |= NodeState::HOVER | NodeState::FOCUS;

        assert!(matched(&tree, btn, "button:hover"));
        assert!(matched(&tree, btn, ":focus"));
        assert!(!matched(&tree, btn, ":active"));
        assert!(matched(&tree, btn, "button:enabled"));
        assert!(!matched(&tree, btn, ":disabled"));
        assert!(matched(&tree, btn, "button:hover:focus"));
    }

    #[test]
    fn is_not_and_structural() {
        let mut tree = StyleTree::new();
        let root = tree.root();
        let a = tree.insert_child(root, node("li", &["odd"]));
        let b = tree.insert_child(root, node("li", &["even"]));
        let c = tree.insert_child(root, node("li", &["odd"]));

        assert!(matched(&tree, b, "li:nth-child(2)"));
        assert!(matched(&tree, a, "li:first-child"));
        assert!(matched(&tree, c, "li:last-child"));
        assert!(!matched(&tree, b, "li:first-child"));
        assert!(matched(&tree, b, "li:not(.odd)"));
        assert!(matched(&tree, a, ":is(.odd, .even)"));
        assert!(!matched(&tree, b, ":is(.odd)"));
        assert!(matched(&tree, c, "li:nth-child(odd)"));
    }

    #[test]
    fn invalid_selector_is_rejected() {
        assert!(parse_selector_list(":::bad").is_err());
        assert!(parse_selector_list(".a >> .b").is_err());
        assert!(parse_selector_list("div.card > h1").is_ok());
    }

    #[test]
    fn attribute_selectors() {
        // 第五批⑮：属性选择器——宿主经 StyleNode.attrs 供属性（BTreeMap）；
        // [attr]=存在即命中（空值也算），带值操作=、、~=、^=、$=、*=
        // 大小写敏感（GUI 树无命名空间）
        let mut tree = StyleTree::new();
        let root = tree.root();
        let mk = |attrs: &[(&str, &str)]| StyleNode {
            name: Some("button".to_string()),
            attrs: attrs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        };
        let a = tree.insert_child(
            root,
            mk(&[("data-kind", "primary"), ("data-tags", "a b c")]),
        );
        let b = tree.insert_child(root, mk(&[("data-on", "")]));
        let c = tree.insert_child(root, mk(&[]));

        assert!(matched(&tree, a, "button[data-kind]"));
        assert!(matched(&tree, a, "button[data-kind=primary]"));
        assert!(matched(&tree, a, "button[data-kind^=prim]"));
        assert!(matched(&tree, a, "button[data-kind$=ary]"));
        assert!(matched(&tree, a, "button[data-kind*=imar]"));
        assert!(
            matched(&tree, a, "button[data-kind~=primary]"),
            "单词表包含"
        );
        assert!(matched(&tree, a, "button[data-tags~=b]"), "多词表包含");
        assert!(!matched(&tree, a, "button[data-tags~=d]"));
        assert!(!matched(&tree, a, "button[data-kind=Primary]"));
        assert!(matched(&tree, b, "[data-on]"), "空值属性存在即命中");
        assert!(!matched(&tree, c, "[data-kind]"));
        assert!(!matched(&tree, c, "button[data-x=y]"));
    }

    #[test]
    fn specificity_orders_id_over_class() {
        let by_id = parse_selector_list("#main").expect("parse");
        let by_class = parse_selector_list(".card .title").expect("parse");
        let s_id = by_id.slice()[0].specificity();
        let s_class = by_class.slice()[0].specificity();
        assert!(s_id > s_class, "{s_id} should beat {s_class}");
    }
}
