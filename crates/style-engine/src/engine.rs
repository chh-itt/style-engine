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

/// ③multi-column 稳态缓存：幻影列 taffy 节点与当前分配（settle_columns
/// 复用；n/colw/分配全等帧零额外布局 pass）。assignment[i] = 子节点 i
/// 所在列（树序）；usize::MAX = 重建后待分配。
#[derive(Default)]
struct MulticolState {
    phantoms: Vec<taffy::NodeId>,
    /// 三期⑤b：spanner 分段行包装（Flex Row 装 n 幻影列）——
    /// column-span:all 模式下每段一行；行模式为空。
    rows: Vec<taffy::NodeId>,
    /// 三期⑤b：spanner 分段模式（容器 Flex Column + 行包装）。
    span_mode: bool,
    /// 三期⑤b：容器孩子序签名（Seg(段序) | Spanner→usize::MAX）——
    /// 行数不变而 spanner 换位时仅重排容器孩子。
    seq_sig: Vec<usize>,
    n: usize,
    colw: f32,
    /// 每真实子件 (段序, 列序)；(usize::MAX, _) = 重建后待分配
    /// （spanner 不参与平衡，恒 MAX）。
    assignment: Vec<(usize, usize)>,
    /// 三期⑤a：断口 margin-top 截断的原值备份（taffy 层）——
    /// 不再列首 / 回退块流时恢复；restyle 时清空（map_style 全量重写
    /// 已把 margin 复位为样式真值）。
    truncated: HashMap<NodeId, taffy::prelude::LengthPercentageAuto>,
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
    /// ①calc 直通：延迟结算条目（restyle 重建，settle_calc 消费）。
    calc_deferred: Vec<crate::layout::DeferredCalc>,
    /// taffy 父链（结算基准 = 父内容尺寸；build_taffy_subtree 填充）。
    taffy_parent: HashMap<taffy::NodeId, taffy::NodeId>,
    /// 三期② absolute 锚定跳走：node → 重挂包含块（Some(cb)=cb≠直父；
    /// None=ICB）。settle_absolute_anchors 每帧重建，collect 消费。
    abs_cb: HashMap<NodeId, Option<NodeId>>,
    /// 三期②：taffy 结构偏离样式镜像的活跃标（重挂过 absolute 即置位；
    /// 全部归位后的下一帧清零——稳态零 absolute 页面跳过整段结构对比）。
    abs_structured: bool,
    /// ②table：display:table 节点注册表（restyle 收集，settle_tables 结算）。
    tables: Vec<NodeId>,
    /// ②table：上次结算列宽缓存（px；全等免重排——稳态帧零额外布局 pass）。
    table_cols: HashMap<NodeId, Vec<f32>>,
    /// 三期④：上次结算的单元格列位签名（(cell, 列起点0基, 列跨, 行跨)；
    /// 全等免重写）。
    table_cells: HashMap<NodeId, Vec<(NodeId, usize, usize, u32)>>,
    /// ③multi-column：多列容器注册表（restyle 收集，settle_columns 结算）。
    multicols: Vec<NodeId>,
    /// ③multi-column：稳态缓存（幻影列节点 + 当前分配；全等免重排）。
    multicol_state: HashMap<NodeId, MulticolState>,
    /// 三期⑤c：列规条带（相对容器 border-box 原点）——settle_column_rules
    /// 逐帧全量重建（几何随列平衡/文本换行漂移，无稳态签名可复用）。
    column_rules: HashMap<NodeId, Vec<crate::paint::ColumnRuleSeg>>,
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
    /// 背景图注册表（第五批⑨）：url() 引用 → 宿主预解码 RGBA。
    images: HashMap<String, crate::paint::ImageRes>,
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
            calc_deferred: Vec::new(),
            taffy_parent: HashMap::new(),
            abs_cb: HashMap::new(),
            abs_structured: false,
            tables: Vec::new(),
            table_cols: HashMap::new(),
            table_cells: HashMap::new(),
            multicols: Vec::new(),
            multicol_state: HashMap::new(),
            column_rules: HashMap::new(),
            styles: HashMap::new(),
            span_styles: HashMap::new(),
            parents: HashMap::new(),
            #[cfg(feature = "text")]
            auto_text: std::collections::HashSet::new(),
            wrap_widths: HashMap::new(),
            images: HashMap::new(),
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

    /// transform ≠ none 的元素成为 absolute/fixed 后代的包含块
    /// （ADR-0009 双时机之 L2：restyle 期谓词，cb walk 消费）。
    fn is_transform_cb(&self, id: NodeId) -> bool {
        self.styles.get(&id).is_some_and(|cs| cs.has_transform())
    }

    /// absolute 叶的可用宽（T5d）：最近 positioned 或 transformed 祖先的内容宽
    /// （border-box − padding − 已生效 border，`used_h_inset`）；均无 → 视口宽。
    fn abs_avail_width(&self, id: NodeId) -> Option<f32> {
        let mut cb = self.parents.get(&id).copied();
        let mut cb_id: Option<NodeId> = None;
        while let Some(p) = cb {
            if self.is_positioned(p) || self.is_transform_cb(p) {
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

    /// 三期② absolute 锚定跳走（A 级缺口收口）：CSS 中 absolute 子件的包含
    /// 块 = 最近 positioned 或 transform≠none 祖先（均无 → 初始包含块 ICB），
    /// 而 taffy 0.14 只按直父 padding box 锚定绝对子件——直父与 cb 之间存在
    /// static 包装层时 inset 百分比基准、静态位置与 auto 边距全部错位。
    /// 本 pass 在样式最终就绪（含 @keyframes 覆写——动画可翻转 has_transform）
    /// 后、布局前执行：用 set_children 移动语义把 absolute 子件重挂到 cb
    /// 节点（cb==直父的常规情形零变化；跳走后 taffy 的 inset 基准/静态位置
    /// 随 cb 正确）。table/multicol 子树维持 v1 契约（settle_columns：absolute
    /// 子件包含块仍为容器）不参与。collect() 按 abs_cb 推导视口坐标；
    /// 稳态帧 desired 与当前子列表全等免 set_children，无 absolute 且结构
    /// 已归位时整段跳过。
    fn settle_absolute_anchors(&mut self) {
        let Some(viewport) = self.taffy_root else {
            return;
        };
        // 1) DFS 求重挂映射：abs_cb[node] = Some(cb)（cb≠直父）| None（ICB）；
        //    abs_in[cb]（None=ICB）= 该 cb 名下需重挂的 absolute 子件。
        let mut abs_cb: HashMap<NodeId, Option<NodeId>> = HashMap::new();
        let mut abs_in: HashMap<Option<NodeId>, Vec<NodeId>> = HashMap::new();
        // 栈项：(节点, 样式父, 最近 cb 候选（None=尚无）, 豁免子树)
        let mut stack: Vec<(NodeId, Option<NodeId>, Option<NodeId>, bool)> =
            vec![(self.tree.root(), None, None, false)];
        while let Some((id, parent, cb, exempt)) = stack.pop() {
            let cb_next = if self.is_positioned(id) || self.is_transform_cb(id) {
                Some(id)
            } else {
                cb
            };
            // v1 契约豁免：table/multicol 容器及其子树——子件列表由
            // settle_tables/settle_columns 管理，此处不触碰。
            let exempt_next = exempt || self.tables.contains(&id) || self.multicols.contains(&id);
            if !exempt_next && parent.is_some() && self.is_absolute(id) && cb != parent {
                // 注意用继承的 cb（严格祖先候选），不得用 cb_next——
                // absolute 节点自身 positioned 会把自己选成自己的 cb。
                abs_cb.insert(id, cb);
                abs_in.entry(cb).or_default().push(id);
            }
            for c in self.tree.children(id) {
                stack.push((*c, Some(id), cb_next, exempt_next));
            }
        }
        self.abs_cb = abs_cb;
        if self.abs_cb.is_empty() && !self.abs_structured {
            return;
        }
        // 2) 结构归位：逐非豁免节点比较 desired taffy 子列表（样式子件剔除
        // 重挂走的 + 重挂进来的）与当前列表，有差才 set_children（move 语义
        // 顺带从旧父摘除——上帧重挂残留随之归位）。
        let mut moved = false;
        let mut stack: Vec<(NodeId, bool)> = vec![(self.tree.root(), false)];
        while let Some((id, exempt)) = stack.pop() {
            let exempt_next = exempt || self.tables.contains(&id) || self.multicols.contains(&id);
            for c in self.tree.children(id) {
                stack.push((*c, exempt_next));
            }
            if exempt_next {
                continue;
            }
            let Some(&tid) = self.taffy_node.get(&id) else {
                continue;
            };
            let mut desired: Vec<taffy::NodeId> = self
                .tree
                .children(id)
                .iter()
                .filter(|c| !self.abs_cb.contains_key(*c))
                .filter_map(|c| self.taffy_node.get(c).copied())
                .collect();
            if let Some(list) = abs_in.get(&Some(id)) {
                desired.extend(list.iter().filter_map(|n| self.taffy_node.get(n).copied()));
            }
            if self.taffy.children(tid).unwrap_or_default() != desired {
                let _ = self.taffy.set_children(tid, &desired);
                moved = true;
            }
            // taffy_parent 同步（collect 幻影补偿共用该表）：重挂子件指 cb，
            // 恢复直父锚定的子件回写直父，防陈旧项误触发补偿。
            for c in self.tree.children(id) {
                let Some(&ctid) = self.taffy_node.get(c) else {
                    continue;
                };
                let expected = match self.abs_cb.get(c) {
                    Some(Some(cb)) => self.taffy_node.get(cb).copied(),
                    Some(None) => Some(viewport),
                    None => Some(tid),
                };
                if let Some(e) = expected {
                    if self.taffy_parent.get(&ctid) != Some(&e) {
                        self.taffy_parent.insert(ctid, e);
                    }
                }
            }
        }
        // 3) ICB：无 positioned/transformed 祖先的 absolute 直接挂合成视口根
        //（无 border/padding → location 相对视口原点；inset 百分比基准 =
        // 视口尺寸，与 CSS ICB 语义一致）。
        let mut vp_desired: Vec<taffy::NodeId> = self
            .taffy_node
            .get(&self.tree.root())
            .copied()
            .into_iter()
            .collect();
        if let Some(list) = abs_in.get(&None) {
            vp_desired.extend(list.iter().filter_map(|n| self.taffy_node.get(n).copied()));
        }
        if self.taffy.children(viewport).unwrap_or_default() != vp_desired {
            let _ = self.taffy.set_children(viewport, &vp_desired);
            moved = true;
        }
        self.abs_structured = moved || !self.abs_cb.is_empty();
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
        // 第五批⑰：@keyframes 动画采样——每帧级联后、布局前覆写（布局与
        // 绘制消费动画值；布局每帧全量重算，动画值变更无需独立失效标）
        self.apply_animations();
        // 三期② absolute 锚定跳走：样式最终就绪（动画可翻转 has_transform
        // → cb 集合逐帧变化）后、布局前，把 absolute 子件重挂到 CSS 包含块
        //（taffy 0.14 只按直父 padding box 锚定绝对子件）。
        self.settle_absolute_anchors();
        if let Some(root) = self.taffy_root {
            let _ = self.taffy.compute_layout(
                root,
                taffy::prelude::Size {
                    width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                    height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                },
            );
            // ①calc 直通：百分比 calc 结算（父尺寸就绪后回写固定值，收敛
            // 上限 3 遍；须在文本换行重排之前——文本折行宽度依赖结算值）。
            self.settle_calc(viewport);
            // ②table：行级 Grid 列模板结算（表内容宽就绪后回写；同样须在
            // 文本换行重排之前——单元格内折行宽依赖列宽）。
            self.settle_tables(viewport);
            // ③multi-column：幻影列创建/列宽/平衡分配结算（同样须在文本
            // 换行重排之前——列内折行约束 = 列宽）。
            self.settle_columns(viewport);
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
                let Some(&ctid) = self.taffy_node.get(&id) else {
                    continue;
                };
                // ③multi-column：子节点重挂幻影列后，换行包含块 = 幻影列
                //（无 padding/border，内缩为 0）。
                let rehomed = self.taffy_parent.get(&ctid).is_some_and(|&p| p != ptid);
                let (pw, inset) = if rehomed {
                    let Some(&effp) = self.taffy_parent.get(&ctid) else {
                        continue;
                    };
                    let Ok(pl) = self.taffy.layout(effp) else {
                        continue;
                    };
                    (pl.size.width, 0.0)
                } else {
                    let (Ok(pline), Some(pcs)) =
                        (self.taffy.layout(ptid), self.styles.get(&parent_id))
                    else {
                        continue;
                    };
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
                    (pline.size.width, inset)
                };
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
                    // ①calc 直通：捕获本节点延迟 calc 并挂接 taffy 节点。
                    for raw in crate::layout::take_calc_deferred() {
                        self.calc_deferred
                            .push(crate::layout::DeferredCalc { node: tid, raw });
                    }
                    // 第五批⑥：同 restyle_node 契约——声明宽优先，否则
                    // taffy auto（块流拉伸到容器内容宽）；测量高兜底。
                    ts.size = taffy::prelude::Size {
                        width: if has_declared_len(&cs, crate::css::property::PropertyId::Width) {
                            ts.size.width
                        } else {
                            taffy::prelude::Dimension::auto()
                        },
                        height: if has_declared_len(&cs, crate::css::property::PropertyId::Height) {
                            ts.size.height
                        } else {
                            taffy::prelude::Dimension::length(h)
                        },
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
                    // 换行宽：声明宽优先（CSS 10.3.7 声明宽即内容可用宽，
                    // 文本按声明宽折行——曾一律用 shrink 夹紧宽，声明 140
                    // 被无界测量 233 盖过而漏折行，bidi-mixed key4 高超差
                    // 根因）。percent 基准取夹紧宽（v1 近似）。
                    let rctx = crate::css::value::ResolveCtx {
                        em: cs.font_size_px(),
                        rem: 16.0,
                        viewport_w: self.media.viewport_w,
                        viewport_h: self.media.viewport_h,
                    };
                    let wrap = match cs.get(crate::css::property::PropertyId::Width) {
                        Some(crate::css::property::DeclValue::LenAuto(Some(lp))) => {
                            lp.resolve(&rctx, width)
                        }
                        _ => None,
                    }
                    .map(|d| d.max(0.0))
                    .unwrap_or(width);
                    let owned = self.span_styles.get(&id).cloned().unwrap_or_default();
                    let span_refs: Vec<(u32, u32, &ComputedStyle)> =
                        owned.iter().map(|(a, b, s)| (*a, *b, s)).collect();
                    let (w, h) =
                        self.text
                            .measure_rich(&text, &cs, &span_refs, Some(wrap), &self.media);
                    if let Some(old) = self.measures.get(&id) {
                        reflow |=
                            (old.0 - w).abs() > f32::EPSILON || (old.1 - h).abs() > f32::EPSILON;
                    }
                    self.measures.insert(id, (w, h));
                    self.wrap_widths.insert(id, Some(wrap));
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
                        // ①calc 直通：捕获本节点延迟 calc 并挂接 taffy 节点。
                        for raw in crate::layout::take_calc_deferred() {
                            self.calc_deferred
                                .push(crate::layout::DeferredCalc { node: tid, raw });
                        }
                        // 第五批⑥：absolute shrink-to-fit（CSS 10.3.7）仅适
                        // 用 width:auto——声明宽优先保留（含 min/max 夹紧由
                        // taffy 消费）；测量高兜底 height:auto。
                        ts.size = taffy::prelude::Size {
                            width: if has_declared_len(cs, crate::css::property::PropertyId::Width)
                            {
                                ts.size.width
                            } else {
                                taffy::prelude::Dimension::length(width)
                            },
                            height: if has_declared_len(
                                cs,
                                crate::css::property::PropertyId::Height,
                            ) {
                                ts.size.height
                            } else {
                                taffy::prelude::Dimension::length(
                                    self.measures.get(&id).map(|m| m.1).unwrap_or(0.0),
                                )
                            },
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
        // ③multi-column：文本重排后列内高度可能变化——二次平衡（稳态
        // 零成本：n/colw/分配全等即返回）。
        self.settle_columns(viewport);
        self.generation += 1;
        let mut boxes = Vec::with_capacity(self.tree.len());
        let mut layout_by_node: HashMap<NodeId, (f32, f32, f32, f32)> =
            HashMap::with_capacity(self.tree.len());
        if self.root_key.is_some() {
            self.collect(self.tree.root(), 0.0, 0.0, &mut boxes, &mut layout_by_node);
        }
        // 三期⑤c：列规几何结算（collect 后 layout_by_node 最新鲜；
        // DisplayList 构建前完成，PaintCtx 直引同一张表）。
        self.settle_column_rules(&layout_by_node);
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
                images: &self.images,
                column_rules: &self.column_rules,
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
        self.calc_deferred.clear();
        self.taffy_parent.clear();
        self.abs_cb.clear();
        self.abs_structured = false;
        self.tables.clear();
        self.table_cols.clear();
        self.table_cells.clear();
        self.multicols.clear();
        self.multicol_state.clear();
        if self.root_key.is_some() {
            let root = self.tree.root();
            let tid = self.build_taffy_subtree(root, None);
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
            // ①calc 直通：树根的结算基准 = 视口根内容尺寸。
            self.taffy_parent.insert(tid, viewport);
            self.taffy_root = Some(viewport);
        }
        self.dirty_struct = false;
        self.dirty_style = true; // 新树需重贴样式
    }

    fn build_taffy_subtree(
        &mut self,
        id: NodeId,
        parent_tid: Option<taffy::NodeId>,
    ) -> taffy::NodeId {
        let children: Vec<NodeId> = self.tree.children(id).to_vec();
        let mut child_ids: Vec<taffy::NodeId> = Vec::new();
        let tid = if children.is_empty() {
            self.taffy
                .new_leaf(taffy::prelude::Style::default())
                .expect("taffy leaf creation")
        } else {
            child_ids = children
                .iter()
                .map(|c| self.build_taffy_subtree(*c, None))
                .collect();
            self.taffy
                .new_with_children(taffy::prelude::Style::default(), &child_ids)
                .expect("taffy node creation")
        };
        self.taffy_node.insert(id, tid);
        // ①calc 直通：父链（后序回填——子 tid 已知，自身 tid 现创建）。
        if let Some(p) = parent_tid {
            self.taffy_parent.insert(tid, p);
        }
        for ct in &child_ids {
            self.taffy_parent.insert(*ct, tid);
        }
        tid
    }

    /// @keyframes 动画采样（第五批⑰）：对声明了 animation-name 且命中
    /// @keyframes 的节点，按 now（宿主帧推进，秒）采样关键帧轨道并覆写
    /// 计算样式。动画层高于作者级联（CSS：动画覆盖普通声明，仅
    /// !important 更高——分层为残余偏差）；缓动按关键帧段施加（CSS 时序
    /// 函数语义）；不可插值对按离散规则（段进度<0.5 取前帧）。
    fn apply_animations(&mut self) {
        use crate::css::property::{AnimDirection, AnimFillMode, DeclValue, PropertyId};
        use std::collections::BTreeMap;
        let dark = self.media.dark;
        let now = self.now as f32;
        let sheet = &self.sheet;
        let styles = &mut self.styles;
        for cs in styles.values_mut() {
            let name = match cs.get(PropertyId::AnimationName) {
                Some(DeclValue::AnimationName(Some(n))) => n.clone(),
                _ => continue,
            };
            let Some(rule) = sheet.keyframes.iter().find(|r| r.name == name) else {
                continue;
            };
            let duration = match cs.get(PropertyId::AnimationDuration) {
                Some(DeclValue::AnimationTime(s)) => *s,
                _ => 0.0,
            };
            if duration <= 0.0 || rule.frames.is_empty() {
                continue;
            }
            let delay = match cs.get(PropertyId::AnimationDelay) {
                Some(DeclValue::AnimationTime(s)) => *s,
                _ => 0.0,
            };
            let iterations = match cs.get(PropertyId::AnimationIterationCount) {
                Some(DeclValue::AnimationIteration(n)) => *n,
                _ => 1.0,
            };
            let timing = match cs.get(PropertyId::AnimationTimingFunction) {
                Some(DeclValue::AnimationTiming(t)) => *t,
                _ => crate::css::property::TimingFn::Ease,
            };
            let direction = match cs.get(PropertyId::AnimationDirection) {
                Some(DeclValue::AnimationDirection(d)) => *d,
                _ => crate::css::property::AnimDirection::Normal,
            };
            let fill = match cs.get(PropertyId::AnimationFillMode) {
                Some(DeclValue::AnimationFillMode(f)) => *f,
                _ => crate::css::property::AnimFillMode::None,
            };
            let local = now - delay;
            // 采样点：未开始（backwards/both → 0）/进行中/已结束
            // （forwards/both → 1）；其余阶段用底层值（不覆写）
            let total = duration * iterations.max(0.0);
            let p_eff: Option<f32> = if local < 0.0 {
                matches!(fill, AnimFillMode::Backwards | AnimFillMode::Both).then_some(0.0)
            } else if iterations.is_infinite() || local < total {
                let raw = local / duration;
                let cycle_index = raw.floor();
                let seg = raw - cycle_index;
                // 方向折叠（缓动在段内施加）
                let folded = match direction {
                    AnimDirection::Normal => seg,
                    AnimDirection::Reverse => 1.0 - seg,
                    AnimDirection::Alternate => {
                        if (cycle_index as i64) % 2 == 0 {
                            seg
                        } else {
                            1.0 - seg
                        }
                    }
                    AnimDirection::AlternateReverse => {
                        if (cycle_index as i64) % 2 == 0 {
                            1.0 - seg
                        } else {
                            seg
                        }
                    }
                };
                Some(folded)
            } else {
                matches!(fill, AnimFillMode::Forwards | AnimFillMode::Both).then_some(1.0)
            };
            let Some(p_eff) = p_eff else { continue };
            // 轨道收集（Parsed 声明；var() 载体不入轨——MVP 偏差）
            let mut tracks: BTreeMap<PropertyId, Vec<(f32, &DeclValue)>> = BTreeMap::new();
            for f in &rule.frames {
                for d in &f.declarations.decls {
                    if let crate::css::decl::DeclSource::Parsed(v) = &d.value {
                        tracks.entry(d.id).or_default().push((f.offset, v));
                    }
                }
            }
            for (pid, mut track) in tracks {
                track.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                // p < 首帧 offset 或 > 末帧 offset → 合成帧取 underlying
                //（CSS：缺 0%/100% 关键帧时以底层值补帧）→ 不覆写
                if p_eff < track[0].0 || p_eff > track[track.len() - 1].0 {
                    continue;
                }
                let sampled = track
                    .iter()
                    .find(|(o, _)| *o == p_eff)
                    .map(|(_, v)| (*v).clone())
                    .or_else(|| {
                        // 区间括位插值；不可插值 → 离散（段进度<0.5 取前帧）
                        let mut seg_pair: Option<(&f32, &DeclValue, &f32, &DeclValue)> = None;
                        for w in track.windows(2) {
                            if w[0].0 <= p_eff && p_eff <= w[1].0 {
                                seg_pair = Some((&w[0].0, w[0].1, &w[1].0, w[1].1));
                                break;
                            }
                        }
                        let (o0, v0, o1, v1) = seg_pair?;
                        let local_t = if o1 > o0 {
                            (p_eff - o0) / (o1 - o0)
                        } else {
                            0.0
                        };
                        let eased = timing.sample(local_t);
                        crate::css::property::lerp_decl(v0, v1, eased, dark)
                            .or_else(|| Some(if eased < 0.5 { v0.clone() } else { v1.clone() }))
                    });
                if let Some(v) = sampled {
                    cs.set_value(pid, v);
                }
            }
        }
    }

    /// ①calc 直通：百分比 calc 结算循环（设计决策见 layout.rs DeferredRaw
    /// 注）。首遍布局后，以父节点已布局内容尺寸为基准解析延迟 calc，
    /// 回写固定值并重算；循环至无变更（上限 3 遍——百分比基准恒为祖先
    /// 派生（DAG），逐遍稳定一层；3 层内链路与浏览器单遍语义一致，
    /// 更深链路记偏差待重估）。
    fn settle_calc(&mut self, viewport: (f32, f32)) {
        use crate::css::value::ResolveCtx;
        const MAX_SETTLE_PASSES: usize = 3;
        for _ in 0..MAX_SETTLE_PASSES {
            let mut updates: Vec<(taffy::NodeId, crate::layout::CalcAxis, f32)> = Vec::new();
            for d in &self.calc_deferred {
                let Some(&parent) = self.taffy_parent.get(&d.node) else {
                    continue;
                };
                let Ok(pl) = self.taffy.layout(parent) else {
                    continue;
                };
                let cw = pl.size.width
                    - pl.border.left
                    - pl.border.right
                    - pl.padding.left
                    - pl.padding.right;
                let ch = pl.size.height
                    - pl.border.top
                    - pl.border.bottom
                    - pl.padding.top
                    - pl.padding.bottom;
                // 三期③槽位扩展：flex-basis 百分比基=父容器主轴内容尺寸
                // （flex-direction 行向=宽、列向=高）；margin/padding 全族
                // 按 CSS 2.1 §8.3/§8.4 恒以包含块宽度为基（含 top/bottom）。
                let basis = match d.raw.axis {
                    crate::layout::CalcAxis::FlexBasis => {
                        let dir = self
                            .taffy
                            .style(parent)
                            .map(|s| s.flex_direction)
                            .unwrap_or(taffy::prelude::FlexDirection::Row);
                        let vertical = matches!(
                            dir,
                            taffy::prelude::FlexDirection::Column
                                | taffy::prelude::FlexDirection::ColumnReverse
                        );
                        if vertical { ch } else { cw }
                    }
                    axis => match axis.basis_axis() {
                        Some(crate::layout::CalcAxis::Width) => cw,
                        _ => ch,
                    },
                };
                let ctx = ResolveCtx {
                    em: d.raw.em,
                    rem: d.raw.rem,
                    viewport_w: d.raw.vw,
                    viewport_h: d.raw.vh,
                };
                let Some(px) = d.raw.expr.resolve(&ctx, basis) else {
                    continue;
                };
                let Some(style) = self.taffy.style(d.node).ok() else {
                    continue;
                };
                let mut target = style.clone();
                d.raw.axis.write(&mut target, px);
                if target != *style {
                    updates.push((d.node, d.raw.axis, px));
                }
            }
            if updates.is_empty() {
                return;
            }
            // 重复条目幂等：同值 set_style 二次应用无副作用（首次应用后
            // changed 检查即拦截），无需去重。
            for (node, slot, px) in updates {
                let Ok(mut style) = self.taffy.style(node).cloned() else {
                    continue;
                };
                slot.write(&mut style, px);
                let _ = self.taffy.set_style(node, style);
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
        }
    }

    /// 三期④：节点 display 快查（缺样式视为 None）。
    fn display_of(&self, id: NodeId) -> Option<crate::css::property::Display> {
        self.styles.get(&id).map(|cs| cs.display())
    }

    /// ②table：列模板结算——首遍布局给出表内容宽后，按首行单元格声明宽
    /// （定宽 px / 百分比 / auto）计算列模板回写各 table-row 的单行 Grid；
    /// 全等缓存则免重排（稳态帧零额外布局 pass）。嵌套表外层先行：变更
    /// 触发一次重排后二次迭代（上限 2 遍，内层表宽度取结算后值）。
    /// 三期④：行发现穿透行组；单元格图（CSS 2.1 §17.2.11.1 简化版）按
    /// colspan 属性分配显式列位；span-n 声明宽度均分给跨内未声明列。
    fn settle_tables(&mut self, viewport: (f32, f32)) {
        if self.tables.is_empty() {
            return;
        }
        for _ in 0..2 {
            let mut changed = false;
            let tables = self.tables.clone();
            for table in tables {
                let Some(&ttid) = self.taffy_node.get(&table) else {
                    continue;
                };
                let Ok(tl) = self.taffy.layout(ttid) else {
                    continue;
                };
                // 表内容宽 = 边框盒 − border − padding（百分比列基准）。
                let tw = (tl.size.width
                    - tl.border.left
                    - tl.border.right
                    - tl.padding.left
                    - tl.padding.right)
                    .max(0.0);
                // 三期④：行发现穿透行组（row-group/header/footer 组为透明
                // 包装）；caption 与杂件跳过（匿名盒修补另批）。
                let mut rows: Vec<NodeId> = Vec::new();
                for &c in self.tree.children(table) {
                    match self.display_of(c) {
                        Some(crate::css::property::Display::TableRow) => rows.push(c),
                        Some(crate::css::property::Display::TableRowGroup) => {
                            for &r in self.tree.children(c) {
                                if self.display_of(r)
                                    == Some(crate::css::property::Display::TableRow)
                                {
                                    rows.push(r);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                // 三期④：单元格图（CSS 2.1 §17.2.11.1 简化版）——逐行游标
                // 分配列位，colspan/rowspan 取属性（StyleNode.attrs，缺省 1），
                // 列数 = 各行跨数和的最大值；④d 行内非单元格元素按匿名单元
                // 格入图（见下方过滤注释）。
                // ④c rowspan：occupancy 集合记录被跨单元格占据的 (行,列)，
                // 后续行游标先跳过占据位（Chromium 语义——跨行单元不挤走
                // 后行单元格，列照常向后开辟）。
                let mut placements: Vec<(NodeId, usize, usize, u32)> = Vec::new();
                let mut occupied: std::collections::BTreeSet<(usize, usize)> = Default::default();
                let mut n_cols = 0usize;
                let mut first_row_len = 0usize;
                for (ri, r) in rows.iter().enumerate() {
                    let mut cursor = 0usize;
                    for &c in self.tree.children(*r) {
                        // ④d 匿名盒（CSS 2.1 §17.2.1 第 3 条简化）：行内非
                        // 单元格元素（框架常见直写 <div>）→ 匿名单元格——
                        // 直接按单元格入图（列位/宽度拉伸与 td 一致，匿名
                        // 包装盒省略、元素自身承担单元格几何）。display:none
                        // 不入图；absolute 出流不作为单元格。
                        if self.display_of(c) == Some(crate::css::property::Display::None) {
                            continue;
                        }
                        if self.styles.get(&c).is_some_and(|cs| {
                            cs.position() == crate::css::property::Position::Absolute
                        }) {
                            continue;
                        }
                        let attrs = &self.tree.node(c).attrs;
                        let span = attrs
                            .get("colspan")
                            .and_then(|v| v.parse::<u32>().ok())
                            .unwrap_or(1)
                            .clamp(1, 1000) as usize;
                        let rspan = attrs
                            .get("rowspan")
                            .and_then(|v| v.parse::<u32>().ok())
                            .unwrap_or(1)
                            .clamp(1, 1000);
                        while occupied.contains(&(ri, cursor)) {
                            cursor += 1;
                        }
                        placements.push((c, cursor, span, rspan));
                        for rr in ri..ri + rspan as usize {
                            for cc in cursor..cursor + span {
                                occupied.insert((rr, cc));
                            }
                        }
                        cursor += span;
                    }
                    n_cols = n_cols.max(cursor);
                    if ri == 0 {
                        first_row_len = placements.len();
                    }
                }
                // 列声明仍取首行单元格；span-n 声明把宽度分给跨内未声明列
                // （等分近似——Chromium 按 min/max-content 分配，简单表一致）。
                // Length 声明 = px+内缩（content-box），Percent = 表宽基不追加。
                let mut declared: Vec<Option<(taffy::prelude::Dimension, f32)>> =
                    vec![None; n_cols];
                let mut claims: Vec<(usize, usize, taffy::prelude::Dimension, f32)> = Vec::new();
                for (cell, start, span, _) in placements[..first_row_len].iter() {
                    let Some(cs) = self.styles.get(cell) else {
                        continue;
                    };
                    let d = map_style(cs, &self.media).size.width;
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
                    if *span == 1 {
                        declared[*start] = Some((d, inset));
                    } else {
                        claims.push((*start, *span, d, inset));
                    }
                }
                for (start, span, d, inset) in &claims {
                    let spanned = *start..(*start + *span);
                    let autos: Vec<usize> =
                        spanned.clone().filter(|i| declared[*i].is_none()).collect();
                    if autos.is_empty() {
                        continue;
                    }
                    let target = if let Some(px) = d.into_option() {
                        px + inset
                    } else if d.is_auto() {
                        continue;
                    } else {
                        d.value() * tw
                    };
                    let fixed: f32 = spanned
                        .filter_map(|i| declared[i])
                        .map(|(dd, ii)| match dd.into_option() {
                            Some(p) => p + ii,
                            None if dd.is_auto() => 0.0,
                            None => dd.value() * tw,
                        })
                        .sum();
                    let share = ((target - fixed) / autos.len() as f32).max(0.0);
                    for i in autos {
                        declared[i] = Some((taffy::prelude::Dimension::length(share), 0.0));
                    }
                }
                let declared: Vec<(taffy::prelude::Dimension, f32)> = declared
                    .into_iter()
                    .map(|d| d.unwrap_or((taffy::prelude::Dimension::auto(), 0.0)))
                    .collect();
                let cols = crate::layout::table_column_template(tw, &declared, n_cols);
                let cols_unchanged = self
                    .table_cols
                    .get(&table)
                    .is_some_and(|prev| *prev == cols);
                let cells_unchanged = self.table_cells.get(&table) == Some(&placements);
                if cols_unchanged && cells_unchanged {
                    continue;
                }
                if !cols_unchanged {
                    let template: Vec<_> = cols
                        .iter()
                        .map(|&px| {
                            taffy::style::GridTemplateComponent::Single(
                                taffy::style_helpers::length(px),
                            )
                        })
                        .collect();
                    for r in &rows {
                        let Some(&rtid) = self.taffy_node.get(r) else {
                            continue;
                        };
                        let Ok(mut rs) = self.taffy.style(rtid).cloned() else {
                            continue;
                        };
                        rs.grid_template_columns = template.clone();
                        // ④c：行高 definite 时同时钉死行轨道——否则跨行单元
                        // 的高度覆写作为 auto 轨道的 min-content 贡献会把整
                        // 条轨道（及同轨其他单元格）撑高；Chromium 语义是跨
                        // 行内容不改显式行高、只向下溢出。
                        if let Some(row_h) = self
                            .styles
                            .get(r)
                            .and_then(|cs| map_style(cs, &self.media).size.height.into_option())
                        {
                            rs.grid_template_rows =
                                vec![taffy::style::GridTemplateComponent::Single(
                                    taffy::style_helpers::length(row_h),
                                )];
                        }
                        let _ = self.taffy.set_style(rtid, rs);
                    }
                    self.table_cols.insert(table, cols);
                }
                // 三期④：单元格显式列位（1 基网格线起点 + 跨数），行位钉在
                // 第 1 行——洞（rowspan/杂件）不再吸附后续单元格。列模板全等
                // 但单元格图变化（如 colspan 属性变更）时仍需回写列位。
                // ④c rowspan：跨行单元留在宿主行网格内（列位照旧），高度
                // 覆写 = 宿主行 + 下方被跨行的 definite 高度和 → 向下溢出
                // 覆盖后续行（行高 definite 时与 Chromium 一致；被跨行 auto
                // 或宿主行 auto 时放弃覆写、按内容高——v1 记偏差，Chromium
                // 会把跨行内容分摊进被跨行高度）。行跨超出末行按 CSS 截断。
                if !cells_unchanged {
                    for (cell, start, span, rspan) in &placements {
                        let Some(&ctid) = self.taffy_node.get(cell) else {
                            continue;
                        };
                        let Ok(mut cst) = self.taffy.style(ctid).cloned() else {
                            continue;
                        };
                        cst.grid_column = taffy::geometry::Line {
                            start: taffy::style::GridPlacement::Line(((*start + 1) as i16).into()),
                            end: taffy::style::GridPlacement::Span(*span as u16),
                        };
                        cst.grid_row = taffy::geometry::Line {
                            start: taffy::style::GridPlacement::Line(1i16.into()),
                            end: taffy::style::GridPlacement::Span(1),
                        };
                        if *rspan > 1 {
                            // 宿主行 = 单元格直接父行（placements 即按行枚举产生）。
                            let ri = rows
                                .iter()
                                .position(|r| self.tree.parent(*cell) == Some(*r))
                                .unwrap_or(0);
                            let end = (ri + *rspan as usize).min(rows.len());
                            let heights: Vec<Option<f32>> = rows[ri..end]
                                .iter()
                                .map(|r| {
                                    self.styles.get(r).map(|cs| {
                                        map_style(cs, &self.media).size.height.into_option()
                                    })
                                })
                                .map(|o| o.flatten())
                                .collect();
                            if heights.iter().all(|h| h.is_some()) {
                                let total: f32 = heights.iter().map(|h| h.unwrap_or(0.0)).sum();
                                if total > 0.0 {
                                    cst.size.height = taffy::prelude::Dimension::length(total);
                                }
                            }
                        }
                        let _ = self.taffy.set_style(ctid, cst);
                    }
                    self.table_cells.insert(table, placements.clone());
                }
                changed = true;
            }
            if !changed {
                return;
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
        }
    }

    /// ③multi-column 结算（settle_calc/settle_tables 同模式的布局期结算）：
    /// pass1 布局给出容器内容宽后解析列数（count 显式；width 模式
    /// n = max(1, ⌊(内容宽+gap)/(理想宽+gap)⌋)）与列宽 colw=(cw−gap·(n−1))/n；
    /// 创建/复用 taffy 幻影列节点（无样式节点、Frame 不报告）并 set_children
    /// 跨父重挂真实子节点（taffy 0.14 支持移动语义）；按子节点 margin-box
    /// 单元高做平衡分配（CSS column-fill:balance 近似——理想高 = 总量/列数
    /// 的贪心填充，空列可承接不可断高子件）；文本重排后二次调用以最终高度
    /// 复衡。稳态（n/colw/分配全等）零额外布局 pass。v1 边界：断口 margin-top
    /// 不截断、column-span/rule 不实现、absolute 子件包含块仍为容器。
    fn settle_columns(&mut self, viewport: (f32, f32)) {
        if self.multicols.is_empty() {
            return;
        }
        let containers = self.multicols.clone();
        for mc in containers {
            let Some(&ctid) = self.taffy_node.get(&mc) else {
                continue;
            };
            let Ok(cl) = self.taffy.layout(ctid) else {
                continue;
            };
            // 容器内容宽 = 边框盒 − border − padding（列宽与 gap 基准）。
            let cw = (cl.size.width
                - cl.border.left
                - cl.border.right
                - cl.padding.left
                - cl.padding.right)
                .max(0.0);
            let Some(cs) = self.styles.get(&mc).cloned() else {
                continue;
            };
            let rctx = crate::css::value::ResolveCtx {
                em: cs.font_size_px(),
                rem: 16.0,
                viewport_w: self.media.viewport_w,
                viewport_h: self.media.viewport_h,
            };
            // gap：声明值优先；缺席 → multicol 语义 normal = 1em。
            let gap = match cs
                .get(crate::css::property::PropertyId::ColumnGap)
                .or_else(|| cs.get(crate::css::property::PropertyId::Gap))
            {
                Some(crate::css::property::DeclValue::Len(lp)) => {
                    lp.resolve(&rctx, 0.0).unwrap_or(0.0)
                }
                _ => cs.font_size_px(),
            };
            let count = match cs.get(crate::css::property::PropertyId::ColumnCount) {
                Some(crate::css::property::DeclValue::ColumnCount(c)) => *c,
                _ => None,
            };
            let width_px = match cs.get(crate::css::property::PropertyId::ColumnWidth) {
                Some(crate::css::property::DeclValue::LenAuto(Some(lp))) => lp.resolve(&rctx, 0.0),
                _ => None,
            };
            let n: usize = match (count, width_px) {
                (Some(c), _) => (c as usize).max(1),
                (None, Some(w)) if w > 0.0 => (((cw + gap) / (w + gap)).floor() as usize).max(1),
                _ => 1,
            };
            if n <= 1 {
                // width 模式回退单列：容器还原块流（map_style 按多列请求
                // 映射 Flex，此处覆写）；如曾有幻影结构则子节点归位容器。
                let mut changed = false;
                if let Some(prev) = self.multicol_state.remove(&mc) {
                    // 三期⑤a：回退块流前恢复全部断口 margin-top 截断
                    // （幻影拆除，子节点回到容器块流）；⑤b 拆净段行。
                    for (&id, &orig) in &prev.truncated {
                        if let Some(&tid) = self.taffy_node.get(&id) {
                            if let Ok(mut ts) = self.taffy.style(tid).cloned() {
                                ts.margin.top = orig;
                                let _ = self.taffy.set_style(tid, ts);
                            }
                        }
                    }
                    for r in &prev.rows {
                        let _ = self.taffy.set_children(*r, &[]);
                    }
                    for p in &prev.phantoms {
                        let _ = self.taffy.set_children(*p, &[]);
                    }
                    // 树序重挂全部真实子件（含 spanner——行序不含它）。
                    let ids: Vec<taffy::NodeId> = self
                        .tree
                        .children(mc)
                        .iter()
                        .filter_map(|c| self.taffy_node.get(c).copied())
                        .collect();
                    let _ = self.taffy.set_children(ctid, &ids);
                    for t in &ids {
                        self.taffy_parent.insert(*t, ctid);
                    }
                    changed = true;
                }
                if let Ok(ts) = self.taffy.style(ctid).cloned() {
                    if ts.display == taffy::prelude::Display::Flex {
                        let mut ms = crate::layout::map_style(&cs, &self.media);
                        ms.display = taffy::prelude::Display::Block;
                        let _ = self.taffy.set_style(ctid, ms);
                        changed = true;
                    }
                }
                if changed {
                    if let Some(root) = self.taffy_root {
                        let _ = self.taffy.compute_layout(
                            root,
                            taffy::prelude::Size {
                                width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                                height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                            },
                        );
                    }
                }
                continue;
            }
            let colw = ((cw - gap * (n as f32 - 1.0)) / n as f32).max(0.0);
            let children: Vec<NodeId> = self.tree.children(mc).to_vec();
            // 三期⑤b：column-span:all 子件切断列流——spanner 前后各成段，
            // 每段独立二分平衡。spanner 模式容器 = Flex Column + 零 gap，
            // 每非空段一行包装（Flex Row 装 n 幻影列），spanner 为容器直
            // 系全宽块；段内列首、spanner 后新段首列的列首均按断口截断
            // margin-top（spanner 强制断行 = 截断边）。
            let is_spanner = |c: &NodeId| -> bool {
                matches!(
                    self.styles.get(c),
                    Some(ccs) if matches!(
                        ccs.get(crate::css::property::PropertyId::ColumnSpan),
                        Some(crate::css::property::DeclValue::ColumnSpan(Some(true)))
                    )
                )
            };
            let mut plans: Vec<Vec<NodeId>> = vec![Vec::new()];
            for &c in &children {
                if is_spanner(&c) {
                    plans.push(Vec::new());
                } else {
                    let i = plans.len() - 1;
                    plans[i].push(c);
                }
            }
            let seq_sig: Vec<usize> = {
                let mut sig = Vec::new();
                let mut seen = vec![false; plans.len()];
                for &c in &children {
                    if is_spanner(&c) {
                        sig.push(usize::MAX);
                    } else {
                        let i = plans
                            .iter()
                            .position(|p| p.contains(&c))
                            .unwrap_or(plans.len() - 1);
                        if !seen[i] {
                            sig.push(i);
                            seen[i] = true;
                        }
                    }
                }
                sig
            };
            let mut st = self.multicol_state.remove(&mc).unwrap_or_default();
            let mut structure_changed = false;
            let has_span = seq_sig.contains(&usize::MAX);
            if st.span_mode != has_span {
                // 模式切换：拆净旧行/幻影（真实子件随重挂归位）。
                for r in &st.rows {
                    let _ = self.taffy.set_children(*r, &[]);
                }
                for p in &st.phantoms {
                    let _ = self.taffy.set_children(*p, &[]);
                }
                st.rows.clear();
                st.phantoms.clear();
                st.assignment = vec![(usize::MAX, usize::MAX); children.len()];
                st.span_mode = has_span;
                structure_changed = true;
            }
            if has_span {
                // 容器样式每帧重断言——restyle 会以 map_style 重写回
                // Row + gap（多列请求的默认映射）。
                let mut ms = crate::layout::map_style(&cs, &self.media);
                ms.display = taffy::prelude::Display::Flex;
                ms.flex_direction = taffy::prelude::FlexDirection::Column;
                ms.gap = taffy::prelude::Size {
                    width: taffy::prelude::LengthPercentage::length(0.0),
                    height: taffy::prelude::LengthPercentage::length(0.0),
                };
                let _ = self.taffy.set_style(ctid, ms);
            } else if structure_changed {
                // 切回行模式：容器还原 map_style 的 Flex Row + gap。
                let _ = self
                    .taffy
                    .set_style(ctid, crate::layout::map_style(&cs, &self.media));
            }
            // 行与幻影：数量不符 → 重建节点并临时均分（单元高与分配无关，
            // 仅供测量）；仅列宽变化 → 原节点回写宽度。
            let need_rows = if has_span { plans.len() } else { 0 };
            let need_phantoms = if has_span { plans.len() * n } else { n };
            if st.rows.len() != need_rows || st.phantoms.len() != need_phantoms {
                for r in &st.rows {
                    let _ = self.taffy.set_children(*r, &[]);
                }
                for p in &st.phantoms {
                    let _ = self.taffy.set_children(*p, &[]);
                }
                st.rows.clear();
                st.phantoms.clear();
                if has_span {
                    for plan in &plans {
                        let row = self
                            .taffy
                            .new_leaf(taffy::prelude::Style {
                                display: taffy::prelude::Display::Flex,
                                flex_direction: taffy::prelude::FlexDirection::Row,
                                size: taffy::prelude::Size {
                                    width: taffy::prelude::Dimension::percent(1.0),
                                    height: taffy::prelude::Dimension::auto(),
                                },
                                gap: taffy::prelude::Size {
                                    width: taffy::prelude::LengthPercentage::length(gap),
                                    height: taffy::prelude::LengthPercentage::length(0.0),
                                },
                                ..Default::default()
                            })
                            .expect("multicol segment row");
                        let phantoms: Vec<taffy::NodeId> = (0..n)
                            .map(|_| {
                                self.taffy
                                    .new_leaf(taffy::prelude::Style {
                                        display: taffy::prelude::Display::Block,
                                        size: taffy::prelude::Size {
                                            width: taffy::prelude::Dimension::length(colw),
                                            height: taffy::prelude::Dimension::auto(),
                                        },
                                        ..Default::default()
                                    })
                                    .expect("multicol phantom column")
                            })
                            .collect();
                        let _ = self.taffy.set_children(row, &phantoms);
                        for &p in &phantoms {
                            self.taffy_parent.insert(p, row);
                        }
                        // 临时均分（测量基座；分配随二分结果重挂）。
                        for (j, &p) in phantoms.iter().enumerate() {
                            let lo = plan.len() * j / n;
                            let hi = plan.len() * (j + 1) / n;
                            let ids: Vec<taffy::NodeId> = plan[lo..hi]
                                .iter()
                                .filter_map(|c| self.taffy_node.get(c).copied())
                                .collect();
                            let _ = self.taffy.set_children(p, &ids);
                            for c in plan[lo..hi].iter().filter_map(|c| self.taffy_node.get(c)) {
                                self.taffy_parent.insert(*c, p);
                            }
                        }
                        st.rows.push(row);
                        st.phantoms.extend(phantoms);
                    }
                    // 容器子序 = 段行 + spanner（树序）。
                    let seq_nodes: Vec<taffy::NodeId> = {
                        let mut v = Vec::new();
                        let mut seen = vec![false; plans.len()];
                        for &c in &children {
                            if is_spanner(&c) {
                                if let Some(&t) = self.taffy_node.get(&c) {
                                    v.push(t);
                                }
                            } else if let Some(i) = plans.iter().position(|p| p.contains(&c)) {
                                if !seen[i] {
                                    seen[i] = true;
                                    v.push(st.rows[i]);
                                }
                            }
                        }
                        v
                    };
                    let _ = self.taffy.set_children(ctid, &seq_nodes);
                    for t in &seq_nodes {
                        self.taffy_parent.insert(*t, ctid);
                    }
                    st.seq_sig = seq_sig;
                } else {
                    let phantoms: Vec<taffy::NodeId> = (0..n)
                        .map(|_| {
                            self.taffy
                                .new_leaf(taffy::prelude::Style {
                                    display: taffy::prelude::Display::Block,
                                    size: taffy::prelude::Size {
                                        width: taffy::prelude::Dimension::length(colw),
                                        height: taffy::prelude::Dimension::auto(),
                                    },
                                    ..Default::default()
                                })
                                .expect("multicol phantom column")
                        })
                        .collect();
                    let _ = self.taffy.set_children(ctid, &phantoms);
                    for (i, &p) in phantoms.iter().enumerate() {
                        self.taffy_parent.insert(p, ctid);
                        let lo = children.len() * i / n;
                        let hi = children.len() * (i + 1) / n;
                        let ids: Vec<taffy::NodeId> = children[lo..hi]
                            .iter()
                            .filter_map(|c| self.taffy_node.get(c).copied())
                            .collect();
                        let _ = self.taffy.set_children(p, &ids);
                        for c in children[lo..hi]
                            .iter()
                            .filter_map(|c| self.taffy_node.get(c))
                        {
                            self.taffy_parent.insert(*c, p);
                        }
                    }
                    st.phantoms = phantoms;
                    st.seq_sig = vec![0];
                }
                st.assignment = vec![(usize::MAX, usize::MAX); children.len()];
                structure_changed = true;
            } else if st.colw != colw {
                for &p in &st.phantoms {
                    if let Ok(mut ps) = self.taffy.style(p).cloned() {
                        ps.size.width = taffy::prelude::Dimension::length(colw);
                        let _ = self.taffy.set_style(p, ps);
                    }
                }
                structure_changed = true;
            } else if st.seq_sig != seq_sig {
                // 行数不变而 spanner 换位：仅重排容器孩子（复用行）。
                if has_span {
                    let seq_nodes: Vec<taffy::NodeId> = {
                        let mut v = Vec::new();
                        let mut seen = vec![false; plans.len()];
                        for &c in &children {
                            if is_spanner(&c) {
                                if let Some(&t) = self.taffy_node.get(&c) {
                                    v.push(t);
                                }
                            } else if let Some(i) = plans.iter().position(|p| p.contains(&c)) {
                                if !seen[i] {
                                    seen[i] = true;
                                    v.push(st.rows[i]);
                                }
                            }
                        }
                        v
                    };
                    let _ = self.taffy.set_children(ctid, &seq_nodes);
                    for t in &seq_nodes {
                        self.taffy_parent.insert(*t, ctid);
                    }
                }
                st.seq_sig = seq_sig;
                structure_changed = true;
            }
            if structure_changed {
                if let Some(root) = self.taffy_root {
                    let _ = self.taffy.compute_layout(
                        root,
                        taffy::prelude::Size {
                            width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                            height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                        },
                    );
                }
            }
            // 平衡分配（三期⑤a 二分 + ⑤b 分段）：每段独立试高 h 的顺序装
            // 箱 fit——列首块 margin-top 截断（断口语义，css-multicol §7）、
            // 非空列放不下换列、末列溢出失败、空列承接不可断高块；fit 单调
            // → 浮点二分 48 次收窄最小可行列高，终值重装箱定分配。
            let idx_of: HashMap<NodeId, usize> =
                children.iter().enumerate().map(|(i, c)| (*c, i)).collect();
            let mut assignment: Vec<(usize, usize)> =
                vec![(usize::MAX, usize::MAX); children.len()];
            for (si, plan) in plans.iter().enumerate() {
                if plan.is_empty() {
                    continue;
                }
                let mut units: Vec<(f32, f32, f32)> = Vec::with_capacity(plan.len());
                for c in plan {
                    let h = self
                        .taffy_node
                        .get(c)
                        .and_then(|t| self.taffy.layout(*t).ok())
                        .map(|l| l.size.height)
                        .unwrap_or(0.0);
                    let (mt, mb) = match self.styles.get(c) {
                        Some(ccs) => {
                            let rctx = crate::css::value::ResolveCtx {
                                em: ccs.font_size_px(),
                                rem: 16.0,
                                viewport_w: self.media.viewport_w,
                                viewport_h: self.media.viewport_h,
                            };
                            let m = ccs.margin();
                            let res = |lp: Option<&crate::css::value::LengthPercentage>| {
                                lp.and_then(|v| v.resolve(&rctx, 0.0)).unwrap_or(0.0)
                            };
                            (res(m[0]), res(m[3]))
                        }
                        None => (0.0, 0.0),
                    };
                    units.push((mt, h, mb));
                }
                let total: f32 = units.iter().map(|u| u.0 + u.1 + u.2).sum();
                let fit = |h: f32| -> Option<Vec<usize>> {
                    let mut assign = Vec::with_capacity(plan.len());
                    let (mut col, mut acc) = (0usize, 0.0f32);
                    for &(mt, uh, mb) in &units {
                        let mut eff = mt + uh + mb;
                        // 列非空（acc>0）且未到末列时才允许溢出换列；空列可
                        // 承接不可断高子件（首件无条件入列）。
                        if acc > 0.0 && acc + eff > h + f32::EPSILON {
                            if col == n - 1 {
                                return None;
                            }
                            col += 1;
                            acc = 0.0;
                            // 断口 margin-top 截断：换列后首件不带 mt 计量。
                            eff = uh + mb;
                        }
                        assign.push(col);
                        acc += eff;
                    }
                    Some(assign)
                };
                let (mut lo, mut hi) = (0.0f32, total);
                for _ in 0..48 {
                    let mid = (lo + hi) * 0.5;
                    if fit(mid).is_some() {
                        hi = mid;
                    } else {
                        lo = mid;
                    }
                }
                let cols = fit(hi + 1.0e-3).or_else(|| fit(total)).unwrap_or_default();
                for (c, col) in plan.iter().zip(cols) {
                    if let Some(&i) = idx_of.get(c) {
                        assignment[i] = (si, col);
                    }
                }
            }
            // 断口 margin-top 截断（三期⑤a + ⑤b 段内列）：段内非首列的
            // 列首件写 margin-top=0（taffy 层），不再是列首的恢复原值。
            // 段首（含 spanner 后新段首列）不截——spanner 边界是强制断行，
            // css-break-3 规定强断边距保留（Chromium 153 实证 y 保留 mt）。
            // 计量恒用样式原值（ccs.margin()），分配跨帧稳定幂等；文本重
            // 排 pass 会以样式重写 margin → 每次调用无条件重算截断 diff。
            let mut trunc_changed = false;
            // 段内列首件（列 0 不算）；entry().or_insert() 保树序第一件——
            // HashMap::collect 是后写覆盖（曾拿到列尾件）。
            let mut col_first: std::collections::HashMap<(usize, usize), NodeId> =
                std::collections::HashMap::new();
            for (c, a) in children.iter().zip(assignment.iter()) {
                if *a != (usize::MAX, usize::MAX) && a.1 > 0 {
                    col_first.entry(*a).or_insert(*c);
                }
            }
            for id in st.truncated.keys().copied().collect::<Vec<_>>() {
                // 仅当该件仍是某段非首列的列首才保留截断；挪回列 0 或换
                // 位都恢复。
                let keep = children.iter().zip(assignment.iter()).any(|(c, a)| {
                    c == &id
                        && *a != (usize::MAX, usize::MAX)
                        && a.1 > 0
                        && col_first.get(a).copied() == Some(id)
                });
                if keep {
                    continue;
                }
                let orig = st.truncated.remove(&id);
                if let (Some(&tid), Some(orig)) = (self.taffy_node.get(&id), orig) {
                    if let Ok(mut ts) = self.taffy.style(tid).cloned() {
                        ts.margin.top = orig;
                        let _ = self.taffy.set_style(tid, ts);
                    }
                }
                trunc_changed = true;
            }
            for (c, a) in children.iter().zip(assignment.iter()) {
                if *a == (usize::MAX, usize::MAX) || a.1 == 0 {
                    continue;
                }
                let isfirst = col_first.get(a).copied() == Some(*c);
                if !isfirst {
                    continue;
                }
                let Some(&tid) = self.taffy_node.get(c) else {
                    continue;
                };
                let Ok(mut ts) = self.taffy.style(tid).cloned() else {
                    continue;
                };
                // taffy 0.14 LengthPercentageAuto 无公开字段——零值用
                // TaffyZero::ZERO 常量比对。已为零（含重排 pass 后重截）
                // 只登记占位；被重排 pass 重写回样式值的重新截断。
                let zero =
                    <taffy::prelude::LengthPercentageAuto as taffy::prelude::TaffyZero>::ZERO;
                if ts.margin.top == zero {
                    st.truncated.entry(*c).or_insert(zero);
                    continue;
                }
                st.truncated.insert(*c, ts.margin.top);
                ts.margin.top = zero;
                let _ = self.taffy.set_style(tid, ts);
                trunc_changed = true;
            }
            if trunc_changed || st.assignment != assignment {
                for (i, &p) in st.phantoms.iter().enumerate() {
                    let si = if has_span { i / n } else { 0usize };
                    let ids: Vec<taffy::NodeId> = children
                        .iter()
                        .zip(assignment.iter())
                        .filter(|(_, a)| **a == (si, i % n))
                        .filter_map(|(c, _)| self.taffy_node.get(c).copied())
                        .collect();
                    let _ = self.taffy.set_children(p, &ids);
                    for c in children
                        .iter()
                        .zip(assignment.iter())
                        .filter(|(_, a)| **a == (si, i % n))
                        .filter_map(|(c, _)| self.taffy_node.get(c))
                    {
                        self.taffy_parent.insert(*c, p);
                    }
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
            }
            st.n = n;
            st.colw = colw;
            st.assignment = assignment;
            self.multicol_state.insert(mc, st);
        }
    }

    /// 三期⑤c：列规几何结算——每行每相邻列间一条竖直条带（视口系，
    /// 相对容器 border-box 原点；paint 层加容器原点）。语义：style ∈
    /// BorderStyle（none/hidden 不画）；v1 仅 solid 实绘，dashed/dotted
    /// 近似 solid（B 级偏差，与 border 策略一致）；width 缺席 = medium
    /// 3px，显式负值钳 0（0 宽不画）；color 走 currentcolor 终结。
    /// 行模式条带 y/高 = 幻影列盒（Flex 拉伸全等 = 容器内容高）；span
    /// 模式逐段独立。空列照画（CSS 未设内容存在性条件；用例规避）。
    fn settle_column_rules(&mut self, layout_by_node: &HashMap<NodeId, (f32, f32, f32, f32)>) {
        self.column_rules.clear();
        for &mc in &self.multicols {
            let Some(segs) = self.column_rule_segs(mc, layout_by_node) else {
                continue;
            };
            if !segs.is_empty() {
                self.column_rules.insert(mc, segs);
            }
        }
    }

    /// 单容器的列规条带（只读；None = 不画/无几何）。
    fn column_rule_segs(
        &self,
        mc: NodeId,
        layout_by_node: &HashMap<NodeId, (f32, f32, f32, f32)>,
    ) -> Option<Vec<crate::paint::ColumnRuleSeg>> {
        use crate::css::property::{DeclValue, PropertyId};
        let cs = self.styles.get(&mc)?;
        let rule_style = match cs.get(PropertyId::ColumnRuleStyle) {
            Some(DeclValue::ColumnRuleStyle(s)) => *s,
            _ => crate::css::property::BorderStyle::None,
        };
        if matches!(rule_style, crate::css::property::BorderStyle::None) {
            return None;
        }
        let st = self.multicol_state.get(&mc)?;
        if st.n < 2 || st.phantoms.is_empty() {
            return None;
        }
        let rctx = crate::css::value::ResolveCtx {
            em: cs.font_size_px(),
            rem: 16.0,
            viewport_w: self.media.viewport_w,
            viewport_h: self.media.viewport_h,
        };
        let rule_w = match cs.get(PropertyId::ColumnRuleWidth) {
            Some(DeclValue::ColumnRuleWidth(Some(lp))) => {
                lp.resolve(&rctx, 0.0).unwrap_or(3.0).max(0.0)
            }
            _ => 3.0, // medium
        };
        if rule_w <= 0.0 {
            return None;
        }
        let rule_color = match cs.get(PropertyId::ColumnRuleColor) {
            Some(DeclValue::Color(cv)) => crate::paint::resolve_color(cv, cs, &self.media),
            _ => crate::paint::resolve_color(
                &crate::css::value::ColorValue::CurrentColor,
                cs,
                &self.media,
            ),
        };
        let (bx, by) = layout_by_node.get(&mc).map(|&(x, y, _, _)| (x, y))?;
        let n = st.n;
        // 行集合：span 模式逐段（rows[i] 行 + 段内幻影切片）；行模式单行
        // （容器直系幻影）。幻影 location 相对父 border-box 原点（行模式
        // 父 = 容器；span 模式再上行一层 row.location）。
        let row_specs: Vec<(Option<taffy::NodeId>, std::ops::Range<usize>)> = if st.span_mode {
            st.rows
                .iter()
                .enumerate()
                .map(|(i, r)| (Some(*r), i * n..(i + 1) * n))
                .collect()
        } else {
            vec![(None, 0..st.phantoms.len())]
        };
        let mut segs = Vec::new();
        for (row, range) in row_specs {
            let row_y = match row {
                Some(r) => self.taffy.layout(r).map(|l| l.location.y).unwrap_or(0.0),
                None => 0.0,
            };
            // (x, y, w, h) 各幻影列（重建帧缺布局的段跳过，下帧自然补齐）。
            let phs: Vec<(f32, f32, f32, f32)> = st.phantoms[range]
                .iter()
                .filter_map(|p| self.taffy.layout(*p).ok())
                .map(|l| (l.location.x, l.location.y, l.size.width, l.size.height))
                .collect();
            if phs.len() < 2 {
                continue;
            }
            let rel_y = row_y + phs[0].1;
            let row_h = phs[0].3;
            for i in 1..phs.len() {
                let (ax, _, aw, _) = phs[i - 1];
                let (cx, _, _, _) = phs[i];
                // 条带中心 = 前列右缘 + 半 gap；宽度 = rule_w。
                let gap = cx - (ax + aw);
                let center = ax + aw + gap * 0.5;
                segs.push(crate::paint::ColumnRuleSeg {
                    x: bx + center - rule_w * 0.5,
                    y: by + rel_y,
                    width: rule_w,
                    height: row_h,
                    color: rule_color,
                });
            }
        }
        Some(segs)
    }

    fn restyle(&mut self) {
        self.styles.clear();
        self.span_styles.clear();
        self.parents.clear();
        self.wrap_widths.clear();
        self.calc_deferred.clear();
        self.tables.clear();
        self.table_cols.clear();
        self.table_cells.clear();
        self.multicols.clear();
        self.multicol_state.clear();
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

    /// 注册背景图（第五批⑨）：background-image: url(ref) 引用 → 宿主
    /// 预解码 RGBA（零副作用——引擎不取 URL、不解码位图格式）；注册于
    /// 样式表设置前后皆可，未注册引用绘制期告警跳过；仅影响绘制
    /// （DisplayList 每帧重建，无需脏标）。
    pub fn add_image(&mut self, reference: &str, width: u32, height: u32, rgba: Vec<u8>) {
        debug_assert_eq!(
            width as usize * height as usize * 4,
            rgba.len(),
            "image rgba buffer must be width*height*4"
        );
        let buf: std::sync::Arc<dyn std::convert::AsRef<[u8]> + Send + Sync> =
            std::sync::Arc::new(rgba);
        self.images.insert(
            reference.to_string(),
            crate::paint::ImageRes {
                width,
                height,
                rgba: buf,
            },
        );
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
        // ②table：display:table 节点登记（settle_tables 按表内容宽结算列模板）。
        if cs.display() == crate::css::property::Display::Table {
            self.tables.push(id);
        }
        // ③multi-column：多列容器登记（settle_columns 布局期建幻影列并分配）。
        if crate::layout::multicol_requested(&cs) {
            self.multicols.push(id);
        }
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
        // ①calc 直通：捕获本节点延迟 calc 并挂接 taffy 节点（结算基准 =
        // 父内容尺寸；无 taffy 节点则弃置——下次 restyle 重新捕获）。
        if let Some(&tid) = self.taffy_node.get(&id) {
            for raw in crate::layout::take_calc_deferred() {
                self.calc_deferred
                    .push(crate::layout::DeferredCalc { node: tid, raw });
            }
        }
        if let Some((w, h)) = self.measures.get(&id) {
            // 文本叶盒宽语义（第五批⑥契约）：声明宽度优先（CSS 显式 width
            // 胜出测量，旧实现被测量值覆写为缺陷）；无声明时交 taffy auto——
            // 块流拉伸到容器内容宽（与浏览器匿名块盒一致），flex/grid 子项
            // 取内容宽，min/max 宽仍由 taffy 夹紧。absolute 例外：restyle 先
            // 落显式测量宽（第三 pass 再夹紧——auto 时 taffy 绝对定位布局
            // 不回退测量值，compute #1 即需可用）。测量高兜底 height:auto=
            // 内容高；声明高优先。
            let width_dim = if has_declared_len(&cs, crate::css::property::PropertyId::Width) {
                ts.size.width
            } else if matches!(
                cs.get(crate::css::property::PropertyId::Position),
                Some(crate::css::property::DeclValue::Position(
                    crate::css::property::Position::Absolute
                ))
            ) {
                // absolute 例外查本地 cs 而非 is_absolute(id)——后者读
                // self.styles，而本节点 cs 到函数尾才 insert，查表恒 None
                // → auto 宽绝对文本叶丢测量宽，taffy 绝对布局对 auto 叶
                // 无测量回退得 0（bidi-mixed key2/key3 宽超差根因）。
                taffy::prelude::Dimension::length(*w)
            } else {
                taffy::prelude::Dimension::auto()
            };
            ts.size = taffy::prelude::Size {
                width: width_dim,
                height: if has_declared_len(&cs, crate::css::property::PropertyId::Height) {
                    ts.size.height
                } else {
                    taffy::prelude::Dimension::length(*h)
                },
            };
        }
        // ②table：单元格宽度交列模板（映射后覆写）——首行声明宽已折入模板，
        // 单元格一律拉伸至列宽（CSS 单元格 % 基准为表宽而非列宽；v1 契约：
        // 模板唯一权威，单元格自身 width 声明不直接生效）。
        if parent_style
            .as_ref()
            .is_some_and(|p| p.display() == crate::css::property::Display::TableRow)
        {
            ts.size.width = taffy::prelude::Dimension::auto();
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
                // 三期②：重挂 absolute 的坐标 = cb border box 原点 + 自身
                // location（taffy 绝对锚定 = cb padding box，location 已含
                // cb border 偏移）；ICB → 视口原点。cb 为样式树祖先，本 DFS
                // 先序保证 layout_by_node[cb] 已就绪（嵌套 absolute 同序）。
                if let Some(cb) = self.abs_cb.get(&id) {
                    let (bx, by) = match cb {
                        Some(cb_id) => layout_by_node
                            .get(cb_id)
                            .map(|&(bx, by, _, _)| (bx, by))
                            .unwrap_or((ox, oy)),
                        None => (0.0, 0.0),
                    };
                    x = bx + l.location.x;
                    y = by + l.location.y;
                } else {
                    x = l.location.x + ox;
                    y = l.location.y + oy;
                }
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
            // 三期②：重挂 absolute 的坐标独立于样式父链——递归基准归零，
            // 由节点自身 cb 分支推导；并跳过幻影 effp 补偿（防双重偏移）。
            if self.abs_cb.contains_key(c) {
                self.collect(*c, 0.0, 0.0, out, layout_by_node);
                continue;
            }
            // ③multi-column / ⑤b：子节点 taffy 父可能是合成层（幻影列 →
            // 段行 → 容器）——沿 taffy_parent 链上溯到样式节点本身，累加
            // 全部合成层偏移（列相对行、行相对容器的 location 之和）。
            let (mut cx, mut cy) = (x, y);
            if let Some(&tid_c) = self.taffy_node.get(c) {
                if let Some(mut effp) = self.taffy_parent.get(&tid_c).copied() {
                    let mut depth = 0usize;
                    while self.taffy_node.get(&id) != Some(&effp) && depth < 16 {
                        if let Ok(pl) = self.taffy.layout(effp) {
                            cx += pl.location.x;
                            cy += pl.location.y;
                        }
                        match self.taffy_parent.get(&effp) {
                            Some(&pp) => effp = pp,
                            None => break,
                        }
                        depth += 1;
                    }
                }
            }
            self.collect(*c, cx, cy, out, layout_by_node);
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

/// 声明长度检查（第五批⑥文本叶契约）：width/height 是否被显式声明
/// （`LenAuto(Some)`；auto=None 不算声明）。声明值优先于文本测量。
fn has_declared_len(cs: &ComputedStyle, pid: crate::css::property::PropertyId) -> bool {
    matches!(
        cs.get(pid),
        Some(crate::css::property::DeclValue::LenAuto(Some(_)))
    )
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
    fn keyframes_animation_sampling() {
        // 第五批⑰：动画采样——线性插值、fill both 端点保持、无 fill 回
        // 底层值；缓动按段施加（linear 下数值即线性）
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "@keyframes grow { from { width: 100px } to { width: 200px } } \
             div { width: 50px; animation: grow 1s linear both; }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("div")).is_ok());
        let idn = *engine.key_to_node.get(&Key(1)).unwrap();
        let width_at = |engine: &mut StyleEngine<Key>, t: f64| -> f32 {
            let _ = engine.frame((400.0, 100.0), 1.0, t);
            match engine
                .styles
                .get(&idn)
                .unwrap()
                .get(crate::css::property::PropertyId::Width)
            {
                Some(crate::css::property::DeclValue::LenAuto(Some(
                    crate::css::value::LengthPercentage::Px(v),
                ))) => *v,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(width_at(&mut engine, 0.0), 100.0);
        assert_eq!(width_at(&mut engine, 0.5), 150.0);
        assert_eq!(width_at(&mut engine, 0.25), 125.0);
        assert_eq!(width_at(&mut engine, 1.0), 200.0);
        // fill both：结束后停在终点
        assert_eq!(width_at(&mut engine, 2.0), 200.0);
        // 无 fill：结束后回底层值
        let report = engine.set_stylesheet(
            "@keyframes grow { from { width: 100px } to { width: 200px } } \
             div { width: 50px; animation: grow 1s linear; }",
        );
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(width_at(&mut engine, 2.0), 50.0);
    }

    #[test]
    fn calc_layout_resolution() {
        // 二期①calc 直通：taffy 原生指针传输层公共接入点被 pub(crate) 内部
        // 阻断——引擎侧结算式直通（layout.rs DeferredRaw / settle_calc）：
        // px 部分首遍折叠，百分比部分以父内容尺寸为基准结算后回写固定值。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "div { width: calc(100px + 50px); height: 20px } \
             p { width: calc(50% + 10px); height: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("div")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("p")).is_ok());
        let frame = engine.frame((400.0, 100.0), 1.0, 0.0);
        let d = frame.find(Key(1)).unwrap();
        assert_eq!(d.width, 150.0, "纯长度 calc 应在布局期正确解析");
        let p = frame.find(Key(2)).unwrap();
        assert_eq!(p.width, 85.0, "含百分比 calc 结算后 = 50%×150+10");
    }

    #[test]
    fn calc_percent_chain_settles() {
        // 二期①calc 直通：两级百分比链——p1 = 50%×200+10 = 110（1 遍结算），
        // p2 = 50%×110+10 = 65（p1 稳定后方可结算，需第 2 遍）；收敛上限
        // 3 遍覆盖 3 层内链路。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "div { width: 200px; height: 20px } \
             p1 { width: calc(50% + 10px); height: 20px } \
             p2 { width: calc(50% + 10px); height: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("div")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("p1")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("p2")).is_ok());
        let frame = engine.frame((400.0, 100.0), 1.0, 0.0);
        let p1 = frame.find(Key(2)).unwrap();
        assert_eq!(p1.width, 110.0, "一级链 1 遍结算");
        let p2 = frame.find(Key(3)).unwrap();
        assert_eq!(p2.width, 65.0, "二级链经第 2 遍结算收敛");
    }

    #[test]
    fn calc_slot_flex_basis_settles_both_axes() {
        // 三期③槽位扩展：flex-basis 含百分比 calc 延迟结算——基=父容器
        // 主轴内容尺寸（行向=宽、列向=高，settle 期按父 flex_direction 判定）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "wrap { width: 400px } \
             row { display: flex; flex-direction: row; width: 400px; height: 20px } \
             rit { flex-basis: calc(25% + 50px); flex-grow: 0; height: 20px } \
             col { display: flex; flex-direction: column; width: 20px; height: 600px } \
             cit { flex-basis: calc(50% - 100px); flex-grow: 0; width: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("wrap")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("rit")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("col")).is_ok());
        assert!(engine.insert(Some(Key(4)), Key(5), mk("cit")).is_ok());
        let frame = engine.frame((400.0, 800.0), 1.0, 0.0);
        assert_eq!(
            frame.find(Key(3)).unwrap().width,
            150.0,
            "行向基=容器内容宽：25%×400+50"
        );
        assert_eq!(
            frame.find(Key(5)).unwrap().height,
            200.0,
            "列向基=容器内容高：50%×600−100"
        );
    }

    #[test]
    fn calc_slot_min_max_settles() {
        // 三期③槽位扩展：min/max-width 含百分比 calc 延迟结算（基=父内容
        // 宽）——min 抬升过窄子盒、max 压制过宽子盒。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "pw { width: 400px; height: 20px } \
             mn { width: 10px; min-width: calc(50% - 30px); height: 20px } \
             mx { width: 1000px; max-width: calc(50% + 50px); height: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("pw")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("mn")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("mx")).is_ok());
        let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
        assert_eq!(
            frame.find(Key(2)).unwrap().width,
            170.0,
            "min-width = 50%×400−30 抬升"
        );
        assert_eq!(
            frame.find(Key(3)).unwrap().width,
            250.0,
            "max-width = 50%×400+50 压制"
        );
    }

    #[test]
    fn calc_slot_margin_percent_base_is_width() {
        // 三期③槽位扩展：margin 含百分比 calc 延迟结算——CSS 2.1 §8.3
        // 百分比 margin（含 top/bottom）恒以包含块宽度为基而非高度：
        // margin-top 10%×400+5 = 45（若误用高基 300×10%=30+5=35）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "pw { width: 400px; height: 300px } \
             it { width: 100px; height: 20px; margin-left: calc(50% - 50px); \
                  margin-top: calc(10% + 5px) }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("pw")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("it")).is_ok());
        let frame = engine.frame((400.0, 400.0), 1.0, 0.0);
        let it = frame.find(Key(2)).unwrap();
        assert_eq!(it.x, 150.0, "margin-left = 50%×400−50");
        assert_eq!(
            it.y, 45.0,
            "margin-top 以宽度为基 = 10%×400+5（首子 margin 与父塌陷后父子同位）"
        );
    }

    #[test]
    fn calc_slot_padding_settles_and_clamps() {
        // 三期③槽位扩展：padding 含百分比 calc 延迟结算（基=包含块内容宽，
        // 含 padding-top——CSS 2.1 §8.4）；负值钳 0（padding 负值非法）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "wrap { width: 400px } \
             it { width: 100px; height: 20px; padding: calc(10% + 5px) } \
             cc { width: 80px; height: 10px } \
             nz { width: 50px; height: 10px; padding-left: calc(0% - 10px) }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("wrap")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("it")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("cc")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("nz")).is_ok());
        let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
        let it = frame.find(Key(2)).unwrap();
        let cc = frame.find(Key(3)).unwrap();
        assert_eq!(it.width, 190.0, "border-box = 100 + 两侧 padding 45");
        assert_eq!(cc.x, 45.0, "内容随 padding-left 内缩（宽基 10%×400+5）");
        assert_eq!(cc.y, 45.0, "padding-top 同以宽度为基");
        let nz = frame.find(Key(4)).unwrap();
        assert_eq!(nz.width, 50.0, "padding-left 负值钳 0（不缩宽度）");
    }

    #[test]
    fn calc_slot_gap_settles() {
        // 三期③槽位扩展：column-gap 含百分比 calc 延迟结算（列隙基=容器
        // 内容宽 20%×400+10 = 90 → 第二项 x=190）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "fx { display: flex; width: 400px; height: 20px; column-gap: calc(20% + 10px) } \
             ia { width: 100px; height: 20px } \
             ib { width: 100px; height: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("fx")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("ia")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("ib")).is_ok());
        let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(3)).unwrap().x, 190.0, "gap = 90 结算后推进");
    }

    #[test]
    fn calc_slot_two_level_chain_converges() {
        // 三期③：跨槽位两级链——mid.width 结算后 inner.min-width 以其为基
        // （需第 2 遍，3 遍上限内收敛）：mid = 50%×400+10 = 210；
        // inner min-width = 50%×210−5 = 100。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mid { width: calc(50% + 10px); height: 20px } \
             inner { width: 10px; min-width: calc(50% - 5px); height: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mid")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("inner")).is_ok());
        let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(1)).unwrap().width, 210.0);
        assert_eq!(
            frame.find(Key(2)).unwrap().width,
            100.0,
            "min-width 以已结算父宽为基（跨槽位第 2 遍）"
        );
    }

    #[test]
    fn table_two_stage_column_settlement() {
        // 二期②：table→块容器、row→单行 Grid（共享列模板）、cell→Grid 项；
        // settle_tables 以表内容宽结算列模板（150 定宽 + 50% + auto 均分），
        // 第二遍重排后各单元格宽度/列位与 CSS 表模型一致。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "tab { display: table; width: 600px } \
             row { display: table-row } \
             ca { display: table-cell; width: 150px } \
             cb { display: table-cell; width: 50% } \
             cc { display: table-cell }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("tab")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("row")).is_ok());
        for (parent, k, name) in [
            (Key(2), Key(4), "ca"),
            (Key(2), Key(5), "cb"),
            (Key(2), Key(6), "cc"),
            (Key(3), Key(7), "ca"),
            (Key(3), Key(8), "cb"),
            (Key(3), Key(9), "cc"),
        ] {
            assert!(engine.insert(Some(parent), k, mk(name)).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let row1 = frame.find(Key(2)).unwrap();
        assert_eq!(row1.width, 600.0, "行宽=表内容宽");
        let w = |k: Key| frame.find(k).unwrap().width;
        let x = |k: Key| frame.find(k).unwrap().x;
        assert_eq!(w(Key(4)), 150.0, "定宽列");
        assert_eq!(w(Key(5)), 300.0, "50% 列 = 表内容宽×0.5");
        assert_eq!(w(Key(6)), 150.0, "auto 列均分剩余");
        assert_eq!(w(Key(7)), 150.0, "第二行共享模板");
        assert_eq!(w(Key(8)), 300.0);
        assert_eq!(w(Key(9)), 150.0);
        assert_eq!(x(Key(4)), 0.0);
        assert_eq!(x(Key(5)), 150.0);
        assert_eq!(x(Key(6)), 450.0, "列位随模板推进");
        // 稳态帧：模板缓存全等 → 不再触发额外重排（幂等）。
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let frame3 = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame3.find(Key(5)).unwrap().width, 300.0);
    }

    #[test]
    fn table_colspan_expands_column_map() {
        // 三期④a：colspan 属性（StyleNode.attrs）→ 单元格图显式列位；
        // span-n 声明宽均分给跨内未声明列（Chromium 简单表一致）。
        // 列模板 [100,100,100,100]：ca 定宽 100 占列 1，cb 宽 300
        // colspan=3 占列 2–4；第二行四格共享模板并显式列位推进。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "tab { display: table; width: 400px } \
             row { display: table-row } \
             ca { display: table-cell; width: 100px; height: 30px } \
             cb { display: table-cell; width: 300px; height: 30px } \
             cd { display: table-cell; height: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        let mk_span = |name: &str, span: &str| StyleNode {
            name: Some(name.to_string()),
            attrs: [("colspan".to_string(), span.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("tab")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(4), mk("ca")).is_ok());
        assert!(
            engine
                .insert(Some(Key(2)), Key(5), mk_span("cb", "3"))
                .is_ok()
        );
        for k in 6..=9 {
            assert!(engine.insert(Some(Key(3)), Key(k), mk("cd")).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let (w, x, y) = (
            |k: Key| frame.find(k).unwrap().width,
            |k: Key| frame.find(k).unwrap().x,
            |k: Key| frame.find(k).unwrap().y,
        );
        assert_eq!(w(Key(4)), 100.0, "定宽列 1");
        assert_eq!(x(Key(5)), 100.0, "跨列单元起于列 2");
        assert_eq!(w(Key(5)), 300.0, "跨 3 列 = 100×3");
        for (i, k) in (6..=9).enumerate() {
            assert_eq!(w(Key(k)), 100.0, "第二行共享模板");
            assert_eq!(x(Key(k)), i as f32 * 100.0, "显式列位推进");
            assert_eq!(y(Key(k)), 30.0, "第二行位于首行下方");
        }
        // 稳态幂等：列位签名全等不重写。
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let frame3 = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame3.find(Key(5)).unwrap().x, 100.0);
    }

    #[test]
    fn table_row_group_stacks_rows() {
        // 三期④a：行组（table-row-group）=纵向透明块包装，行发现穿透；
        // 组内行与表直系行混排（直系行在组后接续堆叠）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "tab { display: table; width: 400px } \
             grp { display: table-row-group } \
             row { display: table-row; height: 30px } \
             row2 { display: table-row; height: 20px } \
             cell { display: table-cell; width: 100px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("tab")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("grp")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(4), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(5), mk("row2")).is_ok());
        assert!(engine.insert(Some(Key(3)), Key(6), mk("cell")).is_ok());
        assert!(engine.insert(Some(Key(4)), Key(7), mk("cell")).is_ok());
        assert!(engine.insert(Some(Key(5)), Key(8), mk("cell")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 行组透明：组内两行 y=0/30，直系行接续 y=50。
        assert_eq!(b(Key(3)).y, 0.0);
        assert_eq!(b(Key(4)).y, 30.0);
        assert_eq!(b(Key(5)).y, 60.0, "直系行接在行组之后（组内 30+30）");
        // 列模板跨行共享（首行单元格声明穿透行组生效）。
        for k in [Key(6), Key(7), Key(8)] {
            assert_eq!(b(k).width, 100.0);
            assert_eq!(b(k).x, 0.0);
        }
        assert_eq!(b(Key(1)).height, 80.0, "表高 = 30+30+20");
    }

    #[test]
    fn table_caption_sits_above_rows() {
        // 三期④a：caption=普通块盒置于行区上方，宽=表内容宽，不参与列发现。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "tab { display: table; width: 400px } \
             cap { display: table-caption; height: 20px } \
             row { display: table-row; height: 30px } \
             cell { display: table-cell; width: 100px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("tab")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("cap")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(3)), Key(4), mk("cell")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        assert_eq!(b(Key(2)).y, 0.0, "caption 在最上");
        assert_eq!(b(Key(2)).height, 20.0);
        assert_eq!(b(Key(2)).width, 400.0, "caption 宽=表内容宽");
        assert_eq!(b(Key(3)).y, 20.0, "行区在 caption 之下");
        assert_eq!(b(Key(4)).width, 100.0, "caption 不参与列发现");
        assert_eq!(b(Key(1)).height, 50.0);
    }

    #[test]
    fn table_rowspan_spans_rows_in_home_grid() {
        // 三期④c：rowspan=2 单元格留在宿主行网格（列位照旧），高度覆写 =
        // 被跨行 definite 高度和（30+20=50）向下溢出覆盖；后行游标跳过被
        // 占列（第二行 col 2–4 被占，单格落列 1）；第三行四格补齐列 1–4。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "tab { display: table; width: 400px } \
             row { display: table-row; height: 30px } \
             row2 { display: table-row; height: 20px } \
             row3 { display: table-row; height: 20px } \
             ca { display: table-cell; width: 100px } \
             cb { display: table-cell; width: 300px } \
             cc { display: table-cell; width: 100px } \
             cd { display: table-cell }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        let mk_span = |name: &str, attrs: &[(&str, &str)]| StyleNode {
            name: Some(name.to_string()),
            attrs: attrs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("tab")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("row2")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("row3")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(5), mk("ca")).is_ok());
        assert!(
            engine
                .insert(
                    Some(Key(2)),
                    Key(6),
                    mk_span("cb", &[("colspan", "3"), ("rowspan", "2")])
                )
                .is_ok()
        );
        assert!(engine.insert(Some(Key(3)), Key(7), mk("cc")).is_ok());
        for k in 8..=11 {
            assert!(engine.insert(Some(Key(4)), Key(k), mk("cd")).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 跨行单元：宿主行内起列 2，高 = 30+20。
        assert_eq!(b(Key(6)).x, 100.0);
        assert_eq!(b(Key(6)).y, 0.0, "无 caption，首行在表顶");
        assert_eq!(b(Key(6)).width, 300.0);
        assert_eq!(b(Key(6)).height, 50.0, "rowspan 覆写 = 30+20");
        // 第二行：col 2–4 被占，唯一格落列 1。
        assert_eq!(b(Key(7)).x, 0.0);
        assert_eq!(b(Key(7)).y, 30.0);
        assert_eq!(b(Key(7)).width, 100.0);
        assert_eq!(b(Key(7)).height, 20.0);
        // 第三行四格补齐列 1–4。
        for (i, k) in (8..=11).enumerate() {
            assert_eq!(b(Key(k)).x, i as f32 * 100.0);
            assert_eq!(b(Key(k)).y, 50.0);
            assert_eq!(b(Key(k)).width, 100.0);
        }
        assert_eq!(b(Key(1)).height, 70.0, "表高 = 30+20+20");
    }

    #[test]
    fn table_row_non_cell_becomes_anonymous_cell() {
        // 三期④d：行内非单元格元素（display:block div）→ 匿名单元格
        // （CSS 2.1 §17.2.1 第 3 条）：入单元格图、列位分配、宽度拉伸；
        // display:none 与 absolute 子件不入图。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "tab { display: table; width: 300px } \
             row { display: table-row; height: 20px } \
             row2 { display: table-row; height: 25px } \
             ca { display: table-cell; width: 100px } \
             cb { display: block; width: 200px } \
             cc { display: block } \
             hidden { display: none }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("tab")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("ca")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(4), mk("cb")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(5), mk("hidden")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(6), mk("row2")).is_ok());
        assert!(engine.insert(Some(Key(6)), Key(7), mk("cc")).is_ok());
        assert!(engine.insert(Some(Key(6)), Key(8), mk("ca")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 首行：td 列 1，div 匿名单元格列 2（列模板 [100,200]）。
        assert_eq!(b(Key(3)).x, 0.0);
        assert_eq!(b(Key(3)).width, 100.0);
        assert_eq!(b(Key(4)).x, 100.0, "div 匿名单元格起列 2");
        assert_eq!(b(Key(4)).width, 200.0);
        // 第二行：div 落列 1（auto 拉伸 100），td 落列 2 拉伸 200。
        assert_eq!(b(Key(7)).x, 0.0);
        assert_eq!(b(Key(7)).width, 100.0);
        assert_eq!(b(Key(8)).x, 100.0);
        assert_eq!(b(Key(8)).width, 200.0, "声明 100 拉伸到列宽 200");
        assert_eq!(b(Key(1)).height, 45.0, "表高 = 20+25");
    }

    #[test]
    fn multicol_balance_two_columns() {
        // 二期③：column-count:2 → settle_columns 建幻影列、按理想高贪心
        // 平衡分配；子节点坐标经幻影补偿后与真实列位一致（列宽=(400−20)/2）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 20px; width: 400px } \
             it { height: 30px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        for k in 2..=7 {
            assert!(engine.insert(Some(Key(1)), Key(k), mk("it")).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let mc = frame.find(Key(1)).unwrap();
        assert_eq!(mc.width, 400.0);
        assert_eq!(mc.height, 90.0, "容器高=最高幻影列（3×30）");
        let b = |k: Key| frame.find(k).unwrap();
        for (i, k) in (2..=4).enumerate() {
            let bx = b(Key(k));
            assert_eq!(bx.x, 0.0, "第 1 列");
            assert_eq!(bx.y, (i as f32) * 30.0);
            assert_eq!(bx.width, 190.0, "列宽 =（400−20）/2");
        }
        for (i, k) in (5..=7).enumerate() {
            let bx = b(Key(k));
            assert_eq!(bx.x, 210.0, "第 2 列 = 190+gap20");
            assert_eq!(bx.y, (i as f32) * 30.0);
        }
    }

    #[test]
    fn multicol_width_mode_four_columns() {
        // 二期③：column-width 模式 n=⌊(cw+gap)/(w+gap)⌋=4、colw=100，
        // 8 项理想高贪心 → 每列 2 项、列距 0。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-width: 100px; column-gap: 0px; width: 400px } \
             it { height: 25px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        for k in 2..=9 {
            assert!(engine.insert(Some(Key(1)), Key(k), mk("it")).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        for (col, k) in [
            (0, 2u32),
            (0, 3),
            (1, 4),
            (1, 5),
            (2, 6),
            (2, 7),
            (3, 8),
            (3, 9),
        ] {
            let bx = b(Key(k));
            assert_eq!(bx.x, (col as f32) * 100.0, "第 {col} 列");
            assert_eq!(bx.width, 100.0);
        }
        assert_eq!(b(Key(2)).y, 0.0);
        assert_eq!(b(Key(3)).y, 25.0);
        assert_eq!(b(Key(8)).y, 0.0);
        assert_eq!(b(Key(9)).y, 25.0);
    }

    #[test]
    fn multicol_revert_single_column() {
        // 二期③：column-width 过宽 → n=1 回退单列块流（首帧即还原，
        // 不残留 Flex 行布局；声明宽保留）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-width: 1000px; width: 400px } \
             it { height: 30px; width: 200px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        for k in 2..=4 {
            assert!(engine.insert(Some(Key(1)), Key(k), mk("it")).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        for (i, k) in (2..=4).enumerate() {
            let bx = b(Key(k));
            assert_eq!(bx.x, 0.0, "回退块流：纵向堆叠");
            assert_eq!(bx.y, (i as f32) * 30.0);
            assert_eq!(bx.width, 200.0, "声明宽保留");
        }
        assert_eq!(frame.find(Key(1)).unwrap().height, 90.0);
    }

    #[test]
    fn multicol_balance_uneven() {
        // 二期③：理想高贪心（ideal=total/n=90）：高 80/10/10/80 →
        // [80,10] | [10,80]，不可断子件整件入列、尾列 min 守护。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px } \
             h1 { height: 80px } h2 { height: 10px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        for (k, name) in [(2u32, "h1"), (3, "h2"), (4, "h2"), (5, "h1")] {
            assert!(engine.insert(Some(Key(1)), Key(k), mk(name)).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        assert_eq!(b(Key(2)).y, 0.0);
        assert_eq!(b(Key(3)).y, 80.0, "第 1 列 [80,10]");
        assert_eq!(b(Key(4)).x, 200.0);
        assert_eq!(b(Key(4)).y, 0.0, "第 2 列 [10,80]");
        assert_eq!(b(Key(5)).x, 200.0);
        assert_eq!(b(Key(5)).y, 10.0);
        assert_eq!(b(Key(2)).width, 200.0, "列宽 = 400/2");
    }

    #[test]
    fn multicol_bisection_balances_uneven() {
        // 三期⑤a：二分平衡（试高顺序装箱，fit 单调 → 浮点二分最小可行
        // 列高）。[80,80,80,10] 双列：贪心 ideal=125 给 [80|80,80,10]
        // max170；二分得 [80,80|80,10] max160（Chromium 平衡语义）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px } \
             h1 { height: 80px } h2 { height: 10px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        for (k, name) in [(2u32, "h1"), (3, "h1"), (4, "h1"), (5, "h2")] {
            assert!(engine.insert(Some(Key(1)), Key(k), mk(name)).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 列 1 [80,80]、列 2 [80,10]；容器高 160（贪心 170）。
        assert_eq!(b(Key(2)).y, 0.0);
        assert_eq!(b(Key(3)).y, 80.0, "列 1 两件 80");
        assert_eq!(b(Key(4)).x, 200.0);
        assert_eq!(b(Key(4)).y, 0.0, "列 2 首件 80");
        assert_eq!(b(Key(5)).x, 200.0);
        assert_eq!(b(Key(5)).y, 80.0, "列 2 尾件 10");
        assert_eq!(b(Key(1)).height, 160.0, "二分平衡列高（贪心 170）");
    }

    #[test]
    fn multicol_break_margin_top_truncated() {
        // 三期⑤a：断口 margin-top 截断（css-multicol §7）——列首件
        // margin-top 不生效。b2(mt20,h30) 平衡进列 2 顶（二分含截断建模：
        // 列 1=[80] 列 2=[30,80] max110 < 无截断建模的 130）；列中件
        // margin 保留（b1 下方若有 margin 仍占位）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px } \
             b1 { height: 80px } \
             b2 { height: 30px; margin-top: 20px } \
             b3 { height: 80px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("b1")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("b2")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("b3")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 列 1 [80]；列 2 [b2(截断 mt→y0), b3(y30)]；容器高 110。
        assert_eq!(b(Key(2)).y, 0.0);
        assert_eq!(b(Key(3)).x, 200.0);
        assert_eq!(b(Key(3)).y, 0.0, "列首 margin-top 截断（不截则 y=20）");
        assert_eq!(b(Key(4)).y, 30.0);
        assert_eq!(b(Key(1)).height, 110.0);
    }

    #[test]
    fn multicol_span_all_splits_flow() {
        // 三期⑤b：column-span:all 切断列流——a/b 平衡进 spanner 前段行
        // （双列并排），spanner 全宽横贯，c/d 收进后段行。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px } \
             blk { height: 100px } sp { column-span: all; height: 50px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("blk")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("blk")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("sp")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(5), mk("blk")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(6), mk("blk")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 前段行：a(0,0,200,100) b(200,0,200,100)；spanner 全宽 y=100；
        // 后段行：c(0,150,200,100) d(200,150,200,100)；容器高 250。
        assert_eq!(b(Key(2)).x, 0.0);
        assert_eq!(b(Key(3)).x, 200.0);
        assert_eq!(b(Key(3)).y, 0.0);
        assert_eq!(b(Key(4)).x, 0.0, "spanner 全宽横贯");
        assert_eq!(b(Key(4)).y, 100.0, "spanner 前段行（高 100）之下");
        assert_eq!(b(Key(4)).width, 400.0);
        assert_eq!(b(Key(4)).height, 50.0);
        assert_eq!(b(Key(5)).x, 0.0);
        assert_eq!(b(Key(5)).y, 150.0, "spanner 之下新段行");
        assert_eq!(b(Key(6)).x, 200.0);
        assert_eq!(b(Key(6)).y, 150.0);
        assert_eq!(b(Key(1)).height, 250.0);
    }

    #[test]
    fn multicol_span_all_two_segments_balance() {
        // 三期⑤b：spanner 分段各段独立二分平衡——a(80) | sp(10) |
        // b(80) c(80) d(10)：前段行高 80；后段 3 件双列二分
        // [80,80|80,10] 行高 90（贪心 170）；容器高 80+10+90=180。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px } \
             b80 { height: 80px } b10 { height: 10px } \
             sp { column-span: all; height: 10px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("b80")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("sp")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("b80")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(5), mk("b80")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(6), mk("b10")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 前段行高 80；spanner y=80 h=10；后段行二分 [80,80|80,10] 高 90
        //（c y=90 col1，d y=90 col2，e y=170 col2）；容器高 180。
        assert_eq!(b(Key(2)).y, 0.0);
        assert_eq!(b(Key(2)).x, 0.0, "单件段占列 1");
        assert_eq!(b(Key(3)).y, 80.0);
        assert_eq!(b(Key(3)).height, 10.0);
        assert_eq!(b(Key(4)).x, 0.0);
        assert_eq!(b(Key(4)).y, 90.0, "后段行列 1 首件");
        assert_eq!(b(Key(5)).x, 200.0);
        assert_eq!(b(Key(5)).y, 90.0, "后段行列 2 首件");
        assert_eq!(b(Key(6)).x, 200.0);
        assert_eq!(b(Key(6)).y, 170.0, "后段行列 2 尾件");
        assert_eq!(b(Key(1)).height, 180.0, "段内二分行高 90（贪心 170）");
    }

    #[test]
    fn multicol_span_segment_start_margin_kept() {
        // 三期⑤b：spanner 边界 = 强制断行——后段首件 margin-top 保留
        // （css-break-3 强断边距保留；Chromium 153 实证 b2 y=360 含 mt20）。
        // 段内列首仍截断（⑤a 语义），spanner 后新列除外。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px } \
             b1 { height: 80px } sp { column-span: all; height: 50px } \
             b2 { height: 80px; margin-top: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("b1")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("sp")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("b2")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        assert_eq!(b(Key(2)).y, 0.0);
        assert_eq!(b(Key(3)).y, 80.0);
        assert_eq!(b(Key(4)).x, 0.0);
        assert_eq!(b(Key(4)).y, 150.0, "段首 margin-top 保留（截断则 130）");
        assert_eq!(b(Key(1)).height, 230.0);
    }

    #[test]
    fn multicol_rule_row_mode_gap_centered() {
        // 三期⑤c：行模式列规——条带落在列间 gap 正中（center = 前列右缘 +
        // 半 gap），y/高 = 幻影列盒（容器内容高）；宽 4px 居中 ⇒ x = 198。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 20px; width: 400px; \
              column-rule: 4px solid red } \
             it { height: 30px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        for k in 2..=7 {
            assert!(engine.insert(Some(Key(1)), Key(k), mk("it")).is_ok());
        }
        let _frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let mc_id = engine.key_to_node[&Key(1)];
        let segs = engine.column_rules.get(&mc_id).expect("行模式列规存在");
        assert_eq!(segs.len(), 1, "相邻列对数 = n−1");
        let s = &segs[0];
        // 列位 0..190 | 210..400：center = 190+10 = 200；x = 200−2。
        assert_eq!(s.x, 198.0);
        assert_eq!(s.y, 0.0);
        assert_eq!(s.width, 4.0);
        assert_eq!(s.height, 90.0, "条带高 = 列盒高（3×30）");
        let c = s.color.components;
        assert!(c[0] > 0.9 && c[3] == 1.0, "red currentcolor 终结（{c:?}）");
    }

    #[test]
    fn multicol_rule_span_mode_per_segment() {
        // 三期⑤c：span 模式列规逐段绘制——前段行（y 0..80）与后段行
        // （y 90..170）各一条；spanner 横贯处不画（列间断点消失）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px; \
              column-rule: 2px solid blue } \
             b80 { height: 80px } sp { column-span: all; height: 10px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("b80")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("sp")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("b80")).is_ok());
        let _frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let mc_id = engine.key_to_node[&Key(1)];
        let segs = engine.column_rules.get(&mc_id).expect("span 模式逐段列规");
        assert_eq!(segs.len(), 2, "每段一行一条");
        let mut ys: Vec<f32> = segs.iter().map(|s| s.y).collect();
        ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(ys, vec![0.0, 90.0], "前段行 y=0；后段行 y=90（80+10）");
        for s in segs {
            assert_eq!(s.x, 199.0, "gap0 居中：x = 200−1");
            assert_eq!(s.width, 2.0);
            assert_eq!(s.height, 80.0);
        }
    }

    #[test]
    fn multicol_rule_style_none_or_zero_width_no_paint() {
        // 三期⑤c：style:none / 宽 0 / 缺省（style 初始 none）→ 无条带。
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        let run = |sheet: &str| {
            let mut engine: StyleEngine<Key> = StyleEngine::new();
            assert!(engine.set_stylesheet(sheet).is_clean());
            assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
            for k in 2..=5 {
                assert!(engine.insert(Some(Key(1)), Key(k), mk("it")).is_ok());
            }
            let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
            let mc_id = engine.key_to_node[&Key(1)];
            engine.column_rules.get(&mc_id).is_none()
        };
        assert!(run(
            "mc { column-count: 2; width: 400px; column-rule: none } \
             it { height: 30px }"
        ));
        assert!(run(
            "mc { column-count: 2; width: 400px; column-rule: 0 solid red } \
             it { height: 30px }"
        ));
        assert!(run(
            "mc { column-count: 2; width: 400px; column-rule-color: red } \
             it { height: 30px }",
        ));
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
        // wrap_widths 同步（绘制折行一致）——第五批⑥后宿主声明宽（300）
        // 不再被文本测量覆写，absolute 夹紧宽 = cb 内容宽 300（折行约束），
        // 而 measures.0 = 该约束下的最宽行（283.97 类值）
        assert_eq!(engine.wrap_widths.get(&id2), Some(&Some(300.0)));
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
    fn transform_parse_2d_and_reject_3d() {
        // ADR-0009 v1：2D 函数列表解析；3D 函数解析拒绝（warn 路径 → 非 clean，
        // 声明丢弃 → 空表 = none）
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let rep = engine.set_stylesheet(
            "div.a { transform: translate(10px, 20%) rotate(45deg) scale(2) skew(10deg); } div.b { transform: matrix(1, 0, 0, 1, 5, 6); } div.d { transform: none; }",
        );
        assert!(rep.is_clean(), "warnings={:?}", rep.warnings);
        let mut engine3d: StyleEngine<Key> = StyleEngine::new();
        assert!(
            !engine3d
                .set_stylesheet("div.c { transform: translate3d(1px, 2px, 3px); }")
                .is_clean()
        );
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "a").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "b").is_ok());
        assert!(mk(&mut engine, Key(4), Some(Key(1)), "d").is_ok());
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        use crate::css::property::TransformFn as TF;
        use crate::css::value::LengthPercentage as LP;
        let cs = |e: &StyleEngine<Key>, k: Key| {
            e.styles
                .get(e.key_to_node.get(&k).unwrap())
                .cloned()
                .unwrap()
        };
        let a = cs(&engine, Key(1));
        assert_eq!(
            a.transform(),
            &[
                TF::Translate(LP::Px(10.0), LP::Percent(0.2)),
                TF::Rotate(45.0),
                TF::Scale(2.0, 2.0),
                TF::Skew(10.0, 0.0),
            ]
        );
        assert!(a.has_transform());
        let b = cs(&engine, Key(2));
        assert_eq!(b.transform(), &[TF::Matrix(1.0, 0.0, 0.0, 1.0, 5.0, 6.0)]);
        let d = cs(&engine, Key(4));
        assert!(d.transform().is_empty());
    }

    #[test]
    fn transform_ancestor_is_absolute_cb_for_avail() {
        // ADR-0009 L2 双时机：transform ≠ none 祖先 → absolute 后代包含块
        // （修正前 cb walk 跳过 static+transformed 的 mid → avail = outer 内容宽
        // 360 → 叶宽 360；修正后夹到 mid 内容宽 200）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.outer { position: relative; width: 400px; padding: 20px; } div.mid { transform: translate(0px, 0px); width: 200px; margin-left: 30px; } div.leaf { position: absolute; top: 0; left: 0; }"
            )
            .is_clean());
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "outer").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "mid").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(2)), "leaf").is_ok());
        assert!(engine.set_leaf_measure(Key(3), 2000.0, 20.0).is_ok());
        assert!(
            engine
                .set_leaf_intrinsic(Key(3), 80.0, 20.0, 2000.0, 20.0)
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b2 = frame.find(Key(2)).unwrap();
        assert_eq!((b2.x, b2.width), (50.0, 200.0)); // 外 padding 20 + margin 30；identity 变换不改布局
        let b3 = frame.find(Key(3)).unwrap();
        // avail = mid 内容宽 200（transformed 祖先成为 cb），夹紧 80..2000
        assert_eq!(b3.width, 200.0);
        // taffy 锚定直父（mid）：x = mid.x，y = outer padding
        assert_eq!((b3.x, b3.y), (50.0, 20.0));
    }

    #[test]
    fn absolute_anchor_jumps_over_static_wrapper() {
        // 三期②锚定跳走：cb（transformed mid）与 absolute 叶之间存在 static
        // 包装层时，taffy 直父锚定会把 inset 基准错挂在 wrap 上（旧：
        // 75+30=105）；跳走后 inset 相对 cb padding box（新：50+30=80）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.outer { position: relative; width: 400px; padding: 20px; } \
                 div.mid { transform: translate(0px, 0px); width: 200px; margin-left: 30px; } \
                 div.wrap { margin-left: 25px; } \
                 div.leaf { position: absolute; top: 20px; left: 30px; }"
                )
                .is_clean()
        );
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "outer").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "mid").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(2)), "wrap").is_ok());
        assert!(mk(&mut engine, Key(4), Some(Key(3)), "leaf").is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b2 = frame.find(Key(2)).unwrap();
        assert_eq!((b2.x, b2.y), (50.0, 20.0)); // 外 padding 20 + margin 30
        // wrap 只给水平 margin：taffy 0.14 末子 margin-bottom 塌陷会把父级
        // 顶下 15px（CSS 2.1 §8.3.1 应落在父底缘之外），本测聚焦锚定跳走，
        // 不掺入该塌陷行为（已知偏差记录于 FEATURES.md）。
        let b3 = frame.find(Key(3)).unwrap();
        assert_eq!(b3.x, 75.0); // wrap 流内位置 = mid 内容原点 + margin 25（旧锚定基准）
        let b4 = frame.find(Key(4)).unwrap();
        // cb = mid：x = mid border box 原点 50 + inset 30；y = 20 + inset 20
        assert_eq!(
            (b4.x, b4.y),
            (80.0, 40.0),
            "absolute 应跳过 static 包装层锚定到 cb"
        );
    }

    #[test]
    fn absolute_icb_anchor_skips_all_static_ancestors() {
        // 三期②ICB：无 positioned/transformed 祖先 → 包含块 = 初始包含块，
        // inset 相对视口原点（旧：直父 wrap 流内位置 50+10/25+10）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.outer { width: 400px; padding: 40px; margin-left: 10px; } \
                 div.wrap { margin-top: 25px; } \
                 div.leaf { position: absolute; top: 10px; left: 10px; }"
                )
                .is_clean()
        );
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "outer").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "wrap").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(2)), "leaf").is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b3 = frame.find(Key(3)).unwrap();
        assert_eq!(
            (b3.x, b3.y),
            (10.0, 10.0),
            "无 positioned/transformed 祖先应锚定 ICB（视口原点 + inset）"
        );
    }

    #[test]
    fn absolute_anchor_reparents_nested_absolute_cb() {
        // 三期②嵌套：absolute 节点自身成为后代 absolute 的 cb（abs1 为
        // positioned）。abs1 锚 ICB（50,40）；abs2 跳过 static wrap 锚定
        // abs1 → (55,45)（旧：wrap 流内 70,60 + inset = 75,65）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.outer { width: 300px; padding: 10px; } \
                 div.abs1 { position: absolute; top: 40px; left: 50px; width: 100px; height: 50px; } \
                 div.wrap { margin-left: 20px; margin-top: 20px; } \
                 div.abs2 { position: absolute; top: 5px; left: 5px; }"
            )
            .is_clean());
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "outer").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "abs1").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(2)), "wrap").is_ok());
        assert!(mk(&mut engine, Key(4), Some(Key(3)), "abs2").is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b2 = frame.find(Key(2)).unwrap();
        assert_eq!((b2.x, b2.y), (50.0, 40.0), "abs1 无 positioned 祖先 → ICB");
        let b4 = frame.find(Key(4)).unwrap();
        assert_eq!(
            (b4.x, b4.y),
            (55.0, 45.0),
            "abs2 应锚定 absolute 祖先 abs1（嵌套 cb 链）"
        );
    }

    #[test]
    fn absolute_anchor_restores_after_style_change() {
        // 三期②恢复路径：样式表替换抹去 transformed/positioned 祖先后，
        // cb 集合变化 → 重挂残留归位，叶改锚 ICB（80,40 → 30,20）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.outer { position: relative; width: 400px; padding: 20px; } \
                 div.mid { transform: translate(0px, 0px); width: 200px; margin-left: 30px; } \
                 div.wrap { margin-left: 25px; } \
                 div.leaf { position: absolute; top: 20px; left: 30px; }"
                )
                .is_clean()
        );
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "outer").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "mid").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(2)), "wrap").is_ok());
        assert!(mk(&mut engine, Key(4), Some(Key(3)), "leaf").is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b4 = frame.find(Key(4)).unwrap();
        assert_eq!((b4.x, b4.y), (80.0, 40.0));
        // 替换样式表：去掉 transform 与 relative → cb 链瓦解 → ICB
        assert!(
            engine
                .set_stylesheet(
                    "div.outer { width: 400px; padding: 20px; } \
                 div.mid { width: 200px; margin-left: 30px; } \
                 div.wrap { margin-left: 25px; margin-top: 15px; } \
                 div.leaf { position: absolute; top: 20px; left: 30px; }"
                )
                .is_clean()
        );
        let frame2 = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b4b = frame2.find(Key(4)).unwrap();
        assert_eq!(
            (b4b.x, b4b.y),
            (30.0, 20.0),
            "cb 消失后应重锚 ICB（重挂残留归位）"
        );
    }

    #[test]
    fn transform_creates_stacking_context_order() {
        // ADR-0008/0009：非定位 transform ≠ none → SC，进 Pos 带键 0——
        // 树序在前的 B（transform）画在树序在后的 A 之上；无 transform 控制组相反。
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let build = |transformed: bool| -> Vec<crate::paint::PaintOp> {
            let mut engine: StyleEngine<Key> = StyleEngine::new();
            let tb = if transformed {
                " transform: translate(0px, 0px);"
            } else {
                ""
            };
            assert!(engine
                .set_stylesheet(&format!(
                    "div.a {{ width: 40px; height: 40px; background-color: #ff0000; }} div.b {{ width: 40px; height: 40px; background-color: #0000ff;{tb} }}"
                ))
                .is_clean());
            assert!(engine.insert(None, Key(1), node("")).is_ok());
            assert!(engine.insert(Some(Key(1)), Key(2), node("b")).is_ok());
            assert!(engine.insert(Some(Key(1)), Key(3), node("a")).is_ok());
            engine.frame((800.0, 600.0), 1.0, 0.0).paint.ops.to_vec()
        };
        let find = |ops: &[crate::paint::PaintOp], rgb: [f32; 3]| {
            ops.iter().position(|op| {
                matches!(op, crate::paint::PaintOp::FillRect { color, .. }
                    if color.components[0] == rgb[0]
                        && color.components[1] == rgb[1]
                        && color.components[2] == rgb[2])
            })
        };
        // 无 transform：Flow 带树序 → 蓝(B) 先画
        let plain = build(false);
        let (blue, red) = (find(&plain, [0.0, 0.0, 1.0]), find(&plain, [1.0, 0.0, 0.0]));
        assert!(blue < red, "控制组应树序绘制（blue={blue:?} red={red:?}）");
        // transform ≠ none：B 进 Pos 带 → 后画（覆盖 A）
        let sc = build(true);
        let (blue, red) = (find(&sc, [0.0, 0.0, 1.0]), find(&sc, [1.0, 0.0, 0.0]));
        assert!(
            blue > red,
            "transform SC 应后画（blue={blue:?} red={red:?}）"
        );
    }

    #[test]
    fn filter_clip_path_sc_effect_parse() {
        // 第四批④：filter/clip-path 仅存在性语义位（Effect）——none 缺席、
        // 任意值存在（多函数列表含内）；不做滤镜/裁剪效果实现
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.a { filter: blur(2px); } div.b { clip-path: inset(50%); } div.c { filter: none; } div.d { filter: blur(1px) saturate(2); }"
            )
            .is_clean());
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "a").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "b").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(1)), "c").is_ok());
        assert!(mk(&mut engine, Key(4), Some(Key(1)), "d").is_ok());
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let cs = |e: &StyleEngine<Key>, k: Key| {
            e.styles
                .get(e.key_to_node.get(&k).unwrap())
                .cloned()
                .unwrap()
        };
        assert!(cs(&engine, Key(1)).has_filter());
        assert!(!cs(&engine, Key(1)).has_clip_path());
        assert!(cs(&engine, Key(2)).has_clip_path());
        assert!(!cs(&engine, Key(3)).has_filter(), "none → 缺席");
        assert!(cs(&engine, Key(4)).has_filter(), "多函数列表 → 存在");
    }

    #[test]
    fn filter_clip_path_trigger_stacking_context_order() {
        // 第四批④：非定位 filter/clip-path ≠ none → SC（Pos 带键 0），后画
        // 覆盖树序控制组；且 op 总数与控制组一致（仅触发、不产生效果 PaintOp）
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let build = |trigger: &str| -> Vec<crate::paint::PaintOp> {
            let mut engine: StyleEngine<Key> = StyleEngine::new();
            assert!(engine
                .set_stylesheet(&format!(
                    "div.a {{ width: 40px; height: 40px; background-color: #ff0000; }} div.b {{ width: 40px; height: 40px; background-color: #0000ff;{trigger} }}"
                ))
                .is_clean());
            assert!(engine.insert(None, Key(1), node("")).is_ok());
            assert!(engine.insert(Some(Key(1)), Key(2), node("b")).is_ok());
            assert!(engine.insert(Some(Key(1)), Key(3), node("a")).is_ok());
            engine.frame((800.0, 600.0), 1.0, 0.0).paint.ops.to_vec()
        };
        let find = |ops: &[crate::paint::PaintOp], rgb: [f32; 3]| {
            ops.iter().position(|op| {
                matches!(op, crate::paint::PaintOp::FillRect { color, .. }
                    if color.components[0] == rgb[0]
                        && color.components[1] == rgb[1]
                        && color.components[2] == rgb[2])
            })
        };
        let plain = build("");
        let (blue, red) = (find(&plain, [0.0, 0.0, 1.0]), find(&plain, [1.0, 0.0, 0.0]));
        assert!(blue < red, "控制组应树序绘制（blue={blue:?} red={red:?}）");
        for (label, ops) in [
            ("filter", build(" filter: blur(0px);")),
            ("clip-path", build(" clip-path: inset(0);")),
            ("will-change", build(" will-change: transform;")),
            ("isolation", build(" isolation: isolate;")),
            ("mix-blend-mode", build(" mix-blend-mode: multiply;")),
        ] {
            let (blue, red) = (find(&ops, [0.0, 0.0, 1.0]), find(&ops, [1.0, 0.0, 0.0]));
            assert!(blue > red, "{label} SC 应后画（blue={blue:?} red={red:?}）");
            assert_eq!(ops.len(), plain.len(), "{label} 不应产生额外 PaintOp");
        }
    }

    #[test]
    fn sc_trigger_full_set_parse_semantics() {
        // 第五批㉒：SC 触发全集解析语义——will-change 按列表成员判定
        // （含触发属性才置位）、isolation/mix-blend-mode 按值判定；
        // 观测量 = 带序（触发 → 蓝(树序后)盖红；未触发 → 红盖蓝树序）
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let build = |trigger: &str| -> Vec<crate::paint::PaintOp> {
            let mut engine: StyleEngine<Key> = StyleEngine::new();
            assert!(engine
                .set_stylesheet(&format!(
                    "div.a {{ width: 40px; height: 40px; background-color: #ff0000; }} div.b {{ width: 40px; height: 40px; background-color: #0000ff;{trigger} }}"
                ))
                .is_clean());
            assert!(engine.insert(None, Key(1), node("")).is_ok());
            assert!(engine.insert(Some(Key(1)), Key(2), node("b")).is_ok());
            assert!(engine.insert(Some(Key(1)), Key(3), node("a")).is_ok());
            engine.frame((800.0, 600.0), 1.0, 0.0).paint.ops.to_vec()
        };
        let find = |ops: &[crate::paint::PaintOp], rgb: [f32; 3]| {
            ops.iter().position(|op| {
                matches!(op, crate::paint::PaintOp::FillRect { color, .. }
                    if color.components[0] == rgb[0]
                        && color.components[1] == rgb[1]
                        && color.components[2] == rgb[2])
            })
        };
        // 触发（b 应后画：blue > red），且不产生任何效果 PaintOp
        for (css, label) in [
            (" will-change: transform;", "will-change 触发属性"),
            (" will-change: transform, color;", "will-change 混合列表"),
            (" will-change: color, opacity;", "will-change 尾部触发"),
            (" isolation: isolate;", "isolation"),
            (" mix-blend-mode: multiply;", "mix-blend-mode"),
        ] {
            let ops = build(css);
            let (blue, red) = (find(&ops, [0.0, 0.0, 1.0]), find(&ops, [1.0, 0.0, 0.0]));
            assert!(blue > red, "{label} 应触发 SC（blue={blue:?} red={red:?}）");
            assert_eq!(ops.len(), build("").len(), "{label} 不应产生额外 PaintOp");
        }
        // 不触发（树序：blue < red）
        for (css, label) in [
            (" will-change: color;", "will-change color"),
            (" will-change: auto;", "will-change auto"),
            (" isolation: auto;", "isolation auto"),
            (" mix-blend-mode: normal;", "mix-blend-mode normal"),
        ] {
            let ops = build(css);
            let (blue, red) = (find(&ops, [0.0, 0.0, 1.0]), find(&ops, [1.0, 0.0, 0.0]));
            assert!(
                blue < red,
                "{label} 不应触发 SC（blue={blue:?} red={red:?}）"
            );
        }
    }

    #[test]
    fn transform_origin_parse_and_affine() {
        // 第五批⑬：transform-origin 解析（关键字/LP/单值缺省 center/关键字
        // 轴归类顺序宽容）+ paint 期 origin 环绕消费（A = T(o)·M·T(−o)）。
        // rotate(90deg) 盒 100×50：R = [0,1,−1,0,0,0]；
        // e = ox − (a·ox + c·oy)、f = oy − (b·ox + d·oy)。
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let affine =
            |css: &str| -> [f32; 6] {
                let mut engine: StyleEngine<Key> = StyleEngine::new();
                assert!(engine
                .set_stylesheet(&format!(
                    "div.b {{ width: 100px; height: 50px; transform: rotate(90deg); {css} }}"
                ))
                .is_clean());
                assert!(engine.insert(None, Key(1), node("")).is_ok());
                assert!(engine.insert(Some(Key(1)), Key(2), node("b")).is_ok());
                for op in engine.frame((800.0, 600.0), 1.0, 0.0).paint.ops.iter() {
                    if let crate::paint::PaintOp::PushTransform { affine } = op {
                        return *affine;
                    }
                }
                panic!("PushTransform 缺失");
            };
        let near = |got: [f32; 6], want: [f32; 6]| {
            for (g, w) in got.iter().zip(want.iter()) {
                assert!((g - w).abs() < 1e-4, "got {got:?} want {want:?}");
            }
        };
        // 默认 = 50% 50%（o=(50,25)）：e = 50 − (0·50 + (−1)·25) = 75、
        // f = 25 − (1·50 + 0·25) = −25
        near(affine(""), [0.0, 1.0, -1.0, 0.0, 75.0, -25.0]);
        near(
            affine(" transform-origin: center;"),
            [0.0, 1.0, -1.0, 0.0, 75.0, -25.0],
        );
        near(
            affine(" transform-origin: 50% 50%;"),
            [0.0, 1.0, -1.0, 0.0, 75.0, -25.0],
        );
        // 0 0 → 纯旋转
        near(
            affine(" transform-origin: 0 0;"),
            [0.0, 1.0, -1.0, 0.0, 0.0, 0.0],
        );
        near(
            affine(" transform-origin: left top;"),
            [0.0, 1.0, -1.0, 0.0, 0.0, 0.0],
        );
        // 顺序宽容：top left ≡ left top
        near(
            affine(" transform-origin: top left;"),
            [0.0, 1.0, -1.0, 0.0, 0.0, 0.0],
        );
        // 单值 top → (50%, 0%)：e = 50 − (0·50 + (−1)·0) = 50、f = 0 − (1·50) = −50
        near(
            affine(" transform-origin: top;"),
            [0.0, 1.0, -1.0, 0.0, 50.0, -50.0],
        );
        // 10px 20px：e = 10 − (0·10 + (−1)·20) = 30、f = 20 − (1·10 + 0·20) = 10
        near(
            affine(" transform-origin: 10px 20px;"),
            [0.0, 1.0, -1.0, 0.0, 30.0, 10.0],
        );
        // 无效值：声明丢弃（warn → 非 clean）
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            !engine
                .set_stylesheet("div.b { transform-origin: red; }")
                .is_clean()
        );
    }

    #[test]
    fn text_align_reaches_paint_op() {
        // 第五批⑳：text-align 实际消费——测量不变宽（盒宽/折行与对齐无关），
        // PaintOp::Text 携带声明值供 sink 折行后 align；默认 Start。
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let align_of = |css: &str| -> crate::css::property::TextAlign {
            let mut engine: StyleEngine<Key> = StyleEngine::new();
            assert!(
                engine
                    .set_stylesheet(&format!("div.b {{ width: 120px;{css} }}"))
                    .is_clean()
            );
            assert!(engine.insert(None, Key(1), node("")).is_ok());
            let mut leaf = node("b");
            leaf.text = Some("wrap me".into());
            assert!(engine.insert(Some(Key(1)), Key(2), leaf).is_ok());
            engine
                .frame((800.0, 600.0), 1.0, 0.0)
                .paint
                .ops
                .iter()
                .find_map(|op| match op {
                    crate::paint::PaintOp::Text { text_align, .. } => Some(*text_align),
                    _ => None,
                })
                .expect("text op 缺失")
        };
        assert!(matches!(
            align_of(""),
            crate::css::property::TextAlign::Start
        ));
        assert!(matches!(
            align_of(" text-align: center;"),
            crate::css::property::TextAlign::Center
        ));
        assert!(matches!(
            align_of(" text-align: right;"),
            crate::css::property::TextAlign::Right
        ));
        assert!(matches!(
            align_of(" text-align: justify;"),
            crate::css::property::TextAlign::Justify
        ));
    }

    #[test]
    fn text_leaf_block_stretch_and_declared_width() {
        // 第五批⑥文本叶语义契约：无声明宽度的文本叶在块流中拉伸到容器
        // 内容宽（与浏览器匿名块盒一致；旧实现=测量自然宽）；声明宽度
        // 优先于测量；高度=测量内容高（有字形非零）。
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.p { width: 220px; font-family: \"DejaVu Sans\"; font-size: 16px; } div.fix { width: 120px; font-family: \"DejaVu Sans\"; font-size: 16px; }"
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p")).is_ok());
        let mut stretch = node("auto");
        stretch.text = Some("shrink wrap candidate".into());
        assert!(engine.insert(Some(Key(1)), Key(2), stretch).is_ok());
        let mut fixed = node("fix");
        fixed.text = Some("shrink wrap candidate".into());
        assert!(engine.insert(Some(Key(1)), Key(3), fixed).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let st = frame.find(Key(2)).unwrap();
        // 自然宽 < 220：叶盒应 = 220（拉伸），而非测量自然宽
        assert!((st.width - 220.0).abs() < 0.5, "leaf width {}", st.width);
        let fx = frame.find(Key(3)).unwrap();
        // 声明宽度优先：120 而非测量值
        assert!((fx.width - 120.0).abs() < 0.5, "fixed width {}", fx.width);
        assert!(st.height > 0.0, "内容高应非零");
    }

    #[test]
    fn margin_collapse_block_siblings() {
        // 第五批⑦评估：taffy 0.14 block 算法内置纵向 margin collapsing
        // （CollapsibleMarginSet/ strut）——相邻兄弟纵 margin 取 max
        // （bottom 20 vs top 30 → 间距 30，非相加 50）；flex 子项不折叠
        // （CSS 语义，flex 上下文无折叠）。
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.p { width: 200px; } div.a { height: 40px; margin-bottom: 20px; background-color: #ff0000; } div.b { height: 40px; margin-top: 30px; background-color: #0000ff; }"
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), node("a")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), node("b")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let a = frame.find(Key(2)).unwrap();
        let b = frame.find(Key(3)).unwrap();
        let gap = b.y - (a.y + a.height);
        assert!((gap - 30.0).abs() < 0.5, "折叠间距应为 30，实测 {gap}");
        // flex 上下文不折叠（CSS：flex 子项 margin 永不相邻，间距相加）
        let mut engine2: StyleEngine<Key> = StyleEngine::new();
        assert!(engine2
            .set_stylesheet(
                "div.p { display: flex; flex-direction: column; width: 200px; } div.a { height: 40px; margin-bottom: 20px; } div.b { height: 40px; margin-top: 30px; }"
            )
            .is_clean());
        assert!(engine2.insert(None, Key(1), node("p")).is_ok());
        assert!(engine2.insert(Some(Key(1)), Key(2), node("a")).is_ok());
        assert!(engine2.insert(Some(Key(1)), Key(3), node("b")).is_ok());
        let frame2 = engine2.frame((800.0, 600.0), 1.0, 0.0);
        let fa = frame2.find(Key(2)).unwrap();
        let fb = frame2.find(Key(3)).unwrap();
        let gap2 = fb.y - (fa.y + fa.height);
        assert!((gap2 - 50.0).abs() < 0.5, "flex 间距应相加 50，实测 {gap2}");
    }

    #[test]
    fn display_inline_contractual_block_participation() {
        // 第五批⑧契约：display:inline* 解析接受并归一（无 IFC）——inline
        // / inline-block 元素按 block 参与布局（纵向堆叠，无行盒并排），
        // 声明不丢弃（is_clean 保持 true，归一化走 tracing 告警非报告失败）。
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.p { width: 300px; } div.a { display: inline; height: 40px; background-color: #ff0000; } div.b { display: inline-block; height: 40px; background-color: #0000ff; }"
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), node("a")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), node("b")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let a = frame.find(Key(2)).unwrap();
        let b = frame.find(Key(3)).unwrap();
        // 纵向堆叠：b 顶 = a 底（block 参与），而非行盒并排
        assert!((b.y - (a.y + a.height)).abs() < 0.5, "inline 应块化堆叠");
        assert!(a.width > 0.0 && b.width > 0.0);
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
        // 第五批⑥契约：host 测量只供固有尺寸——列 flex 交叉轴 stretch 拉
        // 伸到容器内容宽（浏览器 flex 项一致），主轴高 = 内容高 20
        assert_eq!((b.width, b.height), (800.0, 20.0));
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

    #[test]
    fn absolute_text_leaf_sizes() {
        // ⑥ bidi-mixed 回归：auto 宽绝对文本叶取测量宽（restyle 的 absolute
        // 例外须查本地 cs 的 position——self.styles 此刻尚未 insert，查表恒
        // None 会把测量宽丢成 auto，taffy 绝对布局无测量回退得 0）；声明宽
        // 叶按声明宽折行（T5d 换行宽声明优先，非 shrink 夹紧宽）；混排与
        // 纯 RTL 测量宽均为正且与 Chromium golden 同容差。
        let mut engine: StyleEngine<u32> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "body { margin: 0 } .host { position: relative; width: 600px; height: 200px } \
                 .mix { position: absolute; top: 0px; left: 0px; font-family: \"DejaVu Sans\"; font-size: 16px } \
                 .rtl { position: absolute; top: 30px; left: 0px; font-family: \"DejaVu Sans\"; font-size: 16px } \
                 .wrap { position: absolute; top: 60px; left: 0px; width: 140px; font-family: \"DejaVu Sans\"; font-size: 16px }",
            )
            .is_clean());
        let mk = |class: &str, text: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(class.to_string()).collect(),
            text: Some(text.into()),
            ..Default::default()
        };
        assert!(engine.insert(None, 1, mk("host", "")).is_ok());
        assert!(
            engine
                .insert(Some(1), 2, mk("mix", "Hello שלום world עברית 42"))
                .is_ok()
        );
        assert!(engine.insert(Some(1), 3, mk("rtl", "עברית")).is_ok());
        assert!(
            engine
                .insert(Some(1), 4, mk("wrap", "Hello שלום world עברית 42 tail"))
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b2 = frame.find(2).unwrap();
        assert_eq!((b2.x, b2.y), (0.0, 0.0));
        // 测量宽经 taffy 舍入取整（与 conformance 0.5px 容差同源）
        assert!((b2.width - 203.10938).abs() < 0.5, "w={}", b2.width);
        assert_eq!(b2.height, 19.0);
        let b3 = frame.find(3).unwrap();
        assert!((b3.width - 42.390625).abs() < 0.5, "w={}", b3.width);
        assert_eq!((b3.y, b3.height), (30.0, 19.0));
        let b4 = frame.find(4).unwrap();
        assert_eq!((b4.x, b4.y, b4.width), (0.0, 60.0, 140.0));
        assert_eq!(b4.height, 38.0);
    }
}
