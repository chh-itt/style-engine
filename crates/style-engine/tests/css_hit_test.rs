//! F3a（ADR-0023）hit_test 锁测试：顶层命中（后绘优先）/ overflow 裁剪
//! 外不命中 / pointer-events: none 穿透。
//!
//! 语义（ADR-0023）：hit_test = paint 期命中几何表逆序查询（后绘=顶），
//! 祖先 clip 链全含判定；visibility/display:none 天然不入表；
//! pointer-events: none 收集期排除。

#![cfg(feature = "text")]

use style_engine::StyleEngine;
use style_engine::tree::StyleNode;

/// 骨架：#root(1) 块容器 + 子节点（键 2..）。
fn engine_hit(sheet: &str) -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(sheet);
    let mut root = StyleNode::default();
    root.id = Some("root".to_string());
    engine.insert(None, 1, root).unwrap();
    engine
}

#[test]
fn hit_test_topmost_wins() {
    let mut e = engine_hit(
        "#root { width: 200px; height: 200px; background: white; }
         #a { width: 100px; height: 100px; background: red; }
         #b { position: absolute; left: 50px; top: 0; width: 100px; height: 100px; background: blue; }",
    );
    let mut a = StyleNode::default();
    a.id = Some("a".to_string());
    e.insert(Some(1), 2, a).unwrap();
    let mut b = StyleNode::default();
    b.id = Some("b".to_string());
    e.insert(Some(1), 3, b).unwrap();
    let _ = e.frame((400.0, 400.0), 1.0, 0.0);
    let (na, nb) = (e.node_id(&2).unwrap(), e.node_id(&3).unwrap());
    // 重叠区 (75, 50)：a=[0..100]、b=[50..150]（absolute 后绘=顶）。
    assert_eq!(e.hit_test(75.0, 50.0).unwrap().node_id, nb, "后绘 b 优先");
    // 仅 a 区 (25, 25)。
    assert_eq!(e.hit_test(25.0, 25.0).unwrap().node_id, na, "仅 a 命中 a");
}

#[test]
fn hit_test_respects_overflow_clip() {
    let mut e = engine_hit(
        "#root { overflow: hidden; width: 100px; height: 100px; background: white; }
         #child { position: absolute; left: 80px; top: 0; width: 100px; height: 50px; background: red; }",
    );
    e.insert(Some(1), 2, {
        let mut n = StyleNode::default();
        n.id = Some("child".to_string());
        n
    })
    .unwrap();
    let _ = e.frame((400.0, 400.0), 1.0, 0.0);
    // (120, 25)：child 盒内 [80..180] 但出 root clip [0..100] → 不命中。
    assert!(
        e.hit_test(120.0, 25.0).is_none(),
        "clip 外区域不得命中被裁剪子节点"
    );
    // (90, 25)：child 盒内且 root clip 内 → 命中 child。
    assert_eq!(
        e.hit_test(90.0, 25.0).unwrap().node_id,
        e.node_id(&2).unwrap(),
        "clip 内正常命中"
    );
}

#[test]
fn hit_test_skips_pointer_events_none() {
    let mut e = engine_hit(
        "#root { width: 200px; height: 200px; background: white; }
         #a { width: 100px; height: 100px; background: red; }
         #pe { position: absolute; left: 0; top: 0; width: 100px; height: 100px;
               background: blue; pointer-events: none; }",
    );
    let mut a = StyleNode::default();
    a.id = Some("a".to_string());
    e.insert(Some(1), 2, a).unwrap();
    e.insert(Some(1), 3, {
        let mut n = StyleNode::default();
        n.id = Some("pe".to_string());
        n
    })
    .unwrap();
    let _ = e.frame((400.0, 400.0), 1.0, 0.0);
    let na = e.node_id(&2).unwrap();
    // (50, 50)：#pe 盒内但 pointer-events: none → 穿透命中 #a。
    assert_eq!(
        e.hit_test(50.0, 50.0).unwrap().node_id,
        na,
        "pointer-events: none 穿透"
    );
}
