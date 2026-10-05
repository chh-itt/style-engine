//! F1 行内流 v1（css-display-3 IFC / ADR-0021）锁测试：行打包、换行、
//! 组盒参与、nowrap 溢出续排、无参与者回归。
//!
//! 语义：块容器直子行内参与者（文本叶 / inline-block 原子盒 / inline
//! 组盒）贪心行打包 → taffy Absolute+inset 锚定；行高=max(参与者测量
//! 高)；容器 min_height 保持行盒高。几何断言=度量尺。A体B度=仅布局
//! 几何不进 paint。

#![cfg(feature = "text")]

use style_engine::StyleEngine;
use style_engine::tree::StyleNode;

const FONT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../style-engine-demo/assets/fonts/DejaVuSans.ttf"
));

/// 骨架：#root(1) 块容器 → kids（选择器, 文本叶内容）依序插 2..n。
fn engine_inline(sheet: &str, kids: &[(&str, Option<&str>)]) -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.add_font(FONT.to_vec());
    engine.set_stylesheet(sheet);
    let mut root = StyleNode::default();
    root.id = Some("root".to_string());
    engine.insert(None, 1, root).unwrap();
    for (i, (sel, text)) in kids.iter().enumerate() {
        let mut n = StyleNode::default();
        n.id = Some((*sel).to_string());
        if let Some(t) = text {
            n.text = Some((*t).to_string());
        }
        engine.insert(Some(1), (i + 2) as u64, n).unwrap();
    }
    engine
}

/// 节点盒 (x, y, w, h)。视口宽 300 = 行打包容器基准宽。
fn box_of(e: &mut StyleEngine<u64>, key: u64) -> (f32, f32, f32, f32) {
    let fr = e.frame((300.0, 600.0), 1.0, 0.0);
    let b = fr.boxes.iter().find(|b| b.key == key).unwrap();
    (b.x, b.y, b.width, b.height)
}

#[test]
fn leaf_then_inline_block_share_line() {
    let mut e = engine_inline(
        "#root { width: 300px; } #b { display: inline-block; width: 40px; height: 20px; } #t { font-size: 16px; font-family: \"DejaVu Sans\"; }",
        &[("t", Some("hello")), ("b", None)],
    );
    let (_tx, ty, tw, th) = box_of(&mut e, 2);
    let (bx, by, bw, bh) = box_of(&mut e, 3);
    assert_eq!((bw, bh), (40.0, 20.0), "原子盒声明尺寸保持");
    assert_eq!(by, 0.0, "同行参与者顶对齐");
    assert_eq!(ty, 0.0);
    assert!(tw > 0.0 && th > 0.0, "叶测量非零（有字体）");
    assert_eq!(bx, tw, "盒 x=叶宽（advance 接排）");
}

#[test]
fn inline_blocks_wrap_to_next_line() {
    let mut e = engine_inline(
        "#root { width: 300px; } #a, #b { display: inline-block; width: 200px; height: 30px; }",
        &[("a", None), ("b", None)],
    );
    let (ax, ay, _aw, _ah) = box_of(&mut e, 2);
    let (bx, by, _bw, _bh) = box_of(&mut e, 3);
    let (_rx, _ry, _rw, rh) = box_of(&mut e, 1);
    assert_eq!((ax, ay), (0.0, 0.0));
    assert_eq!(
        (bx, by),
        (0.0, 30.0),
        "200+200>300 → 第二盒换行（y=首盒高）"
    );
    assert_eq!(rh, 60.0, "容器 min_height=打包行高（行内内容贡献行盒高）");
}

#[test]
fn leaf_continues_after_box_on_line() {
    let mut e = engine_inline(
        "#root { width: 300px; } #b { display: inline-block; width: 100px; height: 20px; } #t { font-size: 16px; font-family: \"DejaVu Sans\"; }",
        &[("b", None), ("t", Some("world"))],
    );
    let (_bx, _by, _bw, _bh) = box_of(&mut e, 2);
    let (tx, ty, tw, _th) = box_of(&mut e, 3);
    assert_eq!(tx, 100.0, "叶续盒后同行：x=盒宽");
    assert_eq!(ty, 0.0);
    assert!(tw > 0.0);
}

#[test]
fn wrapped_text_pushes_next_box_down() {
    // v1 近似锁定（ADR-0021 D4）：折行叶后续参与者=叶底下行。
    let mut e = engine_inline(
        "#root { width: 300px; } #t { font-size: 16px; font-family: \"DejaVu Sans\"; } #b { display: inline-block; width: 50px; height: 20px; }",
        &[
            (
                "t",
                Some("aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk lll mmm nnn ooo ppp"),
            ),
            ("b", None),
        ],
    );
    let (_tx, _ty, _tw, th) = box_of(&mut e, 2);
    let (_bx, by, _bw, _bh) = box_of(&mut e, 3);
    assert!(th > 20.0, "长文本折行（h={}）", th);
    assert_eq!(by, th, "叶满行占满 → 盒下行 y=叶总高");
}

#[test]
fn inline_group_box_participates() {
    // inline 组盒：#s(display:inline, 无自身文本) + 文本子 #st + 后续叶 #t。
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.add_font(FONT.to_vec());
    engine.set_stylesheet(
        "#root { width: 300px; } #s { display: inline; } #st, #t { font-size: 16px; font-family: \"DejaVu Sans\"; }",
    );
    let mut root = StyleNode::default();
    root.id = Some("root".to_string());
    engine.insert(None, 1, root).unwrap();
    let mut s = StyleNode::default();
    s.id = Some("s".to_string());
    engine.insert(Some(1), 2, s).unwrap();
    let mut st = StyleNode::default();
    st.id = Some("st".to_string());
    st.text = Some("span".to_string());
    engine.insert(Some(2), 3, st).unwrap();
    let mut t = StyleNode::default();
    t.id = Some("t".to_string());
    t.text = Some(" after".to_string());
    engine.insert(Some(1), 4, t).unwrap();
    let (sx, sy, sw, sh) = box_of(&mut engine, 2);
    let (tx, ty, _tw, _th) = box_of(&mut engine, 4);
    assert!(sw > 0.0 && sh > 0.0, "组盒收缩适配（w={} h={}）", sw, sh);
    assert_eq!(sy, 0.0);
    assert!(sx >= 0.0);
    assert_eq!(tx, sw, "后续叶续组盒后：x=组盒宽");
    assert_eq!(ty, 0.0);
}

#[test]
fn nowrap_overflows_and_next_box_stays_on_line() {
    // CSS 行盒溢出续排：nowrap 叶不折行溢出 → 后续盒继续同行（不换行）。
    let mut e = engine_inline(
        "#root { width: 300px; } #t { white-space: nowrap; font-size: 16px; font-family: \"DejaVu Sans\"; } #b { display: inline-block; width: 40px; height: 20px; }",
        &[
            (
                "t",
                Some(
                    "aaa bbb ccc ddd eee fff ggg hhh iii jjj aaa bbb ccc ddd eee fff ggg hhh iii jjj",
                ),
            ),
            ("b", None),
        ],
    );
    let (_tx, _ty, tw, th) = box_of(&mut e, 2);
    let (bx, by, _bw, _bh) = box_of(&mut e, 3);
    assert!(tw > 300.0, "nowrap 不折行溢出（w={}）", tw);
    assert!(th <= 20.0, "单行高（h={}）", th);
    assert_eq!(by, 0.0, "行已溢出 → 盒继续同行");
    assert!(bx >= 300.0, "盒在叶溢出尾后（x={}）", bx);
}

#[test]
fn no_inline_participants_regression() {
    let mut e = engine_inline(
        "#root { width: 300px; } #a { width: 100px; height: 20px; } #b { width: 100px; height: 30px; }",
        &[("a", None), ("b", None)],
    );
    let (_ax, ay, _aw, _ah) = box_of(&mut e, 2);
    let (_bx, by, _bw, _bh) = box_of(&mut e, 3);
    assert_eq!((ay, by), (0.0, 20.0), "无行内参与者 → 块流不变");
}
