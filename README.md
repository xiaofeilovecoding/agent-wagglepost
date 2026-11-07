# 蜂递 · WagglePost

**换 Agent，不丢任务。** 本地多 Agent 路由：一个入口派活，写者交付可核对的候选，另一种 Agent 只读复核同一版本；中断后从已确认的检查点继续。

WagglePost 0.1 是单二进制的本地工具：Rust core + SQLite + 内嵌 React 工作台（仅 `127.0.0.1`）+ 同二进制 CLI。路由与交接组装是本地确定性规则，不额外调用规划模型。

## 快速开始（Windows 10/11 x64）

```powershell
git clone <this-repository> agent-wagglepost
cd agent-wagglepost\web
npm ci
npm run build
cd ..
cargo build --locked --release

.\target\release\wagglepost.exe serve --managed-root $env:TEMP\wagglepost-demo
# 控制台打印一次性启动码；浏览器打开 http://127.0.0.1:8742 输入
```

不传 `--managed-root` 时，默认使用 `%LOCALAPPDATA%\WagglePost`；也可用环境变量 `WAGGLEPOST_MANAGED_ROOT` 指定。CLI 读取同一受管根下的 `cli-session.json`。

## 它做什么

- 提交 → 计划与路线回执 → 人工批准正文/版本 → 隔离 worktree 启动写者 → 冻结候选 → 第二运行时只读复核 → 人工按版本接收或导出补丁。
- 换轨绑定目标运行时、版本、检查点、正文摘要和预算；只有真实 `agent_started` 才发启程/换轨事件。
- 同仓单写者、全局单执行槽；失败也计入启动预算；重复请求按幂等键回放。
- Agent 输出在事件、收件箱、结果和 stderr 日志落盘前脱敏；用户代码和补丁不被改写。

## CLI

```text
wagglepost serve [--port N] [--managed-root DIR] [--cli-repo DIR]...
wagglepost doctor [--json]
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

## 真实与模拟

| 运行时 | 0.1 能力 |
| --- | --- |
| Fake | 离线演示/测试；始终标注「模拟」，可用于确定性门禁 |
| Codex CLI | 从 `PATH` 或 `WAGGLEPOST_CODEX_EXE` 发现；写入能力按 P0 版本/权限门判定 |
| Claude Code | 从 `PATH` 或 `WAGGLEPOST_CLAUDE_EXE` 发现；0.1 仅只读复核 |
| DeepSeek Harness | 仅能力探测，0.1 不派发 |

CI 不安装模型、不读取 Key、不启动真实运行时。`fake` 的运行和候选永远显示为模拟，不能当作真实模型或跨工具续写证据。

## 构建与门禁

```powershell
cd web; npm ci; npm run build; cd ..
cargo test --locked --release -- --test-threads=2
cargo build --locked --release

python -m pip install playwright==1.64.1
python -m playwright install chromium
python scripts/run_ui_gates.py --exe target\release\wagglepost.exe --out ci-artifacts
```

GitHub Actions 只有一个 Windows job：`windows-build-test-ui`。它使用 `npm ci`、`cargo --locked`，在临时受管根和随机回环端口启动生产 exe，运行真实 Chromium，并核对服务端实际资产与 `web/dist/ASSETS.sha`。工作流仅 `contents: read`，触发为 `push` main 与 `pull_request`，不使用 `pull_request_target`，不接入秘密或模型。

## 安全边界

- 仅绑定 `127.0.0.1`；Host/Origin 校验；一次性启动码 → HttpOnly cookie；浏览器写操作需 CSRF。
- CLI 使用独立 bearer token，可用 `--cli-repo`/`WAGGLEPOST_CLI_ALLOWED_REPOS` 限制仓库范围。
- 用户仓库 dirty 时拒绝；worktree 从显式提交创建，不复制未提交内容。
- 结果来自本机 worktree 观察，Agent 自述不自动升级为独立证据。
- stderr 每次尝试最多 2 MiB，日志默认保留 7 天；`doctor` / 工具页显示占用；无 telemetry。

## 已知限制

- **平台**：Windows 10/11 x64 已验证；Linux 源码可编译但未做等价验收，不宣称支持。
- **真实双写者/跨工具接力未验收**：当前只有 fake 换轨的确定性证据；真实第二写者需要单独通过写入能力门并实跑。Claude 在 0.1 仅只读。
- **原生同会话恢复未证明**：没有确认同一原生会话时，UI 明确走“受控检查点回退”，预览完整精简交接包后重新批准。
- **G7b 未测量**：没有真实 token/缓存/整单对照，不宣称节省比例。
- **DSH 仅探测**，未开放派发；分析并行、自动返工、批量评审、外部触发/模型代理/自动发布不在 0.1。
- 本地单用户工具，不适合作为多租户或公网服务部署。

## 开发

```text
src/        Rust core / HTTP / runner / adapters / store
web/        React + TypeScript；npm run build → web/dist（构建时校验并嵌入）
tests/      G1–G8 与 WP 回归；默认 fake、离线、确定性
scripts/    ui_gates.py 与 run_ui_gates.py（真实生产 exe + Chromium）
```

贡献与安全说明见 [CONTRIBUTING.md](CONTRIBUTING.md) 和 [SECURITY.md](SECURITY.md)。许可证：[MIT](LICENSE)。
