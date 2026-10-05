# 失效模型（Invalidation Model）

> 状态：Accepted（2026-10）。多样式表 / @layer / @property 落地前的依赖图基线；
> 新失效源接入时必须能在这张图上指出"从哪进、传到哪"。

## 1. 失效源 → 失效标（宿主推送面，engine.rs setter 实测语义）

| 失效源 | 触发的失效 | frame() 消费路径 |
|---|---|---|
| `set_stylesheet` | `epoch += 1` + `dirty_style` + `container_sizes.clear()` | restyle() 全量 |
| `set_environment` | `media` 变更才 `dirty_style` | restyle() 全量 |
| `insert` / `remove` / `set_children` | `dirty_struct`（remove 另清全部派生缓存） | rebuild_taffy() → 新树重贴样式（rebuild 内置 `dirty_style`） |
| `set_classes` / `set_state` / `set_text` / `set_leaf_measure` / `add_font` | `dirty_style`（add_font 另 `dirty_struct` + TextSystem 探针缓存清空） | restyle() 全量 |
| `set_declarations`（内联声明） | **只** `style_dirty_roots.push(id)`（阶段5 增量） | 无容器规则 → restyle_subtrees(roots)；有 → restyle() 全量 |
| `set_leaf_intrinsic` / `add_image` / `set_scroll_offset` | 无失效标 | intrinsic → 布局消费；image/scroll → 仅 paint（DisplayList 每帧重建） |

## 2. 派生缓存与其失效域

| 缓存 | 内容 | 写入者 | 失效域 |
|---|---|---|---|
| `styles` | NodeId → ComputedStyle（94 槽位全集） | restyle / restyle_subtrees | 全量重写；增量=脏根子树 |
| `measures` | 文本叶测量 (w,h) | restyle 期 auto_text 叶；文本重排 pass | remove 清；全量重算；增量按子树；宿主 set_leaf_measure 接管后退出 auto_text |
| `min_measures` | 行高探针 min-content | restyle 期 | 全量 clear；增量按子树清（与样式失效域同界） |
| `wrap_widths` | 折行约束宽 | 文本重排 pass | 随 measures |
| `span_styles` | 富文本 span 区间样式 | insert 期级联求解 | 随节点生命周期 |
| `container_sizes` | @container 内容盒快照 | set_stylesheet clear；帧末 record_container_sizes | 快照变更 → 收敛环重入（上限 3） |
| taffy 树 | 布局树 | rebuild_taffy / 结算回写 | dirty_struct 全量重建 |
| 选择器匹配 | 规则命中（按 Epoch） | restyle | 规则集合变更（Epoch++）即失效；匹配结果经声明级 diff 无变化不升级为 style 失效（CONTEXT.md 语义） |

## 3. 传播图

```text
结构变更（insert/remove/set_children）
    └→ dirty_struct ─→ rebuild_taffy ─→ dirty_style ─┐
样式表/环境/class/状态/文本/字体 ─→ dirty_style ──────┤
内联声明 ─→ style_dirty_roots ────────────────────────┤
                                                      ▼
                              ① restyle | restyle_subtrees
                                                      │ styles[] 重写（继承链自然传播）
                                                      ▼
                              ② apply_animations → ③ anchors → ④ taffy 布局
                                                      │
              （布局/绘制当前每帧全量——tier 单向向上传染表现为全帧重算）
                                                      ▼
                              ⑬ collect → ⑭ 列规 → ⑮ scrollable → ⑯ DisplayList
```

三档失效粒度 paint < layout < style 单向向上传染（CONTEXT.md）；当前实现中布局与
DisplayList 每帧全量重建，tier 区分只在「增量重样式」与「滚动偏移不触发重排」两处
兑现。F2（增量布局+DisplayList 补丁）落地时在本图上扩展 ④/⑯ 的脏区传播。

## 4. 多样式表 / @layer / @property 接入预检（B1/B2/B4）

- **多样式表**：Stylesheet 从单值变集合 → Epoch 语义细化为「表集合版本」（任一表
  变更即 ++）；`style_dirty_roots` 语义不变；规则优先级合成在 restyle 输入端完成。
- **@layer**：层序是 Stylesheet 的静态属性（解析期定序），不改失效面；revert-layer
  消费层栈快照，级联候选结构需携带层位。
- **@property**：注册表变更 = 样式失效（custom property 初始值/继承性/类型影响
  var() 代换结果）→ 复用 `dirty_style`；注册表本身随 Epoch 失效。
- 任何新失效源**不得**绕过本表的标注义务：改哪个 setter、清哪个缓存、进哪个收敛
  环，三处都要写。
