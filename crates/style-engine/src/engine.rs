//! 样式引擎：宿主推送同步协议 + 帧驱动（restyle → taffy layout）。
//!
//! 引擎零副作用：样式表/树镜像/状态/环境/叶测量全部由宿主推送
//! （ADR-0005/0006）。`frame()` 单调推进 sync→style→layout，返回
//! 生成号戳记的布局帧（T4 起叠加 DisplayList）。

use crate::computed::{ComputedStyle, compute_node};
use crate::css::decl::parse_inline_declarations;
use crate::css::stylesheet::{MediaEnv, Stylesheet};
use crate::error::ParseReport;
use crate::layout::map_style;
use crate::tree::{NodeId, StyleNode, StyleTree};
use std::collections::HashMap;
use std::hash::Hash;

/// 布局帧条目（border-box；坐标相对视口、滚动前）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutEntry<K: Copy> {
    pub key: K,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// 一帧的布局与绘制结果。
#[derive(Debug, Clone)]
pub struct Frame<K: Copy> {
    /// 单调递增帧号。
    pub generation: u64,
    /// 布局盒（树序，含根）。
    pub boxes: Vec<LayoutEntry<K>>,
    /// 本帧绘制清单（树序基元）。
    pub paint: crate::paint::DisplayList,
}

impl<K: Copy + PartialEq> Frame<K> {
    /// 按 key 查找布局盒。
    pub fn find(&self, key: K) -> Option<&LayoutEntry<K>> {
        self.boxes.iter().find(|b| b.key == key)
    }
}

/// 样式引擎实例。K 为宿主节点键（Copy + Eq + Hash）。
pub struct StyleEngine<K: Copy + Eq + Hash + 'static> {
    tree: StyleTree,
    sheet: Stylesheet,
    media: MediaEnv,
    key_to_node: HashMap<K, NodeId>,
    node_to_key: HashMap<NodeId, K>,
    root_key: Option<K>,
    /// 宿主推送的叶测量（文本叶，T5 前由宿主提供）。
    measures: HashMap<NodeId, (f32, f32)>,
    /// 节点滚动偏移（绘制层用；布局不消费）。
    scroll_offsets: HashMap<NodeId, (f32, f32)>,
    epoch: u64,
    generation: u64,
    dirty_struct: bool,
    dirty_style: bool,
    viewport: (f32, f32),
    scale: f32,
    now: f64,
    // taffy 镜像
    taffy: taffy::TaffyTree,
    taffy_root: Option<taffy::NodeId>,
    taffy_node: HashMap<NodeId, taffy::NodeId>,
    styles: HashMap<NodeId, ComputedStyle>,
    /// span 级样式（T5c）：NodeId → (字节起点, 字节终点, 覆盖后的 ComputedStyle)。
    span_styles: HashMap<NodeId, Vec<(u32, u32, ComputedStyle)>>,
    /// 内置文本栈（feature = "text"；字体字节由宿主推送）。
    #[cfg(feature = "text")]
    text: crate::text::TextSystem,
}

impl<K: Copy + Eq + Hash + 'static> Default for StyleEngine<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Copy + Eq + Hash + 'static> StyleEngine<K> {
    pub fn new() -> Self {
        Self {
            tree: StyleTree::new(),
            sheet: Stylesheet::default(),
            media: MediaEnv::default(),
            key_to_node: HashMap::new(),
            node_to_key: HashMap::new(),
            root_key: None,
            measures: HashMap::new(),
            scroll_offsets: HashMap::new(),
            epoch: 0,
            generation: 0,
            dirty_struct: true,
            dirty_style: true,
            viewport: (0.0, 0.0),
            scale: 1.0,
            now: 0.0,
            taffy: taffy::TaffyTree::new(),
            #[cfg(feature = "text")]
            text: crate::text::TextSystem::new(),
            taffy_root: None,
            taffy_node: HashMap::new(),
            styles: HashMap::new(),
            span_styles: HashMap::new(),
        }
    }

    // ---------- 推送协议 ----------

    /// 全量替换样式表（容错：坏规则跳过并记录）。
    pub fn set_stylesheet(&mut self, source: &str) -> ParseReport {
        let sheet = crate::css::stylesheet::parse_stylesheet(source);
        self.sheet = sheet;
        self.epoch += 1;
        self.dirty_style = true;
        self.sheet.report.clone()
    }

    /// 更新媒体环境（视口外因素：配色/动效偏好）。
    pub fn set_environment(&mut self, env: MediaEnv) {
        if self.media != env {
            self.media = env;
            self.dirty_style = true;
        }
    }

    /// 插入节点。`parent=None` 声明根（至多一次）。
    pub fn insert(
        &mut self,
        parent: Option<K>,
        key: K,
        node: StyleNode,
    ) -> Result<(), crate::error::ContractError> {
        if self.key_to_node.contains_key(&key) {
            return Err(crate::error::ContractError::DuplicateNode);
        }
        match parent {
            None => {
                if self.root_key.is_some() {
                    return Err(crate::error::ContractError::RootExists);
                }
                let root = self.tree.root();
                *self.tree.node_mut(root) = node;
                self.key_to_node.insert(key, root);
                self.node_to_key.insert(root, key);
                self.root_key = Some(key);
                self.dirty_struct = true;
                Ok(())
            }
            Some(pk) => {
                let Some(&pid) = self.key_to_node.get(&pk) else {
                    return Err(crate::error::ContractError::UnknownNode);
                };
                let id = self.tree.insert_child(pid, node);
                self.key_to_node.insert(key, id);
                self.node_to_key.insert(id, key);
                self.dirty_struct = true;
                Ok(())
            }
        }
    }

    /// 移除节点及其子树（根移除 = 清空引擎树）。
    pub fn remove(&mut self, key: K) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        if Some(key) == self.root_key {
            // 清空重来
            self.tree = StyleTree::new();
            self.key_to_node.clear();
            self.node_to_key.clear();
            self.measures.clear();
            self.scroll_offsets.clear();
            self.root_key = None;
            self.taffy_root = None;
            self.taffy_node.clear();
            self.styles.clear();
            self.dirty_struct = true;
            self.dirty_style = true;
            return Ok(());
        }
        // 收集子树 keys 一并解除映射
        let mut stack = vec![id];
        let mut doomed = Vec::new();
        while let Some(cur) = stack.pop() {
            doomed.push(cur);
            stack.extend(self.tree.children(cur).iter().copied());
        }
        for d in doomed {
            if let Some(k) = self.node_to_key.remove(&d) {
                self.key_to_node.remove(&k);
            }
            self.measures.remove(&d);
            self.scroll_offsets.remove(&d);
            self.styles.remove(&d);
            self.taffy_node.remove(&d);
        }
        self.tree.remove(id);
        self.dirty_struct = true;
        Ok(())
    }

    /// 重设子节点顺序（所有 key 须已是该父节点的子节点）。
    pub fn set_children(
        &mut self,
        parent: K,
        children: &[K],
    ) -> Result<(), crate::error::ContractError> {
        let Some(&pid) = self.key_to_node.get(&parent) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        let mut ids = Vec::with_capacity(children.len());
        for k in children {
            let Some(&cid) = self.key_to_node.get(k) else {
                return Err(crate::error::ContractError::UnknownNode);
            };
            if self.tree.parent(cid) != Some(pid) {
                return Err(crate::error::ContractError::UnknownNode);
            }
            ids.push(cid);
        }
        self.tree.set_children(pid, &ids);
        self.dirty_struct = true;
        Ok(())
    }

    /// 更新节点 class 列表。
    pub fn set_classes(
        &mut self,
        key: K,
        classes: &[String],
    ) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        self.tree.node_mut(id).classes = classes.iter().cloned().collect();
        self.dirty_style = true;
        Ok(())
    }

    /// 更新节点交互状态位。
    pub fn set_state(
        &mut self,
        key: K,
        state: crate::tree::NodeState,
    ) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        self.tree.node_mut(id).state = state;
        self.dirty_style = true;
        Ok(())
    }

    /// 更新节点文本（叶内容）。
    pub fn set_text(
        &mut self,
        key: K,
        text: Option<String>,
    ) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        self.tree.node_mut(id).text = text;
        self.dirty_style = true;
        Ok(())
    }

    /// 更新内联声明（style 属性文本；容错解析，报告随返回值给出）。
    pub fn set_declarations(
        &mut self,
        key: K,
        style_text: &str,
    ) -> Result<ParseReport, crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        let (block, report) = parse_inline_declarations(style_text);
        self.tree.node_mut(id).declarations = block;
        self.dirty_style = true;
        Ok(report)
    }

    /// 推送叶测量（文本叶尺寸；T5 后由内置 parley 测量接管）。
    pub fn set_leaf_measure(
        &mut self,
        key: K,
        width: f32,
        height: f32,
    ) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        self.measures.insert(id, (width, height));
        self.dirty_style = true;
        Ok(())
    }

    /// 推送节点滚动偏移（绘制消费；T4 生效）。
    pub fn set_scroll_offset(
        &mut self,
        key: K,
        x: f32,
        y: f32,
    ) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        self.scroll_offsets.insert(id, (x, y));
        Ok(())
    }

    /// 当前样式表纪元（单调递增）。
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    // ---------- 帧驱动 ----------

    /// 推进一帧：结构同步 → 重算样式 → taffy 布局 → 收集布局盒。
    pub fn frame(&mut self, viewport: (f32, f32), scale: f32, now: f64) -> Frame<K> {
        self.viewport = viewport;
        self.scale = scale;
        self.now = now;
        if self.dirty_struct {
            self.rebuild_taffy();
        }
        if self.dirty_style {
            self.restyle();
        }
        if let Some(root) = self.taffy_root {
            let _ = self.taffy.compute_layout(
                root,
                taffy::prelude::Size {
                    width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                    height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                },
            );
        }
        self.generation += 1;
        let mut boxes = Vec::with_capacity(self.tree.len());
        let mut layout_by_node: HashMap<NodeId, (f32, f32, f32, f32)> =
            HashMap::with_capacity(self.tree.len());
        if self.root_key.is_some() {
            self.collect(self.tree.root(), &mut boxes, &mut layout_by_node);
        }
        let mut paint = crate::paint::DisplayList::default();
        crate::paint::build_display_list(
            &crate::paint::PaintCtx {
                tree: &self.tree,
                styles: &self.styles,
                layout: &layout_by_node,
                scroll: &self.scroll_offsets,
                env: &self.media,
                spans: &self.span_styles,
            },
            self.tree.root(),
            self.generation,
            &mut paint,
        );
        Frame {
            generation: self.generation,
            boxes,
            paint,
        }
    }

    fn rebuild_taffy(&mut self) {
        self.taffy = taffy::TaffyTree::new();
        self.taffy_node.clear();
        self.taffy_root = None;
        if self.root_key.is_some() {
            let root = self.tree.root();
            let tid = self.build_taffy_subtree(root);
            self.taffy_root = Some(tid);
        }
        self.dirty_struct = false;
        self.dirty_style = true; // 新树需重贴样式
    }

    fn build_taffy_subtree(&mut self, id: NodeId) -> taffy::NodeId {
        let children: Vec<NodeId> = self.tree.children(id).to_vec();
        let tid = if children.is_empty() {
            self.taffy
                .new_leaf(taffy::prelude::Style::default())
                .expect("taffy leaf creation")
        } else {
            let child_ids: Vec<taffy::NodeId> = children
                .iter()
                .map(|c| self.build_taffy_subtree(*c))
                .collect();
            self.taffy
                .new_with_children(taffy::prelude::Style::default(), &child_ids)
                .expect("taffy node creation")
        };
        self.taffy_node.insert(id, tid);
        tid
    }

    fn restyle(&mut self) {
        self.styles.clear();
        self.span_styles.clear();
        let root = self.tree.root();
        self.restyle_node(root, None);
        self.dirty_style = false;
    }

    /// 宿主推入字体数据（feature = "text"）；字体变化影响文本测量，
    /// 触发全树样式/布局失效。
    #[cfg(feature = "text")]
    pub fn add_font(&mut self, data: Vec<u8>) {
        self.text.add_font(data);
        self.dirty_style = true;
        self.dirty_struct = true;
    }

    fn restyle_node(&mut self, id: NodeId, parent_id: Option<NodeId>) {
        let parent_style = parent_id.and_then(|p| self.styles.get(&p).cloned());
        let cs = compute_node(
            &self.tree,
            id,
            &self.sheet,
            &self.media,
            parent_style.as_ref(),
        );
        // T5c：span 级联求解——以节点基样式为 parent，复用 compute_node（空临时树）
        let spans = self.tree.node(id).spans.clone();
        let text_len = self
            .tree
            .node(id)
            .text
            .as_ref()
            .map(|t| t.len() as u32)
            .unwrap_or(0);
        if spans.is_empty() {
            self.span_styles.remove(&id);
        } else {
            let mut resolved = Vec::with_capacity(spans.len());
            for sp in &spans {
                let mut tmp = crate::tree::StyleTree::new();
                let tmp_id = tmp.insert_child(
                    tmp.root(),
                    crate::tree::StyleNode {
                        declarations: sp.declarations.clone(),
                        ..Default::default()
                    },
                );
                let scs = compute_node(&tmp, tmp_id, &self.sheet, &self.media, Some(&cs));
                resolved.push((sp.range.0.min(text_len), sp.range.1.min(text_len), scs));
            }
            self.span_styles.insert(id, resolved);
        }
        // T5：未推送测量的文本叶 → 内置 parley 测量（无字体时宽高 0，不落表）
        #[cfg(feature = "text")]
        if !self.measures.contains_key(&id)
            && self
                .tree
                .node(id)
                .text
                .as_ref()
                .is_some_and(|t| !t.is_empty())
        {
            let text = self.tree.node(id).text.clone().unwrap_or_default();
            let owned = self.span_styles.get(&id).cloned().unwrap_or_default();
            let span_refs: Vec<(u32, u32, &ComputedStyle)> =
                owned.iter().map(|(a, b, s)| (*a, *b, s)).collect();
            // max_advance 接线属 T5c-2（taffy measure 回调后可传入可用宽）
            let (w, h) = self.text.measure_rich(&text, &cs, &span_refs, None);
            if w > 0.0 || h > 0.0 {
                self.measures.insert(id, (w, h));
            }
        }
        let mut ts = map_style(&cs, &self.media);
        if let Some((w, h)) = self.measures.get(&id) {
            ts.size = taffy::prelude::Size {
                width: taffy::prelude::Dimension::length(*w),
                height: taffy::prelude::Dimension::length(*h),
            };
        }
        if let Some(&tid) = self.taffy_node.get(&id) {
            let _ = self.taffy.set_style(tid, ts);
        }
        self.styles.insert(id, cs);
        let children: Vec<NodeId> = self.tree.children(id).to_vec();
        for c in children {
            self.restyle_node(c, Some(id));
        }
    }

    fn collect(
        &self,
        id: NodeId,
        out: &mut Vec<LayoutEntry<K>>,
        layout_by_node: &mut HashMap<NodeId, (f32, f32, f32, f32)>,
    ) {
        if let Some(&tid) = self.taffy_node.get(&id) {
            if let Ok(l) = self.taffy.layout(tid) {
                let box_rect = (l.location.x, l.location.y, l.size.width, l.size.height);
                layout_by_node.insert(id, box_rect);
                if let Some(&key) = self.node_to_key.get(&id) {
                    out.push(LayoutEntry {
                        key,
                        x: box_rect.0,
                        y: box_rect.1,
                        width: box_rect.2,
                        height: box_rect.3,
                    });
                }
            }
        }
        for c in self.tree.children(id) {
            self.collect(*c, out, layout_by_node);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::NodeState;

    #[test]
    fn rich_text_spans_reach_paint() {
        use crate::tree::TextSpan;
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.set_stylesheet("").is_clean());
        let span_decls =
            crate::css::decl::parse_inline_declarations("color: #ff0000; font-weight: 700").0;
        assert!(
            engine
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("span".into()),
                        text: Some("hello link world".into()),
                        spans: std::iter::once(TextSpan {
                            range: (6, 10),
                            declarations: span_decls,
                        })
                        .collect(),
                        ..Default::default()
                    },
                )
                .is_ok()
        );
        let frame = engine.frame((400.0, 100.0), 1.0, 0.0);
        let (text, spans) = frame
            .paint
            .ops
            .iter()
            .find_map(|op| match op {
                crate::paint::PaintOp::Text { text, spans, .. } => Some((text.as_str(), spans)),
                _ => None,
            })
            .expect("text op");
        assert_eq!(text, "hello link world");
        assert_eq!(spans.len(), 1);
        let s = &spans[0];
        assert_eq!((s.start, s.end), (6, 10));
        assert_eq!(s.color.components, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(s.font_weight, 700.0);
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    struct Key(u32);

    fn text_node(text: &str) -> StyleNode {
        StyleNode {
            name: Some("span".into()),
            text: Some(text.into()),
            ..Default::default()
        }
    }

    #[test]
    fn contract_errors() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert_eq!(
            engine.insert(None, Key(2), StyleNode::default()),
            Err(crate::error::ContractError::RootExists)
        );
        assert_eq!(
            engine.insert(Some(Key(9)), Key(2), StyleNode::default()),
            Err(crate::error::ContractError::UnknownNode)
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), StyleNode::default())
                .is_ok()
        );
        assert_eq!(
            engine.insert(Some(Key(1)), Key(2), StyleNode::default()),
            Err(crate::error::ContractError::DuplicateNode)
        );
        assert_eq!(
            engine.set_text(Key(7), None),
            Err(crate::error::ContractError::UnknownNode)
        );
    }

    #[test]
    fn flex_row_layout() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), StyleNode::default())
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), StyleNode::default())
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(1), "display: flex; flex-direction: row")
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(2), "width: 100px; height: 50px")
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(3), "width: 100px; height: 50px")
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.boxes.len(), 3);
        let b2 = frame.find(Key(2)).unwrap();
        let b3 = frame.find(Key(3)).unwrap();
        assert_eq!((b2.x, b2.width, b2.height), (0.0, 100.0, 50.0));
        assert_eq!((b3.x, b3.width), (100.0, 100.0));
        // 根为 flex 容器：高度收缩为内容 50px
        let root = frame.find(Key(1)).unwrap();
        assert_eq!(root.height, 50.0);
    }

    #[test]
    fn restyle_picks_up_new_stylesheet() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(
                    Some(Key(1)),
                    Key(2),
                    StyleNode {
                        name: Some("div".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        assert!(engine.set_stylesheet("div { width: 100px }").is_clean());
        assert!(
            engine
                .set_declarations(Key(1), "display: flex; flex-direction: column")
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().width, 100.0);
        let report = engine.set_stylesheet("div { width: 300px }");
        assert!(report.is_clean());
        assert_eq!(engine.epoch(), 2);
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().width, 300.0);
    }

    #[test]
    fn structural_rebuild_after_remove() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), StyleNode::default())
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), StyleNode::default())
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(1), "display: flex; flex-direction: row")
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(2), "width: 100px; height: 50px")
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(3), "width: 100px; height: 50px")
                .is_ok()
        );
        engine.frame((800.0, 600.0), 1.0, 0.0);
        assert!(engine.remove(Key(2)).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.boxes.len(), 2);
        assert!(frame.find(Key(2)).is_none());
        assert_eq!(frame.find(Key(3)).unwrap().x, 0.0);
        assert!(engine.remove(Key(2)).is_err());
    }

    #[test]
    fn set_children_reorders() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), StyleNode::default())
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), StyleNode::default())
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(1), "display: flex; flex-direction: row")
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(2), "width: 100px; height: 50px")
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(3), "width: 100px; height: 50px")
                .is_ok()
        );
        // 交换顺序
        assert!(engine.set_children(Key(1), &[Key(3), Key(2)]).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(3)).unwrap().x, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().x, 100.0);
        // 非子节点 → 契约错误
        assert_eq!(
            engine.set_children(Key(2), &[Key(1)]),
            Err(crate::error::ContractError::UnknownNode)
        );
    }

    #[test]
    fn leaf_measure_sizes_text() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), text_node("hello"))
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(1), "display: flex; flex-direction: column")
                .is_ok()
        );
        // 未推送测量：叶 0 高
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().height, 0.0);
        assert!(engine.set_leaf_measure(Key(2), 100.0, 20.0).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = frame.find(Key(2)).unwrap();
        assert_eq!((b.width, b.height), (100.0, 20.0));
    }

    #[test]
    fn pseudo_state_restyling() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(
                    Some(Key(1)),
                    Key(2),
                    StyleNode {
                        name: Some("button".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        assert!(
            engine
                .set_stylesheet(
                    "button { width: 50px; height: 20px } button:hover { height: 40px }"
                )
                .is_clean()
        );
        assert!(
            engine
                .set_declarations(Key(1), "display: flex; flex-direction: column")
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().height, 20.0);
        assert!(engine.set_state(Key(2), NodeState::HOVER).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().height, 40.0);
    }

    #[test]
    fn clear_via_root_removal() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), StyleNode::default())
                .is_ok()
        );
        assert!(engine.remove(Key(1)).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert!(frame.boxes.is_empty());
        // 根槽位已释放，可重新声明
        assert!(engine.insert(None, Key(5), StyleNode::default()).is_ok());
    }
}
