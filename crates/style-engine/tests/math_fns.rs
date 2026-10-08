//! A6 min()/max()/clamp() 数学函数锁测试。
//!
//! CalcNode 扩展 Min/Max/Clamp（value.rs）：解析（逗号参数、clamp 恰
//! 三参、嵌套数学函数递归）、求值（clamp(MIN,VAL,MAX)=max(MIN,min(VAL,
//! MAX))）、百分比延迟结算（has_percent 直通 → 映射期 lp_defer）。
//! 消费路径与 calc() 完全一致（LengthPercentage::Calc）。

use style_engine::StyleEngine;
use style_engine::tree::StyleNode;

/// 建 doc(指定宽) > t(测试属性) 双节点引擎，返回 t 盒几何。
fn box_of(sheet_t: &str, doc_w: f32, viewport: (f32, f32)) -> (f32, f32, f32, f32) {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(&format!(
        "#doc {{ width: {w}px; }} #t {{ {t} }}",
        w = doc_w as u32,
        t = sheet_t
    ));
    let doc = StyleNode {
        id: Some("doc".to_string()),
        ..StyleNode::default()
    };
    engine.insert(None, 1, doc).unwrap();
    let t = StyleNode {
        id: Some("t".to_string()),
        ..StyleNode::default()
    };
    engine.insert(Some(1), 2, t).unwrap();
    let f = engine.frame(viewport, 1.0, 0.0);
    let b = f.boxes.iter().find(|b| b.key == 2).unwrap();
    (b.x, b.y, b.width, b.height)
}

#[test]
fn min_picks_smaller() {
    // 300 父：50%=150 vs 100px → 100
    let (_, _, w, _) = box_of(
        "width: min(50%, 100px); height: 20px;",
        300.0,
        (400.0, 300.0),
    );
    assert_eq!(w, 100.0);
    // 150 父：50%=75 vs 100px → 75
    let (_, _, w, _) = box_of(
        "width: min(50%, 100px); height: 20px;",
        150.0,
        (400.0, 300.0),
    );
    assert_eq!(w, 75.0);
    // 300 父：150px vs 50%=150 → 150（并列取任一）
    let (_, _, w, _) = box_of(
        "width: min(150px, 50%); height: 20px;",
        300.0,
        (400.0, 300.0),
    );
    assert_eq!(w, 150.0);
}

#[test]
fn max_picks_larger() {
    let (_, _, w, _) = box_of(
        "width: max(50%, 100px); height: 20px;",
        300.0,
        (400.0, 300.0),
    );
    assert_eq!(w, 150.0);
    let (_, _, w, _) = box_of(
        "width: max(50%, 100px); height: 20px;",
        150.0,
        (400.0, 300.0),
    );
    assert_eq!(w, 100.0);
}

#[test]
fn clamp_sandwiches() {
    // clamp(MIN, VAL, MAX) = max(MIN, min(VAL, MAX))
    let (_, _, w, _) = box_of(
        "width: clamp(50px, 25%, 100px); height: 20px;",
        300.0,
        (400.0, 300.0),
    );
    assert_eq!(w, 75.0, "夹中间");
    let (_, _, w, _) = box_of(
        "width: clamp(120px, 25%, 100px); height: 20px;",
        300.0,
        (400.0, 300.0),
    );
    assert_eq!(w, 120.0, "MIN>VAL>MAX → MIN 生效");
    let (_, _, w, _) = box_of(
        "width: clamp(10px, 300px, 100px); height: 20px;",
        300.0,
        (400.0, 300.0),
    );
    assert_eq!(w, 100.0, "VAL>MAX → MAX 生效");
}

#[test]
fn nested_math_and_viewport() {
    // calc(min(50%, 100px) + 20px) @200 父 = min(100,100)+20 = 120
    let (_, _, w, _) = box_of(
        "width: calc(min(50%, 100px) + 20px); height: 20px;",
        200.0,
        (400.0, 300.0),
    );
    assert_eq!(w, 120.0);
    // min(10vw, 100px) @400 视口 = 40
    let (_, _, w, _) = box_of(
        "width: min(10vw, 100px); height: 20px;",
        800.0,
        (400.0, 300.0),
    );
    assert_eq!(w, 40.0);
    // 三参 min 折叠：min(300px, 200px, 250px) → 200
    let (_, _, w, _) = box_of(
        "width: min(300px, 200px, 250px); height: 20px;",
        400.0,
        (400.0, 300.0),
    );
    assert_eq!(w, 200.0);
}

#[test]
fn rejects_malformed() {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    // clamp 两参非法；min 纯数字非法（长度语境须量纲值）
    let r1 = engine.set_stylesheet("#t { width: clamp(1px, 2px); }");
    assert!(!r1.is_clean(), "clamp 两参应报错");
    let r2 = engine.set_stylesheet("#t { width: min(1, 2); }");
    assert!(!r2.is_clean(), "纯数字 min 应报错");
    let r3 = engine.set_stylesheet("#t { width: min(50%, 100px); height: 10px; }");
    assert!(r3.is_clean(), "合法 min 不应报错");
}
