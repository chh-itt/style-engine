//! The style tree: a mirror of host-pushed input (ADR-0005
//! mirror-push-sync).
//!
//! StyleTree stores only "style input" (type name/id/classes/state/inline
//! declarations/text) and no layout or paint state; the host keeps the mirror
//! consistent through the push sync protocol. Keys are slotmap `NodeId`s
//! (stable, Copy, hashable), used for selector matching and engine indexing.

use crate::css::decl::DeclarationBlock;
use bitflags::bitflags;
use slotmap::{SecondaryMap, SlotMap, new_key_type};
use smallvec::SmallVec;

new_key_type! {
    /// A style-tree node key.
    pub struct NodeId;
}

bitflags! {
    /// Interaction state bits (pushed by the host; the basis for matching
    /// non-structural pseudo-classes).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct NodeState: u16 {
        /// :hover (hovering).
        const HOVER = 1 << 0;
        /// :active (pressed).
        const ACTIVE = 1 << 1;
        /// :focus (focused).
        const FOCUS = 1 << 2;
        /// :focus-visible (keyboard focus, visible).
        const FOCUS_VISIBLE = 1 << 3;
        /// :focus-within (self or a descendant focused).
        const FOCUS_WITHIN = 1 << 4;
        /// :disabled (disabled).
        const DISABLED = 1 << 5;
        /// :checked (checked).
        const CHECKED = 1 << 6;
    }
}

impl Default for NodeState {
    fn default() -> Self {
        Self::empty()
    }
}

/// Pseudo-element variants (C1/ADR-0015, P9-3): not host-constructible —
/// reserved for the engine's materialize_pseudos. Defined in tree.rs
/// (selector.rs references the mapping, avoiding a reverse dependency).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PseudoWhich {
    /// ::before (first-child pseudo node).
    Before,
    /// ::after (last-child pseudo node).
    After,
    /// ::marker (P9-3, css-lists-3 §3.1): the list-item marker pseudo node —
    /// hosted as the first child, before ::before; its text is machine
    /// synthesized by the engine from the host's list-style-* values (the
    /// §3.2 content algorithm); for hosts that are not list-item, the
    /// content computes to none (suppressing the box).
    Marker,
}

/// Style input for a single node (the host owns the semantics; the engine
/// mirrors the storage).
#[derive(Debug, Clone, Default)]
pub struct StyleNode {
    /// Element type name (e.g. "button"); None means no type-selector
    /// matching.
    pub name: Option<String>,
    /// Document-unique id (#foo).
    pub id: Option<String>,
    /// class list (matched by exact item).
    pub classes: SmallVec<[String; 4]>,
    /// Interaction state bits.
    pub state: NodeState,
    /// Inline declarations (style attribute semantics).
    pub declarations: DeclarationBlock,
    /// Text content (leaf nodes; used for :empty checks and T5 text layout).
    pub text: Option<String>,
    /// Replaced-content image reference (C3, css-images-3; ADR-0017): a
    /// reference name pre-registered by the host via `add_image`; None = not
    /// a replaced element. Painted by fitting into the content box per
    /// object-fit/object-position; intrinsic sizing is injected
    /// automatically (when set_leaf_intrinsic was not called manually).
    pub image: Option<String>,
    /// Attribute table (fifth batch ⑮, the attribute-selector data source):
    /// the host supplies \[attr\]/\[attr=value\] matching data; BTreeMap
    /// guarantees deterministic iteration order. GUI trees have no
    /// namespaces, and values are case-sensitive.
    pub attrs: std::collections::BTreeMap<String, String>,
    /// Rich-text spans (T5c): declarations overriding the byte range
    /// [range.0, range.1) of the text.
    pub spans: SmallVec<[TextSpan; 2]>,
    /// Pseudo-element marker (C1): Some = a pseudo node materialized by the
    /// engine (no host semantics, excluded from structural pseudo-class
    /// counting / the host mirror; text is supplied by the content computed
    /// value).
    pub pseudo: Option<PseudoWhich>,
}

/// Span-level rich text: declarations within the range override the base
/// style through the cascade (the engine reuses compute_node, with the
/// node's base style as parent, giving override semantics consistent with
/// selector rules).
#[derive(Debug, Clone, PartialEq)]
pub struct TextSpan {
    /// Text byte range [range.0, range.1).
    pub range: (u32, u32),
    /// Declarations overriding the base style within the range (cascaded
    /// with the node's base style as parent).
    pub declarations: DeclarationBlock,
}

impl StyleNode {
    /// :empty semantics: no element children and no non-empty text.
    pub fn is_empty(&self) -> bool {
        self.text.as_ref().is_none_or(|t| t.is_empty())
    }
}

/// The style-tree mirror. The root node always exists (the root container of
/// the host UI tree).
#[derive(Debug)]
pub struct StyleTree {
    nodes: SlotMap<NodeId, StyleNode>,
    parent: SecondaryMap<NodeId, Option<NodeId>>,
    children: SecondaryMap<NodeId, Vec<NodeId>>,
    root: NodeId,
}

impl Default for StyleTree {
    fn default() -> Self {
        Self::new()
    }
}

impl StyleTree {
    /// Creates a new mirror tree (the root node always exists).
    pub fn new() -> Self {
        let mut nodes = SlotMap::with_key();
        let root = nodes.insert(StyleNode::default());
        Self {
            nodes,
            parent: SecondaryMap::new(),
            children: SecondaryMap::new(),
            root,
        }
    }

    /// The root node key.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Reads a node's style input by key.
    pub fn node(&self, id: NodeId) -> &StyleNode {
        &self.nodes[id]
    }

    /// Reads/writes a node's style input by key (the host push-mirror
    /// channel).
    pub fn node_mut(&mut self, id: NodeId) -> &mut StyleNode {
        &mut self.nodes[id]
    }

    /// A reference to the node's storage slot (for opaque identity: slotmap
    /// storage addresses are stable and unique).
    pub(crate) fn node_ref(&self, id: NodeId) -> &StyleNode {
        &self.nodes[id]
    }

    /// Inserts a child (appended to the end of children), returning the new
    /// NodeId.
    pub fn insert_child(&mut self, parent: NodeId, node: StyleNode) -> NodeId {
        let id = self.nodes.insert(node);
        self.parent.insert(id, Some(parent));
        if !self.children.contains_key(parent) {
            self.children.insert(parent, Vec::new());
        }
        self.children.get_mut(parent).unwrap().push(id);
        id
    }

    /// Resets the child order (mirroring the host order; ids must already be
    /// children of this parent).
    pub fn set_children(&mut self, parent: NodeId, ids: &[NodeId]) {
        if !self.children.contains_key(parent) {
            self.children.insert(parent, Vec::new());
        }
        let list = self.children.get_mut(parent).unwrap();
        list.clear();
        list.extend_from_slice(ids);
    }

    /// Removes a node and its entire subtree, detaching it from the parent
    /// chain.
    pub fn remove(&mut self, id: NodeId) {
        let mut stack = vec![id];
        let mut doomed = Vec::new();
        while let Some(cur) = stack.pop() {
            doomed.push(cur);
            if let Some(kids) = self.children.get(cur) {
                stack.extend(kids.iter().copied());
            }
        }
        if let Some(Some(p)) = self.parent.get(id)
            && let Some(list) = self.children.get_mut(*p)
        {
            list.retain(|c| *c != id);
        }
        for d in doomed {
            self.children.remove(d);
            self.parent.remove(d);
            self.nodes.remove(d);
        }
    }

    /// The parent node key (None at the root).
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.parent.get(id).copied().flatten()
    }

    /// Child node keys (mirroring the host order).
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        self.children.get(id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Whether this is an engine-materialized pseudo-element node (C1).
    pub fn is_pseudo(&self, id: NodeId) -> bool {
        self.nodes.get(id).is_some_and(|n| n.pseudo.is_some())
    }

    /// Total node count (including the root).
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the tree is empty (the root always exists, so always false).
    pub fn is_empty(&self) -> bool {
        self.nodes.len() == 0
    }

    /// Whether this is a root node (a user root: parent = the synthetic
    /// super root).
    ///
    /// ADR-0010: the arena root slot is a synthetic super root (never bound
    /// to a key, always empty styles), and user roots are its children — the
    /// `:root` pseudo-class matches all user roots.
    pub fn is_root(&self, id: NodeId) -> bool {
        id != self.root && self.parent(id) == Some(self.root)
    }

    /// Whether this is the synthetic super root (the arena root slot; takes
    /// part in no cascade/paint semantics, serving only as a mount point).
    pub fn is_super_root(&self, id: NodeId) -> bool {
        id == self.root
    }
}
