//! A3 多根引擎与 top-layer 锁测试（ADR-0010）。
//!
//! 锁定面：①多根 overlay 根在 (0,0) 视口锚定（不纵向堆叠）②overlay 根
//! 显式 absolute 定位生效 ③top-layer 绘制序（文档根→非 top overlay→
//! top 层进层序）④overlay 根是级联根（不继承文档根）⑤remove 语义
//! （overlay 摘除不动文档；文档根移除=全清）⑥set_top_layer 契约。

use style_engine::StyleEngine;
use style_engine::css::property::{CursorKind, DeclValue, PropertyId};
use style_engine::error::ContractError;
use style_engine::paint::PaintOp;
use style_engine::tree::StyleNode;

fn root_with_sheet(id: &str, sheet: &str) -> (StyleEngine<u64>, u64) {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(sheet);
    let node = StyleNode {
        id: Some(id.to_string()),
        ..StyleNode::default()
    };
    engine.insert(None, 1, node).unwrap();
    (engine, 1)
}

#[test]
fn multi_root_overlays_anchor_at_origin() {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(
        "#doc { width: 100px; height: 100px; } \
         #pop1 { width: 50px; height: 40px; } \
         #pop2 { width: 30px; height: 20px; }",
    );
    let mk = |id: &str| StyleNode {
        id: Some(id.to_string()),
        ..StyleNode::default()
    };
    engine.insert(None, 1, mk("doc")).unwrap();
    engine.insert(None, 2, mk("pop1")).unwrap();
    engine.insert(None, 3, mk("pop2")).unwrap();
    let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
    let by_key = |k: u64| frame.boxes.iter().find(|b| b.key == k).unwrap();
    // 文档根：块流填充视口宽（现语义），高=样式高
    assert_eq!((by_key(1).x, by_key(1).y), (0.0, 0.0));
    assert_eq!((by_key(1).width, by_key(1).height), (100.0, 100.0));
    // overlay 根：视口原点锚定、显式尺寸（不随文档根纵叠）
    assert_eq!(
        (by_key(2).x, by_key(2).y, by_key(2).width, by_key(2).height),
        (0.0, 0.0, 50.0, 40.0)
    );
    assert_eq!(
        (by_key(3).x, by_key(3).y, by_key(3).width, by_key(3).height),
        (0.0, 0.0, 30.0, 20.0)
    );
}

#[test]
fn overlay_root_explicit_position_honored() {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(
        "#doc { width: 100px; height: 100px; } \
         #pop { position: absolute; top: 10px; left: 20px; width: 30px; height: 30px; }",
    );
    let mk = |id: &str| StyleNode {
        id: Some(id.to_string()),
        ..StyleNode::default()
    };
    engine.insert(None, 1, mk("doc")).unwrap();
    engine.insert(None, 2, mk("pop")).unwrap();
    let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
    let pop = frame.boxes.iter().find(|b| b.key == 2).unwrap();
    // 作者显式定位：相对视口（超根=ICB 盒）锚定 (20, 10)
    assert_eq!((pop.x, pop.y), (20.0, 10.0));
    assert_eq!((pop.width, pop.height), (30.0, 30.0));
}

#[test]
fn top_layer_paint_order() {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(
        "#doc { width: 100px; height: 100px; background-color: #f00; } \
         #a { width: 30px; height: 30px; background-color: #0f0; } \
         #b { width: 30px; height: 30px; background-color: #00f; }",
    );
    let mk = |id: &str| StyleNode {
        id: Some(id.to_string()),
        ..StyleNode::default()
    };
    engine.insert(None, 1, mk("doc")).unwrap();
    engine.insert(None, 2, mk("a")).unwrap();
    engine.insert(None, 3, mk("b")).unwrap();
    let colors = |frame: &style_engine::Frame<u64>| -> Vec<AlphaF32> {
        frame
            .paint
            .ops
            .iter()
            .filter_map(|op| match op {
                PaintOp::FillRect { color, .. } => Some(AlphaF32(color.components)),
                _ => None,
            })
            .collect()
    };
    // 插入序：doc → a → b
    let f = engine.frame((400.0, 300.0), 1.0, 0.0);
    assert_eq!(
        colors(&f),
        vec![
            AlphaF32([1.0, 0.0, 0.0, 1.0]),
            AlphaF32([0.0, 1.0, 0.0, 1.0]),
            AlphaF32([0.0, 0.0, 1.0, 1.0])
        ]
    );
    // a 进层：doc → b → a（top 层最后）
    engine.set_top_layer(2, true).unwrap();
    let f = engine.frame((400.0, 300.0), 1.0, 0.0);
    assert_eq!(
        colors(&f),
        vec![
            AlphaF32([1.0, 0.0, 0.0, 1.0]),
            AlphaF32([0.0, 0.0, 1.0, 1.0]),
            AlphaF32([0.0, 1.0, 0.0, 1.0])
        ]
    );
    // b 也进层：进层序 a 先 b 后 → doc → a → b
    engine.set_top_layer(3, true).unwrap();
    let f = engine.frame((400.0, 300.0), 1.0, 0.0);
    assert_eq!(
        colors(&f),
        vec![
            AlphaF32([1.0, 0.0, 0.0, 1.0]),
            AlphaF32([0.0, 1.0, 0.0, 1.0]),
            AlphaF32([0.0, 0.0, 1.0, 1.0])
        ]
    );
    // a 出层：doc → 非 top overlay(a) → top(b) = red, green, blue
    engine.set_top_layer(2, false).unwrap();
    let f = engine.frame((400.0, 300.0), 1.0, 0.0);
    assert_eq!(
        colors(&f),
        vec![
            AlphaF32([1.0, 0.0, 0.0, 1.0]),
            AlphaF32([0.0, 1.0, 0.0, 1.0]),
            AlphaF32([0.0, 0.0, 1.0, 1.0])
        ]
    );
}

/// AlphaF32：f32 分量不实现 Eq 的相等包装。
#[derive(Debug, PartialEq)]
struct AlphaF32([f32; 4]);

#[test]
fn overlay_roots_are_cascade_roots() {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(
        "#doc { cursor: text; caret-color: #0891b2; width: 100px; height: 100px; }",
    );
    let mk = |id: &str| StyleNode {
        id: Some(id.to_string()),
        ..StyleNode::default()
    };
    engine.insert(None, 1, mk("doc")).unwrap();
    engine.insert(None, 2, mk("pop")).unwrap();
    let _ = engine.frame((400.0, 300.0), 1.0, 0.0);
    let pop_cs = engine.computed_style(2).unwrap();
    // overlay 根不继承文档根的继承属性（cursor 初始 auto 而非 text）
    assert_eq!(
        pop_cs.value(PropertyId::Cursor),
        Some(&DeclValue::Cursor(CursorKind::Auto))
    );
}

#[test]
fn remove_semantics_overlay_vs_document() {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(
        "#doc { width: 100px; height: 100px; } \
         #pop { width: 30px; height: 30px; }",
    );
    let mk = |id: &str| StyleNode {
        id: Some(id.to_string()),
        ..StyleNode::default()
    };
    engine.insert(None, 1, mk("doc")).unwrap();
    engine.insert(None, 2, mk("pop")).unwrap();
    engine.insert(Some(2), 3, mk("")).unwrap();
    let f = engine.frame((400.0, 300.0), 1.0, 0.0);
    assert_eq!(f.boxes.len(), 3);
    // overlay 根摘除：子树（key 3）一并消失，文档根不动
    engine.remove(2).unwrap();
    let f = engine.frame((400.0, 300.0), 1.0, 0.0);
    assert_eq!(f.boxes.len(), 1);
    assert!(f.boxes.iter().all(|b| b.key == 1));
    // 文档根移除 = 全清（此后可重建）
    engine.remove(1).unwrap();
    let f = engine.frame((400.0, 300.0), 1.0, 0.0);
    assert!(f.boxes.is_empty());
    engine.insert(None, 10, mk("doc")).unwrap();
    engine.insert(None, 11, mk("pop")).unwrap();
    let f = engine.frame((400.0, 300.0), 1.0, 0.0);
    assert_eq!(f.boxes.len(), 2);
}

#[test]
fn set_top_layer_contract() {
    let (mut engine, doc) = root_with_sheet("doc", "#doc { width: 10px; height: 10px; }");
    assert_eq!(
        engine.set_top_layer(doc, true),
        Err(ContractError::NotOverlayRoot)
    );
    assert_eq!(
        engine.set_top_layer(99, true),
        Err(ContractError::UnknownNode)
    );
    let pop = StyleNode {
        id: Some("pop".to_string()),
        ..StyleNode::default()
    };
    engine.insert(None, 2, pop).unwrap();
    assert!(engine.set_top_layer(2, true).is_ok());
    // 重复移入幂等
    assert!(engine.set_top_layer(2, true).is_ok());
    assert!(engine.set_top_layer(2, false).is_ok());
    assert!(engine.set_top_layer(2, false).is_ok());
}
