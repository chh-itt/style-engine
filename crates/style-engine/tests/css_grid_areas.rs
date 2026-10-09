//! E5 grid-template-areas 与命名线（css-grid-1 / ADR-0020）锁测试。
//!
//! 语义：区域矩形 → 子放置边线（起 = 界+1、止 = 界+2）；线名注册表
//! （模板顶层 `[名]` 槽，1 基线号）→ 放置名解析（区域名 → 全名线 →
//! strip `-start`/`-end` 裸名 → 未知名 = Auto）；span 名 = start 后第
//! 1 次名线。几何断言 = 度量尺（对齐 Chrome 网格布局行为）。

use style_engine::StyleEngine;
use style_engine::tree::StyleNode;

/// 骨架：#g(1) grid 容器 → #c(2) 子项。
fn engine_grid(sheet: &str) -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(sheet);
    let root = StyleNode {
        id: Some("g".to_string()),
        ..StyleNode::default()
    };
    engine.insert(None, 1, root).unwrap();
    let c = StyleNode {
        id: Some("c".to_string()),
        ..StyleNode::default()
    };
    engine.insert(Some(1), 2, c).unwrap();
    engine
}

/// 节点盒 (x, y, w, h)。
fn box_of(e: &mut StyleEngine<u64>, key: u64) -> (f32, f32, f32, f32) {
    let fr = e.frame((800.0, 600.0), 1.0, 0.0);
    let b = fr.boxes.iter().find(|b| b.key == key).unwrap();
    (b.x, b.y, b.width, b.height)
}

#[test]
fn area_rectangle_places_child() {
    // ①2×2 区域 + 2 列模板：grid-area: a → 全区（行 1..3、列 1..3），
    //   默认 stretch → 盒 = 轨迹并集 (0,0,300,150)。
    let mut e = engine_grid(
        "#g { display: grid; width: 300px; height: 300px; \
         grid-template-columns: 100px 200px; grid-template-rows: 50px 100px; \
         grid-template-areas: \"a a\" \"a a\"; } \
         #c { grid-area: a; }",
    );
    let (x, y, w, h) = box_of(&mut e, 2);
    assert_eq!((x, y), (0.0, 0.0), "区域首格 (1,1) → 原点");
    assert_eq!((w, h), (300.0, 150.0), "区域跨 2 列 2 行（stretch 满轨）");
}

#[test]
fn named_lines_span_column() {
    // ②[a] 100px [b] 200px + grid-column: a / b → 列 1..2 → w=100。
    let mut e = engine_grid(
        "#g { display: grid; width: 300px; \
         grid-template-columns: [a] 100px [b] 200px; } \
         #c { grid-column: a / b; height: 20px; }",
    );
    let (x, _y, w, _h) = box_of(&mut e, 2);
    assert_eq!(x, 0.0);
    assert_eq!(w, 100.0, "线名 a..b = 列 1..2");
}

#[test]
fn integer_line_placement() {
    // ③grid-column: 2 → 第二列（x=100）；grid-row: 1。
    let mut e = engine_grid(
        "#g { display: grid; width: 300px; \
         grid-template-columns: 100px 200px; grid-auto-rows: 30px; } \
         #c { grid-column: 2; grid-row: 1; height: 20px; }",
    );
    let (x, _y, _w, _h) = box_of(&mut e, 2);
    assert_eq!(x, 100.0, "整数线号 2 = 第二列起点");
}

#[test]
fn span_integer_crosses_tracks() {
    // ④grid-column: 1 / span 2 → 两列并集 w=300。
    let mut e = engine_grid(
        "#g { display: grid; width: 300px; \
         grid-template-columns: 100px 200px; } \
         #c { grid-column: 1 / span 2; height: 20px; }",
    );
    let (_x, _y, w, _h) = box_of(&mut e, 2);
    assert_eq!(w, 300.0, "span 2 = 两列轨迹并集");
}

#[test]
fn span_named_line_extends_to_name() {
    // ⑤grid-column: a / span b（[a] 100px [mid] 200px [b]）→ 线 1..3。
    let mut e = engine_grid(
        "#g { display: grid; width: 300px; \
         grid-template-columns: [a] 100px [mid] 200px [b]; } \
         #c { grid-column: a / span b; height: 20px; }",
    );
    let (_x, _y, w, _h) = box_of(&mut e, 2);
    assert_eq!(w, 300.0, "span b = 至首条名线 b（线 3）");
}

#[test]
fn unknown_name_falls_back_auto() {
    // ⑥grid-column: zz（无名亦无区域）→ Auto（spec：不存在的名视作
    // auto）→ 自动放置于首格 (0,0)，宽=首列。
    let mut e = engine_grid(
        "#g { display: grid; width: 300px; \
         grid-template-columns: 100px 200px; } \
         #c { grid-column: zz; height: 20px; }",
    );
    let (x, _y, w, _h) = box_of(&mut e, 2);
    assert_eq!(x, 0.0, "未知名 → Auto 自动放置");
    assert_eq!(w, 100.0, "单轨宽");
}

#[test]
fn non_rectangular_areas_invalid() {
    // ⑦非矩形区域（对角格破坏矩形性）→ 整条声明无效 → 无区域注册 →
    // grid-area: a 解析失败 → Auto 自动放置（不 panic）。
    let mut e = engine_grid(
        "#g { display: grid; width: 300px; \
         grid-template-columns: 100px 100px; grid-auto-rows: 30px; \
         grid-template-areas: \"a a\" \"a b\" \"a a\"; } \
         #c { grid-area: a; height: 20px; }",
    );
    let (x, _y, w, _h) = box_of(&mut e, 2);
    assert_eq!(x, 0.0, "声明无效 → 自动放置回退");
    assert_eq!(w, 100.0, "首列宽（非区域跨）");
}

#[test]
fn areas_create_implicit_column() {
    // ⑧3 列区域 + 2 列模板 → 第三列为隐式轨（grid-auto-columns: 50px）
    // → 区域列 1..4 → w = 100+200+50 = 350。
    let mut e = engine_grid(
        "#g { display: grid; width: 400px; grid-auto-columns: 50px; \
         grid-template-columns: 100px 200px; \
         grid-template-areas: \"a a a\"; } \
         #c { grid-area: a; height: 20px; }",
    );
    let (_x, _y, w, _h) = box_of(&mut e, 2);
    assert_eq!(w, 350.0, "区域第三列 = 隐式轨（auto-columns 定宽）");
}

#[test]
fn grid_shorthand_via_var_suspension() {
    // P9-2（ADR-0040）：grid 简写含 var() → 挂起 10 长手 → 计算值期
    // 代换展开（模板形 rows / cols）→ 几何与直写长手一致。
    let mut e = engine_grid(
        "#g { display: grid; width: 400px; --t: 50px 100px / 100px 300px; \
          grid: var(--t); }",
    );
    let (x, y, w, h) = box_of(&mut e, 2);
    assert_eq!((x, y), (0.0, 0.0));
    assert_eq!((w, h), (100.0, 50.0), "var() 挂起展开：列 100px 行 50px");
}

#[test]
fn grid_shorthand_track_form_direct() {
    // 轨道形直写（非 var 路径）：grid: 50px 100px / 100px 300px。
    let mut e = engine_grid("#g { display: grid; width: 400px; grid: 50px 100px / 100px 300px; }");
    let (_x, _y, w, h) = box_of(&mut e, 2);
    assert_eq!((w, h), (100.0, 50.0));
}

#[test]
fn grid_shorthand_auto_flow_rows_form() {
    // rows / auto-flow <auto-rows>（隐含 row）：显式 2 行 + 自动行 30px
    // → 第 3 行子项（height auto → stretch）高 = 30（自动行尺寸生效）。
    let mut e = engine_grid(
        "#g { display: grid; width: 400px; grid: 40px 40px / auto-flow 30px; } \
         #c { grid-row: 3; }",
    );
    let (_x, y, _w, h) = box_of(&mut e, 2);
    assert_eq!(y, 80.0, "第 3 行 = 两显式行之后");
    assert_eq!(h, 30.0, "自动行尺寸 = auto-rows 30px");
}
