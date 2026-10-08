//! 差分测试基座（实施期 Phase 0 前置⑥）：**增量引擎链 ≡ 全量重建**。
//!
//! 不变量：同一操作序列，`hot` 引擎逐 op 推进并在每步 `frame()`（增量
//! 路径：restyle_subtrees / 各结算缓存 / 容器快照 / 文本探针缓存全部
//! 跨帧存续），与 `cold` 引擎（每步从零重建、按已生效操作全量重放、
//! 单帧出结果）在布局盒、DisplayList、滚动量程三面**逐位相等**——
//! 增量捷径若漏失效/错失效，此处即暴露。
//!
//! 双样式表跑两轮：无 @container（命中 `restyle_subtrees` 增量路径）与
//! 有 @container（`has_container_rules` 退全量 restyle + 容器快照收敛）。
//! 附带锁定 frame() 幂等不变量（SETTLEMENT-PIPELINE：无变更再帧逐位
//! 一致，generation 除外）。
//!
//! proptest 仅 dev-dep（C4 纪律）；失败时输出最小化 op 序列，可直接
//! 回放（`replay_debug`）。

use proptest::prelude::*;
use style_engine::StyleEngine;
use style_engine::engine::Frame;
use style_engine::tree::{NodeState, StyleNode};

// ---------- 操作模型 ----------

/// 随机操作（key 全域 1..KEY_SPAN，跳过当前状态不合法的 op）。
#[derive(Debug, Clone)]
enum Op {
    /// 插入（parent=None 仅在无根时生效）。
    Insert {
        parent: Option<u64>,
        key: u64,
        class: usize,
        text: usize,
        decl: usize,
    },
    /// 移除子树（根移除=清空引擎）。
    Remove {
        key: u64,
    },
    /// 子序重排（children 轮转 perm%len 位）。
    Reorder {
        key: u64,
        perm: usize,
    },
    SetClasses {
        key: u64,
        class: usize,
    },
    SetState {
        key: u64,
        state: usize,
    },
    SetText {
        key: u64,
        text: usize,
    },
    SetDecls {
        key: u64,
        decl: usize,
    },
}

impl Arbitrary for Op {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;
    fn arbitrary_with(_: ()) -> Self::Strategy {
        prop_oneof![
            5 => (prop::option::of(1u64..KEY_SPAN), 1u64..KEY_SPAN, 0usize..CLASSES.len(), 0usize..TEXTS.len(), 0usize..DECLS.len())
                .prop_map(|(parent, key, class, text, decl)| Op::Insert { parent, key, class, text, decl }),
            1 => (1u64..KEY_SPAN).prop_map(|key| Op::Remove { key }),
            1 => (1u64..KEY_SPAN, 0usize..8).prop_map(|(key, perm)| Op::Reorder { key, perm }),
            2 => (1u64..KEY_SPAN, 0usize..CLASSES.len()).prop_map(|(key, class)| Op::SetClasses { key, class }),
            1 => (1u64..KEY_SPAN, 0usize..8).prop_map(|(key, state)| Op::SetState { key, state }),
            2 => (1u64..KEY_SPAN, 0usize..TEXTS.len()).prop_map(|(key, text)| Op::SetText { key, text }),
            4 => (1u64..KEY_SPAN, 0usize..DECLS.len()).prop_map(|(key, decl)| Op::SetDecls { key, decl }),
        ]
        .boxed()
    }
}

const KEY_SPAN: u64 = 17;

/// class 池（与两张样式表并集对齐）。
const CLASSES: &[&[&str]] = &[
    &[],
    &["box"],
    &["item"],
    &["row"],
    &["col"],
    &["texty"],
    &["abs"],
    &["scroll"],
    &["ct"],
    &["cq"],
    &["box", "item"],
];

/// 状态位池（bitflags 2 的 BitOr 非 const，运行时构造）。
fn states() -> Vec<NodeState> {
    vec![
        NodeState::empty(),
        NodeState::HOVER,
        NodeState::ACTIVE,
        NodeState::FOCUS,
        NodeState::HOVER | NodeState::ACTIVE,
        NodeState::FOCUS | NodeState::FOCUS_WITHIN,
        NodeState::DISABLED,
        NodeState::CHECKED,
    ]
}

/// 文本池（0 号=None；text 特性下注册 DejaVu 实测，无字体环境测量为 0 亦确定）。
const TEXTS: &[Option<&str>] = &[
    None,
    Some("Hello world"),
    Some("Lorem ipsum dolor sit amet consectetur adipiscing"),
    Some("中文文本在包含块内容宽约束下折行的一段示例"),
    Some("ab"),
];

/// 内联声明池（覆盖盒模型/flex/absolute/overflow/transform/渐变/百分比）。
const DECLS: &[&str] = &[
    "",
    "width: 100px",
    "height: 40px",
    "width: 50%",
    "padding: 8px; border: 2px solid #333",
    "margin: 4px 8px",
    "background-color: #48c",
    "color: #c33; font-size: 13px",
    "display: flex; gap: 6px",
    "flex-direction: column",
    "position: absolute; top: 12px; left: 8px",
    "position: absolute; right: 4px; bottom: 4px",
    "opacity: 0.6",
    "overflow: scroll; height: 60px",
    "transform: translate(4px, 8px)",
    "min-width: 30px; max-width: 120px",
    "width: 60px; height: 24px; background: linear-gradient(90deg, #f00, #00f)",
    "flex: 1",
    "width: 200px",
    "background: radial-gradient(circle at 30% 30%, #ff0, #0f0)",
];

/// 基座样式表（**无** @container——命中 restyle_subtrees 增量路径）。
const SHEET_BASE: &str = r#"
#root { width: 400px; height: 300px; background: #e8e8e8; }
.box { padding: 4px; border: 1px solid #999; }
.item { margin: 2px; width: 60px; height: 20px; background: #cc4; }
.row { display: flex; gap: 6px; }
.col { display: flex; flex-direction: column; gap: 4px; }
.texty { font-size: 12px; color: #333; font-family: "DejaVu Sans"; }
.abs { position: absolute; top: 10px; left: 10px; }
.scroll { overflow: scroll; height: 60px; }
.ct { container-type: size; }
.cq { width: 80px; height: 16px; background: #4c8; }
.item:hover { background: #88f; }
.item:focus { border-color: #08f; }
div > .item { width: 70px; }
.item:first-child { margin-top: 8px; }
.item:last-child { margin-bottom: 8px; }
div + .box { margin-left: 12px; }
@media (min-width: 200px) { .row { gap: 10px; } }
"#;

/// 容器样式表（基座 + @container——has_container_rules 退全量 restyle）。
const SHEET_CONTAINER: &str = r#"
#root { width: 400px; height: 300px; background: #e8e8e8; }
.box { padding: 4px; border: 1px solid #999; }
.item { margin: 2px; width: 60px; height: 20px; background: #cc4; }
.row { display: flex; gap: 6px; }
.col { display: flex; flex-direction: column; gap: 4px; }
.texty { font-size: 12px; color: #333; font-family: "DejaVu Sans"; }
.abs { position: absolute; top: 10px; left: 10px; }
.scroll { overflow: scroll; height: 60px; }
.ct { container-type: size; }
.cq { width: 80px; height: 16px; background: #4c8; }
.item:hover { background: #88f; }
.item:focus { border-color: #08f; }
div > .item { width: 70px; }
.item:first-child { margin-top: 8px; }
.item:last-child { margin-bottom: 8px; }
div + .box { margin-left: 12px; }
@media (min-width: 200px) { .row { gap: 10px; } }
@container (min-width: 300px) { .cq { background: #c48; width: 150px; } }
@container (max-width: 299px) { .cq { background: #48c; } }
"#;

// ---------- 阴影树与驱动 ----------

#[derive(Default)]
struct Shadow {
    parent: std::collections::HashMap<u64, Option<u64>>,
    children: std::collections::HashMap<u64, Vec<u64>>,
    alive: std::collections::HashSet<u64>,
    root: Option<u64>,
}

impl Shadow {
    fn subtree_keys(&self, key: u64) -> Vec<u64> {
        let mut out = Vec::new();
        let mut stack = vec![key];
        while let Some(k) = stack.pop() {
            out.push(k);
            stack.extend(self.children.get(&k).cloned().unwrap_or_default());
        }
        out
    }
}

/// 已生效操作（重放日志——concrete、保序、全合法）。
#[derive(Debug, Clone)]
enum Applied {
    Root(u64, StyleNode, String),
    Child(u64, u64, StyleNode, String),
    Remove(u64),
    Reorder(u64, Vec<u64>),
    Classes(u64, Vec<String>),
    State(u64, NodeState),
    Text(u64, Option<String>),
    Decls(u64, String),
}

fn node_from(class: usize, text: usize, key: u64, is_root: bool) -> StyleNode {
    StyleNode {
        name: Some(["div", "span", "button"][key as usize % 3].to_string()),
        id: Some(if is_root {
            "root".to_string()
        } else {
            format!("n{key}")
        }),
        classes: CLASSES[class % CLASSES.len()]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        text: TEXTS[text % TEXTS.len()].map(|s| s.to_string()),
        ..StyleNode::default()
    }
}

struct Driver {
    hot: StyleEngine<u64>,
    shadow: Shadow,
    applied: Vec<Applied>,
}

impl Driver {
    fn new(sheet: &'static str) -> Self {
        let mut hot: StyleEngine<u64> = StyleEngine::new();
        hot.set_stylesheet(sheet);
        register_fonts(&mut hot);
        Self {
            hot,
            shadow: Shadow::default(),
            applied: Vec::new(),
        }
    }

    /// 应用一个 op 到 hot 引擎 + 阴影树；不合法则跳过（返回 false）。
    fn apply(&mut self, op: &Op) -> bool {
        match *op {
            Op::Insert {
                parent,
                key,
                class,
                text,
                decl,
            } => {
                if self.shadow.alive.contains(&key) {
                    return false;
                }
                match parent {
                    None => {
                        if self.shadow.root.is_some() {
                            return false;
                        }
                        let node = node_from(class, text, key, true);
                        if self.hot.insert(None, key, node.clone()).is_err() {
                            return false;
                        }
                        let decl_text = DECLS[decl % DECLS.len()].to_string();
                        let _ = self.hot.set_declarations(key, &decl_text);
                        self.shadow.alive.insert(key);
                        self.shadow.parent.insert(key, None);
                        self.shadow.children.entry(key).or_default();
                        self.shadow.root = Some(key);
                        self.applied.push(Applied::Root(key, node, decl_text));
                    }
                    Some(pk) => {
                        if !self.shadow.alive.contains(&pk) {
                            return false;
                        }
                        let node = node_from(class, text, key, false);
                        if self.hot.insert(Some(pk), key, node.clone()).is_err() {
                            return false;
                        }
                        let decl_text = DECLS[decl % DECLS.len()].to_string();
                        let _ = self.hot.set_declarations(key, &decl_text);
                        self.shadow.alive.insert(key);
                        self.shadow.parent.insert(key, Some(pk));
                        self.shadow.children.entry(key).or_default();
                        self.shadow.children.entry(pk).or_default().push(key);
                        self.applied.push(Applied::Child(pk, key, node, decl_text));
                    }
                }
                true
            }
            Op::Remove { key } => {
                if !self.shadow.alive.contains(&key) {
                    return false;
                }
                if self.hot.remove(key).is_err() {
                    return false;
                }
                for k in self.shadow.subtree_keys(key) {
                    self.shadow.alive.remove(&k);
                    self.shadow.children.remove(&k);
                    self.shadow.parent.remove(&k);
                }
                if self.shadow.root == Some(key) {
                    self.shadow.root = None;
                    self.shadow.children.clear();
                    self.shadow.alive.clear();
                    self.shadow.parent.clear();
                } else if let Some(Some(pk)) = self.shadow.parent.get(&key).copied()
                    && let Some(list) = self.shadow.children.get_mut(&pk)
                {
                    list.retain(|c| *c != key);
                }
                self.applied.push(Applied::Remove(key));
                true
            }
            Op::Reorder { key, perm } => {
                let Some(kids) = self.shadow.children.get(&key).cloned() else {
                    return false;
                };
                if kids.len() < 2 {
                    return false;
                }
                let r = perm % kids.len();
                let mut rotated = kids.clone();
                rotated.rotate_left(r);
                if self.hot.set_children(key, &rotated).is_err() {
                    return false;
                }
                self.shadow.children.insert(key, rotated.clone());
                self.applied.push(Applied::Reorder(key, rotated));
                true
            }
            Op::SetClasses { key, class } => {
                if !self.shadow.alive.contains(&key) {
                    return false;
                }
                let classes: Vec<String> = CLASSES[class % CLASSES.len()]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                if self.hot.set_classes(key, &classes).is_err() {
                    return false;
                }
                self.applied.push(Applied::Classes(key, classes));
                true
            }
            Op::SetState { key, state } => {
                if !self.shadow.alive.contains(&key) {
                    return false;
                }
                let st = states()[state % 8];
                if self.hot.set_state(key, st).is_err() {
                    return false;
                }
                self.applied.push(Applied::State(key, st));
                true
            }
            Op::SetText { key, text } => {
                if !self.shadow.alive.contains(&key) {
                    return false;
                }
                let t = TEXTS[text % TEXTS.len()].map(|s| s.to_string());
                if self.hot.set_text(key, t.clone()).is_err() {
                    return false;
                }
                self.applied.push(Applied::Text(key, t));
                true
            }
            Op::SetDecls { key, decl } => {
                if !self.shadow.alive.contains(&key) {
                    return false;
                }
                let d = DECLS[decl % DECLS.len()].to_string();
                if self.hot.set_declarations(key, &d).is_err() {
                    return false;
                }
                self.applied.push(Applied::Decls(key, d));
                true
            }
        }
    }
}

#[cfg(feature = "text")]
fn register_fonts(engine: &mut StyleEngine<u64>) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../style-engine-demo/assets/fonts/DejaVuSans.ttf");
    if let Ok(bytes) = std::fs::read(path) {
        engine.add_font(bytes);
    }
}

#[cfg(not(feature = "text"))]
fn register_fonts(_engine: &mut StyleEngine<u64>) {}

/// 从零重建引擎：重放全部已生效操作，单帧出结果。
fn cold_frame(sheet: &str, applied: &[Applied]) -> Frame<u64> {
    let mut cold: StyleEngine<u64> = StyleEngine::new();
    cold.set_stylesheet(sheet);
    register_fonts(&mut cold);
    for a in applied {
        match a {
            Applied::Root(key, node, decls) => {
                cold.insert(None, *key, node.clone()).expect("重放 Root");
                cold.set_declarations(*key, decls).expect("重放 Root decls");
            }
            Applied::Child(pk, key, node, decls) => {
                cold.insert(Some(*pk), *key, node.clone())
                    .expect("重放 Child");
                cold.set_declarations(*key, decls)
                    .expect("重放 Child decls");
            }
            Applied::Remove(key) => {
                cold.remove(*key).expect("重放 Remove");
            }
            Applied::Reorder(key, order) => {
                cold.set_children(*key, order).expect("重放 Reorder");
            }
            Applied::Classes(key, classes) => {
                cold.set_classes(*key, classes).expect("重放 Classes");
            }
            Applied::State(key, st) => {
                cold.set_state(*key, *st).expect("重放 State");
            }
            Applied::Text(key, text) => {
                cold.set_text(*key, text.clone()).expect("重放 Text");
            }
            Applied::Decls(key, decls) => {
                cold.set_declarations(*key, decls).expect("重放 Decls");
            }
        }
    }
    cold.frame((VIEWPORT.0, VIEWPORT.1), 1.0, 0.0)
}

const VIEWPORT: (f32, f32) = (400.0, 300.0);

/// 三面逐位断言：boxes / paint ops / scrollable（frame 世代号除外——
/// `DisplayList.generation` 与 `Frame.generation` 同为单调帧号，逐帧递增）。
fn assert_frame_eq(hot: &Frame<u64>, cold: &Frame<u64>, ctx: &str) {
    assert_eq!(hot.boxes, cold.boxes, "{ctx}: 布局盒不等");
    assert_eq!(hot.paint.ops, cold.paint.ops, "{ctx}: DisplayList 不等");
    assert_eq!(hot.scrollable, cold.scrollable, "{ctx}: 滚动量程不等");
}

/// 跑一条 op 序列：每步应用→hot 帧两次（第二帧验幂等）→cold 重建帧→三面比较。
fn run_case(sheet: &'static str, ops: &[Op], label: &str) {
    let mut driver = Driver::new(sheet);
    for (step, op) in ops.iter().enumerate() {
        if !driver.apply(op) {
            continue;
        }
        let ctx = format!("[{label} step {step}] {op:?}");
        let hot1 = driver.hot.frame((VIEWPORT.0, VIEWPORT.1), 1.0, 0.0);
        let hot2 = driver.hot.frame((VIEWPORT.0, VIEWPORT.1), 1.0, 0.0);
        assert_frame_eq(&hot1, &hot2, &format!("{ctx}（frame 幂等性）"));
        let cold = cold_frame(sheet, &driver.applied);
        assert_frame_eq(&hot1, &cold, &ctx);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn incremental_matches_full_recompute(ops in prop::collection::vec(any::<Op>(), 0..70)) {
        run_case(SHEET_BASE, &ops, "base");
    }

    #[test]
    fn incremental_matches_full_recompute_with_container(ops in prop::collection::vec(any::<Op>(), 0..70)) {
        run_case(SHEET_CONTAINER, &ops, "container");
    }
}
