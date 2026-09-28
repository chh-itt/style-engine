//! 选择器适配：selectors 0.40（SelectorImpl/Parser/Element）+ 树匹配。
//!
//! selectors 0.40 与其配对的 cssparser 0.37 绑定（`cssparser-sel` 重命名
//! 依赖）；解析层的 cssparser 0.38 与之无类型交集——选择器预lude 以源
//! 文本形式交给 selectors 重解析（见 docs/DEPENDENCIES.md）。

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
    NonTSPseudoClass, ParseRelative, Parser as SelectorParserTrait,
    PseudoElement as PseudoElementTrait, SelectorImpl as SelectorImplTrait, SelectorList,
    SelectorParseErrorKind,
};
use selectors::{Element as ElementTrait, OpaqueElement};
use std::fmt;

/// selectors 内部使用的 cssparser（0.37，重命名依赖）。
use cssparser_sel as sel_css;

/// selectors 关联类型的字符串包装：String 未实现 0.37 cssparser 的
/// `ToCss` 与 `PrecomputedHash`，这里补齐（哈希用 FNV-1a，仅影响快筛）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct SelString(String);

impl SelString {
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

/// 选择器词表：类型/id/类名/属性值统一用 SelString（GUI 树无命名空间）。
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

/// 非树结构伪类（T0 子集，FEATURES.md）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PseudoClass {
    Hover,
    Active,
    Focus,
    FocusVisible,
    FocusWithin,
    Disabled,
    Enabled,
    Checked,
}

impl PseudoClass {
    /// 伪类名 → PseudoClass（大小写不敏感）。
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

    /// 与节点状态位求值。
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

/// 伪元素（T0 仅解析接受；匹配恒 false，生成内容随 T5 落地）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PseudoElement {
    Before,
    After,
}

impl sel_css::ToCss for PseudoElement {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        dest.write_str(match self {
            Self::Before => "::before",
            Self::After => "::after",
        })
    }
}

impl PseudoElementTrait for PseudoElement {
    type Impl = StyleSelectorImpl;
}

/// selectors 解析器钩子：接受状态伪类，开启 :is()/:where()。
#[derive(Debug, Default, Clone, Copy)]
pub struct SelectorParser;

impl<'i> SelectorParserTrait<'i> for SelectorParser {
    type Impl = StyleSelectorImpl;
    type Error = SelectorParseErrorKind<'i>;

    fn parse_is_and_where(&self) -> bool {
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
}

/// 本引擎的选择器列表类型。
pub type StyleSelectorList = SelectorList<StyleSelectorImpl>;

/// 从预lude 源文本解析选择器列表。
///
/// 失败信息为 Debug 文本（用于 ParseReport 警告；选择器解析失败整条
/// 规则按容错丢弃）。
pub fn parse_selector_list(source: &str) -> Result<StyleSelectorList, String> {
    // cssparser 0.37：Parser 借用可变的 ParserInput
    let mut input = sel_css::ParserInput::new(source);
    let mut parser = sel_css::Parser::new(&mut input);
    SelectorList::parse(&SelectorParser, &mut parser, ParseRelative::No)
        .map_err(|e| format!("{e:?}"))
}

/// 树视图：selectors Element 特征的挂载点。
#[derive(Clone, Debug)]
pub struct TreeNode<'a>(&'a StyleTree, crate::tree::NodeId);

impl<'a> TreeNode<'a> {
    pub fn new(tree: &'a StyleTree, id: crate::tree::NodeId) -> Self {
        Self(tree, id)
    }

    pub fn id(&self) -> crate::tree::NodeId {
        self.1
    }

    fn eq_case(case: CaseSensitivity, a: &str, b: &str) -> bool {
        match case {
            CaseSensitivity::CaseSensitive => a == b,
            _ => a.eq_ignore_ascii_case(b),
        }
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
        false
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        let parent = self.0.parent(self.1)?;
        let list = self.0.children(parent);
        let pos = list.iter().position(|c| *c == self.1)?;
        if pos == 0 {
            None
        } else {
            Some(TreeNode(self.0, list[pos - 1]))
        }
    }

    fn next_sibling_element(&self) -> Option<Self> {
        let parent = self.0.parent(self.1)?;
        let list = self.0.children(parent);
        let pos = list.iter().position(|c| *c == self.1)?;
        list.get(pos + 1).map(|id| TreeNode(self.0, *id))
    }

    fn first_element_child(&self) -> Option<Self> {
        self.0
            .children(self.1)
            .first()
            .map(|id| TreeNode(self.0, *id))
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
        _local_name: &SelString,
        _operation: &AttrSelectorOperation<&SelString>,
    ) -> bool {
        // MVP：无属性模型，属性选择器恒不匹配（偏差记录 FEATURES.md）
        false
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
        _pe: &PseudoElement,
        _context: &mut MatchingContext<Self::Impl>,
    ) -> bool {
        // 伪元素规则暂不匹配（生成内容 T5 落地）
        false
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
        self.0.node(self.1).is_empty() && self.0.children(self.1).is_empty()
    }

    fn is_root(&self) -> bool {
        self.0.is_root(self.1)
    }

    fn add_element_unique_hashes(&self, _filter: &mut BloomFilter) -> bool {
        // 不参与 bloom 快筛（树规模有限）
        false
    }
}

/// 单节点选择器列表匹配。
pub fn matches(tree: &StyleTree, id: crate::tree::NodeId, list: &StyleSelectorList) -> bool {
    match_specificity(tree, id, list).is_some()
}

/// 匹配并返回命中选择器的特异性（级联排序键）；未命中 → None。
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
    fn specificity_orders_id_over_class() {
        let by_id = parse_selector_list("#main").expect("parse");
        let by_class = parse_selector_list(".card .title").expect("parse");
        let s_id = by_id.slice()[0].specificity();
        let s_class = by_class.slice()[0].specificity();
        assert!(s_id > s_class, "{s_id} should beat {s_class}");
    }
}
