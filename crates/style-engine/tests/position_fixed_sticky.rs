//! A4 position:fixed / position:sticky 锁测试。
//!
//! fixed：包含块 = transformed 祖先或 ICB 视口（positioned 祖先不算，
//! CSS 2.1/3）——taffy 无 fixed，映射 Absolute 后由
//! settle_absolute_anchors 的 tcb 候选判定重挂。sticky：解析入库、
//! in-flow 布局（taffy Relative），粘滞偏移属宿主滚动运行时
//!（ADR-0006 引擎无运行期状态；宿主可经 scroll_offsets 绘制期施加）。

use style_engine::StyleEngine;
use style_engine::css::property::{DeclValue, Position, PropertyId};
use style_engine::tree::StyleNode;

fn build(sheet: &str, nodes: &[(&str, Option<u64>, u64)]) -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(sheet);
    for &(id, parent, key) in nodes {
        let mut n = StyleNode::default();
        n.id = Some(id.to_string());
        engine.insert(parent, key, n).unwrap();
    }
    engine
}

#[test]
fn fixed_ignores_positioned_ancestor() {
    // fixed 子件在 positioned（relative 偏移至 (50,20)）祖先内：盒=视口
    // 坐标，无视祖先偏移（relative 祖先不是 fixed 的包含块）
    let mut engine = build(
        "#outer { width: 300px; } \
         #wrap { position: relative; left: 50px; top: 20px; width: 200px; height: 200px; } \
         #fix { position: fixed; top: 10px; left: 10px; width: 30px; height: 30px; }",
        &[
            ("outer", None, 1),
            ("wrap", Some(1), 2),
            ("fix", Some(2), 3),
        ],
    );
    let f = engine.frame((400.0, 300.0), 1.0, 0.0);
    let fix = f.boxes.iter().find(|b| b.key == 3).unwrap();
    assert_eq!(
        (fix.x, fix.y, fix.width, fix.height),
        (10.0, 10.0, 30.0, 30.0),
        "fixed 应锚定视口（ICB），实际 {:?}",
        (fix.x, fix.y)
    );
    // 祖先正常流内
    let wrap = f.boxes.iter().find(|b| b.key == 2).unwrap();
    assert_eq!((wrap.x, wrap.y), (50.0, 20.0));
}

#[test]
fn fixed_uses_transformed_ancestor_as_cb() {
    // transformed 祖先（layout 盒 x=70）构成 fixed 的包含块
    let mut engine = build(
        "#outer { padding-left: 70px; width: 300px; } \
         #wrap { transform: translate(0px, 0px); width: 200px; height: 200px; } \
         #fix { position: fixed; top: 5px; left: 5px; width: 20px; height: 20px; }",
        &[
            ("outer", None, 1),
            ("wrap", Some(1), 2),
            ("fix", Some(2), 3),
        ],
    );
    let f = engine.frame((400.0, 300.0), 1.0, 0.0);
    let fix = f.boxes.iter().find(|b| b.key == 3).unwrap();
    let wrap = f.boxes.iter().find(|b| b.key == 2).unwrap();
    assert_eq!(
        (fix.x, fix.y),
        (wrap.x + 5.0, wrap.y + 5.0),
        "fixed 的 cb=transformed 祖先（layout 盒坐标），实际 {:?} wrap={:?}",
        (fix.x, fix.y),
        (wrap.x, wrap.y)
    );
}

#[test]
fn fixed_overlay_root_viewport_anchor() {
    // ADR-0010 overlay 根带 position:fixed：视口 (60,40) 锚定
    let mut engine = build(
        "#doc { width: 100px; height: 100px; } \
         #pop { position: fixed; top: 40px; left: 60px; width: 80px; height: 50px; }",
        &[("doc", None, 1), ("pop", None, 2)],
    );
    let f = engine.frame((400.0, 300.0), 1.0, 0.0);
    let pop = f.boxes.iter().find(|b| b.key == 2).unwrap();
    assert_eq!(
        (pop.x, pop.y, pop.width, pop.height),
        (60.0, 40.0, 80.0, 50.0)
    );
}

#[test]
fn sticky_parses_and_lays_out_in_flow() {
    let mut engine = build(
        "#doc { width: 300px; } \
         #st { position: sticky; top: 10px; width: 100px; height: 40px; }",
        &[("doc", None, 1), ("st", Some(1), 2)],
    );
    let f = engine.frame((400.0, 300.0), 1.0, 0.0);
    let st = f.boxes.iter().find(|b| b.key == 2).unwrap();
    // in-flow：常规块位置（零偏移），几何与 relative 一致
    assert_eq!((st.x, st.y, st.width, st.height), (0.0, 0.0, 100.0, 40.0));
    // ComputedStyle 可读（宿主按其滚动运行时施加粘滞偏移）
    let cs = engine.computed_style(2).unwrap();
    assert_eq!(
        cs.value(PropertyId::Position),
        Some(&DeclValue::Position(Position::Sticky))
    );
    // sticky 是定位元素（absolute 后代的 cb）但自身不脱离流
    assert_eq!(f.boxes.len(), 2);
}
