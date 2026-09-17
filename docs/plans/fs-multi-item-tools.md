# FS 工具多项化方案（multi-item fs tools）

> 目标：`read_file` / `edit_file`（含替换）/ `write_file` 每次调用支持**多项操作**——多处阅读、多处修改/替换、多文件写入，减少工具往返（工具批次是串行 flight，N 次调用 = N 个 flight；多项化后 1 次调用 = 1 个 flight，同时省 N-1 次模型轮次）。

## 1. 现状与约束

| 事实 | 出处 | 对方案的影响 |
|---|---|---|
| fs 工具集中在 `flux-tools/src/fs.rs`，单文件逻辑完整（CRLF 容错、10MB 上限、nearest_line_hint、行号卫生） | `fs.rs` | 多项版应**复用单文件核心**，抽自由函数而非复制 |
| Tool 派生宏：`Vec<T>` → `{"type":"array","items":{"type":<标量>}}`，非原始项类型退化为 `"string"` | `flux-macros/src/lib.rs::infer_array_item_type` | **不支持 array-of-object schema，必须扩展宏**（或手写 `schema()`，state 工具有手写先例） |
| 反序列化走 `serde_json::from_value(Value::Object(args))`，Value 管道嵌套对象原样保留 | 宏 `call()`；architecture.md T 注 | 传输/解析层零改动，MCP 嵌套参数天然可用 |
| 所有工具结果过 `Chat::bounded_output`（8000 chars 内联，超出整体入 buf + `buf_read` 分页） | `flux-chat/src/chat.rs` | 多文件大输出**自动被中央层接住**，无需新机制；但分页定位依赖清晰的分段头 |
| schema 契约被 `flux-tools/tests/tool_schemas.rs` 逐字段 pin 死 | tests/tool_schemas.rs | 新工具需同步 pin（新增，不改旧） |
| UI 卡片折叠摘要按工具名取单字段（`read_file: 'path'`） | `clients/web/src/lib/toolcard.ts::SUMMARY_FIELD_BY_TOOL` | 数组字段需小改（取首项 path 或显示 "N files"）；图标按名称前缀匹配，`edit_files`/`read_files` 自动命中，无需改 |
| 注册点：`flux-server/src/main.rs`；导出：`flux-tools/src/lib.rs` | — | 各加 1-2 行 |
| 工具批次串行执行（one flight at a time） | AGENTS.md「Tool flights」 | 多项化的主要收益来源 |
| `ToolCtx.cancel` 供长任务提前退出 | flux-core/src/tool.rs | 多项循环应在项间检查 |

## 2. 方向选型

### 方案 A：新增复数工具，单项保留（推荐）
`read_files` / `edit_files`（+ 可选 `write_files`），旧工具原样保留。

- ✅ 单项调用零变化：LLM 契约稳定，小操作不背多字段负担；pinned schema 只增不改。
- ✅ 语义清晰：每个工具一种动词，错误/原子性按文件分组，模型易推理。
- ✅ 渐进上线：M2 上 read_files、M3 上 edit_files，互不阻塞。
- ⚠️ 工具数 +2~3：靠 description 引导「单项用单数、多项用复数」。

### 方案 B：原位扩展现有工具（`file_path` 变 optional + `items` 数组）
- ❌ 单/多双形态 union：`additionalProperties:false` 下校验语义模糊（单形态和多形态互斥只能靠运行时检查），模型出错率升高。
- ❌ 破坏 pinned 契约与所有现有测试；UI `toolSummary` 取字段逻辑要兼容两形态。
- ✅ 唯一优点：工具数量不变。不推荐。

### 方案 C：统一 `fs_batch`（`ops:[{op:"read"|"edit"|"write"|...}]`）
- ❌ union schema 无法精确约束（每 op 字段不同 → 退化为宽 object，丢失严格性）。
- ❌ 读失败要不要中断写？混合语义复杂，与「工具=单一职责」的现有设计冲突。
- ✅ 一轮调用混合读写——但读-改-写本就该分轮（先看后改），混合收益存疑。不推荐。

**结论：方案 A。** 用户可见能力 = 「每次操作多项」，实现载体 = 复数工具族。

## 3. 详细设计（方案 A）

### 3.1 `read_files` — 多处阅读

```jsonc
// schema
{
  "files": [{
    "file_path": "string",   // required
    "offset": "integer",     // optional, 1-based
    "limit": "integer"       // optional
  }]                          // required, 1..=16 项
}
```

- 单文件逻辑抽为自由函数 `read_file_segment(path, offset, limit) -> Result<String, CoreError>`（现 `ReadFileTool::execute` 去掉 ctx.resolve 的部分），单项工具与多项工具共用。
- **失败不阻断**：某文件读失败（不存在/二进制/越界）→ 该项输出错误段，继续其余项；全部失败才 `Err` 汇总。
- 输出分段（单文件版的行号脚注原样保留在段内，`buf_read` 分页时可定位）：

```
=== src/main.rs ===
<内容，含 read_file 原有脚注>
=== src/lib.rs — read failed: ... ===
```

- 上限：`MAX_ITEMS = 16`，超限直接报错（防单调用放大输出/占用）。空数组报错。
- cancel：项间检查 `ctx.cancel.is_cancelled()`，提前返回已读分段。

### 3.2 `edit_files` — 多处修改/替换（核心）

```jsonc
// schema
{
  "edits": [{
    "file_path": "string",    // required
    "old_string": "string",   // required
    "new_string": "string",   // required（空串=删除）
    "replace_all": "boolean"  // optional
  }]                          // required, 1..=16 项
}
```

行号式替换**不进本工具**（见 3.4）。

**执行语义**：

1. **分组**：按 resolved path 分组，保持首次出现顺序；同文件 edits 按列出顺序串行。
2. **每文件一次 I/O**：`read_for_edit` 读一次（10MB / UTF-8 守卫沿用）；CRLF 判定一次（沿用现规则：CRLF 文件 + 多行 LF-only old_string → normalize 空间内 splice，`to_crlf` 回写）。
3. **顺序匹配**：每个 edit 在**当前（含前序 edit 结果）内容**上做 `match_indices` —— 允许后续 edit 引用前面 edit 生成的新文本；唯一性 / `replace_all` 语义与单文件版完全一致；no-match 错误带 `edit i/n` 前缀 + `nearest_line_hint`。
4. **原子性：per-file all-or-nothing**。任一 edit 失败 → 该文件整体放弃（内存丢弃，不写盘），其余文件照常应用。理由：
   - 写盘本身无事务，跨文件「伪原子」要两阶段且回滚不可靠；
   - 失败定位清晰：输出明确标注哪些文件已写、哪些未动，模型重试只需重发失败文件；
   - （备选：全局 all-or-nothing——所有文件先在内存全验通过才统一写。代价：一个 old_string 拼错导致全部不落盘，重试放大。列为开放决策 D-1。）
5. **显式拒绝**：同一文件重复的 `(old_string, new_string, replace_all)` 项 → 报错；空 `edits`、`old_string == new_string` → 报错（与单项版一致）。
6. **报告行号**：每 edit 的命中行号按其应用时点的实际内容计算（顺序应用后行号真实）。

**输出格式**（每文件一块，结果一句话可判读；edit 序号为模型列表里的 1-based 全局序号，交错文件也能对上）：

```
src/main.rs: 3 edit(s) applied, file now 240 lines.
  edit 1: replaced 1 occurrence at line 42
  edit 2: replaced 2 occurrences (replace_all)
  edit 3: replaced 1 occurrence at line 101
src/lib.rs: FAILED — edit 1: no exact match for old_string (18 chars). closest line 87: fn main() { …. File untouched.
```

全部文件失败 → 整体 Err（报告文本作为错误消息）；至少一个文件成功 → Ok。

### 3.3 `write_files` — 多文件写入（可选，backlog）

`files: [{file_path, content}]`，复用 `create_dir_all`。多文件 scaffold 场景存在但低频，**先观察 read/edit 使用率再决定**（开放决策 D-2）。

### 3.4 `replace_lines` 多项 — 第一版不做（决策记录）

行号漂移使多项行号替换的组合状态难推理：第 2 项的行号依赖第 1 项是否改变行数，模型出错率高；`edit_files` 已覆盖绝大多数「多处替换」需求。若未来确需，规则预定为：**同文件 items 按 `start_line` 降序应用，全部行号基于调用前的一次读取**。

### 3.5 宏改造（前置工作，M1）

`flux-macros` 新增 **`ToolItem` derive**：

- 对嵌套参数 struct（`ReadItem` / `EditItem` / …）生成 `fn item_schema() -> serde_json::Value`——复用现有 `build_schema_fields`（doc 注释→description、`Option<T>`→optional、required 列表、`additionalProperties:false`）。
- `Tool` derive 的 `infer_array_item_type`：项类型为**非原始路径类型**时，`items` 生成 `#item_ty::item_schema()` 调用（而非字面 json!）；原始类型保持现状。未 derive `ToolItem` 的项类型直接编译失败——fail loudly。
- 覆盖 `Option<Vec<T>>` 同路径。
- 测试：`flux-macros/tests/derive_tool.rs` 加 array-of-object 形状用例。

> 备选（不推荐）：多项工具手写 `schema()`（state 工具先例）。省宏改造，但丢失「字段+doc 注释=契约」的单一事实来源，schema 与 struct 漂移风险长期存在。

### 3.6 引导（description 即提示词）

新工具 description 尾部附使用规则（模型看的提示词承载在派生宏的 description 里）：

- `read_files`：「Read multiple files in one call. For a single file prefer read_file.」
- `edit_files`：「Apply multiple exact-text replacements across one or more files in one call. Each file is atomic: one failed edit leaves that file untouched. For a single edit prefer edit_file.」

## 4. 改动面清单

| 文件 | 改动 |
|---|---|
| `crates/flux-macros/src/lib.rs` | +`ToolItem` derive；`infer_array_item_type` 非原始项 → `item_schema()` 调用 |
| `crates/flux-macros/tests/derive_tool.rs` | array-of-object schema 用例 |
| `crates/flux-tools/src/fs.rs` | 抽 `read_one` / 单文件 edit 核心；新增 `ReadFilesTool`、`EditFilesTool` + item 类型（`#[derive(ToolItem, Deserialize)]`）；单元测试 |
| `crates/flux-tools/src/lib.rs` | re-export |
| `crates/flux-server/src/main.rs` | 注册 2 个新工具 |
| `crates/flux-tools/tests/tool_schemas.rs` | pin `read_files` / `edit_files` 完整嵌套 schema |
| `clients/web/src/lib/toolcard.ts` | `SUMMARY_FIELD_BY_TOOL`：数组字段取 `[0].file_path`（或 "N files"）；图标无改动 |
| `README.md` / `README-en.md` / `AGENTS.md` / `docs/architecture*.md` | 工具清单/架构图行更新 |
| `CHANGELOG.md` | 记录 |
| `flux-core` / proto / MCP 桥 | **零改动**（Value 管道已支持嵌套） |

## 5. 测试计划

- **read_files**：多文件顺序输出；单文件失败不阻断；offset/limit 透传；空数组 / >16 项报错；越界 offset 的单文件脚注语义保持。
- **edit_files**：
  - 同文件多 edit 顺序语义（edit 2 匹配 edit 1 产出的文本）；
  - 失败文件不落盘（磁盘字节不变）+ 其余文件已写；
  - replace_all、no-match（含 hint）、多义（无 replace_all）错误带 `edit i/n` 前缀；
  - CRLF 文件 + 多行 LF old_string（沿用单项版用例形态）；
  - 重复项、`old_string == new_string`、10MB / 非 UTF-8 透传。
- **schema**：tool_schemas pin（含嵌套 items 逐字段断言）。
- **宏**：derive_tool 的 item_schema 形状、`Option` 字段在 item 内的可选性。

## 6. 分阶段实施

| 阶段 | 内容 | 产出 |
|---|---|---|
| M1 | `ToolItem` derive + Tool derive 嵌套 items + 宏测试 | flux-macros |
| M2 | `read_files`（抽 `read_one`）+ 测试 + schema pin | flux-tools / server |
| M3 | `edit_files`（per-file 原子、顺序语义）+ 测试 + schema pin | flux-tools / server |
| M4 | UI summary 小改 + README/AGENTS/architecture/CHANGELOG | web / docs |
| backlog | D-2 `write_files`；D-4 `replace_lines` 多项（降序规则） | — |

## 7. 开放决策

| # | 决策 | 建议 |
|---|---|---|
| D-1 | edit_files 原子性：per-file（推荐）vs 全局 all-or-nothing | per-file：重试面最小、无需两阶段 |
| D-2 | `write_files` 本期做 or backlog | backlog：低频，先观察 |
| D-3 | 单项工具去留 | 保留：单项是高频主路径，双轨靠 description 引导 |
| D-4 | `replace_lines` 多项 | 不做：行号漂移，edit_files 已覆盖 |
| D-5 | 项数上限 | 16：平衡吞吐与输出放大 |
