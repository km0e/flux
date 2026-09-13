# Flux 既定取舍（Accepted Tradeoffs）

> 明确接受、勿当缺陷修的既有妥协。新增条目需说明「为何接受」与「重开评估的条件」。

## Accepted tradeoffs

### T-02 bash runner 类命令的固有风险

以命令首词判定类别的防御语义下，`eval`/`sudo`/解释器直调类 runner 无需藏匿即可执行任意命令——这是「按首词放行」的固有语义而非绕过（提示文案展示完整命令）。已排除：runner 黑名单（误禁合法授权）、禁用首词放行（牺牲便利）。审批层已删，本条保留以防审批层恢复时重蹈。

### T-03 state 工具的通用 KV 行为

state 写任意 key / 读未知 key 返回空串是**设计行为**（agent 跨轮次通用 KV 通道；schema 枚举只作引导非白名单；读写对称）——勿当缺陷修。

### T-05 Explorer git 状态用 git CLI 而非 git2/gix

`fsbrowse` 的目录状态收集仍 shell 到 `git` 子进程（每列目录两次：`rev-parse --show-toplevel` + `status --porcelain -z`），已用 `-- .` pathspec 限定被列子树、`-unormal` 折叠 untracked 目录、blocking 线程池执行消化成本。未引入 `git2`（C 工具链依赖 + unsafe + worktree/配置兼容面）或 `gix`（纯 Rust 但 status 高层 API 仍在变动、依赖树重）——对「每 15s 对已加载目录重列」的 UI 负载，CLI 兼容性与零新增重依赖胜出。重开评估条件：status 调用成为实测热点（大仓库冷扫描 > 数百 ms），或 gix status API 进入稳定层。

### T-06 web 构建缺失不做启动期校验

web 伺服（默认开）启动不校验 dist 布局（曾试过 `index.html` + ≥1 JS + ≥1 CSS 的 fail-fast，已删）：构建不完整在**请求时**自然暴露——index 读失败 → 500 + warn 日志；缺失资产 → 404（浏览器 console 可见）。理由：服务端拒绝启动会把「agent 可用、UI 资产待补」的正常状态误判为致命错误；失败链路本身已可归因（warn 指名路径 / console 指名资源）。重开评估条件：请求期暴露被证明难以归因（如用户反馈分不清 assets_dir 配错与构建缺失），或引入多入口页面使失败形态复杂化。

### T-07 前端流式渲染不换渲染器/不引流式库

流式 markdown 管线维持 marked + splitAndFold/ParagraphSplitter + renderStableSlice + FenceCache（调研定案，勿再提议更换）：① marked 无增量续接 API——末尾 `text` token 吸收未闭合行内构造，按 token 边界缓存会丢内联上下文；② micromark 真流式入口仅 Node.js，浏览器退化为全量 re-parse（O(n²) 依旧）；③ 生态内「streaming markdown renderer」（streamdown、Vercel AI SDK 等）实为 remend 自愈不完整块 + 全量 re-parse + DOM diff，且全带 React peer dep。现有管线的块级缓存/稳定前缀方案比这些实现更精细；残余 O(n²) 仅存于无空行墙式文本的病态输入（实测 64KB ≈ 12ms，被「当前段落」钳制）。重开评估条件：出现框架无关、带稳定前缀缓存契约的增量渲染器，或真实输入实测超帧预算。

### T-08 上下文压缩不建设，长会话由用户主动处理

Transcript 维持单一累积、无自动预算裁剪、无 compaction/摘要机制（定案：预算裁剪与 compaction 两阶段均不做）：长会话撞模型上下文窗的表现就是 provider 400 → 轮失败（error 事件），恢复手段是既有功能——fork（从任意 user 消息重启）、rebase、新开 chat——由用户在合适的时机主动切分上下文。为何接受：单用户小团队的典型会话长度内撞窗罕见；自动裁剪/摘要会引入第一个 archive 边界，牵动 fork/rebase 语义、buf 生命周期与 proto，复杂度与当前收益不成比例；重启时的设计要点（原底稿并入）：① 裁剪粒度 = 整轮——轮是 machine 的原子提交单元，绝不拆半轮；② 裁剪只作用于发往 provider 的装配视图，不改写 store；被裁轮次的大输出仍可经 buf_read 引用（buf 按 call id 锚定，不随 transcript 失效）——这是裁剪可行性的关键支撑；③ 预算 = context_window × 安全系数（~0.7），估算器先 chars/4 并留 trait；④ compaction 的落点 = 引擎重建原语（机器门 + re-begin），与 provider 热切换同一条路；⑤ compaction 引入第一个 archive 边界——fork 点 / rebase base 必须在存活历史内，UI 需可见的压缩通知，store 需归档表。重开评估条件：真实使用中撞窗成为高频痛点（长会话被频繁打断、用户疲于手动切分），或出现必须全量装载超大仓库的需求。

### T-09 Usage 的 cache-write 由推导维持，不加 wire 字段

`W = max(0, in − cached)`（OpenAI 兼容语义下 cached ⊆ prompt，推导精确），wire `Usage` 不加 `cache_write_tokens` 字段。为何接受：单独字段只服务「单独上报 cache write 的上游」，当前全部兼容端点都不上报，字段只会是恒零噪音。重开评估条件：接入单独上报 cache write 的上游（Anthropic 风格 usage、vLLM 扩展），或成本估算需要 cache-write 单价档。

### T-10 可观测性（/healthz、/metrics、round span）暂不建设

无指标端点、无健康检查、无 round 级结构化观测——只有 stderr tracing（与自愈/重建路径的结构化日志字段）。为何接受：单进程、单用户/小团队、绑定 127.0.0.1 的部署形态下，日志足以回答当前全部排障问题；指标面（端点、直方图桶、跨 crate 静态计数器）是给一个尚不存在的运维面提前固化形状。重启时的设计要点（原底稿并入）：① `/healthz` 纯 liveness；`/metrics` 手写 Prometheus 文本（固定桶直方图 + AtomicU64 计数器，零新依赖）；② 计数器静态放 flux-core（provider/chat/loop 均可 bump），gauge（active_chats、mcp 状态）scrape 时读；③ 打点：round 总数/时长/失败、token 三项、tool_calls、buf_writes、provider_retries；④ round 关联 = chat_id + 轮序号日志字段，不引 OTel。重开评估条件：部署到无法靠日志归因的环境（多实例聚合、长驻服务 SLA），或「这轮为什么慢 / 哪个 provider 抖动」成为高频排障问题。
