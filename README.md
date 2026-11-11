# 蜂递 · WagglePost

**换 Agent，不丢任务。** 本地多 Agent 路由：一个入口派活，写者交付可核对的候选，另一种 Agent 只读复核同一版本；中断后从已确认的检查点继续。派发 → 独立复核 → 按候选接收，三步都可查证。

WagglePost 0.2 是单二进制的本地工具：Rust core + SQLite + 内嵌 React 工作台（仅 `127.0.0.1`）+ 同二进制 CLI。路由与交接组装是本地确定性规则，不额外调用规划模型。

## 一次真实换班（合成样例，零模型调用）

写者配额阻塞后，换班包按字节预算组包：必保留字段（目标、验收、范围、已确认进度、失败位置、下一步）从不裁剪；剩余空间先放变更路径和检查结果，再放证据；装不下的可选条目整段省略并在账单中列明「本次省略」。下面是一次真实运行的换班正文（节选，完整输出可本地复现）：

```text
## 目标 / Goal
在配额阻塞后从检查点换班继续，保持必保留内容完整。
Resume from the checkpoint after the quota block with must-keep content intact.
唯一结尾句：W3 证据链在此结束 ⏹ unique-ending-marker-9f2c

## 已确认进度 / Confirmed progress
上一写者已保存的改动文件：
tracked-evidence-00.txt, tracked-evidence-01.txt, …（共 60 个，完整清单随包发送）

## 失败位置 / Failure location
simulated quota exhausted

## 下一步 / Next step
从检查点继续：先复核已改动内容，再补齐目标 / continue from the checkpoint: review the kept changes, then finish the goal

## 汇报契约（必须遵守）
结束时输出且只输出一个标记为 wagglepost-result 的围栏 JSON 块：…
```

同一运行的交接账单——完整候选正文与实际交付的对照：

| 账单项 | 字节 |
| --- | --- |
| 必保留下限（必保留内容 + 标题 + 分隔 + 汇报契约 + 省略提示预留） | 3,062 |
| 实际交付正文（必保留全部完整 + 可选材料 + 省略提示） | 4,566 |
| 对照：本地构造的全量重讲载荷（仅对照，永不发送） | 5,743 |

实际交付 4,566 字节 < 对照 5,743 字节；六个必保留字段全部完整送达，唯一结尾句原样保留在目标段末尾。这是「全部装入」场景；数字由确定性测试 `tests/wp_evidence.rs` 产生（固定输入固定字节），本机重跑即得相同字节。

「确实省略」场景来自同一条真实派发路径（复核者交接，写者改动了 300 个文件，变更路径清单超出预算被整段省略）：必保留下限 1,258 字节、实际交付 1,521 字节、对照 97,779 字节，省略 1 项并附省略提示；双语验收与唯一结尾句仍完整送达。两个场景的数据都来自 `tests/wp_evidence.rs` 的真实运行，README 各自标注。

## 3 分钟无账号演示（全部模拟，零模型调用）

**方式 A · 便携包（无需源码）**：每个 Windows CI run 都产出 `wagglepost-windows-x64-portable` 产物——ZIP 内含 `wagglepost.exe`、`LICENSE`、`SHA256SUMS.txt`，打包前已在源码树外完成启动冒烟。到 [Actions 页面](https://github.com/xiaofeilovecoding/agent-wagglepost/actions) 打开一个已完成的 run，在 Artifacts 区下载：

```powershell
# 解压到任意目录后，在该目录内：
.\wagglepost.exe serve --managed-root $env:TEMP\wagglepost-demo
# 控制台打印一次性启动码；浏览器打开它提示的 http://127.0.0.1:<port> 输入
```

**方式 B · 从源码构建**：

```powershell
git clone https://github.com/xiaofeilovecoding/agent-wagglepost.git agent-wagglepost
cd agent-wagglepost\web
npm ci
npm run build
cd ..
cargo build --locked --release

.\target\release\wagglepost.exe serve --managed-root $env:TEMP\wagglepost-demo
# 控制台打印一次性启动码；浏览器打开它提示的 http://127.0.0.1:<port> 输入
```

进入工作台后：

1. 点「试用样例 · 决策续答」：中断决策 → 预览正文 → 确认续答 → 复核接收，全链路一次走完。
2. 点「试用样例 · 额度换轨」：额度阻塞 → 保留成果 → 预览换轨正文 → 确认换轨 → 复核接收。
3. 观察顶部 Agent 状态条（待你决定 → 执行中 → 可接手）、送达动效（只随服务器确认事件起飞）和任务详情里的交接账单（字节数、digest、包含/排除项、对照载荷）。

样例任务始终由 `fake` 模拟运行时执行并明确标注「模拟」——不需要任何模型账号。

## 它做什么

- 提交 → 计划与路线回执（每个候选都有可复算的拒绝原因）→ 预览实际发送正文并批准（digest 绑定）→ 隔离 worktree 启动写者 → 冻结候选 → 第二运行时只读复核同一候选 → 人工按版本接收或导出补丁。
- 中断换轨绑定目标运行时、版本、检查点、正文摘要和预算；只有真实 `agent_started` 才发启程/换轨事件；整树退出确认前不转交写槽。
- 同仓单写者、全局单执行槽；失败也计入启动预算；重复请求按幂等键回放。
- Agent 输出在事件、收件箱、结果和 stderr 日志落盘前脱敏；用户代码和补丁不被改写。

## 真实能力矩阵（与 `wagglepost doctor` / 工作台「工具」页同一语义）

能力按五档状态管理：**未安装 → 需配置 → 已探测 → 验证通过 → 不支持**。验证绑定精确版本与二进制路径——版本或路径变化、探针超时或退出非零都会让验证失效（fail closed）；help 旗标只作探测，任何角色都不因 help 字符串而开放；重跑一次真实任务自动升级。下表是 0.2 开发机的验收基线（v0.2.0，2026-11-10）：

| 运行时 | 写者 | 复核者 | 状态与证据 |
| --- | --- | --- | --- |
| OpenCode v2.2.6 | —（实验/仅检测） | ✅ 验证通过（offline 规则探针 + real_loop） | 官方 standalone zip 固定版安装；`run --standalone --format json` 结构化事件；复核者运行在受管快照（候选自带的 opencode 配置/指令文件复制时剥除、符号链接跳过），使用隔离用户目录，权限默认拒绝，仅允许 read、glob、grep。禁用项目配置，每次启动先离线核对完整策略及来源，再传递凭据。开发期间已通过蜂递调度实测真实模型复核 |
| DeepSeek Harness (DSH) 0.2.1-rc.5 | —（实验/仅检测） | —（实验/仅检测） | `headless --json` 传输契约实测通过（NDJSON 事件、厂商 usage）；headless 无原生权限限制机制，因此角色保持实验，不进入自动路由（`--session-id` 续跑未经验证，不宣称） |
| Claude Code 2.1.278 | — | 已探测 | `--disallowedTools`+`--strict-mcp-config` 可证边界；本批未实跑，保持已探测 |
| Codex CLI | 未安装 | 未安装 | 本机未检测到入口；`--sandbox` 契约在 help 可核，安装后 `wagglepost verify codex` 写入验证记录 |
| Fake | ✅（模拟） | ✅（模拟） | 进程内模拟运行时；fixture 契约测试；UI 永远标注「模拟」 |

说明：

- **写者实验标记的含义**：该运行时无法把 shell/写工具原生约束在受管 worktree 内（蜂递侧有 Job Object 进程树 containment、单写锁与 worktree 边界，但「进程能逃出 worktree 写别的路径」无法被厂商机制排除），因此不进入自动路由，也无法被选为写者派发（派发门槛直接拒绝）；实验角色在能力表中如实展示，等待未来出现可证实的原生约束机制，且一次真实成功运行也不会解除该标记。
- **你机器上的矩阵会不同**：`wagglepost doctor --json` 输出本机真实状态；`wagglepost verify <adapter>` 用无账号的离线契约（版本 + help 传输旗标）写入验证记录，随后第一次真实任务把证据升级为 real_loop。
- 只在所声明版本和角色有证据时宣称支持；版本漂移会使验证失效并提示重新验证。

## 接入你已有的 CLI（可选，无需任何账号也能用 fake）

| 运行时 | 发现方式 | 需要的配置 |
| --- | --- | --- |
| OpenCode | `WAGGLEPOST_OPENCODE_EXE`、`PATH`、或 `D:\dev\tools\opencode-*` 固定版目录 | `DEEPSEEK_API_KEY`（环境变量，仅白名单转发给隔离进程）或 `WAGGLEPOST_OPENCODE_MODEL=provider/model` DeepSeek Key 通过环境变量提供。模型选择变量只设置模型 ID，隔离进程不会读取个人 OpenCode 登录状态。 |
| DSH | `WAGGLEPOST_DSH_EXE`、`PATH`（桌面版自带 `dsh.cmd` 可直接命中） | 其自身登录态；角色为实验，不自动路由 |
| Codex | `WAGGLEPOST_CODEX_EXE`、`PATH` | 其自身登录态 |
| Claude Code | `WAGGLEPOST_CLAUDE_EXE`、`PATH`（npm shim 会自动解析到真实 exe） | 其自身登录态 |

Key 只从环境读取，只转发给隔离的运行时进程；不写入回执、日志、UI 或 Git。探测过程只做版本/help 的只读检查，不提交模型、不安装、不扫描个人会话或凭据库。

## 交接账单与用量

任务详情在派发/换轨/续答确认旁显示「将发送的范围」：**必须发送**（必保留内容，按模式固定，从不裁剪）、**本次省略**（装不下的可选条目，整段省略并计数）、UTF-8 字节数与必保留下限、digest、工作树与候选短标识；digest 绑定该次实际发出的正文，换预算、任务版本或运行时身份后旧确认失效；必保留内容超限时预览直接返回 required/limit 并提示减少必保留内容或调高上限，派发零启动。本地构造的对照载荷字节（同任务全量重讲的粗略基线）仅作对比展示、永不发送。标题、编号、中文与 emoji 均按实际 UTF-8 字节计，边界不破坏字符。token/缓存/费用只在提供方真实返回时展示并注明来源（如 `opencode step_finish.part.tokens`），未知留空；不为估算发起额外模型请求。

## CLI

```text
wagglepost serve [--port N] [--managed-root DIR] [--cli-repo DIR]...
wagglepost doctor [--json]                            runtime capability check
wagglepost verify <adapter>                           offline no-account rule probe (help=probe only)
wagglepost submit --file draft.json --json
wagglepost status <task_id> --json
wagglepost request show <request_id> --json
wagglepost approve <task_id> <plan_hash> [--json]
wagglepost approve <task_id> <plan_hash> --digest <sha256>
wagglepost answer <decision_id> <option_id> [--json]
wagglepost answer <decision_id> <option_id> --digest <sha256>
wagglepost dispatch <task_id> --json
wagglepost cancel <attempt_id> --json
```

`--cli-repo` 可重复，限制 CLI 授权仓库；浏览器会话有独立 token，不受 CLI 仓库列表限制。CLI 与 Web 不共享凭据。

## 构建与门禁

```powershell
cd web; npm ci; npm run build; cd ..
cargo test --locked --release -- --test-threads=2
cargo test --locked --release --no-run   # 全部测试目标零警告
cargo build --locked --release

python -m pip install playwright==1.64.1
python -m playwright install chromium
python scripts/run_ui_gates.py --exe target\release\wagglepost.exe --out ci-artifacts
```

GitHub Actions 只有一个 Windows job：`windows-build-test-ui`。它使用 `npm ci`、`cargo --locked`，在临时受管根和随机回环端口启动生产 exe，运行真实 Chromium（16 项行为门禁），核对服务端实际资产与 `web/dist/ASSETS.sha`，并打包 Windows x64 便携 ZIP（exe + LICENSE + SHA256SUMS，打包前在源码树外冒烟启动）。工作流仅 `contents: read`，触发为 `push` main 与 `pull_request`，不使用 `pull_request_target`，不接入秘密或模型；artifact 只含合成测试数据（截图、门禁 JSON 与便携包）。

## 安全边界

- 仅绑定 `127.0.0.1`；Host/Origin 校验；一次性启动码 → HttpOnly cookie；浏览器写操作需 CSRF。
- CLI 使用独立 bearer token，可用 `--cli-repo`/`WAGGLEPOST_CLI_ALLOWED_REPOS` 限制仓库范围。
- 用户仓库 dirty 时拒绝；worktree 从显式提交创建，不复制未提交内容。
- 结果来自本机 worktree 观察，Agent 自述不自动升级为独立证据。
- 新运行时经 argv 数组启动（禁 shell 拼接、禁浏览器注入 executable/flag）；stdout 单帧限长 1 MiB；stderr 每次尝试最多 2 MiB，日志默认保留 7 天；无 telemetry。
- Windows Job Object 先纳管后运行；取消/超时必须确认整树退出才算停止，未知状态禁止自动重派。

## 已知限制

- **平台**：Windows 10/11 x64 已验证；Linux 源码可编译但未做等价验收，不宣称支持。
- **真实第二写者未验收**：OpenCode/DSH 的写者角色如实标为实验（无原生 worktree 约束机制）；跨运行时真实续写仍是开放项。真实模型复核已验收（OpenCode reviewer，见能力矩阵）。
- **原生同会话恢复未证明**：没有确认同一原生会话时，UI 明确走「受控检查点回退」，预览完整精简交接包后重新批准。
- **ZCode / Gemini CLI / Copilot CLI**：计划支持；ZCode 无公开可验证的运行协议，产品只显示计划状态，不读取其本地会话数据。
- 分析并行、自动返工、批量评审回传、外部触发/模型代理/自动发布不在 0.2。
- 本地单用户工具，不适合作为多租户或公网服务部署。

## 开发

```text
src/            Rust core / HTTP / runner / adapters / capability / store
web/            React + TypeScript；npm run build → web/dist（构建时校验并嵌入）
tests/          G1–G8、WP 回归、能力表与 adapter 契约 fixture；默认离线、确定性
scripts/        ui_gates.py 与 run_ui_gates.py（真实生产 exe + Chromium）
artifacts/      交付证据（不进仓库公开历史的部分在本地受管根）
```

贡献与安全说明见 [CONTRIBUTING.md](CONTRIBUTING.md) 和 [SECURITY.md](SECURITY.md)。许可证：[MIT](LICENSE)。English documentation: [README.en.md](README.en.md)。
