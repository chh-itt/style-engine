# CSS 语法层用 cssparser + selectors，属性语法层自研

目标既然是兼容真实 CSS 文本，词法/语法解析与选择器匹配直接采用 servo 的 cssparser + selectors（stylo 同源、CSS 错误恢复语义现成、specificity 与组合子匹配免费）。属性层的语法（属性名 → 值的文法、简写展开、custom properties）自研：我们只做子集，且这是我们真正拥有设计自由的地方。两者都不 fork；cssparser 足够低层，不会把属性语义强加给我们。

## Considered Options

- lightningcss：它的属性 IR 面向 CSS 转换/压缩而非级联计算，拉入整棵属性模型依赖树，与"子集 + 自己的 ComputedStyle"路径冲突；仅作为简写展开行为的参考实现。
- 完全自写解析器：被否，重新发明 CSS 错误恢复（"非法声明跳过该声明、继续解析"）违背项目根基，成本数周且必然更差。

## Consequences

- 需要实现 selectors 的 `SelectorImpl` trait 把选择器映射到我们的节点数据。
- cssparser/selectors 为 MPL-2.0，作为未修改的依赖使用无许可证负担。
- 选择器子集边界（type/class/universal/伪类/后代/子代/分组起步）必须文档化，超出子集的 CSS 按容错语义警告并忽略。
