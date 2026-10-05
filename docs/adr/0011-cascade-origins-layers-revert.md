# ADR-0011: 级联序键扩维——真实起源栈、@layer 层序与 revert 族

日期：2026-02-27（B1 批）
状态：已接受

## 背景

31 项差距分析的 B 批首项要求把级联模型补齐到 css-cascade-5 的三个维度：
**起源栈**（UA / User / Author + !important 反转）、**层序**（@layer）与
**回滚关键字**（revert / revert-layer）。

B1 之前的现状（各维度的真实起点）：

- `Origin { Default, Stylesheet, Inline }` 三值阶梯；`Default` 是引擎
  初值物化层的伪起源（不在声明流里出现），`!important` 按
  "Default-important 最强" 的 UA 语义反转。**没有** User / UserAgent
  真实起源——origin 栈只是类型化的两档。
- `@layer` 落入 at-rule 兜底臂："unsupported at-rule skipped"（整块
  丢弃 + 告警）。
- CSS 宽关键字（initial / inherit / unset / revert）**没有声明级拦截**：
  走每属性值解析器 → 解析失败 → 整条声明丢弃。即 `color: revert`
  现在等同于把颜色声明删掉（对可继承属性恰好近似 unset，对非继承
  属性则是错的——revert 应回滚到更低起源而非初始值）。

## 决策

### 1. 起源栈：新增 UserAgent / User 两个真实起源

`Origin { Default, UserAgent, User, Stylesheet, Inline }`（`Stylesheet`
= Author）。优先级阶梯（低→高，css-cascade-5 §6.4；`rank(important)`）：

| rank | 档位 |
|---|---|
| 0 | Default normal（引擎初值物化） |
| 1 | UserAgent normal |
| 2 | User normal |
| 3 | Author normal（Stylesheet|Inline 合并桶） |
| 4 | Author important（Stylesheet|Inline 合并桶；反转起点） |
| 5 | User important |
| 6 | UserAgent important |
| 7 | Default important（引擎初值反转位，保持既有语义） |

> **实施修订（落地定案）**：Inline 不设独立档位，并入 Author 桶
>（rank 3 normal / rank 4 important）。理由：内联声明的特异性 =
> `u32::MAX`，桶内比较天然胜出，与"内联独立档"逐位等价（已验算：
> 内联 normal > 样式表 normal；内联 important > 样式表 important；
> 桶间 origin 顺序不变），且与 cascade-5"元素附属声明"语义一致、
> beats 键省一维。

- `Default` 保持伪起源（初值在 computed.rs 物化，不进候选流）；
  其 normal=0 / important=8 两端锚定整个阶梯，行为与 B1 前逐位一致
  （单根回归的位相同性由 262 项测试背书）。
- **User 起源以引擎 API 落地**：`StyleEngine::set_user_stylesheet(&str)`
  （解析存储第二样式表，restyle 全量失效；match 时以 `Origin::User`
  参战）。UserAgent 起源类型与 rank 就位（revert 的回滚终点语义完备），
  产生 UA 声明的宿主钩子留待后续需求（B4 @property 层可复用）。

### 2. @layer：解析期定序的 u32 序数

- `Rule.layer_rank: u32`；`u32::MAX` = 未分层（含内联声明）。
- 解析期维护层注册表：每个完整层路径（`["a","b"]`）按**先现序**登记
  （父路径先于子路径；语句形 `@layer a, b;`、块形、点名 `a.b`、嵌套块
  四种写法等价登记；匿名层每次出现 = 新的唯一路径）。
- 层比较 = 路径字典序的前缀性质：父层直含样式排在子层之前（子层胜）、
  同父兄弟按先现序、未分层胜一切分层（normal 轴）。
- **beats 序键**（新）：`(origin_importance_rank, layer_key, specificity,
  order, decl_index)`。`layer_key`：normal = `layer_rank`；important =
  `u32::MAX - layer_rank`（css-cascade-5：important 轴层序反转——
  未分层 important 输给一切分层 important，早层 important 胜晚层）。
- 选 u32 序数而非路径 Vec：`Candidate` 保持 `Copy`（A8 decl_index 依赖
  拷贝语义），beats 无需回查注册表。

### 3. revert / revert-layer：级联赛后回滚

- 级联从"每属性只留冠军"改为"每属性留**全候选**"；赛后
  `resolve_revert` 处理 `DeclValue::WideKeyword(Revert / RevertLayer)`
  冠军：
  - **revert**：取严格更低 `origin_importance_rank` 档的最优候选
    （同档内层序/特异性照常）；无更低端 → 移除该属性声明（回落
    Default 物化初值）。
  - **revert-layer**：取与冠军同 `origin_importance_rank`、
    `layer_rank` 严格更小的最优候选；无 → 按 revert 语义继续回滚。
- custom properties 同机制（冠军 token 恰为宽关键字 ident 时按同样
  规则回滚到次名 token 候选）。
- initial / inherit / unset 借同一通道在 computed.rs 物化：
  Initial → 初始值；Inherit → 强制继承（含非继承属性——修正既有
  "丢弃≈unset" 偏差）；Unset → `inherits()` 二分。

## 后果

- ** breaking（0.x）**：`Origin` 扩两变体（`#[non_exhaustive]` 已存在，
  下游 match 需补臂）；`Rule` 加字段；`cascade_declarations` 签名加
  User 表参数。
- 引擎默认（无用户表、无 @layer、无回滚关键字）路径的级联结果与
  B1 前逐位一致——`rank()` 数值微调但相对序不变；262 项回归锁定。
- 层注册表在解析期定序，`Stylesheet` 自包含（引擎帧路径零新增查表）；
  层序不跨样式表合并（每表独立层树——B2 多表 + @import 时以"表拼接
  顺序 = 层树合并顺序"接线，届时按 css-cascade-5 跨表层合并扩展）。
- revert 族语义完整覆盖五关键字；`flex: initial`（flex 专属关键字，
  css-flexbox-1）例外走原简写路径。
