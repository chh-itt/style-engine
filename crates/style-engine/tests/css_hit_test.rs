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
fn hit_test_clip_path_circle_precise() {
    // P4 D4（ADR-0037）：clip-path 折线精确判定——盒内圆外不命中。
    let mut e = engine_hit(
        "#root { width: 200px; height: 200px; background: white; }
         #c { position: absolute; left: 0; top: 0; width: 100px; height: 100px;
              background: red; clip-path: circle(50px at 50% 50%); }",
    );
    e.insert(Some(1), 2, {
        let mut n = StyleNode::default();
        n.id = Some("c".to_string());
        n
    })
    .unwrap();
    let _ = e.frame((400.0, 400.0), 1.0, 0.0);
    // 圆心：命中 #c。
    assert_eq!(
        e.hit_test(50.0, 50.0).unwrap().node_id,
        e.node_id(&2).unwrap(),
        "圆心命中"
    );
    // (5,5)：盒 [0..100]² 内、距心 √(45²+45²)≈63.6 > 50 → 圆外不命中 #c，
    // 落到无 clip 的 root。
    assert_eq!(
        e.hit_test(5.0, 5.0).unwrap().node_id,
        e.node_id(&1).unwrap(),
        "clip-path 圆外穿透到下层"
    );
}

#[test]
fn hit_test_transformed_node() {
    // P4 D4：命中随 transform 走——rotate(90deg) 后几何互换，原盒外点
    // 命中、原盒内点不命中。
    let mut e = engine_hit(
        "#root { width: 200px; height: 200px; background: white; }
         #t { position: absolute; left: 100px; top: 0; width: 50px; height: 100px;
              background: red; transform: rotate(90deg); }",
    );
    e.insert(Some(1), 2, {
        let mut n = StyleNode::default();
        n.id = Some("t".to_string());
        n
    })
    .unwrap();
    let _ = e.frame((400.0, 400.0), 1.0, 0.0);
    let nt = e.node_id(&2).unwrap();
    let nroot = e.node_id(&1).unwrap();
    // 盒 (100,0,50,100) 绕中心 (125,50) 转 90° → 覆盖 [75..175]×[25..75]。
    // (160,50)：原盒外、变换后盒内 → 命中 #t。
    assert_eq!(
        e.hit_test(160.0, 50.0).unwrap().node_id,
        nt,
        "旋转后区域命中"
    );
    // (105,10)：原盒内、变换后盒外（y=10 < 25）→ 不命中 #t，落到 root。
    assert_eq!(
        e.hit_test(105.0, 10.0).unwrap().node_id,
        nroot,
        "旋转后原盒外区域不命中"
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
