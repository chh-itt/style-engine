//! background-repeat space/round 与 border-image-repeat round/space 锁测试
//!（css-backgrounds-3 §2.4/§5.5；退化平铺值几何直读绘制基元）。
//!
//! 度量尺 = `Frame.paint` 落矩形：背景平铺经 `PaintOp::Gradient`（线性渐变
//! 源无内在片尺寸 → 片位纯几何）；border-image 经 `PaintOp::Image`（src
//! 子域窗标识角/边/中心片）。断言以实测 ops 极值推导区域几何，避免与
//! content/border 盒语义耦合；源片 30px = 50×50 图 slice 10 的中段跨度。

use style_engine::StyleEngine;
use style_engine::paint::PaintOp;
use style_engine::tree::StyleNode;

/// border-image 源片沿轴尺寸：50×50 图 slice 10 → 中段跨度 30（源 px）。
const TILE: f32 = 30.0;

/// border-image 片记录：(x, y, w, h, src_x, src_y, src_w, src_h)。
type ImageTile = (f32, f32, f32, f32, f32, f32, f32, f32);

fn engine(sheet: &str, with_image: bool) -> StyleEngine<u64> {
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet(sheet);
    let root = StyleNode {
        id: Some("root".to_string()),
        ..StyleNode::default()
    };
    e.insert(None, 1, root).unwrap();
    let leaf = StyleNode {
        id: Some("t".to_string()),
        ..StyleNode::default()
    };
    e.insert(Some(1), 2, leaf).unwrap();
    if with_image {
        e.add_image("t.png", 50, 50, vec![255; 50 * 50 * 4]);
    }
    e
}

/// 全部背景渐变片 (x, y, w, h)。
fn grads(e: &mut StyleEngine<u64>) -> Vec<(f32, f32, f32, f32)> {
    let f = e.frame((800.0, 600.0), 1.0, 0.0);
    f.paint
        .ops
        .iter()
        .filter_map(|op| match op {
            PaintOp::Gradient {
                x,
                y,
                width,
                height,
                ..
            } => Some((*x, *y, *width, *height)),
            _ => None,
        })
        .collect()
}

/// 全部 border-image 片 (x, y, w, h, src_x, src_y, src_w, src_h)。
fn images(e: &mut StyleEngine<u64>) -> Vec<ImageTile> {
    let f = e.frame((800.0, 600.0), 1.0, 0.0);
    f.paint
        .ops
        .iter()
        .filter_map(|op| match op {
            PaintOp::Image {
                x,
                y,
                width,
                height,
                src_x,
                src_y,
                src_w,
                src_h,
                ..
            } => Some((*x, *y, *width, *height, *src_x, *src_y, *src_w, *src_h)),
            _ => None,
        })
        .collect()
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

/// border-image 实测边界：九片 ops 极值 → (左, 上, 边区 len_x, 边区
/// len_y)（区框宽减两角 10px）。
fn bi_frame(im: &[ImageTile]) -> (f32, f32, f32, f32) {
    let left = im.iter().map(|r| r.0).fold(f32::INFINITY, f32::min);
    let top = im.iter().map(|r| r.1).fold(f32::INFINITY, f32::min);
    let right = im
        .iter()
        .map(|r| r.0 + r.2)
        .fold(f32::NEG_INFINITY, f32::max);
    let bottom = im
        .iter()
        .map(|r| r.1 + r.3)
        .fold(f32::NEG_INFINITY, f32::max);
    (left, top, right - left - 20.0, bottom - top - 20.0)
}

fn sorted(mut v: Vec<f32>) -> Vec<f32> {
    v.sort_by_key(|a| a.to_bits());
    v.dedup_by(|a, b| near(*a, *b));
    v
}

// ------------------------------------------------------ background round/space

#[test]
fn background_round_rescales_to_integer_tile_count() {
    // round：n=round(100/30)=3 → 片宽 100/3（缩放因子 10/9，css-backgrounds-3
    // §2.4「rescaled so that it does」）；y：n=round(50/30)=2 → 片高 25。
    let mut e = engine(
        "#t { width: 100px; height: 50px; background-image: linear-gradient(red, blue); \
         background-size: 30px 30px; background-repeat: round; }",
        false,
    );
    let g = grads(&mut e);
    assert_eq!(g.len(), 6, "{g:?}");
    let tw = 100.0 / 3.0;
    for (x, y, w, h) in &g {
        assert!(near(*w, tw), "round 片宽 {w} ≈ {tw}");
        assert!(near(*h, 25.0), "round 片高 {h} ≈ 25");
        assert!(
            near(*x, 0.0) || near(*x, tw) || near(*x, 2.0 * tw),
            "round x {x}"
        );
        assert!(near(*y, 0.0) || near(*y, 25.0), "round y {y}");
    }
}

#[test]
fn background_space_flush_edges_and_even_gaps() {
    // space：x 容 n=3 片 → 间隙均摊 5、首片贴左缘（0）、末片贴右缘
    //（70+30=100）；y 仅容 1 片（n=1）→ 等价 no-repeat：单片按
    // background-position（y=10px）定位（css-backgrounds-3 §2.4）。
    let mut e = engine(
        "#t { width: 100px; height: 50px; background-image: linear-gradient(red, blue); \
         background-size: 30px 30px; background-repeat: space; background-position: 0px 10px; }",
        false,
    );
    let mut g = grads(&mut e);
    g.sort_by_key(|t| t.0.to_bits());
    assert_eq!(g.len(), 3, "{g:?}");
    for (i, x) in [0.0f32, 35.0, 70.0].iter().enumerate() {
        assert!(near(g[i].0, *x) && near(g[i].1, 10.0), "{g:?}");
        assert!(
            near(g[i].2, 30.0) && near(g[i].3, 30.0),
            "space 不缩放 {g:?}"
        );
    }
    assert!(near(g[0].0, 0.0), "首片贴左缘");
    assert!(near(g[2].0 + g[2].2, 100.0), "末片贴右缘");
    assert!(near(g[1].0 - (g[0].0 + g[0].2), 5.0), "间隙均摊 5");
}

#[test]
fn background_space_single_image_uses_position() {
    // n≤1（容不下两片）→ 单片按 background-position 定位：x 容 1 片 →
    // x=20px；y 容 0 片 → y=5px；两轴交叉单片 (20,5,60,60)。
    let mut e = engine(
        "#t { width: 100px; height: 50px; background-image: linear-gradient(red, blue); \
         background-size: 60px 60px; background-repeat: space; background-position: 20px 5px; }",
        false,
    );
    let g = grads(&mut e);
    assert_eq!(g.len(), 1, "{g:?}");
    assert!(
        near(g[0].0, 20.0) && near(g[0].1, 5.0) && near(g[0].2, 60.0) && near(g[0].3, 60.0),
        "{g:?}"
    );
}

#[test]
fn background_two_value_form_is_per_axis() {
    // `space round`：第一值作用 x（space → 3 片 gap5、片宽不缩放 30）、
    // 第二值作用 y（round → 2 片、片高缩放 25）——逐轴独立取笛卡尔积。
    let mut e = engine(
        "#t { width: 100px; height: 50px; background-image: linear-gradient(red, blue); \
         background-size: 30px 30px; background-repeat: space round; }",
        false,
    );
    let g = grads(&mut e);
    assert_eq!(g.len(), 6, "{g:?}");
    for (x, y, w, h) in &g {
        assert!(
            near(*x, 0.0) || near(*x, 35.0) || near(*x, 70.0),
            "space x {x}"
        );
        assert!(near(*y, 0.0) || near(*y, 25.0), "round y {y}");
        assert!(near(*w, 30.0), "x 轴 space 片宽不缩放 {w}");
        assert!(near(*h, 25.0), "y 轴 round 片高缩放 {h}");
    }
}

// ------------------------------------------------- border-image round / space

#[test]
fn border_image_round_rescales_edge_tiles() {
    // round：边区 n=round(len/30) 整数片等分拉伸 → 片长 len/n（缩放因子
    // len/(30n)）、首尾贴角、无截断（css-backgrounds-3 §5.5 sides）。
    let mut e = engine(
        "#t { width: 100px; height: 100px; border: 10px solid black; \
         border-image-source: url(t.png); border-image-slice: 10; \
         border-image-width: 10px; border-image-repeat: round; }",
        true,
    );
    let im = images(&mut e);
    let (left, top, len_x, len_y) = bi_frame(&im);
    // 四角仍 10×10 单片（永不平铺）。
    assert_eq!(im.iter().filter(|r| r.6 == 10.0 && r.7 == 10.0).count(), 4);
    // 上/下边：src (10,0,30,10) / (10,40,30,10)。
    let n = ((len_x / TILE).round() as usize).max(1);
    let tw = len_x / n as f32;
    for (sy, tag) in [(0.0, "top"), (40.0, "bottom")] {
        let mut tiles: Vec<_> = im
            .iter()
            .filter(|r| r.5 == sy && r.6 == 30.0 && r.7 == 10.0)
            .collect();
        tiles.sort_by_key(|r| r.0.to_bits());
        assert_eq!(tiles.len(), n, "{tag}");
        for (i, t) in tiles.iter().enumerate() {
            assert!(
                near(t.0, left + 10.0 + tw * i as f32) && near(t.2, tw),
                "{tag} {t:?}"
            );
        }
        assert!(near(tiles[0].0, left + 10.0), "{tag} 首片贴左角");
        assert!(
            near(tiles[n - 1].0 + tiles[n - 1].2, left + 10.0 + len_x),
            "{tag} 末片贴右角"
        );
    }
    // 左/右边：src (0,10,10,30) / (40,10,10,30)。
    let ns = ((len_y / TILE).round() as usize).max(1);
    let th = len_y / ns as f32;
    for (sx, tag) in [(0.0, "left"), (40.0, "right")] {
        let mut tiles: Vec<_> = im
            .iter()
            .filter(|r| r.4 == sx && r.6 == 10.0 && r.7 == 30.0)
            .collect();
        tiles.sort_by_key(|r| r.1.to_bits());
        assert_eq!(tiles.len(), ns, "{tag}");
        for (i, t) in tiles.iter().enumerate() {
            assert!(
                near(t.1, top + 10.0 + th * i as f32) && near(t.3, th),
                "{tag} {t:?}"
            );
        }
    }
}

#[test]
fn border_image_space_distributes_gaps_flush_edges() {
    // space：边区 n=floor(len/30) 片（片长=源片 30，不缩放）、首尾贴角、
    // 间隙均摊（MDN：extra space distributed in between tiles）。
    let mut e = engine(
        "#t { width: 100px; height: 100px; border: 10px solid black; \
         border-image-source: url(t.png); border-image-slice: 10; \
         border-image-width: 10px; border-image-repeat: space; }",
        true,
    );
    let im = images(&mut e);
    let (left, top, len_x, len_y) = bi_frame(&im);
    // 上边：n 片、片宽 30、贴角、间隙均摊。
    let n = (len_x / TILE) as usize;
    assert!(n >= 2, "测试前提：边区至少容两片");
    let gap = (len_x - TILE * n as f32) / (n - 1) as f32;
    let mut tiles: Vec<_> = im
        .iter()
        .filter(|r| r.5 == 0.0 && r.6 == 30.0 && r.7 == 10.0)
        .collect();
    tiles.sort_by_key(|r| r.0.to_bits());
    assert_eq!(tiles.len(), n);
    for (i, t) in tiles.iter().enumerate() {
        assert!(
            near(t.0, left + 10.0 + (TILE + gap) * i as f32) && near(t.2, TILE),
            "上边 {t:?}"
        );
    }
    assert!(near(tiles[0].0, left + 10.0), "首片贴左角");
    assert!(
        near(tiles[n - 1].0 + tiles[n - 1].2, left + 10.0 + len_x),
        "末片贴右角"
    );
    // 左边：ns 片、片高 30、贴角、间隙均摊。
    let ns = (len_y / TILE) as usize;
    assert!(ns >= 2, "测试前提：边区至少容两片");
    let gapy = (len_y - TILE * ns as f32) / (ns - 1) as f32;
    let mut sides: Vec<_> = im
        .iter()
        .filter(|r| r.4 == 0.0 && r.6 == 10.0 && r.7 == 30.0)
        .collect();
    sides.sort_by_key(|r| r.1.to_bits());
    assert_eq!(sides.len(), ns);
    for (i, t) in sides.iter().enumerate() {
        assert!(
            near(t.1, top + 10.0 + (TILE + gapy) * i as f32) && near(t.3, TILE),
            "左边 {t:?}"
        );
    }
    assert!(near(sides[0].1, top + 10.0), "首片贴上角");
    assert!(
        near(sides[ns - 1].1 + sides[ns - 1].3, top + 10.0 + len_y),
        "末片贴下角"
    );
}

#[test]
fn border_image_two_value_form_applies_per_side_axis() {
    // `round space`：第一值 → 上/下（round 整数片等分缩放）、第二值 →
    // 左/右（space 源片顺排不缩放）——两值时第一值作用 top/middle/bottom、
    // 第二值作用 left/right。
    let mut e = engine(
        "#t { width: 100px; height: 100px; border: 10px solid black; \
         border-image-source: url(t.png); border-image-slice: 10; \
         border-image-width: 10px; border-image-repeat: round space; }",
        true,
    );
    let im = images(&mut e);
    let (left, top, len_x, len_y) = bi_frame(&im);
    // 上/下 round：n=round(len/30) 片、片长 len/n（缩放）。
    let n = ((len_x / TILE).round() as usize).max(1);
    let tw = len_x / n as f32;
    let mut tops: Vec<_> = im
        .iter()
        .filter(|r| r.5 == 0.0 && r.6 == 30.0 && r.7 == 10.0)
        .collect();
    tops.sort_by_key(|r| r.0.to_bits());
    assert_eq!(tops.len(), n, "round 作用上边");
    assert!(tops.iter().all(|t| near(t.2, tw)), "round 缩放 {tops:?}");
    assert!(near(tops[0].0, left + 10.0));
    // 左/右 space：ns=floor(len/30) 片、片长=源片 30（不缩放）。
    let ns = (len_y / TILE) as usize;
    let mut sides: Vec<_> = im
        .iter()
        .filter(|r| r.4 == 0.0 && r.6 == 10.0 && r.7 == 30.0)
        .collect();
    sides.sort_by_key(|r| r.1.to_bits());
    assert_eq!(sides.len(), ns, "space 作用左边");
    assert!(
        sides.iter().all(|t| near(t.3, TILE)),
        "space 不缩放 {sides:?}"
    );
    assert!(near(sides[0].1, top + 10.0), "首片贴上角");
    assert!(
        near(sides[ns - 1].1 + sides[ns - 1].3, top + 10.0 + len_y),
        "末片贴下角"
    );
}

#[test]
fn border_image_fill_middle_round_cross_product() {
    // fill + round：中心按 rep 逐轴 round 取笛卡尔积 → n×n 片、片长
    // len/n、src 恒 (10,10,30,30)（css-backgrounds-3 §5.5 middle）；
    // 总片数 = 4 角 + 2n 上/下 + 2ny 左/右 + n×ny 中心。
    let mut e = engine(
        "#t { width: 100px; height: 100px; border: 10px solid black; \
         border-image-source: url(t.png); border-image-slice: 10 fill; \
         border-image-width: 10px; border-image-repeat: round; }",
        true,
    );
    let im = images(&mut e);
    let (left, top, len_x, len_y) = bi_frame(&im);
    let n = ((len_x / TILE).round() as usize).max(1);
    let ny = ((len_y / TILE).round() as usize).max(1);
    let tw = len_x / n as f32;
    let th = len_y / ny as f32;
    let mid: Vec<_> = im
        .iter()
        .filter(|r| r.4 == 10.0 && r.5 == 10.0 && r.6 == 30.0 && r.7 == 30.0)
        .collect();
    assert_eq!(mid.len(), n * ny, "中心 n×ny 片");
    for t in &mid {
        assert!(near(t.2, tw) && near(t.3, th), "中心片 {t:?}");
    }
    let xs = sorted(mid.iter().map(|r| r.0).collect());
    let ys = sorted(mid.iter().map(|r| r.1).collect());
    assert_eq!(xs.len(), n);
    assert_eq!(ys.len(), ny);
    for (i, x) in xs.iter().enumerate() {
        assert!(near(*x, left + 10.0 + tw * i as f32), "中心 x {x}");
    }
    for (j, y) in ys.iter().enumerate() {
        assert!(near(*y, top + 10.0 + th * j as f32), "中心 y {y}");
    }
    assert_eq!(
        im.len(),
        4 + 2 * n + 2 * ny + n * ny,
        "角 + 边 + 中心总片数"
    );
}

#[test]
fn border_image_fill_middle_space_grid() {
    // fill + space：中心逐轴 space → n=floor(len/30) 片、片长=源片 30
    //（不缩放）、中段内首尾贴边、间隙均摊。
    let mut e = engine(
        "#t { width: 100px; height: 100px; border: 10px solid black; \
         border-image-source: url(t.png); border-image-slice: 10 fill; \
         border-image-width: 10px; border-image-repeat: space; }",
        true,
    );
    let im = images(&mut e);
    let (left, top, len_x, len_y) = bi_frame(&im);
    let ns = (len_x / TILE) as usize;
    let nsy = (len_y / TILE) as usize;
    assert!(ns >= 2 && nsy >= 2, "测试前提：中段至少容两片");
    let gap = (len_x - TILE * ns as f32) / (ns - 1) as f32;
    let gapy = (len_y - TILE * nsy as f32) / (nsy - 1) as f32;
    let mid: Vec<_> = im
        .iter()
        .filter(|r| r.4 == 10.0 && r.5 == 10.0 && r.6 == 30.0 && r.7 == 30.0)
        .collect();
    assert_eq!(mid.len(), ns * nsy, "中心 n×ny 片");
    for t in &mid {
        assert!(near(t.2, TILE) && near(t.3, TILE), "space 不缩放 {t:?}");
    }
    let xs = sorted(mid.iter().map(|r| r.0).collect());
    assert_eq!(xs.len(), ns);
    for (i, x) in xs.iter().enumerate() {
        assert!(
            near(*x, left + 10.0 + (TILE + gap) * i as f32),
            "中心 x {x}"
        );
    }
    assert!(near(xs[0], left + 10.0), "中心首片贴左边");
    assert!(
        near(xs[ns - 1] + TILE, left + 10.0 + len_x),
        "中心末片贴右边"
    );
    let ys = sorted(mid.iter().map(|r| r.1).collect());
    assert_eq!(ys.len(), nsy);
    for (j, y) in ys.iter().enumerate() {
        assert!(
            near(*y, top + 10.0 + (TILE + gapy) * j as f32),
            "中心 y {y}"
        );
    }
    assert!(near(ys[0], top + 10.0), "中心首片贴上边");
    assert!(
        near(ys[nsy - 1] + TILE, top + 10.0 + len_y),
        "中心末片贴下边"
    );
}
