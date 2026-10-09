//! P9-4 容器失效收窄锁测试（ADR-0035 D2 的 P9-4 增量门收窄）。
//!
//! 不变量：
//! 1. 快照表空（无 container-type 元素）→ 容器规则不可命中 → 脏根走
//!    `restyle_subtrees` 增量路径，普通声明变更照常生效；
//! 2. 容器后出现（container-type 类加入）→ record_container_sizes 收敛
//!    环驱动一次全量 pass → 容器规则以新鲜快照命中；
//! 3. 容器移除 → 规则停止命中（快照表重建为空）。
//!
//! 度量尺 = .cq 盒几何（80px↔150px）与 FillRect 背景色（#4c8↔#c48）。

#![cfg(feature = "layout")]

use style_engine::StyleEngine;
use style_engine::tree::StyleNode;

const SHEET: &str = r#"
#root { width: 400px; height: 300px; }
#wrap { width: 360px; height: 200px; }
.ct { container-type: size; }
.cq { width: 80px; height: 16px; background: #4c8; }
@container (min-width: 300px) { .cq { background: #c48; width: 150px; } }
"#;

fn node(id: &str, classes: &[String], text: Option<&str>) -> StyleNode {
    StyleNode {
        id: Some(id.to_string()),
        classes: classes.to_vec().into(),
        text: text.map(|s| s.to_string()),
        ..StyleNode::default()
    }
}

fn engine() -> StyleEngine<u64> {
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet(SHEET);
    e.insert(None, 1, node("root", &[], None)).unwrap();
    e.insert(Some(1), 2, node("wrap", &[], None)).unwrap();
    e.insert(Some(2), 3, node("cq", &["cq".to_string()], None))
        .unwrap();
    e
}

/// (key, width) + FillRect 背景色（按盒矩形匹配）。
fn probe(e: &mut StyleEngine<u64>, key: u64) -> (f32, Option<[f32; 4]>) {
    let fr = e.frame((800.0, 600.0), 1.0, 0.0);
    let b = fr.find(key).unwrap();
    let mut bg = None;
    for op in fr.paint.ops.iter() {
        if let style_engine::PaintOp::FillRect {
            x,
            y,
            width,
            height: _,
            color,
            ..
        } = op
            && (*x - b.x).abs() < 0.5
            && (*y - b.y).abs() < 0.5
            && (*width - b.width).abs() < 0.5
        {
            bg = Some(color.components);
        }
    }
    (b.width, bg)
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.5
}

#[test]
fn container_rules_without_containers_take_incremental_path() {
    // 无 .ct → 快照表空 → SetDecls 增量路径；普通声明照常生效，容器
    // 规则不命中（宽 80 / #4c8）。
    let mut e = engine();
    let (w, bg) = probe(&mut e, 3);
    assert!(near(w, 80.0), "初始宽 {w}");
    assert!(bg.unwrap()[0] < 0.4, "初始背景应为 #4c8");
    // 触发增量重样式：无关声明变更。
    e.set_declarations(3, "height: 16px; width: 80px").unwrap();
    let (w, bg) = probe(&mut e, 3);
    assert!(near(w, 80.0), "无容器时规则不命中，宽不变 {w}");
    let bg = bg.unwrap();
    assert!(bg[0] < 0.4 && bg[2] > 0.4, "背景仍 #4c8 {bg:?}");
}

#[test]
fn container_appearing_later_converges_to_match() {
    // 容器后出现：加 .ct 到 wrap → record_container_sizes 驱动收敛 →
    // 规则命中（宽 150 / #c48）。
    let mut e = engine();
    e.set_classes(2, &["ct".to_string()]).unwrap();
    let (w, bg) = probe(&mut e, 3);
    assert!(near(w, 150.0), "容器出现后规则应命中（宽 {w}）");
    let bg = bg.unwrap();
    assert!(bg[0] > 0.6 && bg[1] < 0.4, "背景应 #c48 {bg:?}");
}

#[test]
fn container_removed_stops_matching() {
    // 容器移除：快照表重建为空 → 规则停止命中 → 回落 80 / #4c8。
    let mut e = engine();
    e.set_classes(2, &["ct".to_string()]).unwrap();
    let (w, _) = probe(&mut e, 3);
    assert!(near(w, 150.0));
    e.set_classes(2, &[]).unwrap();
    let (w, bg) = probe(&mut e, 3);
    assert!(near(w, 80.0), "容器移除后应回落（宽 {w}）");
    let bg = bg.unwrap();
    assert!(bg[0] < 0.4, "背景应回落 #4c8 {bg:?}");
}

#[test]
fn container_size_change_rematches_within_frame() {
    // 容器尺寸变化（360→200，min-width:300 不再满足）→ 同帧收敛环
    // 重匹配 → 宽回落 80。
    let mut e = engine();
    e.set_classes(2, &["ct".to_string()]).unwrap();
    let (w, _) = probe(&mut e, 3);
    assert!(near(w, 150.0));
    e.set_declarations(2, "width: 200px; height: 200px")
        .unwrap();
    let (w, _) = probe(&mut e, 3);
    assert!(near(w, 80.0), "容器收窄后规则应失配（宽 {w}）");
}
