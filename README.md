# 蜂递 · WagglePost

**换 Agent，不丢任务。** 本地多 Agent 路由工具：一个入口派活，写者交付可核对的候选，另一种 Agent 只读复核同一版本；中断时从已核对的检查点接着做。

WagglePost 0.1 是一个单二进制本地服务：Rust core + SQLite + 内嵌 React Web（`127.0.0.1`）+ 同二进制 CLI（`wagglepost`）。路由与交接组装跑在本地规则上，零额外模型调用。

## 它解决什么

- **路由回执可反查**：派发前看到候选、淘汰原因（稳定 `reason_code`，中英双语）、权限、预算和每一步发给谁；运行后能从输出追溯到实际 attempt、目标 worktree、输入版本。
- **可核对的断点换轨**：额度、取消或失联后，先确认旧写者进程树退出，保留改动为检查点，显式批准后再换 Agent 续跑（真实换写者要求第二适配器单独通过写入能力门槛；未通过时如实显示阻塞并保留补丁导出）。
- **输出直接进入下一步**：第二运行时从冻结候选的哈希、关键变更与证据索引开始复核，不重读前一段对话；按角色组装的正文默认 8 KiB 预算，批准绑定正文摘要（sha256）。
- **同仓单写者 / 单执行槽**：全局 1 个运行槽，同仓库最多 1 个写入者；每次真实启动计数，失败也计数，重启、换人、重复回答不会恢复已用额度。
- **进程归属**：Windows 原生 Job Object 先纳管（CREATE_SUSPENDED → Assign → Resume），停止以 Job 句柄确认退出为准；环境变量白名单过滤，凭据不传入 Agent。

## 快速开始

```powershell
# 开发构建（或 cargo build --release）
cargo build
.\target\debug\wagglepost.exe serve            # 默认 127.0.0.1:8742
# 控制台会打印一次性启动码（startup code），浏览器打开后输入即可进入工作台
```

Web 工作台：提交任务 → 查看路线与原因 → 确认派发 → 独立 worktree 产出 → 第二 Agent 复核同一候选 → 展示差异并人工接收（补丁导出，不改用户主分支）。

CLI（与 Web 共用同一回执契约，读写 `~/.wagglepost` 同源的会话文件）：

```text
wagglepost doctor [--json]                    # 运行时能力自检
wagglepost submit --file draft.json --json    # 提交任务草案（默认入待批准）
wagglepost status <task_id> --json            # 任务快照（含回执与时间线）
wagglepost inbox --after <seq> --json         # 收件箱（待你处理 / 已回收）
wagglepost request show <request_id> --json   # 请求对账（响应未知时按原 ID 查）
wagglepost answer <decision_id> <option_id>          # 预览将发送的交接正文（不提交、不启动）
wagglepost answer <decision_id> <option_id> --digest <sha256>   # 确认提交（摘要须与预览一致；答后排队）
wagglepost why <task_id> --json               # 路由原因 + 事件
```

## 运行时

| 运行时 | 0.1 职责 | 入场验证 |
| --- | --- | --- |
| Codex CLI | 写者（`exec --json --sandbox workspace-write`）+ 可选只读 | 本机实测 `0.166.0-alpha.5.1`，JSONL 事件、用量、resume 均已验证 |
| Claude Code | 只读复核（`--print --output-format stream-json`，工具收窄 Read/Grep/Glob，strict-mcp 关闭 MCP） | 本机实测 `2.1.278`，事件与用量已验证 |
| DeepSeek Harness | 仅能力探测（0.1 不派发） | 本机 `0.2.1-rc.5`，未过同等门槛前如实列为阻塞 |
| Fake | 离线演示与测试（固定脚本注入额度阻塞、重复事件、挂起、非零退出） | 始终标注「模拟」；生产动作只由真实事件触发 |

`wagglepost doctor` 输出每个运行时的版本与能力（写入/只读/续答/可取消）。

## 安全边界

- 仅绑定 `127.0.0.1`，Host/Origin 校验，一次性启动码换 HttpOnly 会话，浏览器写操作需 CSRF；CLI 走独立 Bearer 通道。
- 用户仓库 dirty 时拒绝并提示先提交或显式给定 `base_commit`；worktree 从显式 commit 创建，原工作目录 dirty 内容不复制。
- 结果采集来自本机对 worktree 的观察（`git diff/status`），Agent 自述不会自动升级成检查证据；接收绑定 task revision + 候选哈希。
- 日志 2 MiB/attempt 上限，输出先截断再持久化；无 telemetry。

## 开发

```text
src/            Rust core（store/events/routing/handoff/runner/adapters/http）
web/            React/TS 前端（npm run build → web/dist，随二进制嵌入）
tests/          G1–G8 离线门禁（fake 运行时，确定性，无需网络）
scripts/        ui_gates.py（Playwright 行为门禁，需本机 Playwright）
```

```powershell
cargo test                                      # 25 项离线门禁
cd web; npm install; npm run build; cd ..       # 构建前端后 cargo build 会重新嵌入
python scripts/ui_gates.py http://127.0.0.1:8765 <startup-code>   # 11 项浏览器门禁
```

## 已知边界（0.1）

- 真实换写者（第二原生工具继承部分改动续写）要求第二适配器独立通过写入门槛；本批未通过，换轨候选如实阻塞、保留补丁导出与人工接手。
- 分析并行、自动返工、批量评审回传、外部触发/通知、模型代理与自动发布不在 0.1 范围。
- 优先验收 Windows 10 x64；Unix 路径保留但未经等效验收。

License: MIT
