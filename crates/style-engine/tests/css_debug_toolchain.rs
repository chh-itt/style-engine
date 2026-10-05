//! 调试工具链锁测试（F3e，ADR-0027 D1）：三个零新状态人读视图的输出契约。
//!
//! 锁定面：
//! 1. `ComputedStyle::debug_dump` —— 显式物化槽位行（`css_name: {Debug}`）
//!    + custom properties（`--name: value`）+ 度量行；纯投影不改级联。
//! 2. `debug::display_list_dump` —— Push/Pop 树缩进（PushClip 子作用域内
//!    的 op 缩进深于顶层）+ 头行汇总（ops/generation）。
//! 3. `StyleEngine::layout_tree_dump` + `Frame::boxes_dump` —— 结构树视图
//!    （标签/键/缩进）与几何盒视图（key/x/y/w/h）分离且各自树序。

use style_engine::debug::display_list_dump;
use style_engine::tree::StyleNode;
use style_engine::{Frame, StyleEngine};

fn node(name: &str, classes: &[&str], text: &str) -> StyleNode {
    let mut n = StyleNode::default();
    if !name.is_empty() {
        n.name = Some(name.to_string());
    }
    n.classes = classes.iter().map(|c| c.to_string()).collect();
    if !text.is_empty() {
        n.text = Some(text.to_string());
    }
    n
}

#[test]
fn computed_style_dump_lists_slots_and_customs() {
    let mut e: StyleEngine<u64> = StyleEngine::new();
    let report =
        e.set_stylesheet(".leaf { background-color: #3366cc; color: white; --brand: crimson; }");
    assert!(report.is_clean(), "{report:?}");
    e.insert(None, 1, node("div", &["page"], "")).unwrap();
    e.insert(Some(1), 2, node("span", &["leaf"], "hi")).unwrap();
    let _ = e.frame((400.0, 300.0), 1.0, 0.0);

    let dump = e.computed_style(2).expect("leaf computed").debug_dump();
    // 显式物化槽位（引擎路径全集物化，全部槽位有行）
    assert!(dump.contains("background-color: "), "dump:\n{dump}");
    assert!(dump.contains("color: "), "dump:\n{dump}");
    // custom properties 终值（字典序段）
    assert!(dump.contains("--brand: crimson"), "dump:\n{dump}");
    // 度量行
    assert!(dump.contains("[metrics] ch="), "dump:\n{dump}");
    // 无伪元素标记
    assert!(!dump.contains("[pseudo]"), "dump:\n{dump}");
}

#[test]
fn display_list_dump_indents_push_pop_scopes() {
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet(
        "#page { background-color: white; width: 300px; height: 200px; } \
         .leaf { overflow: hidden; background-color: red; } \
         .chip { background-color: blue; width: 10px; height: 10px; }",
    );
    let mut page = node("div", &[], "");
    page.id = Some("page".to_string());
    e.insert(None, 1, page).unwrap();
    e.insert(Some(1), 2, node("div", &["leaf"], "")).unwrap();
    e.insert(Some(2), 3, node("div", &["chip"], "")).unwrap();
    let frame = e.frame((400.0, 300.0), 1.0, 0.0);

    let dump = display_list_dump(&frame.paint);
    let mut lines = dump.lines();
    let header = lines.next().expect("header");
    assert!(header.starts_with("DisplayList ops="), "header: {header}");
    assert!(header.contains(&format!("generation={}", frame.generation)));

    // 绘制语义：元素自身背景先于其 overflow 作用域绘制，故 PushClip 行
    // 印在父级缩进层；Push/Pop 之间（子树）的 op 才缩进更深 2 格。
    let lines: Vec<&str> = dump.lines().collect();
    let indent_of = |l: &str| l.len() - l.trim_start().len();
    let top = lines
        .iter()
        .copied()
        .find(|l| l.contains("[000]"))
        .expect("first op line");
    let pi = lines
        .iter()
        .position(|l| l.contains("PushClip"))
        .expect("PushClip present (overflow: hidden)");
    let push = lines[pi];
    let child = lines.get(pi + 1).copied().expect("op inside clip scope");
    let pop = lines
        .iter()
        .rev()
        .copied()
        .find(|l| l.contains("PopClip"))
        .expect("PopClip present");
    assert_eq!(
        indent_of(push),
        indent_of(top),
        "push opens at parent level:\npush:\n{push}\ntop:\n{top}"
    );
    assert_eq!(
        indent_of(child),
        indent_of(push) + 2,
        "child op indented inside scope:\nchild:\n{child}\npush:\n{push}"
    );
    assert_eq!(
        indent_of(pop),
        indent_of(push),
        "pop closes back at parent level:\npop:\n{pop}\npush:\n{push}"
    );
}

#[test]
fn layout_tree_and_boxes_dumps_separate_structure_and_geometry() {
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet(
        "#page { display: flex; padding: 10px; } \
         .btn { width: 80px; height: 24px; }",
    );
    let mut page = node("div", &[], "");
    page.id = Some("page".to_string());
    e.insert(None, 1, page).unwrap();
    let mut btn = node("button", &["btn"], "OK");
    btn.id = Some("submit".to_string());
    e.insert(Some(1), 2, btn).unwrap();
    let frame: Frame<u64> = e.frame((400.0, 300.0), 1.0, 0.0);

    // 结构树：显示序 + 深度缩进 + 键/类/id/文本标签。
    let tree = e.layout_tree_dump();
    let root_line = tree
        .lines()
        .find(|l| l.contains("key=1"))
        .expect("root line");
    let btn_line = tree
        .lines()
        .find(|l| l.contains("key=2"))
        .expect("button line");
    assert!(root_line.contains("<div #page>"), "root: {root_line}");
    assert!(
        btn_line.contains("<button #submit.btn>") && btn_line.contains("text=\"OK\""),
        "btn: {btn_line}"
    );
    assert!(
        btn_line.starts_with("  ") && !root_line.starts_with(' '),
        "depth indent:\n{tree}"
    );

    // 几何盒：树序逐盒 key/x/y/w/h；子盒在根盒之后。
    let boxes = frame.boxes_dump();
    let rb = boxes
        .lines()
        .find(|l| l.contains("key=1"))
        .expect("root box");
    let bb = boxes
        .lines()
        .find(|l| l.contains("key=2"))
        .expect("button box");
    assert!(rb.contains("x=") && rb.contains("w="), "root box: {rb}");
    assert!(
        bb.contains("w=80") || bb.contains("w=80.00"),
        "btn box: {bb}"
    );
    let r_pos = boxes.find("key=1").expect("root idx");
    let b_pos = boxes.find("key=2").expect("btn idx");
    assert!(r_pos < b_pos, "tree order: {boxes}");
}
