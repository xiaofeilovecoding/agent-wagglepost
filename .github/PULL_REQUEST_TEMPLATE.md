## 变更说明 / What changed

## 动机 / Why

## 验证 / Verification
- [ ] `cargo test -- --test-threads=2` 全绿 / all green
- [ ] `cargo test --no-run` 零警告 / zero warnings across all test targets
- [ ] 浏览器门禁 / browser gates: `python scripts/run_ui_gates.py --exe target/release/wagglepost.exe --out ci-artifacts`
- [ ] 未提交真实凭据、私有日志或生成证据 / no credentials, private logs, or generated evidence committed

## 能力边界 / Capability claims
（若涉及运行时能力或权限边界，说明证据类型：fixture 契约 / offline contract / real loop /
if this touches runtime capabilities, state the evidence tier: fixture contract / offline contract / real loop）
