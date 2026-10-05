//! E4 浮动（css-position-3 / ADR-0019）锁测试：出流/环绕/clear 钳位。
//!
//! 语义：浮动盒出流（taffy Absolute + inset 锚定）+ 同父浮栈水平适配
//! 贪心放置 + 兄弟环绕（margin 增量）+ clear 钳位（margin-top 增量）。
//! 几何断言 = 度量尺（对齐 Chrome 盒模型行为）。A体B度 = 仅布局几何
//! 不进 paint。

use style_engine::StyleEngine;
use style_engine::tree::StyleNode;

/// 骨架：#root(1) 容器 → #f(2) 浮盒 → #s(3) 后续块。
fn engine_float(sheet: &str) -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(sheet);
    let mut root = StyleNode::default();
    root.id = Some("root".to_string());
    engine.insert(None, 1, root).unwrap();
    let mut f = StyleNode::default();
    f.id = Some("f".to_string());
    engine.insert(Some(1), 2, f).unwrap();
    let mut s = StyleNode::default();
    s.id = Some("s".to_string());
    engine.insert(Some(1), 3, s).unwrap();
    engine
}

/// 节点盒 (x, y, w, h)。
fn box_of(e: &mut StyleEngine<u64>, key: u64) -> (f32, f32, f32, f32) {
    let fr = e.frame((800.0, 600.0), 1.0, 0.0);
    let b = fr.boxes.iter().find(|b| b.key == key).unwrap();
    (b.x, b.y, b.width, b.height)
}

#[test]
fn float_left_out_of_flow_and_sibling_wraps() {
    let mut e = engine_float(
        "#f { float: left; width: 100px; height: 50px; } #s { width: 300px; height: 20px; }",
    );
    let (fx, fy, fw, fh) = box_of(&mut e, 2);
    assert_eq!((fx, fy), (0.0, 0.0), "左浮盒锚定容器内容左上");
    assert_eq!((fw, fh), (100.0, 50.0), "浮盒尺寸保持");
    let (sx, sy, sw, _sh) = box_of(&mut e, 3);
    assert_eq!(sx, 100.0, "后续兄弟环绕（左侵占 = 浮盒宽）");
    assert_eq!(sy, 0.0, "浮盒出流 → 兄弟上移至容器顶");
    assert_eq!(sw, 300.0, "声明宽保持");
}

#[test]
fn clear_both_pushes_below_float() {
    let mut e = engine_float(
        "#f { float: left; width: 100px; height: 50px; } #s { clear: both; height: 20px; }",
    );
    let (sx, sy, _sw, _sh) = box_of(&mut e, 3);
    assert_eq!(sx, 0.0, "clear 盒钳位后全宽（无横向环绕）");
    assert_eq!(sy, 50.0, "clear:both → y 钳至浮盒底缘");
}

#[test]
fn float_right_aligns_right_edge() {
    let mut e = engine_float(
        "#f { float: right; width: 100px; height: 50px; } #s { width: 300px; height: 20px; }",
    );
    let (fx, _fy, fw, _fh) = box_of(&mut e, 2);
    assert_eq!(fx, 700.0, "右浮盒右缘对齐容器内容右缘（800−100）");
    assert_eq!(fw, 100.0);
    let (sx, _sy, _sw, _sh) = box_of(&mut e, 3);
    assert_eq!(sx, 0.0, "右浮侵占右侧——兄弟左缘不动");
}

#[test]
fn float_stacks_along_band() {
    // 两左浮盒同带堆叠：第二只前缘 = 第一只右缘（水平适配贪心）。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet(
        "#a { float: left; width: 50px; height: 30px; } #b { float: left; width: 60px; height: 30px; }",
    );
    let mut root = StyleNode::default();
    root.id = Some("root".to_string());
    e.insert(None, 1, root).unwrap();
    let mut a = StyleNode::default();
    a.id = Some("a".to_string());
    e.insert(Some(1), 2, a).unwrap();
    let mut b = StyleNode::default();
    b.id = Some("b".to_string());
    e.insert(Some(1), 3, b).unwrap();
    let (ax, ay, _aw, _ah) = box_of(&mut e, 2);
    let (bx, by, _bw, _bh) = box_of(&mut e, 3);
    assert_eq!((ax, ay), (0.0, 0.0));
    assert_eq!(
        (bx, by),
        (50.0, 0.0),
        "同带堆叠：第二浮盒前缘=第一右缘、同带顶"
    );
}

#[test]
fn float_none_keeps_normal_flow() {
    let mut e = engine_float("#f { width: 100px; height: 50px; } #s { height: 20px; }");
    let (_fx, fy, _fw, _fh) = box_of(&mut e, 2);
    let (sx, sy, _sw, _sh) = box_of(&mut e, 3);
    assert_eq!(fy, 0.0);
    assert_eq!(sy, 50.0, "无浮动：正常块流堆叠");
    assert_eq!(sx, 0.0, "无横向偏移");
}
