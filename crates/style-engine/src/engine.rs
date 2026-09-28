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
    /// 滚动容器量程（ADR-0007）：key → 各轴最大滚动量（px，非滚动轴为 0）。
    /// 引擎不夹紧偏移——量程供宿主 clamp 用（零副作用、幂等）。
    pub scrollable: HashMap<K, (f32, f32)>,
}

impl<K: Copy + PartialEq> Frame<K> {
    /// 按 key 查找布局盒。
    pub fn find(&self, key: K) -> Option<&LayoutEntry<K>> {
        self.boxes.iter().find(|b| b.key == key)
    }
}

/// 非文本叶固有尺寸区间（T5d）：((min_w, min_h), (max_w, max_h))。
pub(crate) type IntrinsicSize = ((f32, f32), (f32, f32));

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
    /// 文本叶父指针（T5c-2）：换行重测量需读包含块宽度。
    parents: HashMap<NodeId, NodeId>,
    /// 内置自动测量的文本叶集合（T5c-2）：仅这些节点参与换行重测量。
    #[cfg(feature = "text")]
    auto_text: std::collections::HashSet<NodeId>,
    /// 文本叶测量所用换行约束（T5c-2）：绘制与测量折行一致；缺席 = 无界。
    wrap_widths: HashMap<NodeId, Option<f32>>,
    /// 文本叶最小内容尺寸（T5d，shrink-to-fit 下限；restyle 期随自动测量产出）。
    #[cfg(feature = "text")]
    min_measures: HashMap<NodeId, (f32, f32)>,
    /// 宿主推送的非文本叶固有尺寸区间（T5d）：(min, max) 各轴。
    intrinsics: HashMap<NodeId, IntrinsicSize>,
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
            parents: HashMap::new(),
            #[cfg(feature = "text")]
            auto_text: std::collections::HashSet::new(),
            wrap_widths: HashMap::new(),
            #[cfg(feature = "text")]
            min_measures: HashMap::new(),
            intrinsics: HashMap::new(),
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
        // class 语义归一（CSS class 属性为空格分隔 token）
        let mut node = node;
        node.classes = normalize_classes(&node.classes);
        // span 区间契约校验（T5c）：UTF-8 字节边界内、有序、不越界；
        // 无文本节点的 span 一律非法（区间无处着落）。
        for span in &node.spans {
            let (start, end) = (span.range.0 as usize, span.range.1 as usize);
            let ok = node.text.as_ref().is_some_and(|t| {
                start <= end
                    && end <= t.len()
                    && t.is_char_boundary(start)
                    && t.is_char_boundary(end)
            });
            if !ok {
                return Err(crate::error::ContractError::InvalidSpan);
            }
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
            self.span_styles.clear();
            self.parents.clear();
            self.wrap_widths.clear();
            #[cfg(feature = "text")]
            self.min_measures.clear();
            self.intrinsics.clear();
            #[cfg(feature = "text")]
            self.auto_text.clear();
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
            self.span_styles.remove(&d);
            self.parents.remove(&d);
            self.wrap_widths.remove(&d);
            #[cfg(feature = "text")]
            self.min_measures.remove(&d);
            self.intrinsics.remove(&d);
            #[cfg(feature = "text")]
            self.auto_text.remove(&d);
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
        self.tree.node_mut(id).classes = normalize_classes(classes);
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
        // 宿主测量接管后不再参与自动换行重测量（T5c-2）
        #[cfg(feature = "text")]
        self.auto_text.remove(&id);
        self.dirty_style = true;
        Ok(())
    }

    /// 推送非文本叶固有尺寸区间（T5d，shrink-to-fit 第三 pass 消费）：
    /// (min_w, min_h) / (max_w, max_h) 各轴独立。definite 首选尺寸仍走
    /// [`StyleEngine::set_leaf_measure`]；absolute 叶按包含块宽夹紧到区间内。
    pub fn set_leaf_intrinsic(
        &mut self,
        key: K,
        min_w: f32,
        min_h: f32,
        max_w: f32,
        max_h: f32,
    ) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        self.intrinsics.insert(id, ((min_w, min_h), (max_w, max_h)));
        Ok(())
    }

    /// 节点是否 position: absolute（T5d：不按父流宽换行，走 shrink-to-fit）。
    fn is_absolute(&self, id: NodeId) -> bool {
        matches!(
            self.styles
                .get(&id)
                .and_then(|cs| cs.get(crate::css::property::PropertyId::Position)),
            Some(crate::css::property::DeclValue::Position(
                crate::css::property::Position::Absolute
            ))
        )
    }

    /// 节点是否定位元素（position != static，absolute 叶的包含块判定）。
    fn is_positioned(&self, id: NodeId) -> bool {
        matches!(
            self.styles
                .get(&id)
                .and_then(|cs| cs.get(crate::css::property::PropertyId::Position)),
            Some(crate::css::property::DeclValue::Position(
                crate::css::property::Position::Relative | crate::css::property::Position::Absolute
            ))
        )
    }

    /// absolute 叶的可用宽（T5d）：最近 positioned 祖先的内容宽（border-box
    /// − padding − 已生效 border，`used_h_inset`）；无 positioned 祖先 → 视口宽。
    fn abs_avail_width(&self, id: NodeId) -> Option<f32> {
        let mut cb = self.parents.get(&id).copied();
        let mut cb_id: Option<NodeId> = None;
        while let Some(p) = cb {
            if self.is_positioned(p) {
                cb_id = Some(p);
                break;
            }
            cb = self.parents.get(&p).copied();
        }
        match cb_id {
            Some(p) => {
                let tid = *self.taffy_node.get(&p)?;
                let pline = self.taffy.layout(tid).ok()?;
                let cbcs = self.styles.get(&p)?;
                let rctx = crate::css::value::ResolveCtx {
                    em: cbcs.font_size_px(),
                    rem: 16.0,
                    viewport_w: self.media.viewport_w,
                    viewport_h: self.media.viewport_h,
                };
                let inset: f32 = [
                    (crate::css::property::PropertyId::PaddingLeft, None),
                    (crate::css::property::PropertyId::PaddingRight, None),
                    (
                        crate::css::property::PropertyId::BorderLeftWidth,
                        Some(crate::css::property::PropertyId::BorderLeftStyle),
                    ),
                    (
                        crate::css::property::PropertyId::BorderRightWidth,
                        Some(crate::css::property::PropertyId::BorderRightStyle),
                    ),
                ]
                .iter()
                .filter_map(|(pid, style_pid)| used_h_inset(cbcs, *pid, *style_pid, &rctx))
                .sum();
                Some((pline.size.width - inset).max(0.0))
            }
            None => Some(self.viewport.0),
        }
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
        // T5c-2：文本叶换行——pass1 布局给出包含块宽后，white-space: normal 的
        // 自动测量文本叶按 max_advance 重测量并按需二次布局。包含块内容宽 =
        // 父 border-box − 父左右 padding − 已生效 border（style none 时宽归零）；
        // shrink-to-fit 父宽受无界文本影响的场景为残余偏差。
        #[cfg(feature = "text")]
        if let Some(root) = self.taffy_root {
            let mut remeasure: Vec<(NodeId, f32)> = Vec::new();
            for &id in self.measures.keys() {
                if !self.auto_text.contains(&id) {
                    continue;
                }
                // absolute 叶不按父流宽换行——shrink-to-fit 由第三 pass 夹紧（T5d）
                if self.is_absolute(id) {
                    continue;
                }
                let Some(cs) = self.styles.get(&id) else {
                    continue;
                };
                let wraps = matches!(
                    cs.get(crate::css::property::PropertyId::WhiteSpace),
                    None | Some(crate::css::property::DeclValue::WhiteSpace(
                        crate::css::property::WhiteSpace::Normal
                    ))
                );
                if !wraps {
                    continue;
                }
                let Some(&parent_id) = self.parents.get(&id) else {
                    continue;
                };
                let Some(&ptid) = self.taffy_node.get(&parent_id) else {
                    continue;
                };
                let (Ok(pline), Some(pcs)) = (self.taffy.layout(ptid), self.styles.get(&parent_id))
                else {
                    continue;
                };
                let pw = pline.size.width;
                let rctx = crate::css::value::ResolveCtx {
                    em: pcs.font_size_px(),
                    rem: 16.0,
                    viewport_w: self.media.viewport_w,
                    viewport_h: self.media.viewport_h,
                };
                let inset: f32 = [
                    (crate::css::property::PropertyId::PaddingLeft, None),
                    (crate::css::property::PropertyId::PaddingRight, None),
                    (
                        crate::css::property::PropertyId::BorderLeftWidth,
                        Some(crate::css::property::PropertyId::BorderLeftStyle),
                    ),
                    (
                        crate::css::property::PropertyId::BorderRightWidth,
                        Some(crate::css::property::PropertyId::BorderRightStyle),
                    ),
                ]
                .iter()
                .filter_map(|(pid, style_pid)| used_h_inset(pcs, *pid, *style_pid, &rctx))
                .sum();
                remeasure.push((id, (pw - inset).max(0.0)));
            }
            let mut reflow = false;
            for (id, avail) in remeasure {
                let Some(text) = self.tree.node(id).text.clone() else {
                    continue;
                };
                let Some(cs) = self.styles.get(&id).cloned() else {
                    continue;
                };
                let owned = self.span_styles.get(&id).cloned().unwrap_or_default();
                let span_refs: Vec<(u32, u32, &ComputedStyle)> =
                    owned.iter().map(|(a, b, s)| (*a, *b, s)).collect();
                let (w, h) =
                    self.text
                        .measure_rich(&text, &cs, &span_refs, Some(avail), &self.media);
                if let Some(old) = self.measures.get(&id) {
                    reflow |= (old.0 - w).abs() > f32::EPSILON || (old.1 - h).abs() > f32::EPSILON;
                }
                self.measures.insert(id, (w, h));
                self.wrap_widths.insert(id, Some(avail));
                if let Some(&tid) = self.taffy_node.get(&id) {
                    let mut ts = map_style(&cs, &self.media);
                    ts.size = taffy::prelude::Size {
                        width: taffy::prelude::Dimension::length(w),
                        height: taffy::prelude::Dimension::length(h),
                    };
                    let _ = self.taffy.set_style(tid, ts);
                }
            }
            // 第三 pass（T5d，shrink-to-fit，CSS 10.3.7）：absolute 叶按包含块
            // 可用宽夹紧——width = clamp(min_content, avail, max_content)。
            // cb = 最近 positioned 祖先（无 → 视口宽）；近似：可用宽未扣自身
            // margin/静态位置。文本叶 max_content 取 pass1 无界测量（wrap pass
            // 已跳过 absolute 叶，measures 仍是无界值）；LeafMeasure 叶用
            // set_leaf_intrinsic 区间。
            let mut shrink: Vec<(NodeId, f32)> = Vec::new();
            for &id in self.measures.keys() {
                if !self.is_absolute(id) || !self.tree.children(id).is_empty() {
                    continue;
                }
                let (min_w, max_w) = if self.auto_text.contains(&id) {
                    let Some(min) = self.min_measures.get(&id) else {
                        continue;
                    };
                    let Some(m) = self.measures.get(&id) else {
                        continue;
                    };
                    (min.0, m.0.max(min.0))
                } else if let Some(((mnw, _), (mxw, _))) = self.intrinsics.get(&id) {
                    (*mnw, *mxw)
                } else {
                    continue;
                };
                let Some(raw_avail) = self.abs_avail_width(id) else {
                    continue;
                };
                // CSS 10.3.7：夹紧对象是内容宽——content-box 语义下先扣自身
                // 水平 padding+border（border 仅 style 非 none 计入）
                let avail = match self.styles.get(&id) {
                    Some(cs) => {
                        let rctx = crate::css::value::ResolveCtx {
                            em: cs.font_size_px(),
                            rem: 16.0,
                            viewport_w: self.media.viewport_w,
                            viewport_h: self.media.viewport_h,
                        };
                        let inset: f32 = [
                            (crate::css::property::PropertyId::PaddingLeft, None),
                            (crate::css::property::PropertyId::PaddingRight, None),
                            (
                                crate::css::property::PropertyId::BorderLeftWidth,
                                Some(crate::css::property::PropertyId::BorderLeftStyle),
                            ),
                            (
                                crate::css::property::PropertyId::BorderRightWidth,
                                Some(crate::css::property::PropertyId::BorderRightStyle),
                            ),
                        ]
                        .iter()
                        .filter_map(|(pid, style_pid)| used_h_inset(cs, *pid, *style_pid, &rctx))
                        .sum();
                        (raw_avail - inset).max(0.0)
                    }
                    None => raw_avail,
                };
                let width = avail.min(max_w).max(min_w);
                shrink.push((id, width));
            }
            for (id, width) in shrink {
                if self.auto_text.contains(&id) {
                    let text = self.tree.node(id).text.clone().unwrap_or_default();
                    let Some(cs) = self.styles.get(&id).cloned() else {
                        continue;
                    };
                    let owned = self.span_styles.get(&id).cloned().unwrap_or_default();
                    let span_refs: Vec<(u32, u32, &ComputedStyle)> =
                        owned.iter().map(|(a, b, s)| (*a, *b, s)).collect();
                    let (w, h) =
                        self.text
                            .measure_rich(&text, &cs, &span_refs, Some(width), &self.media);
                    if let Some(old) = self.measures.get(&id) {
                        reflow |=
                            (old.0 - w).abs() > f32::EPSILON || (old.1 - h).abs() > f32::EPSILON;
                    }
                    self.measures.insert(id, (w, h));
                    self.wrap_widths.insert(id, Some(width));
                } else {
                    let h = self.measures.get(&id).map(|m| m.1).unwrap_or(0.0);
                    if let Some(old) = self.measures.get(&id) {
                        reflow |= (old.0 - width).abs() > f32::EPSILON;
                    }
                    self.measures.insert(id, (width, h));
                }
                if let Some(&tid) = self.taffy_node.get(&id) {
                    if let Some(cs) = self.styles.get(&id) {
                        let mut ts = map_style(cs, &self.media);
                        ts.size = taffy::prelude::Size {
                            width: taffy::prelude::Dimension::length(width),
                            height: taffy::prelude::Dimension::length(
                                self.measures.get(&id).map(|m| m.1).unwrap_or(0.0),
                            ),
                        };
                        let _ = self.taffy.set_style(tid, ts);
                    }
                }
            }
            if reflow {
                let _ = self.taffy.compute_layout(
                    root,
                    taffy::prelude::Size {
                        width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                        height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                    },
                );
            }
        }
        self.generation += 1;
        let mut boxes = Vec::with_capacity(self.tree.len());
        let mut layout_by_node: HashMap<NodeId, (f32, f32, f32, f32)> =
            HashMap::with_capacity(self.tree.len());
        if self.root_key.is_some() {
            self.collect(self.tree.root(), 0.0, 0.0, &mut boxes, &mut layout_by_node);
        }
        // ADR-0007：滚动容器可滚动外延（宿主推进偏移的量程）。识别 = overflow
        // ∈ {auto, scroll}（两轴独立）；外延 = padding box 与全部后代 border box
        // 并集相对 padding box 原点的最大超出。近似：绝对定位后代一并并入，
        // 随定位模型细化；偏移本身归宿主（set_scroll_offset），引擎不夹紧。
        let mut scrollable: HashMap<K, (f32, f32)> = HashMap::new();
        if self.root_key.is_some() {
            let border_px = |cs: &ComputedStyle,
                             w_id: crate::css::property::PropertyId,
                             s_id: crate::css::property::PropertyId|
             -> f32 {
                let painted = !matches!(
                    cs.get(s_id),
                    Some(crate::css::property::DeclValue::BorderStyle(
                        crate::css::property::BorderStyle::None
                    ))
                ) && cs.get(s_id).is_some();
                if !painted {
                    return 0.0;
                }
                match cs.get(w_id) {
                    Some(crate::css::property::DeclValue::BorderWidth(Some(lp))) => lp
                        .resolve(
                            &crate::css::value::ResolveCtx {
                                em: cs.font_size_px(),
                                rem: 16.0,
                                viewport_w: self.media.viewport_w,
                                viewport_h: self.media.viewport_h,
                            },
                            0.0,
                        )
                        .unwrap_or(0.0),
                    _ => 0.0,
                }
            };
            for (&key, &id) in &self.key_to_node {
                let Some(cs) = self.styles.get(&id) else {
                    continue;
                };
                let scrollable_axis = |pid: crate::css::property::PropertyId| {
                    // 解析层已把 auto 归一为 Scroll（引擎内语义等价，见 parse_overflow）
                    matches!(
                        cs.get(pid),
                        Some(crate::css::property::DeclValue::Overflow(
                            crate::css::property::Overflow::Scroll
                        ))
                    )
                };
                let ox = scrollable_axis(crate::css::property::PropertyId::OverflowX);
                let oy = scrollable_axis(crate::css::property::PropertyId::OverflowY);
                if !ox && !oy {
                    continue;
                }
                let Some(&(x, y, w, h)) = layout_by_node.get(&id) else {
                    continue;
                };
                use crate::css::property::PropertyId;
                let bl = border_px(cs, PropertyId::BorderLeftWidth, PropertyId::BorderLeftStyle);
                let bt = border_px(cs, PropertyId::BorderTopWidth, PropertyId::BorderTopStyle);
                let br = border_px(
                    cs,
                    PropertyId::BorderRightWidth,
                    PropertyId::BorderRightStyle,
                );
                let bb = border_px(
                    cs,
                    PropertyId::BorderBottomWidth,
                    PropertyId::BorderBottomStyle,
                );
                let (px0, py0) = (x + bl, y + bt);
                let (pw, ph) = ((w - bl - br).max(0.0), (h - bt - bb).max(0.0));
                let mut ex = px0 + pw;
                let mut ey = py0 + ph;
                let mut stack: Vec<NodeId> = self.tree.children(id).to_vec();
                while let Some(c) = stack.pop() {
                    if let Some(&(cx, cy, cw, ch)) = layout_by_node.get(&c) {
                        ex = ex.max(cx + cw);
                        ey = ey.max(cy + ch);
                    }
                    stack.extend(self.tree.children(c).iter().copied());
                }
                let max_x = if ox { (ex - px0 - pw).max(0.0) } else { 0.0 };
                let max_y = if oy { (ey - py0 - ph).max(0.0) } else { 0.0 };
                if max_x > 0.0 || max_y > 0.0 {
                    scrollable.insert(key, (max_x, max_y));
                }
            }
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
                wrap_widths: &self.wrap_widths,
            },
            self.tree.root(),
            self.generation,
            &mut paint,
        );
        Frame {
            generation: self.generation,
            boxes,
            paint,
            scrollable,
        }
    }

    fn rebuild_taffy(&mut self) {
        self.taffy = taffy::TaffyTree::new();
        self.taffy_node.clear();
        self.taffy_root = None;
        if self.root_key.is_some() {
            let root = self.tree.root();
            let tid = self.build_taffy_subtree(root);
            // 合成视口根（ICB）：树根成为其子，树根自身的 margin 得以生效
            //（taffy 不应用根节点 margin；CSS 中根盒 margin 相对初始包含块生效）。
            let viewport = self
                .taffy
                .new_leaf(taffy::prelude::Style {
                    display: taffy::prelude::Display::Block,
                    ..Default::default()
                })
                .expect("viewport root");
            self.taffy
                .set_children(viewport, &[tid])
                .expect("viewport root children");
            self.taffy_root = Some(viewport);
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
        self.parents.clear();
        self.wrap_widths.clear();
        #[cfg(feature = "text")]
        self.min_measures.clear();
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
        if let Some(p) = parent_id {
            self.parents.insert(id, p);
        }
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
            let (w, h) = self
                .text
                .measure_rich(&text, &cs, &span_refs, None, &self.media);
            if w > 0.0 || h > 0.0 {
                self.measures.insert(id, (w, h));
                let min = self
                    .text
                    .measure_min_content(&text, &cs, &span_refs, &self.media);
                self.min_measures.insert(id, min);
                self.auto_text.insert(id);
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

    /// 深度优先收集布局盒。taffy 的 `location` 是父相对坐标，故携带祖先累计偏移
    /// 合成视口绝对坐标（合成视口根引入后，树根不再是 taffy 根）。
    fn collect(
        &self,
        id: NodeId,
        ox: f32,
        oy: f32,
        out: &mut Vec<LayoutEntry<K>>,
        layout_by_node: &mut HashMap<NodeId, (f32, f32, f32, f32)>,
    ) {
        let mut x = ox;
        let mut y = oy;
        if let Some(&tid) = self.taffy_node.get(&id) {
            if let Ok(l) = self.taffy.layout(tid) {
                x = l.location.x + ox;
                y = l.location.y + oy;
                let box_rect = (x, y, l.size.width, l.size.height);
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
            self.collect(*c, x, y, out, layout_by_node);
        }
    }
}

/// class 语义归一（CSS class 属性为空格分隔 token）：把每个条目按 ASCII
/// 空白拆开并丢弃空项——宿主传 `"row alt"` 与 `["row", "alt"]` 等价。
fn normalize_classes(input: &[String]) -> smallvec::SmallVec<[String; 4]> {
    input
        .iter()
        .flat_map(|c| c.split_ascii_whitespace())
        .map(String::from)
        .collect()
}

/// 两阶段包含块内容宽的水平内缩项（T5c-2 收口）：padding 恒计；border 仅在
/// style 非 none 时计入（CSS used width：style none 时边框宽归零，初始
/// medium 不参与）。width 侧兼容 Len 与 BorderWidth 两种物化。
fn used_h_inset(
    cs: &ComputedStyle,
    width_id: crate::css::property::PropertyId,
    style_id: Option<crate::css::property::PropertyId>,
    rctx: &crate::css::value::ResolveCtx,
) -> Option<f32> {
    if let Some(sid) = style_id {
        let none = match cs.get(sid) {
            Some(crate::css::property::DeclValue::BorderStyle(s)) => {
                matches!(s, crate::css::property::BorderStyle::None)
            }
            _ => true,
        };
        if none {
            return None;
        }
    }
    match cs.get(width_id) {
        Some(crate::css::property::DeclValue::Len(lp)) => lp.resolve(rctx, 0.0),
        Some(crate::css::property::DeclValue::BorderWidth(Some(lp))) => lp.resolve(rctx, 0.0),
        _ => None,
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

    #[test]
    fn margin_and_block_flow() {
        // 树根 margin 生效（合成视口根）+ 无 margin 定宽块级子盒靠 inline-start（不被 auto-margin 居中）
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.card { width: 300px; height: 160px; margin: 40px; padding: 16px; } div.kid { width: 100px; height: 20px; }"
            )
            .is_clean());
        assert!(
            engine
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("div".into()),
                        classes: std::iter::once("card".into()).collect(),
                        ..Default::default()
                    },
                )
                .is_ok()
        );
        assert!(
            engine
                .insert(
                    Some(Key(1)),
                    Key(2),
                    StyleNode {
                        name: Some("div".into()),
                        classes: std::iter::once("kid".into()).collect(),
                        ..Default::default()
                    },
                )
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.boxes.len(), 2);
        assert_eq!(frame.boxes[0].x, 40.0);
        assert_eq!(frame.boxes[0].y, 40.0);
        // 子盒：padding 内 inline-start，而非 auto-margin 居中
        assert_eq!(frame.boxes[1].x, 56.0); // 40 + 16
    }

    /// 真实字体（demo 资产，同仓自由许可）：让测量/换行通路在测试中真实生效。
    const TEST_FONT: &[u8] = include_bytes!("../../style-engine-demo/assets/fonts/DejaVuSans.ttf");
    const TEST_FONT_CJK: &[u8] =
        include_bytes!("../../style-engine-demo/assets/fonts/NotoSansSC.ttf");

    #[test]
    fn wrap_avail_subtracts_padding_and_used_border() {
        // T5c-2 收口：包含块内容宽 = width − padding − 已生效 border
        //（border-style 为 none 时宽归零，初始 medium 不计入）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                // 泛族 sans-serif 在零副作用集合中无解析（ADR-0006），测试显式指到注册字体。
                // 父级 auto 宽（content-box 下 auto 与 box-sizing 无关）：
                // wrap avail = 根 border-box − 自身 padding − 有效 border。
                "div { font-family: \"DejaVu Sans\"; } div.p { padding: 10px; } div.b { padding: 10px; border: 5px solid black; }"
            )
            .is_clean());
        let node = |classes: &str, text: bool| StyleNode {
            name: Some("div".into()),
            classes: if classes.is_empty() {
                Default::default()
            } else {
                std::iter::once(classes.to_string()).collect()
            },
            text: if text {
                Some("the quick brown fox jumps over the lazy dog".into())
            } else {
                None
            },
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node("", false)).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), node("p", false))
                .is_ok()
        );
        assert!(engine.insert(Some(Key(2)), Key(3), node("", true)).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(4), node("b", false))
                .is_ok()
        );
        assert!(engine.insert(Some(Key(4)), Key(5), node("", true)).is_ok());
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let id3 = *engine.key_to_node.get(&Key(3)).unwrap();
        let id5 = *engine.key_to_node.get(&Key(5)).unwrap();
        assert_eq!(engine.wrap_widths.get(&id3), Some(&Some(780.0)));
        assert_eq!(engine.wrap_widths.get(&id5), Some(&Some(770.0)));
    }

    #[test]
    fn z_index_auto_not_materialized_as_number() {
        // z-index: auto → ZIndex(None)（不再物化为 Number(0)）；
        // 数字 → ZIndex(Some(n))，paint effective_z 仅认 Some。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.a { position: relative; z-index: auto; } div.n { position: relative; z-index: 5; }"
            )
            .is_clean());
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node("a")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), node("n")).is_ok());
        let _ = engine.frame((400.0, 100.0), 1.0, 0.0);
        let ida = *engine.key_to_node.get(&Key(1)).unwrap();
        let idn = *engine.key_to_node.get(&Key(2)).unwrap();
        assert_eq!(
            engine
                .styles
                .get(&ida)
                .and_then(|cs| cs.get(crate::css::property::PropertyId::ZIndex)),
            Some(&crate::css::property::DeclValue::ZIndex(None))
        );
        assert_eq!(
            engine
                .styles
                .get(&idn)
                .and_then(|cs| cs.get(crate::css::property::PropertyId::ZIndex)),
            Some(&crate::css::property::DeclValue::ZIndex(Some(5.0)))
        );
    }

    #[test]
    fn insert_rejects_invalid_span_ranges() {
        // span 区间契约：UTF-8 边界内、有序、不越界；无文本节点的 span 非法。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let span = |start: u32, end: u32| crate::tree::TextSpan {
            range: (start, end),
            declarations: crate::css::decl::parse_inline_declarations("color: #ff0000").0,
        };
        let mk = |spans: std::iter::Once<crate::tree::TextSpan>| StyleNode {
            name: Some("div".into()),
            text: Some("héllo".into()),
            spans: spans.collect(),
            ..Default::default()
        };
        // h(0) é(1..3) l(3) l(4) o(5) → len 6，边界 {0,1,3,4,5,6}
        assert!(
            engine
                .insert(None, Key(1), mk(std::iter::once(span(1, 3))))
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), mk(std::iter::once(span(2, 4))))
                .is_err()
        ); // 2 非字符边界
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), mk(std::iter::once(span(0, 10))))
                .is_err()
        ); // 越界
        assert!(
            engine
                .insert(Some(Key(1)), Key(4), mk(std::iter::once(span(4, 2))))
                .is_err()
        ); // 倒置
        let textless = StyleNode {
            name: Some("div".into()),
            spans: std::iter::once(span(0, 1)).collect(),
            ..Default::default()
        };
        assert!(engine.insert(Some(Key(1)), Key(5), textless).is_err()); // 无文本
    }

    #[test]
    fn scroll_bounds_reported_and_hidden_skipped() {
        // ADR-0007：overflow∈{auto,scroll} 上报量程（padding box + 后代并集）；
        // hidden 仅裁剪、不上报。content-box 默认：padbox = 220×120（含 padding），
        // 内容底 310 → 量程 310 − 120 = 190。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.list { width: 200px; height: 100px; overflow-y: scroll; padding: 10px; } div.hid { width: 200px; height: 100px; overflow-y: hidden; padding: 10px; } div.item { width: 200px; height: 150px; }"
            )
            .is_clean());
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: if classes.is_empty() {
                Default::default()
            } else {
                std::iter::once(classes.to_string()).collect()
            },
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node("")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), node("list")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), node("item")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(4), node("item")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(5), node("hid")).is_ok());
        assert!(engine.insert(Some(Key(5)), Key(6), node("item")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.scrollable.get(&Key(2)), Some(&(0.0, 190.0)));
        assert!(!frame.scrollable.contains_key(&Key(5)));
    }

    #[test]
    fn scroll_offset_translates_paint() {
        // 宿主偏移 → PushScroll/PopScroll 平移层（不改布局盒）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet("div.list { width: 200px; height: 100px; overflow-y: scroll; }")
                .is_clean()
        );
        let node = || StyleNode {
            name: Some("div".into()),
            classes: std::iter::once("list".to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node()).is_ok());
        assert!(engine.set_scroll_offset(Key(1), 0.0, 40.0).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert!(frame.paint.ops.iter().any(|op| matches!(
            op,
            crate::paint::PaintOp::PushScroll { dx, dy } if *dx == 0.0 && *dy == 40.0
        )));
        assert!(
            frame
                .paint
                .ops
                .iter()
                .any(|op| matches!(op, crate::paint::PaintOp::PopScroll))
        );
    }

    #[test]
    fn class_attribute_is_space_separated_tokens() {
        // class 属性按空格拆 token："row alt" 同时命中 .row 与 .alt。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.row { width: 200px; height: 40px; } div.alt { background-color: #164e63; }"
                )
                .is_clean()
        );
        let node = StyleNode {
            name: Some("div".into()),
            classes: std::iter::once("row alt".to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = frame.find(Key(1)).unwrap();
        assert_eq!((b.x, b.y, b.width, b.height), (0.0, 0.0, 200.0, 40.0));
    }

    #[test]
    fn min_content_is_widest_atom() {
        // T5d：min-content = 0 宽强制断行后的最宽行（最宽不可断原子）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(
            engine
                .set_stylesheet("div { font-family: \"DejaVu Sans\"; font-size: 16px; }")
                .is_clean()
        );
        assert!(
            engine
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("div".into()),
                        text: Some("hello world foo".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let id = *engine.key_to_node.get(&Key(1)).unwrap();
        let cs = engine.styles.get(&id).cloned().unwrap();
        let (min_w, _) = engine.min_measures.get(&id).copied().unwrap();
        let (max_w, _) = engine.measures.get(&id).copied().unwrap();
        assert!(max_w > min_w && min_w > 0.0);
        // 三个原子中最宽者（"world" 的 w 宽于 "hello"/"foo"，逐个对照）
        let mut widest = 0.0f32;
        for word in ["hello", "world", "foo"] {
            let (ww, _) = engine.text.measure(word, &cs, &MediaEnv::default());
            widest = widest.max(ww);
        }
        assert!((min_w - widest).abs() < 0.5);
    }

    #[test]
    fn absolute_text_shrinks_to_fit() {
        // T5d 第三 pass：absolute 文本叶 = clamp(min_content, cb 内容宽, max_content)。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div { font-family: \"DejaVu Sans\"; font-size: 16px; } div.host { position: relative; width: 300px; height: 200px; } div.tip { position: absolute; top: 10px; left: 20px; }"
            )
            .is_clean());
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, text: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: if parent.is_some() {
                        std::iter::once("tip".to_string()).collect()
                    } else {
                        std::iter::once("host".to_string()).collect()
                    },
                    text: Some(text.into()),
                    ..Default::default()
                },
            )
        };
        // 长文本：被 300−0(insets) 夹紧 → 宽 < 无界宽
        assert!(mk(&mut engine, Key(1), None, "host").is_ok());
        assert!(
            mk(
                &mut engine,
                Key(2),
                Some(Key(1)),
                "the quick brown fox jumps over the lazy dog again and again"
            )
            .is_ok()
        );
        // 短文本：max-content ≤ 可用宽 → 保持固有宽（不拉伸）
        assert!(mk(&mut engine, Key(3), Some(Key(1)), "short").is_ok());
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let id2 = *engine.key_to_node.get(&Key(2)).unwrap();
        let id3 = *engine.key_to_node.get(&Key(3)).unwrap();
        let (w2, _) = engine.measures.get(&id2).copied().unwrap();
        let (w3, _) = engine.measures.get(&id3).copied().unwrap();
        assert!(w2 <= 300.0 && w2 > 0.0);
        // 短文本未拉伸到可用宽（300），仍是自身 max-content
        let short_max = {
            let cs = engine.styles.get(&id3).cloned().unwrap();
            let (mw, _) = engine.text.measure("short", &cs, &MediaEnv::default());
            mw
        };
        assert!((w3 - short_max).abs() < 0.5);
        // wrap_widths 同步（绘制折行一致）
        assert_eq!(engine.wrap_widths.get(&id2), Some(&Some(w2)));
    }

    #[test]
    fn leaf_intrinsic_clamps_absolute() {
        // T5d：LeafMeasure 叶固有区间在 absolute 语境下夹紧宽度。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.host { position: relative; width: 300px; height: 200px; } div.wide-host { position: relative; width: 1000px; height: 200px; } div.chip { position: absolute; top: 0; left: 0; }"
            )
            .is_clean());
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: if class.is_empty() {
                        Default::default()
                    } else {
                        std::iter::once(class.to_string()).collect()
                    },
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "host").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(2)), "chip").is_ok());
        assert!(mk(&mut engine, Key(4), Some(Key(1)), "wide-host").is_ok());
        assert!(mk(&mut engine, Key(5), Some(Key(4)), "chip").is_ok());
        for k in [Key(3), Key(5)] {
            assert!(engine.set_leaf_measure(k, 500.0, 20.0).is_ok());
            assert!(
                engine
                    .set_leaf_intrinsic(k, 80.0, 20.0, 500.0, 20.0)
                    .is_ok()
            );
        }
        let frame = engine.frame((1200.0, 600.0), 1.0, 0.0);
        // 300 宽宿主：500 被夹到 300；1000 宽宿主：保持 500
        let b3 = frame.find(Key(3)).unwrap();
        assert_eq!(b3.width, 300.0);
        let b5 = frame.find(Key(5)).unwrap();
        assert_eq!(b5.width, 500.0);
    }

    #[test]
    fn scroll_offset_set_is_idempotent() {
        // ADR-0007 宿主集成边界（滚轮夹紧在宿主）：同值重设不改变任何
        // 状态与输出——偏移表幂等、布局盒不动（滚动只是绘制期平移）、
        // 量程不变。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.list { position: relative; width: 240px; height: 80px; overflow-y: scroll; } div.row { height: 40px; }"
            )
            .is_clean());
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: if classes.is_empty() {
                Default::default()
            } else {
                std::iter::once(classes.to_string()).collect()
            },
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node("")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), node("list")).is_ok());
        for k in [Key(3), Key(4), Key(5)] {
            assert!(engine.insert(Some(Key(2)), k, node("row")).is_ok());
        }
        let _ = engine.frame((400.0, 300.0), 1.0, 0.0);
        let id2 = *engine.key_to_node.get(&Key(2)).unwrap();
        assert!(engine.set_scroll_offset(Key(2), 0.0, 20.0).is_ok());
        let first = *engine.scroll_offsets.get(&id2).unwrap();
        assert!(engine.set_scroll_offset(Key(2), 0.0, 20.0).is_ok());
        assert_eq!(engine.scroll_offsets.get(&id2), Some(&first));
        let f1 = engine.frame((400.0, 300.0), 1.0, 0.0);
        let s1 = f1.scrollable.get(&Key(2)).copied();
        let b1 = f1.find(Key(3)).unwrap();
        let f2 = engine.frame((400.0, 300.0), 1.0, 0.0);
        assert_eq!(f2.scrollable.get(&Key(2)), s1.as_ref());
        let b2 = f2.find(Key(3)).unwrap();
        assert_eq!(
            (b1.x, b1.y, b1.width, b1.height),
            (b2.x, b2.y, b2.width, b2.height)
        );
    }

    #[test]
    fn cjk_min_content_is_single_char() {
        // T5d × CJK：CJK 的 min-content = 单字宽（UAX #14 表意文字逐字可断），
        // 与 Latin 的「最宽词」语义相对；max-content 仍为无界整行。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT_CJK.to_vec());
        assert!(
            engine
                .set_stylesheet(
                    "div { font-family: \"Noto Sans SC\"; font-size: 16px; white-space: normal; }"
                )
                .is_clean()
        );
        assert!(
            engine
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("div".into()),
                        text: Some("样式引擎渲染检查".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let id1 = *engine.key_to_node.get(&Key(1)).unwrap();
        let min = engine
            .min_measures
            .get(&id1)
            .copied()
            .expect("CJK 文本叶应有 min-content");
        let cs = engine.styles.get(&id1).cloned().unwrap();
        let (single, _) = engine.text.measure("样", &cs, &MediaEnv::default());
        assert!(
            (min.0 - single).abs() < 0.6,
            "min={} 单字宽={}",
            min.0,
            single
        );
        let (max, _) = engine
            .text
            .measure("样式引擎渲染检查", &cs, &MediaEnv::default());
        assert!(max > single + 1.0);
    }

    #[test]
    fn absolute_without_positioned_ancestor_uses_initial_cb() {
        // CSS 10.3.7：无 positioned 祖先 → 包含块 = 初始包含块（视口）。
        // LeafMeasure 2000 宽叶在 800 视口被夹到 800（≥ min 80）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet("div.leaf { position: absolute; top: 0; left: 0; }")
                .is_clean()
        );
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: if class.is_empty() {
                        Default::default()
                    } else {
                        std::iter::once(class.to_string()).collect()
                    },
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "leaf").is_ok());
        assert!(engine.set_leaf_measure(Key(2), 2000.0, 20.0).is_ok());
        assert!(
            engine
                .set_leaf_intrinsic(Key(2), 80.0, 20.0, 2000.0, 20.0)
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b2 = frame.find(Key(2)).unwrap();
        assert_eq!(b2.width, 800.0);
    }

    #[test]
    fn line_height_resolves_into_layout() {
        // 盘点修复回归：line-height 此前已解析入库但无任何消费者（无效声明）。
        // 绝对行高单行 = 盒高（parley Absolute 行盒语义）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.host { width: 300px; } div.t { font-family: \"DejaVu Sans\"; font-size: 16px; line-height: 40px; }"
            )
            .is_clean());
        let node = |classes: &str, text: Option<&str>| StyleNode {
            name: Some("div".into()),
            classes: if classes.is_empty() {
                Default::default()
            } else {
                std::iter::once(classes.to_string()).collect()
            },
            text: text.map(String::from),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node("host", None)).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), node("t", Some("Hello World")))
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b2 = frame.find(Key(2)).unwrap();
        assert!(
            (b2.height - 40.0).abs() < 0.6,
            "line-height: 40px 单行盒高应≈40，实际 {}",
            b2.height
        );
    }

    #[test]
    fn letter_spacing_widens_measures() {
        // 盘点修复回归：letter-spacing 此前完全无消费者（连访问器都没有）。
        // "Hello World Test" 16 字符 ≥2 字符间隙 → 字距 4px 至少拉开 2 间隙。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet("div { font-family: \"DejaVu Sans\"; font-size: 16px; } div.s { letter-spacing: 4px; }")
            .is_clean());
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: if classes.is_empty() {
                Default::default()
            } else {
                std::iter::once(classes.to_string()).collect()
            },
            text: Some("Hello World Test".into()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node("")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), node("s")).is_ok());
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let id1 = *engine.key_to_node.get(&Key(1)).unwrap();
        let id2 = *engine.key_to_node.get(&Key(2)).unwrap();
        let cs1 = engine.styles.get(&id1).cloned().unwrap();
        let cs2 = engine.styles.get(&id2).cloned().unwrap();
        let env = MediaEnv::default();
        let (w0, _) = engine.text.measure("Hello World Test", &cs1, &env);
        let (w4, _) = engine.text.measure("Hello World Test", &cs2, &env);
        assert!(
            w4 - w0 > 8.0,
            "letter-spacing: 4px 应显著加宽测量（Δ={}）",
            w4 - w0
        );
        // min-content 同步生效：最宽原子被拉开
        let (m0, _) = engine
            .text
            .measure_min_content("Hello World Test", &cs1, &[], &env);
        let (m4, _) = engine
            .text
            .measure_min_content("Hello World Test", &cs2, &[], &env);
        assert!(m4 > m0 + 4.0, "字距应作用于 min-content（Δ={}）", m4 - m0);
    }

    #[test]
    fn wrap_two_phase_frame_layout() {
        // T5c-2：两阶段帧通路（无字体时 remeasure 集为空，验证不回归）
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet("div.p { width: 200px; padding: 10px; }")
                .is_clean()
        );
        assert!(
            engine
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("div".into()),
                        classes: std::iter::once("p".into()).collect(),
                        ..Default::default()
                    },
                )
                .is_ok()
        );
        assert!(
            engine
                .insert(
                    Some(Key(1)),
                    Key(2),
                    StyleNode {
                        name: Some("div".into()),
                        text: Some("text".into()),
                        ..Default::default()
                    },
                )
                .is_ok()
        );
        let frame = engine.frame((400.0, 100.0), 1.0, 0.0);
        assert_eq!(frame.boxes.len(), 2);
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
