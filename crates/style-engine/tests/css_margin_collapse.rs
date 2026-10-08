//! P1-1 边距折叠重估 + padding 长手回归锁（css2 §8.3 / §8.4）。
//!
//! 重估结论（探针实证 → 锁测试固化）：taffy 0.14 block 算法原生纵向
//! margin collapsing 在本管线（结算层环绕）下覆盖标准语义——相邻兄弟
//! max、父-子穿透（无 border/padding/行内容阻隔）、空块自塌穿、负值
//! （正 max + 负 min）、浮动/绝对不参与塌缩、flex 子项不塌缩（既有
//! `margin_collapse_block_siblings` 锁）。结算 pass（floats/lines）在
//! 塌缩结果上运行，不扰动折叠间距（本文件 settle 环绕锁）。
//!
//! 同批修复 padding 物理四长手 + 逻辑四长手解析路由 bug：曾误入
//! parse_corner_radius → Radius 值族，读取方 cs.padding()（Len 族）静默
//! 归零；简写（decl.rs parse_len 展开）与初始值（Len）不受影响，故既有
//! 测试未暴露。修复后 padding 长手阻断父-子塌缩等 §8.4 语义成立。

use style_engine::StyleEngine;
use style_engine::tree::StyleNode;

fn node(name: &str, classes: &str) -> StyleNode {
    StyleNode {
        name: Some(name.to_string()),
        classes: classes.split_whitespace().map(str::to_string).collect(),
        ..Default::default()
    }
}

fn box_of(e: &mut StyleEngine<u64>, key: u64) -> (f32, f32, f32, f32) {
    let fr = e.frame((800.0, 600.0), 1.0, 0.0);
    let b = fr.boxes.iter().find(|b| b.key == key).unwrap();
    (b.x, b.y, b.width, b.height)
}

#[test]
fn parent_child_top_margin_collapses_through() {
    // 父无 border/padding/行内内容 → 首子 margin-top 塌出父外：父子
    // border-box 同位（y=30），父高不含子 margin。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    assert!(
        e.set_stylesheet("div.p { width: 200px; } div.c { height: 40px; margin-top: 30px; }")
            .is_clean()
    );
    assert!(e.insert(None, 1, node("div", "p")).is_ok());
    assert!(e.insert(Some(1), 2, node("div", "c")).is_ok());
    let (px, py, pw, ph) = box_of(&mut e, 1);
    let (cx, cy, cw, _ch) = box_of(&mut e, 2);
    assert_eq!((py, cy), (30.0, 30.0), "塌出父外：父子同位 30");
    assert_eq!((px, pw), (cx, cw), "横向对齐");
    assert_eq!(ph, 40.0, "父高=子高（margin 不入父）");
}

#[test]
fn empty_block_self_collapses_between_siblings() {
    // 空块（无高/边框/内边距）自塌穿：a(mb 20) e(mt 20/mb 30) b(mt 30)
    // → 三者并为一个塌缩集 max(20,20,30,30)=30。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    assert!(
        e.set_stylesheet(
            "div.p { width: 200px; } div.a { height: 40px; margin-bottom: 20px; } \
             div.e { margin-top: 20px; margin-bottom: 30px; } \
             div.b { height: 40px; margin-top: 30px; }"
        )
        .is_clean()
    );
    assert!(e.insert(None, 1, node("div", "p")).is_ok());
    assert!(e.insert(Some(1), 2, node("div", "a")).is_ok());
    assert!(e.insert(Some(1), 3, node("div", "e")).is_ok());
    assert!(e.insert(Some(1), 4, node("div", "b")).is_ok());
    let (_ax, ay, _aw, ah) = box_of(&mut e, 2);
    let (_ex, ey, _ew, eh) = box_of(&mut e, 3);
    let (_bx, by, _bw, _bh) = box_of(&mut e, 4);
    assert_eq!(by - (ay + ah), 30.0, "a→b 间距 = 塌缩集 30");
    assert_eq!(eh, 0.0, "空块高 0");
    // 空块 border-box 落点非规范可观察量（引擎差异在案）：位于 a 的
    // bottom margin 之后、b 之前的塌缩带内。
    assert!((ey - (ay + ah)).abs() < 40.0, "空盒位置在塌缩带内");
}

#[test]
fn negative_margins_collapse_max_positive_plus_min_negative() {
    // 塌缩集 = 正 max + 负 min：mb 20 与 mt −10 → 间距 10。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    assert!(
        e.set_stylesheet(
            "div.p { width: 200px; } div.a { height: 40px; margin-bottom: 20px; } \
             div.b { height: 40px; margin-top: -10px; }"
        )
        .is_clean()
    );
    assert!(e.insert(None, 1, node("div", "p")).is_ok());
    assert!(e.insert(Some(1), 2, node("div", "a")).is_ok());
    assert!(e.insert(Some(1), 3, node("div", "b")).is_ok());
    let (_ax, ay, _aw, ah) = box_of(&mut e, 2);
    let (_bx, by, _bw, _bh) = box_of(&mut e, 3);
    assert_eq!(by - (ay + ah), 10.0, "20 + (−10) = 10");
}

#[test]
fn float_child_margin_not_collapsed_with_parent() {
    // 浮动脱离常规流，不与父塌缩：f.y = margin-top 30。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    assert!(
        e.set_stylesheet(
            "div.p { width: 200px; height: 120px; } \
             div.f { float: left; width: 60px; height: 40px; margin-top: 30px; }"
        )
        .is_clean()
    );
    assert!(e.insert(None, 1, node("div", "p")).is_ok());
    assert!(e.insert(Some(1), 2, node("div", "f")).is_ok());
    let (fx, fy, _fw, _fh) = box_of(&mut e, 2);
    assert_eq!(fy, 30.0, "浮动 margin 生效不塌缩");
    assert_eq!(fx, 0.0, "左浮贴容器内容左缘");
}

#[test]
fn sibling_collapse_survives_settle_passes() {
    // 塌缩间距 + 下游结算 pass（floats/lines）共存不扰动：兄弟折叠 30、
    // 右浮盒落右上、后续块正常排布。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    assert!(
        e.set_stylesheet(
            "div.p { width: 200px; } div.a { height: 40px; margin-bottom: 20px; } \
             div.b { height: 40px; margin-top: 30px; } \
             div.fl { float: right; width: 30px; height: 30px; } \
             div.c { height: 20px; }"
        )
        .is_clean()
    );
    assert!(e.insert(None, 1, node("div", "p")).is_ok());
    assert!(e.insert(Some(1), 2, node("div", "a")).is_ok());
    assert!(e.insert(Some(1), 3, node("div", "b")).is_ok());
    assert!(e.insert(Some(1), 4, node("div", "fl")).is_ok());
    assert!(e.insert(Some(1), 5, node("div", "c")).is_ok());
    let (_ax, ay, _aw, ah) = box_of(&mut e, 2);
    let (_bx, by, _bw, bh) = box_of(&mut e, 3);
    let (flx, fly, _flw, _flh) = box_of(&mut e, 4);
    let (_cx, cy, _cw, _ch) = box_of(&mut e, 5);
    assert_eq!(by - (ay + ah), 30.0, "折叠间距在结算后保持 30");
    assert_eq!((flx, fly), (170.0, 0.0), "右浮盒贴容器右上");
    assert_eq!(cy, by + bh, "后续块紧随 b");
}

#[test]
fn padding_longhand_applied_and_blocks_collapse() {
    // padding-top 长手生效（修复路由 bug）：子 y = padding 10 + margin 30
    // = 40；margin 不再穿透父（§8.4 阻隔条件：padding 非零）。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    assert!(
        e.set_stylesheet(
            "div.p { width: 200px; padding-top: 10px; } \
             div.c { height: 40px; margin-top: 30px; }"
        )
        .is_clean()
    );
    assert!(e.insert(None, 1, node("div", "p")).is_ok());
    assert!(e.insert(Some(1), 2, node("div", "c")).is_ok());
    let (_px, py, _pw, ph) = box_of(&mut e, 1);
    let (_cx, cy, _cw, _ch) = box_of(&mut e, 2);
    assert_eq!(py, 0.0, "父 border-box 不再被子 margin 推移");
    assert_eq!(cy - py, 40.0, "padding 10 + margin 30（不塌缩）");
    assert_eq!(ph, 80.0, "父高 = padding 10 + margin 30 + 子 40");
}

#[test]
fn padding_longhand_calc_and_logical_inline() {
    // 长手 calc 直通结算（正值腿——原测试仅负值钳位腿）：calc(0% + 10px)
    // → 内容左缩 10；逻辑长手 padding-inline-start（ltr → left）同效。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    assert!(
        e.set_stylesheet(
            "div.p { width: 120px; padding-left: calc(0% + 10px); } \
             div.c { width: 100px; height: 10px; } \
             div.q { width: 120px; padding-inline-start: 15px; } \
             div.d { width: 100px; height: 10px; }"
        )
        .is_clean()
    );
    assert!(e.insert(None, 1, node("div", "p")).is_ok());
    assert!(e.insert(Some(1), 2, node("div", "c")).is_ok());
    assert!(e.insert(Some(1), 3, node("div", "q")).is_ok());
    assert!(e.insert(Some(3), 4, node("div", "d")).is_ok());
    let (_px, _py, _pw, _ph) = box_of(&mut e, 1);
    let (cx, _cy, _cw, _ch) = box_of(&mut e, 2);
    let (_qx, _qy, _qw, _qh) = box_of(&mut e, 3);
    let (dx, _dy, _dw, _dh) = box_of(&mut e, 4);
    assert_eq!(cx, 10.0, "长手 calc padding-left 结算 10");
    assert_eq!(dx - _qx, 15.0, "逻辑长手 padding-inline-start（ltr→left）");
}
