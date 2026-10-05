# ADR-0027: 基建批 F3e——调试工具链、fuzz/proptest 扩面、像素回归扩面、README 与 clippy 清零

日期: 2026-02-14（F3e 批次开工）
状态: 已接受
关联: ADR-0023（hit_test+serde 机器通道）、ADR-0024（背景全集）、ADR-0025（clip-path）、ADR-0026（border-image+字体深化）、FEATURES.md ㉘/C5（增量决策）、PERFORMANCE.md

## 背景

31 项缺口的最后一组为基建项：增量布局+DisplayList 补丁、hit_test（已 F3a 落地）、
调试工具链、serde feature（DisplayList serde 已 F3a2 落地）、fuzz/proptest/Pixel
扩容、README。开工审计（批次纪律：先盘点在场性）结论：

| 项 | 审计结论 | 处置 |
|---|---|---|
| hit_test | F3a 已落地（paint 期命中表+hit_test API+node_id 通道） | 无余量 |
| serde feature | F3a2 已落地（paint_dump typed 镜像+JSON 往返锁） | 无余量 |
| 增量布局+DisplayList 补丁 | FEATURES ㉘/C5 已有实测决策：重样式增量落地；布局/绘制全量维持，重估触发条件三条（帧均破预算/shaping 占比破预算/动画高频改布局属性）；differential.rs proptest 已锁「增量链 ≡ 全量重建」逐位等价 | 重审计后维持，见 D2 |
| 调试工具链 | 无 crate 内人读面（computed_style() 只返回结构体；serde dump 是机器通道） | 新交付，见 D1 |
| fuzz/proptest 扩面 | fuzz_grammar.rs 五靶点（前置⑦） | 扩 +3 靶点，见 D3 |
| Pixel 扩容 | conformance 双通道 29 用例 | 扩 F3b/F3c/F3d 面，见 D4 |
| README | 已存在且成体系（定位/上手/workspace/门禁/文档索引） | 轻量刷新，见 D5 |
| clippy 清零 | 存量警告集中 engine/paint/computed/text（F2 及更早区域） | 清零，见 D6 |

## 决策

### D1: 调试工具链 = 三个零冗余人读视图（新 `debug` 模块）

通道分工：**机器通道** = F3a2 serde dump（typed 往返）；**人读通道** = 本批三视图。
不新增序列化方言，全部复用 `Debug` derive 输出——新变体自动跟进、零重复维护；
人读面的价值在**组织**（过滤/排序/缩进）而非重新措辞。

1. `ComputedStyle::debug_dump(&self) -> String`（computed.rs）——显式物化槽位
   （`PropertyId::ALL` 序，`css_name: {Debug}` 每行）+ custom properties 终值 +
   字体度量 + 伪元素标记。None 槽位不出现（=未显式出现，语义即缺省）。
2. `debug::display_list_dump(dl: &DisplayList) -> String`——每 op 一行 `{:?}`，
   `Push*` 缩进 / `Pop*` 反缩进成树视图；头部 `ops=N gen=G`。op 序号供宿主
   对照 `hit_test`/录制。
3. `StyleEngine::layout_tree_dump(&self) -> String` + `debug::boxes_dump(&Frame)`
   ——树结构视图（键/标签/伪节点/深度缩进，含引擎实体化伪节点）与几何视图
   （Frame.boxes 逐盒 `key/x/y/w/h`）分离：引擎有树无帧几何、Frame 有几何无树，
   两视图各自零新状态，宿主按需取用。

### D2: 增量布局+DisplayList 补丁——重审计后维持既有决策

FEATURES ㉘/C5 的实测决策（帧均 0.460ms vs 预算 5.0ms，10× 余量）依然成立；
三条重估触发条件不变。**增量正确性面已由 differential.rs proptest 常驻锁定**
（热引擎逐 op 推进 ≡ 冷引擎全量重建，布局盒/DisplayList/滚动量程逐位相等），
增量布局若重开，锁定面直接复用。DisplayList 补丁（diff/增量提交）与增量绘制
绑定同一触发条件——两者都是「全量正确性已锁、性能余量耗尽才值得换复杂度」
的形态。本 ADR 记录重审计结论，登记面维持 FEATURES T2 既有条目。

### D3: fuzz/proptest 扩面 +3 靶点

在 fuzz_grammar.rs 五靶点（属性文法/简写展开/var() 代换/@条件/capture_tokens）
后追加：

6. **@font-face 块文法**（F3d 新面）：随机描述符拼接 → 注册不 panic + 无效规则
   容错丢弃 + `font_faces()` 合并序（user→主→附加）不变量。
7. **绘制文法族**：gradient（linear/radial/conic）/clip-path/border-image/
   background 简写随机 token 拼接 → 解析不 panic + DeclValue 往返稳定。
8. **hit_test 几何不变量**：`hit_test(x,y)=Some(h)` ⇒ 该 NodeId 反查键存在且
   Frame.boxes 中对应盒含该点（命中表=border-box 收集，clip/pointer-events
   排除面已由锁测试单独覆盖）；框外点 ⇒ None（无 transform 场景）。

### D4: Pixel 扩面

conformance 像素通道新增 F3b/F3c/F3d 面用例（多层背景/clip-path 裁剪/border-image
九宫格/text-shadow/small-caps 合成），覆盖「解析绿但画错」的语义回归面。
具体用例随切片④定。

### D5: README 轻量刷新

Feature 门禁节补 `serde` 行；文档索引补 debug 模块；workspace/上手面随三视图
落位后补一句。不重写——现有文本经阶段沉淀，定位与契约表述仍是权威。

### D6: clippy 清零

存量警告（engine: type_complexity/into_iter_on_ref/collapsible_if/filter_next/
manual_div_ceil 等；paint: too_many_arguments；computed: clone_on_copy/
too_many_arguments；text: type_complexity）逐条修复或局部 `#[allow]`（带
理由注释）。清零后 run.ps1 门禁全绿不受影响；新代码零警告随批保持。

## 后果

- 人读调试面零依赖新增（无 serde/无第三方）——`debug` 模块纯 core 格式化。
- `ComputedStyle::debug_dump`/`layout_tree_dump`/`boxes_dump` 为新增公共 API
  （0.x 加性，无破坏性）。
- fuzz 靶点扩面只动 dev-dep 测试文件，运行时面零变化。
- 增量布局/DisplayList 补丁的最终关闭理由（重审计维持）落在 ADR-0027 D2，
  与 FEATURES T2 条目互链——31 项清单中该项以「决策维持+等价性已锁」收口。

## 落地记录（F3e 实施）

- **D1 调试工具链**：`src/debug.rs` `display_list_dump`（头部 `DisplayList
  ops={n} generation={g}`；Push{Clip,ClipPath,Opacity,Transform,Scroll} 打印
  后 depth+1、Pop* 打印前 saturating_sub(1)，作用域开启行印在父级缩进处——
  元素自身 bg/text 先于自身 clip push，锁测试 `css_debug_toolchain.rs` 三件
  断言 push==top 缩进、child==push+2、pop==push）；`ComputedStyle::debug_dump`
  全 169 槽 `{css_name}: {v:?}` + [metrics]/[pseudo]；`Frame::boxes_dump`/
  `StyleEngine::layout_tree_dump`（`<name #id.class…> key=K [pseudo] text="…"`）。
- **D3 fuzz 扩面**：`fuzz_grammar.rs` +3 靶（⑥ @font-face 注册表不变量
  （family/src 非空、stretch∈50..=200、urange≤0x10FFFF）；⑦ 32 条绘制声明池
  不 panic；⑧ hit_test Some ⇒ 命中点 ∈ 命中盒 border-box ±1e-3）。
- **D4 像素锁**：`pixel_invariants.rs` 七锁全绿；实施期抓出 circle 百分比
  半径整声明丢弃缺陷（cssparser `parse_nested_block` 未消费 token 整块 Err
  × F3c 误读 spec）——修复与锁重基线详见 ADR-0025 落地修正节与
  CHANGELOG F3e 条。
- **D6 clippy 清零**：78→0。type_complexity 8 处按 lint 官方建议 type alias
  化（engine.rs `ImportLoader`（pub）/`AbsCbStackItem`/`GridLineMap`/
  `GridAreaMap`/`TruncCandidate`、text.rs `SpanStyle`/`ScaledSpan`）；ptr_arg
  1（style_channels `&mut Vec`→`&mut slice`，下游全 `&[ContainerCtx]` 共享
  借用）、field_reassign 1（DisplayList 结构体更新语法）、parens 5、
  to_vec 2、identical-if 1、while-let 2（parse_pos_toks 用 let-chains）、
  let-else→? 1、unwrap_or_default 1、doc-list 1；too_many_arguments 7 处
  带因注释 `#[allow]`（paint 九宫格三 fn、compute_node_in 公开 API、
  collect_sheet、peniko_gradient）。
- **门禁**：fmt + `clippy --workspace --all-features -D warnings`（0 警告）
  + test --workspace --all-features **490 绿**（477→490：+7 pixel 锁、
  +3 fuzz 靶、+3 clip 文法锁）。powerset/deny 无依赖变化未重跑（上批绿）。
