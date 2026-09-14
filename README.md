# Flux

> English version: [README-en.md](README-en.md)

一个通用编码 Agent 框架：Rust 编写的本地服务器 + 浏览器 Web UI 前端。

## 功能特性

- **内置工具**：`read_file`、`edit_file`、`write_file`、`replace_lines`、`list_directory`、`glob`、`grep`、`bash`，以及每 chat 的 `question`（向用户提问）、`state_get`/`state_set`、`buf_read`（溢出输出分页读取）。输出限幅防止上下文溢出：所有工具结果统一经过 8000 字符内联预算——超限输出整体存入每 chat 的溢出缓冲，模型用 `buf_read` 分页读取；grep 按匹配窗口塑形（上限 500 条），glob 上限 500 条。
- **Agent Skills**：惰性加载的能力包（`SKILL.md` 目录，遵循 [Agent Skills 标准](https://agentskills.io)）——模型经 `skill_list` 发现、按需经 `skill_read` 加载完整指令（不向 prompt 注入任何内容；项目技能在 `<workdir>/.flux/skills/`，全局在 `~/.flux/skills/`，同名时项目覆盖全局）。Web UI 的 Skills 对话框管理它们：从本地目录或 git URL 安装（可选 subpath 支持多技能仓库）、删除全局条目——立即生效，无需重启。
- **LLM Provider**：OpenAI / OpenAI 兼容端点——纯端点（id · url · api key），由 Web UI 的 Providers 对话框管理、存入服务端数据库（没有配置文件）。**本地模型注册表**按模型保存请求参数（自动从 [models.dev](https://models.dev) 元数据富化）；切换对话的 provider 在轮边界热切换——引擎在全量历史上原地 re-begin，不会中断。
- **流式输出**：经 gRPC-Web 实时推送文本 + 推理（reasoning）增量——接收与渲染解耦一帧（delta 追加到 raw 缓冲，rAF 合帧渲染，每帧至多一次增量渲染）。已提交段落只渲染一次、append-only 追加，代码块稳定打字、高亮恰好一次；粘滞滚动状态机让跟随平滑，不把正在阅读的用户拽回去。
- **MCP 客户端**：把外部 MCP 服务器作为子进程启动并暴露其工具。由 Web UI 的 MCP 对话框管理（存入服务端数据库；**persist-first + 即时应用**——管理器启动子进程、注册工具，匹配的对话在轮边界重建引擎），并带自愈监督：异常退出的子进程按封顶退避自动重启。
- **无审批**：工具直接执行——没有确认环节。对话的 workdir 边界作为调用上下文（`ToolCtx`）到达工具，路径在边界内解析，工具错误以结果文本返回、模型自行读取并纠正。真正的隔离来自 OS/容器边界。内置 `question` 工具让模型在轮次中向用户提问（agent 产出问题文本 + 选项，经对话内的内联卡作答）。
- **流取消与插话**：随时停止生成（Stop 按钮或 Esc）；轮次进行中发送消息走**单个 interrupt-send RPC**——服务端把「取消当前轮」与「排队我的消息」融合为一次操作，插话顺序由构造保证。
- **从任意消息 fork**：非破坏性分支——新对话复制源 transcript 至所选用户轮**之前**，该轮内容预填进 fork 的输入框；源对话原样不动。
- **对话历史**：跨重启的持久化对话与完整消息历史；30s 会话宽限期让租约在页面刷新后存活。
- **内置终端**：交互式 shell（e4pty PTY，xterm.js UI），走专用 `/ws/term` 侧信道——每 chat 可开多个，经 dock「+」或空态动作按需创建；跨 tab/对话切换保活，页面刷新后在会话宽限期内重连同一 PTY（256 KiB scrollback 回放）。
- **Tab 化右坞**：打开的文件以 tab 累积（多文件、编辑器式）；文件正文横向滚动；`.md` 经共享 markdown 管道渲染并带 Raw 切换；终端 tab 恒钉其后。
- **Web UI**：React 聊天界面，由 flux-server **默认伺服**，与 Connect API 同端口（单一监听器）——`./scripts/run-server.sh`（UI 缺失时自动构建）；`--no-web` 无头运行。仅限本机之外暴露必须经 TLS 反向代理——服务端没有认证层。

## 项目布局

```
flux/
├── proto/flux/v1/          # 线协议契约（唯一契约源 → Rust + TS codegen）
├── crates/                 # Rust workspace（server + 内核 + 工具）
├── clients/web/            # Web UI（React 19 + Radix + Tailwind v4 + zustand，Vite）
└── docs/
```

## 环境要求

| 工具 | 版本 | 说明 |
|------|------|------|
| Rust | stable 通道 | 仓库钉 `rust-toolchain.toml`（stable + rustfmt/clippy），rustup 自动就位 |
| Node | **24 LTS** | 唯一支持线（`engines` 声明，pnpm 对其他版本告警）；推荐 nvm 管理；脚本对版本敏感的 flag 自带守卫 |
| pnpm | 11.x | 版本钉在 `clients/package.json` 的 `packageManager`；脚本发现缺失会自动安装 |
| protoc | 系统二进制 | `protobuf-compiler`（apt）/ `brew install protobuf`——`flux-proto` 编译期调用 |
| Chrome | 较新版本即可 | 仅 headless e2e（`pnpm run ui-check`）需要 |

buf 等前端契约工具全部走 `clients/web` 的 devDependencies（pnpm 严格布局解析），无需全局安装。

## 快速开始

```bash
cargo build --release
./scripts/run-server.sh             # UI 缺失时自动构建，然后同时伺服 UI + API
# 或：cargo run -p flux-server      # Connect API + 二进制旁的 web-ui/（如存在），
                                    # --web-assets-dir 可指定任意构建产物目录
```

没有配置文件——一切要么是 CLI flag（`--host`、`--port`、`--db-path`、
`--preamble`、`--no-web`、`--web-assets-dir`；见 `flux-server --help`），
要么由 UI 管理进服务端数据库。

打开 `http://127.0.0.1:8080`，在顶栏的 Providers 对话框添加一个 provider
端点，然后在新建对话对话框里选择工作目录，开始对话。

## 安装（发布产物）

无需克隆源码，直接使用 [Releases](https://github.com/km0e/flux/releases) 的产物：

```bash
# ① 服务端二进制（headless）
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/km0e/flux/releases/latest/download/flux-server-installer.sh | sh
# ② 浏览器 UI（解压至 ~/.flux/web-ui，服务器自动识别）
mkdir -p ~/.flux && curl -fL \
  https://github.com/km0e/flux/releases/latest/download/flux-web-ui.tar.gz | tar xz -C ~/.flux
```

（Windows PowerShell 同理，使用 `flux-server-installer.ps1`；安装至
`~/.cargo/bin`，PATH 脚本与卸载 receipt 自动写入。）

**带浏览器 UI 的完整形态**：下载平台归档解压即用 —— `web-ui/` 与二进制并排。

UI 资产解析顺序：`--web-assets-dir` > 二进制旁 `web-ui/` > `~/.flux/web-ui` >
无（headless）。

## 文档

| 文档 | 回答的问题 |
|-----|---------------------|
| [`docs/architecture.md`](docs/architecture.md) | 内部如何工作？（当前架构，含图示；另有英文版） |
| [`docs/decisions.md`](docs/decisions.md) | 既定取舍——我们刻意选择不改进什么 |
| [`CHANGELOG.md`](CHANGELOG.md) | 版本历次变更（dist 解析它作为 GitHub Release 说明） |
| [`AGENTS.md`](AGENTS.md) | AI 编码助手的约定与上下文 |

## 线协议（Connect / gRPC-Web）

浏览器经 [Connect](https://connectrpc.com)（gRPC-Web）与 UI 同端口通信。
`proto/flux/v1/*.proto` 是**唯一契约源**——Rust 绑定在构建期生成
（`flux-proto`），TypeScript 绑定由 `buf generate` 派生（web 包的
prebuild/pretest 钩子）。

| 服务族 | RPC | 用途 |
|--------|------|---------|
| EventService | `Subscribe` | 会话级事件流：身份锚（流开 = attach/采纳，首帧 `ready` 携 token + leases），全部对话事件 + keepalive；流断 = detach |
| ChatService | CreateChat · ListChats · OpenChat · ClaimChat · CloseChat · DeleteChat · RenameChat · SendMessage · CancelRound · ForkChat · SwitchProvider · AnswerQuestion | 对话控制（租约门控；调用者的会话 token 走 `x-flux-session` metadata） |
| ProviderService / ModelService | ListProviders · GetModels · AddProvider · RemoveProvider / ListModels · SaveModel · RemoveModel · SyncModels | Provider 注册表 + 本地模型注册表（api_key 永不出服务端） |
| McpService | ListServers · AddServer · RemoveServer | MCP 启动列表（persist-first + 即时应用） |
| SkillService | ListSkills · AddSkill · RemoveSkill | 全局技能管理 |
| FileSystemService | FsList · FsRead | workdir 选择器 + 文件 Explorer |

应用级失败走应答的内联 `error` 字段；传输/基础设施失败走 gRPC status。
完整语义——身份生命周期、租约/观看者模型、流元素、快照对账——见
`docs/architecture.md` §3.8。

## Web 前端

React 19 + TypeScript on Vite；Radix UI 原语承载 dialog/menu/tooltip/tabs
行为；zustand 管理应用状态；Tailwind CSS v4 驱动组件样式；marked +
DOMPurify + highlight.js 驱动命令式 markdown/流式渲染管线（rAF 合帧、
append-only 段落——见 `docs/architecture.md` §4）。

```bash
cd clients && pnpm install   # pnpm workspace 根
cd web
pnpm test                    # vitest 单元测试
pnpm run build               # tsc --noEmit + vite build → dist/
```

## 脚本

常用开发任务的辅助脚本。所有脚本均有 `.sh`（Linux/macOS）与 `.ps1`（Windows）成对版本。

| 脚本 | 说明 |
|--------|-------------|
| `scripts/run-server.sh` | 经 `cargo run --release` 启动 flux-server（CLI flag 透传）。Web UI 默认伺服——缺失时脚本自动构建（`--no-web` 无头）；数据库默认 `~/.flux/flux.db`（`--db-path` 覆盖）。 |
| `scripts/test.sh` | 全量验证：`fmt` → `clippy` → `cargo test` → `tsc` → `vitest` → `vite build` |
| `scripts/package-web.sh` | 构建 Web UI（Vite）并组装可伺服根目录——`index.html` + 内容哈希 `assets/*`——输出到 `clients/web/dist`（或 `--out DIR`） |
| `scripts/fetch-fonts.sh` | 从官方发布刷新内置字体（JetBrains Mono + Nerd Font 补丁、IBM Plex Sans，OFL-1.1）。字体文件已提交入库——此脚本仅供升级时手动运行，构建过程不访问网络 |

常用工作流：

```bash
# 本地开发
./scripts/run-server.sh                     # 启动服务端 + UI（web 默认开，UI 缺失自动构建）
./scripts/test.sh                           # 运行全部检查

# 发布打包（dist——配置唯一来源 dist-workspace.toml，本地与 CI 读同一份定义）
cargo install cargo-dist --locked           # 一次
dist build                                  # 本地产出发布形状产物（宿主目标：归档 + web-ui + checksum）
dist build --target aarch64-unknown-linux-gnu   # 本地交叉编译（需 cargo-zigbuild；Windows 目标需 cargo-xwin）

# 正式发布
git tag v0.x.y && git push origin v0.x.y    # release.yml：5 平台原生构建 → GitHub Release
```
