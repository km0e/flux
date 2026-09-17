# Flux 既定取舍（Accepted Tradeoffs）

> 明确接受、不应作为缺陷修复的既有妥协。新增条目需说明「为何接受」与「重开评估的条件」，追加在文件末尾、不重排既有条目。
>
> 格式：一问题一条目，**标题即问题**（`T-xx` 编号是全仓引用锚点——代码注释、`Cargo.toml`、CHANGELOG、AGENTS.md 均按其引用，刻意保留）。条目内按时间先后记录方案行「`- 日期: 方案; advantage: …`」——首方案的 advantage 写为何选择，后续方案写相对上一方案改进了什么；**最新一行即当前方案**。被否决/回滚的旧方案同样成行留档，日期不可考的如实标注「日期不详」。机制细节就近链接 architecture.md 对应小节（Record, don't argue），不在此重复论证。first recorded 日期取当前 git 历史中该条目的最早存世记录（仓库在 v0.1.0 / v0.1.4 曾压缩历史，实际首次记录可能更早）。

## Accepted tradeoffs

### T-02 bash 首词放行下 runner 类命令的任意执行风险如何处置（first recorded 2026-09-12）

- 2026-09-12: 维持按命令首词判定类别的放行语义——`eval`/`sudo`/解释器直调类 runner 无需藏匿即可执行任意命令，作为该语义的固有风险接受，不算绕过（提示文案始终展示完整命令）；advantage: 相对被否的两个方案——runner 黑名单会误禁合法授权、禁用首词放行牺牲便利——维持按首词放行避免了这两类代价。审批层已删，本条保留以防审批层恢复时重蹈。

### T-03 state 工具对未知 key 的读写行为是否算缺陷（first recorded 2026-09-12）

- 2026-09-12: 通用 KV 语义按设计保留——写任意 key、读未知 key 返回空串；advantage: 该行为就是特性本身——agent 的跨轮次通用 KV 通道，schema `enum` 只作引导非白名单、读写对称都是该目的的构成部分，白名单化只会取消这个通道。不应作为缺陷修复。机制背景见 architecture.md「沙箱语义：workdir / current_dir」（边界语义只由 `workdir`/`current_dir` 两个权威键承担，与新键无涉）。

### T-05 Explorer 目录 git 状态用 git CLI 还是 git2/gix（first recorded 2026-09-12）

- 2026-09-12: 仍 shell 到 `git` 子进程——每列目录两次（`rev-parse --show-toplevel` + `status --porcelain -z`），已用 `-- .` pathspec 限定被列子树、`-unormal` 折叠 untracked 目录、blocking 线程池执行消化成本；advantage: 相对 `git2`（C 工具链依赖 + unsafe + worktree/配置兼容面）与 `gix`（纯 Rust 但 status 高层 API 仍在变动、依赖树重）——CLI 兼容性与零新增重依赖胜出，对「每 15s 对已加载目录重列」的 UI 负载足够。机制背景见 architecture.md「Web UI 静态伺服 → 文件 Explorer」。重开评估条件：status 调用成为实测热点（大仓库冷扫描 > 数百 ms），或 gix status API 进入稳定层。

### T-07 前端流式渲染是否换渲染器/引流式库（first recorded 2026-09-12）

- 2026-09-12: 维持 marked + splitAndFold/ParagraphSplitter + renderStableSlice + FenceCache（调研定案，不应再提议更换）；advantage: 相对全部候选——① marked 无增量续接 API，末尾 `text` token 吸收未闭合行内构造，按 token 边界缓存会丢内联上下文；② micromark 真流式入口仅 Node.js，浏览器退化为全量 re-parse（O(n²) 依旧）；③ 生态内「streaming markdown renderer」（streamdown、Vercel AI SDK 等）实为 remend 自愈不完整块 + 全量 re-parse + DOM diff，且全带 React peer dep——现有管线的块级缓存/稳定前缀方案比这些实现更精细，残余 O(n²) 仅存于无空行墙式文本的病态输入（实测 64KB ≈ 12ms，被「当前段落」钳制）。机制背景见 architecture.md「前端：流式渲染」。重开评估条件：出现框架无关、带稳定前缀缓存契约的增量渲染器，或真实输入实测超帧预算。

### T-08 长会话撞模型上下文窗：自动裁剪/compaction 还是用户手动切分（first recorded 2026-09-14）

- 2026-09-14: 均不建设——Transcript 维持单一累积、无自动预算裁剪、无 compaction/摘要；撞窗的表现就是 provider 400 → 轮失败（error 事件），恢复手段是既有功能——fork（从任意 user 消息重启）、rebase、新开 chat——由用户在合适的时机主动切分上下文；advantage: 单用户小团队的典型会话长度内撞窗罕见，而自动裁剪/摘要会引入第一个 archive 边界，牵动 fork/rebase 语义、buf 生命周期与 proto，复杂度与当前收益不成比例。
  - 重启时的设计要点（原底稿并入，非当前行为）：① 裁剪粒度 = 整轮——轮仍是裁剪的语义单元，绝不拆半轮（store 的原子提交单元是批次：每个工具批次与其 assistant 段一起落盘，轮由若干批次组成，见 T-11）；② 裁剪只作用于发往 provider 的装配视图，不改写 store；被裁轮次的大输出仍可经 buf_read 引用（buf 按 call id 锚定，不随 transcript 失效）——这是裁剪可行性的关键支撑；③ 预算 = context_window × 安全系数（~0.7），估算器先 chars/4 并留 trait；④ compaction 的落点 = 引擎重建原语（机器门 + re-begin），与 provider 热切换同一条路；⑤ compaction 引入第一个 archive 边界——fork 点 / rebase base 必须在存活历史内，UI 需可见的压缩通知，store 需归档表。
  - 重开评估条件：真实使用中撞窗成为高频痛点（长会话被频繁打断、用户疲于手动切分），或出现必须全量装载超大仓库的需求。

### T-09 Usage 的 cache-write 是否加 wire 字段（first recorded 2026-09-14）

- 2026-09-14: 不加字段——维持推导 `W = max(0, in − cached)`（OpenAI 兼容语义下 cached ⊆ prompt，推导精确）；advantage: 单独字段只服务「单独上报 cache write 的上游」，当前全部兼容端点都不上报，字段只会是恒零噪音。重开评估条件：接入单独上报 cache write 的上游（Anthropic 风格 usage、vLLM 扩展），或成本估算需要 cache-write 单价档。

### T-10 可观测性（/healthz、/metrics、round span）是否建设（first recorded 2026-09-14）

- 2026-09-14: 暂不建设——无指标端点、无健康检查、无 round 级结构化观测，只有 stderr tracing（与自愈/重建路径的结构化日志字段）；advantage: 单进程、单用户/小团队、绑定 127.0.0.1 的部署形态下，日志足以回答当前全部排障问题；指标面（端点、直方图桶、跨 crate 静态计数器）是给一个尚不存在的运维面提前固化形状。
  - 重启时的设计要点（原底稿并入，非当前行为）：① `/healthz` 纯 liveness；`/metrics` 手写 Prometheus 文本（固定桶直方图 + AtomicU64 计数器，零新依赖）；② 计数器静态放 flux-core（provider/chat/loop 均可 bump），gauge（active_chats、mcp 状态）scrape 时读；③ 打点：round 总数/时长/失败、token 三项、tool_calls、buf_writes、provider_retries；④ round 关联 = chat_id + 轮序号日志字段，不引 OTel。
  - 重开评估条件：部署到无法靠日志归因的环境（多实例聚合、长驻服务 SLA），或「这轮为什么慢 / 哪个 provider 抖动」成为高频排障问题。

### T-11 服务端关停机制：优雅关停还是 crash-only（first recorded 2026-09-14）

- 日期不详（早于 squash）: 优雅关停——`with_graceful_shutdown` + drain 等待连接收尾；advantage: 无记录（初版行为，历史 squash 前的动机未存世——保留此行只为记录演化）。
- 2026-09-14: crash-only——不安装任何信号 handler（SIGTERM/SIGINT 保持默认处置，进程即时死亡，即使有浏览器流在场），持久性改由存储层契约无条件承担；advantage: 相对上一行的优雅关停——① 它只覆盖死法全集中最小的子集（SIGKILL、OOM、panic、断电、容器强杀均不经过它），数据正确性不应依赖死法；② 等待连接收尾正是当初「必须关前端才能退出」的根因——Subscribe 响应体永不自然结束（keepalive 泵永转），`serve.await` 被在途流永久楔死，drain 反而一次都跑不到，机制在其唯一需要的场景里价值为零。优雅关停实现已删。
  - 方案要点：持久性由存储层契约承担——transcript 提交**批次原子**（machine 在每个批次边界、续流开启前提交 assistant 段 + 批次结果；轮末提交末段），WAL + 事务写保证「任何死法都落在同一个可恢复点」——已提交批次永不丢，持久尾部永不携带悬空 tool_call（OpenAI 兼容 API 对其 400），崩溃窗口缩到当前存活段（秒级）；工具子进程清理不依赖优雅路径：进程内（`kill_on_drop` + 进程组守卫 + 协作取消）覆盖活着的进程，内核侧 `PR_SET_PDEATHSIG`（flux-tools `arm_parent_death_signal`）覆盖进程自身死亡的所有方式。机制背景见 architecture.md「对话循环」与「持久化」。
  - 残余代价（接受）：关停时客户端看到的是连接重置（RST）而非干净 EOF——客户端本就有重连 + R2 快照调和，纯观感差异；尾部未答消息由用户手动重发（模型看得到历史，幂等窗口本就随进程内存消亡）；断电可能丢最后几笔提交（`synchronous=NORMAL`，进程崩溃零丢失，不逐笔 fsync）。
  - 重开评估条件：出现无法接受 RST 观感、或需要「关停前强制收割在飞轮次」的部署形态（如多实例编排的 drain 钩子），且存储层契约被证明不足以支撑恢复。

### T-12 工具输出的 ANSI 清理放在哪一层（first recorded 2026-09-16）

- 2026-09-16: strip 只放在 flux-tools `BashTool::execute`（结果进入 `bounded_output` 之前）——不上提 flux-chat 的统一漏斗、不做前端兜底；捕获层已是管道（isatty=false），自觉程序自动关色，只对无视 isatty 的程序（开 ansi feature 的 tracing subscriber 是典型）兜底；advantage: 相对「统一漏斗清理」——范围最小化保真：① 文件类工具（read_file/grep/glob）**绝不 strip**——模型基于其内容做 edit_file，strip 会让模型视图与磁盘内容错位（字符数/偏移不一致，edit 往返失败），保真优先于观感；② MCP 结果不清理（协议文本，风险低）；③ 前端不兜底——改动前已入库的带 ANSI 历史重开时原样显示，不做数据迁移；④ 裸 `\r` 丢弃——进度条覆盖帧折叠为残字连排（与主流 coding agent 的 exec 输出 strip 同取舍）。重开评估条件：带色 MCP 工具成为高频困扰（可泛化为 per-tool 卫生声明），或旧历史可读性抱怨集中（做一次性迁移清洗）。

### T-13 skill 激活去重：专表/目录 watch 还是 transcript 派生（first recorded 2026-09-16）

- 2026-09-16: 不建专表、不建目录文件 watch——chat-owned `skill_read` 的去重索引在两个组装点（spawn / apply_rebuild）从 history 重派生（键 = 名字 + 词法规范化 rel），Tier-1 目录是 begin 点的快照；advantage: 相对专表——专用表会引入第二真相源与 fork keep-set 复制 SQL，而 transcript 只增不减（T-08），已提交的 skill_read 结果永远在模型可见上下文里，transcript 本身就是激活记录的真相源，fork / 重建 / 重启随重派生天然正确；相对文件 watch（Claude Code 做 watch）——flux 的活刷新口是 `skill_list` 的每调用即扫 + `skill_read` 未知名自纠错，全局技能在边界外只有它能枚举。
  - 方案要点：溢出判别 = 结果字符数 > INLINE_BUDGET，溢出内容经 buf_entries 回读；命中（规范化内容 hash 相同）返回短注不重注（溢出过则附 buf_read ref），未命中返回全文并记账——不建任何新存储。
  - 代价（接受）：派生的 store 读仅发生在「溢出过的技能路径」（每路径至多一次，技能文件 ≤256KB、路径屈指可数）；快照过期由节内文案显式声明并指向 `skill_list`；词法规范化键（非 canonical）= 技能内 symlink 别名至多多一次全文读。机制背景见 architecture.md「工具系统 → Agent Skills」。
  - 重开评估条件：目录过期被证明高频影响任务（再议 SkillService 安装即触发的 gate 重建钩子——机制已存在，只需接线），或压缩（T-08 落地时）需要把派生源切换为压缩视图（索引语义不变，只换输入）。

### T-14 web UI 资产解析：磁盘优先链还是嵌入二进制+显式覆盖（first recorded 2026-09-17）

- 日期不详（早于本条记录）: 磁盘优先解析链——三层磁盘候选（CLI flag / 二进制旁 / flux home），「二进制已刷新、磁盘目录未跟上」的静默 UI 滞后靠版本戳警告兜底；advantage: 无记录（初版行为——保留此行只为记录演化）。
- 2026-09-17: rust-embed 嵌入二进制（feature `web-ui-embed`，默认开）——release 嵌入、debug 运行时读仓库 dist（路径钉在编译期 `CARGO_MANIFEST_DIR`——dev 磁盘回退）；`~/.flux/web-ui` / `--web-assets-dir` 降为 **Gitea `custom/` 式逐路径覆盖**（同名文件遮蔽嵌入底座，其余回落）；advantage: 相对上一行的磁盘优先链——该类错位在嵌入侧**结构性不可能**（同一构建产出），覆盖目录成为唯一可错位面且仍带戳警告；发布管线随之砍掉独立 `flux-web-ui.tar.gz` 工件与归档内 `web-ui/` 载荷。
  - 代价（均已接受）：① 从源码构建默认需要 node/pnpm——无工具链时 build.rs 写占位页保证编译成功（`FLUX_WEB_UI_NO_BUILD=1` 跳过构建、`--no-default-features` 纯无头）；② rust-embed 宏展开要求目录存在（所有模式），占位页即为此兜底；③ 新哈希文件名不触发 cargo 的 `include_bytes!` 追踪——由 build.rs `rerun-if-changed` 补位；④ 二进制 +~2.5MB（woff2 字体不可压，占大头）；⑤ 半覆盖可能拼出混版本站点——用户显式定制的固有属性，Gitea 同款；⑥ 启动期**不做**覆盖目录/占位页校验（原 T-06 的存续部分并入本条）——「agent 可用、UI 待补」不是致命状态，失败在请求期归因：index 读失败 → 500 + warn、缺失资产 → 404（曾试过 fail-fast 已删）。机制背景见 architecture.md「Web UI 静态伺服」。
  - 重开评估条件：占位页被证明误导真实用户（转 fail-fast 并在启动日志给修复指引）、覆盖目录错位成为高频支持负担（考虑覆盖目录整体验证或按目录版本门控），或引入多入口页面使请求期失败形态复杂化。

### T-15 终端 Nerd Font 面是否子集化（first recorded 2026-09-17）

- 日期不详（未记录）: 子集化实验——切到 prompt 图标区段（~38 KB）；advantage: dist/二进制立省 ~1 MB（依据当时的「~95% 是文件图标家族、纯冗余」账目）。
- 2026-09-17: 回滚——`JetBrainsMonoNerdFontMono-Regular.woff2` 保持上游全量（~1.0 MB）随 dist 分发，**不做子集化**；advantage: 相对上一行的子集化——网页终端跑的是**任意真实程序**，不只 our prompt——nvim/主题/eza --icons 等输出的 Nerd 字形全部落在 Unicode 私有区（U+E000-F8FF 及若干散点），**系统字体对该区没有任何回退**，缺一个字形就是一个实心豆腐块；实测子集化后 nvim 无法正常使用（用户报告）。当时的「~95% 冗余」账目只对静态巡览成立，对交互式终端不成立——面内图标家族（Material/Devicons/Codicons/Seti/Octicons/Weather）正是第三方 prompt/工具生态实际引用的码位，「浏览器终端永远不打印」是错误前提。代价（接受）：dist/二进制 +~1 MB（dist 最大单项）。机制背景见 architecture.md「前端：分层结构 → 终端 tab」。重开评估条件：**不存在**「按码位清单」的静态子集——只有当浏览器平台提供动态字体回退（PUA 段落到系统已装 NF 字体的 fallback，CSS `unicode-range` 依赖已知码位清单，同样做不到）或终端引入「输出字形白名单」的显式契约（程序声明自己只用哪些私有区字形）时才可重开；届时也应做成按需懒加载的分段 face，而非砍单一 face。

### T-16 流式跟随的脱离信号如何判定（first recorded 2026-09-17）

- 日期不详（早于本条记录）: 脱离信号 = scroll 事件读作上滚即脱离（无容差）；advantage: 无记录（初版行为——保留此行只为记录演化起点）。
- 2026-09-17: 收紧为**上滚且已离开底部 ≥2px**（goingUp 且 !nearBottomPx(pane, 2)，容差覆盖分数缩放的取整），钉底的 goingUp 事件按渲染收缩的 clamp 处理；不走 wheel/pointer/touch 等用户输入事件作唯一脱离信号；advantage: 相对上一行的初版判定——钉在底部时流式边界渲染净变矮（段落提交 reflow、代码块 fold-back 删节点）会让浏览器把 scrollTop 向下 clamp 并派发 scroll 事件，读作上滚却不是用户离开：误判即脱离且后续所有 `scheduleFollow` 被 stick 门挡为 no-op，跟随死亡直到手动滚回 96px 重附窗口（展开思考块流式查看时概率性触发；收起态内容不占 pane 滚动高度，永不触发）——clamp 免疫 + 容差让流式跟随结构性存活。备选「程序化写入后同步 lastTop」未采用：rAF 写入自身触发的 scroll 事件已同步 lastTop，clamp 免疫已覆盖写入与收缩同帧的时序。不走输入事件判定的原因：那需要第二套监听面并对输入方式逐一枚举，jsdom 也无法合成。代价（接受）：从底部向上滚的前 ≤2px 首个事件可能晚一拍才脱离，次个事件必然脱离容差带——以 detach 的轻微滞后换结构性存活。机制背景见 architecture.md「前端：流式渲染 → 滚动跟随」。重开评估条件：容差被证明吞掉真实用户的首个滚轮 tick（触控板惯性首帧通常 >2px，实际风险低），或出现必须区分程序化/用户滚动的新交互（如滚动位置联动 UI），再议输入事件判定。
