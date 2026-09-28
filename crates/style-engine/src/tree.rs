//! 样式树：宿主推送输入的镜像（ADR-0005 mirror-push-sync）。
//!
//! StyleTree 只存储「样式输入」（类型名/id/类/状态/内联声明/文本），
//! 不含布局与绘制状态；宿主通过 push 同步协议维持镜像一致。键为
//! slotmap `NodeId`（稳定、Copy、可哈希），供选择器匹配与引擎索引。

use crate::css::decl::DeclarationBlock;
use bitflags::bitflags;
use slotmap::{SecondaryMap, SlotMap, new_key_type};
use smallvec::SmallVec;

new_key_type! {
    /// 样式树节点键。
    pub struct NodeId;
}

bitflags! {
    /// 交互状态位（宿主推送；非树结构伪类匹配依据）。
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct NodeState: u16 {
        const HOVER = 1 << 0;
        const ACTIVE = 1 << 1;
        const FOCUS = 1 << 2;
        const FOCUS_VISIBLE = 1 << 3;
        const FOCUS_WITHIN = 1 << 4;
        const DISABLED = 1 << 5;
        const CHECKED = 1 << 6;
    }
}

impl Default for NodeState {
    fn default() -> Self {
        Self::empty()
    }
}

/// 单个节点的样式输入（宿主拥有语义，引擎镜像存储）。
#[derive(Debug, Clone, Default)]
pub struct StyleNode {
    /// 元素类型名（如 "button"）；None 则不参与类型选择器匹配。
    pub name: Option<String>,
    /// 文档内唯一 id（#foo）。
    pub id: Option<String>,
    /// class 列表（按精确项匹配）。
    pub classes: SmallVec<[String; 4]>,
    /// 交互状态位。
    pub state: NodeState,
    /// 内联声明（style 属性语义）。
    pub declarations: DeclarationBlock,
    /// 文本内容（叶节点；:empty 判定与 T5 文本布局用）。
    pub text: Option<String>,
    /// 富文本 span（T5c）：声明覆盖文本的字节区间 [range.0, range.1)。
    pub spans: SmallVec<[TextSpan; 2]>,
}

/// span 级富文本：区间内声明以级联覆盖基样式（引擎复用 compute_node 求解，
/// 以节点基样式为 parent，得到与选择器规则一致的覆盖语义）。
#[derive(Debug, Clone, PartialEq)]
pub struct TextSpan {
    pub range: (u32, u32),
    pub declarations: DeclarationBlock,
}

impl StyleNode {
    /// :empty 语义：无元素子节点且无非空文本。
    pub fn is_empty(&self) -> bool {
        self.text.as_ref().is_none_or(|t| t.is_empty())
    }
}

/// 样式树镜像。根节点固定存在（宿主 UI 树的根容器）。
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

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn node(&self, id: NodeId) -> &StyleNode {
        &self.nodes[id]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut StyleNode {
        &mut self.nodes[id]
    }

    /// 节点存储位置引用（opaque 身份用：slotmap 存储地址稳定且唯一）。
    pub(crate) fn node_ref(&self, id: NodeId) -> &StyleNode {
        &self.nodes[id]
    }

    /// 插入子节点（追加到 children 末尾），返回新 NodeId。
    pub fn insert_child(&mut self, parent: NodeId, node: StyleNode) -> NodeId {
        let id = self.nodes.insert(node);
        self.parent.insert(id, Some(parent));
        if !self.children.contains_key(parent) {
            self.children.insert(parent, Vec::new());
        }
        self.children.get_mut(parent).unwrap().push(id);
        id
    }

    /// 重设子节点顺序（镜像宿主顺序；id 须已是该父节点的子节点）。
    pub fn set_children(&mut self, parent: NodeId, ids: &[NodeId]) {
        if !self.children.contains_key(parent) {
            self.children.insert(parent, Vec::new());
        }
        let list = self.children.get_mut(parent).unwrap();
        list.clear();
        list.extend_from_slice(ids);
    }

    /// 移除节点及其整个子树，并从父链摘除。
    pub fn remove(&mut self, id: NodeId) {
        let mut stack = vec![id];
        let mut doomed = Vec::new();
        while let Some(cur) = stack.pop() {
            doomed.push(cur);
            if let Some(kids) = self.children.get(cur) {
                stack.extend(kids.iter().copied());
            }
        }
        if let Some(Some(p)) = self.parent.get(id) {
            if let Some(list) = self.children.get_mut(*p) {
                list.retain(|c| *c != id);
            }
        }
        for d in doomed {
            self.children.remove(d);
            self.parent.remove(d);
            self.nodes.remove(d);
        }
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.parent.get(id).copied().flatten()
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        self.children.get(id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// 节点总数（含根）。
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() == 0
    }

    pub fn is_root(&self, id: NodeId) -> bool {
        id == self.root
    }
}
