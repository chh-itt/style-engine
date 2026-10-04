# 性能预算与门禁（阶段 5 / C5）

本文档是性能预算的唯一权威来源：四场景门禁阈值、阈值推导、优化归因史与增量重样式设计。CI 的 `perf-gate` 作业与 `run.ps1` 的 perf 段共同执行同一门禁程序。

## 门禁入口

```pwsh
cargo run --release -p style-engine --example perf_gate
```

- 四场景各跑 60 帧（首帧预热不计入样本），输出平均/p95 与逐场景 PASS/FAIL；**判据=平均值 ≤ 阈值**（p95 打印作参考），任一场景超阈值 → 进程退出码 1（门禁失败）。
- `debug_assertions` 下断言自动跳过（仅打印提示）——debug 数字无意义，门禁必须 release 跑。
- 阈值常量 `BUDGET_*_MS` 在 `examples/perf_gate.rs`；调整阈值必须同步更新本文档并在 CHANGELOG 记录理由。

## 四场景与实测（Windows 本机，rustc 1.98.1，release）

| 场景 | 口径 | 阈值 | 实测均值 | p95 | 结果 |
|---|---|---|---|---|---|
| box_1k | 1000 盒树（根+9 分支×110 叶，`.leaf:nth-child(odd)`）全量帧 | 5.0 ms | 0.460 ms | 0.904 ms | PASS（均值余量 10.9×） |
| incr | 同树，逐 tick 轮换叶 `set_declarations`（`background-color` 翻转/清除）后 `frame` | 2.0 ms | 0.489 ms | 0.724 ms | PASS（均值余量 4.1×） |
| text_50 | 50 文本叶全管线稳态帧（`feature = "text"`，DejaVuSans 内嵌） | 8.0 ms | 0.867 ms | 1.204 ms | PASS（均值余量 9.2×） |
| scroll | 200 行盒滚动容器逐帧 `set_scroll_offset`+`frame` | 3.0 ms | 0.086 ms | 0.178 ms | PASS（均值余量 34.9×） |

门禁输出行：`120fps 帧预算 8.33 ms；门禁失败 0 项`。

## 阈值推导

1. **上界**：每个阈值都是 120fps 帧预算（8.33 ms）的真子集——单场景超阈值即意味着该维度无法支撑 120fps。
2. **取整 + 机器余量**：阈值取整齐数，与实测 p95 之间保持 ≥2.7× 余量（incr 最紧：2.0/0.724），吸收 CI 机型与负载波动。
3. **门禁真实性**：阈值在优化前实测中真实触发过失败（incr 27.684 ms > 2.0 ms）——门禁不是事后迎合数字，预算纪律先于优化存在。

## 优化归因史（阶段 5 内，全部实测）

| 里程碑 | box_1k | incr | 备注 |
|---|---|---|---|
| 阶段 5 首测 | 1.068 ms | 27.684 ms | incr 场景暴露：`set_declarations` 置 dirty_style → 全树重样式 |
| 修复 A：`restyle_node` 父样式 `.cloned()` → 借用 | ≈ −15~20% | 27.7 → 16.0~24.7 ms | 每节点一次 `ComputedStyle` 深拷贝纯属浪费 |
| 槽位化：`ComputedStyle.values` `BTreeMap` → `Vec<Option<DeclValue>>`（`PropertyId::slot()` 索引，`SLOT_COUNT=94`，对齐锁测试 `slot_alignment`） | 1.068 → 0.460 ms | — | `compute_node_in` 16 µs/节点 → 9.9 µs/节点（−38%）：log-n 走查+逐项分配 → 下标写入+整块克隆 |
| 增量重样式（㉙ 升级落地） | — | 0.489 ms（**56×**） | 脏根子树重算 ~40 µs（2 叶），其余为无变更帧基线 |

text_50 / scroll 随上述修复同步改善：text_50 0.798 → 0.867 ms（噪声量级，文本成本主体在 shaping）；scroll 0.170 → 0.086 ms。

## 增量重样式设计（engine.rs）

**正确性域论证**：节点样式求值只依赖 ①自身/祖先树数据（兄弟声明互不影响，`:nth-child` 按树位）②继承父样式（子树重算即重取）③祖先容器快照（有容器规则在场时退全量）→ `set_declarations` 的失效可收敛到「自身+后代」子树。

- `set_declarations` 将目标节点推入 `style_dirty_roots`（不再整树 dirty_style）。
- `frame()` 择路：`dirty_style` → 全量 `restyle()`；否则脏根非空 → 有容器规则（`sheet.has_container_rules`）退全量（容器快照可能被子树新样式反向影响），无容器规则 → `restyle_subtrees(roots)`。
- `restyle_subtrees`：过滤已移除根 → 子树任一节点属表格（`tables`）或多列（`multicols`）登记 → 保守退全量（两阶段结算的全等缓存依赖全树登记，未改登记无双重登记风险）→ text 特性下逐节点清 `min_measures` → 逐根沿祖先链重建容器栈后 `restyle_node`。
- `RestyleGuard`（`done: SecondaryMap<NodeId,u32>` + pass 计数）：同一 restyle 调用内脏根互为祖先/后代时去重，避免重复求值；全量路径传空守卫，语义不变。
- **仍未增量**：布局（taffy 全树）与 DisplayList 重建每帧全量——余量实测充足（见上表），且二者增量化的正确性风险（taffy 缓存语义 × 脏区跟踪）远大于当前收益。

## 独立基准（release）

- `examples/scroll_bench.rs`：200 行滚动全量程 60 tick——平均 0.079 ms/帧、p95 0.093、最差 0.130，120fps 余量 **105.4×**。
- `examples/text_bench.rs`（`--features text`）：50 文本叶——首帧 2.937 ms（含探针冷启动）、稳态平均 0.734 ms（p95 1.048）、空盒基线 0.017 ms、单叶摊销 14.3 µs，120fps 预算内稳态可承 ≈ **581** 文本叶。

## 环境敏感性

- 本文档所有数字为 Windows 本机 release 实测；CI（ubuntu-latest）执行同一阈值。ubuntu 单核性能与开发机同量级，且阈值余量 ≥2.7×，机型波动被预算吸收。
- 首帧（字体表加载 + shaping 预热）不计入门禁：每场景 tick 0 为预热帧，不进入样本。
- 若后续阈值调整，先在两类环境各实测 3 组，取 p95 最大值 × 2 为新阈值下界。
