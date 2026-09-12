# Flux 既定取舍（Accepted Tradeoffs）

> 明确接受、勿当缺陷修的既有妥协。新增条目需说明「为何接受」与「重开评估的条件」。

## Accepted tradeoffs

### T-01 VACUUM 写锁窗口

WAL 下 VACUUM 重写整库、全程持写锁（freelist 达阈值才触发，失败仅瞬时写错误 + 日志）。已定案接受；如需硬化：启动监听前执行一次，或改增量清理模式。

### T-02 bash runner 类命令的固有风险

以命令首词判定类别的防御语义下，`eval`/`sudo`/解释器直调类 runner 无需藏匿即可执行任意命令——这是「按首词放行」的固有语义而非绕过（提示文案展示完整命令）。已排除：runner 黑名单（误禁合法授权）、禁用首词放行（牺牲便利）。审批层已删，本条保留以防审批层恢复时重蹈。

### T-03 state 工具的通用 KV 行为

state 写任意 key / 读未知 key 返回空串是**设计行为**（agent 跨轮次通用 KV 通道；schema 枚举只作引导非白名单；读写对称）——勿当缺陷修。

### T-04 性能项排除

流式扇出逐 viewer 克隆（单 viewer 无收益）、无界队列加固（防御性）、全局锁跨异步等待两阶段化（单用户收益≈0——多窗口前提变化时按新证据重开评估）。破坏性协议改动（二进制帧/压缩）经实测线格式非瓶颈而否决。

### T-05 Explorer git 状态用 git CLI 而非 git2/gix

`fsbrowse` 的目录状态收集仍 shell 到 `git` 子进程（每列目录两次：`rev-parse --show-toplevel` + `status --porcelain -z`），已用 `-- .` pathspec 限定被列子树、`-unormal` 折叠 untracked 目录、blocking 线程池执行消化成本。未引入 `git2`（C 工具链依赖 + unsafe + worktree/配置兼容面）或 `gix`（纯 Rust 但 status 高层 API 仍在变动、依赖树重）——对「每 15s 对已加载目录重列」的 UI 负载，CLI 兼容性与零新增重依赖胜出。重开评估条件：status 调用成为实测热点（大仓库冷扫描 > 数百 ms），或 gix status API 进入稳定层。

### T-06 web 构建缺失不做启动期校验

web 伺服（默认开）启动不校验 dist 布局（曾试过 `index.html` + ≥1 JS + ≥1 CSS 的 fail-fast，已删）：构建不完整在**请求时**自然暴露——index 读失败 → 500 + warn 日志；缺失资产 → 404（浏览器 console 可见）。理由：服务端拒绝启动会把「agent 可用、UI 资产待补」的正常状态误判为致命错误；失败链路本身已可归因（warn 指名路径 / console 指名资源）。重开评估条件：请求期暴露被证明难以归因（如用户反馈分不清 assets_dir 配错与构建缺失），或引入多入口页面使失败形态复杂化。

### T-07 前端流式渲染不换渲染器/不引流式库

流式 markdown 管线维持 marked + splitAndFold/ParagraphSplitter + renderStableSlice + FenceCache（2026-08-28 调研定案，勿再提议更换）：① marked 无增量续接 API——末尾 `text` token 吸收未闭合行内构造，按 token 边界缓存会丢内联上下文；② micromark 真流式入口仅 Node.js，浏览器退化为全量 re-parse（O(n²) 依旧）；③ 生态内「streaming markdown renderer」（streamdown、Vercel AI SDK 等）实为 remend 自愈不完整块 + 全量 re-parse + DOM diff，且全带 React peer dep。现有管线的块级缓存/稳定前缀方案比这些实现更精细；残余 O(n²) 仅存于无空行墙式文本的病态输入（实测 64KB ≈ 12ms，被「当前段落」钳制）。重开评估条件：出现框架无关、带稳定前缀缓存契约的增量渲染器，或真实输入实测超帧预算。
