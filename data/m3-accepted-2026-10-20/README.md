# M3 端到端冒烟 · 验收快照（2026-10-20）

本目录是本次 M3 端到端冒烟验收的只读快照，防止后续并行验证覆盖滚动归档。

## 结果

- 脚本：`tests/smoke_m3_e2e.sh`（当前 HEAD 语义；真实 opencode `opencode-go/glm-5.3-flash`）
- 团队池：8 实例（planner×2 / executor×2 / acceptor×2 / decision_maker×2）
- 检查结果：phase1 **97/97**、phase2 **42/42**，合计 **139/139 PASS，0 FAIL**，退出码 0/0
  （run1 `00:34:03`；run2 复跑 `01:14:36` 同样 139/139；run3 `01:23:15` 用
  `opencode-go/glm-5.3` 全量模型在并发/上游抖动下再次 139/139，三次逐条检查集一致）
- 重启恢复：phase1 结束后 `kill -9` 服务端并重启，A/B/D 终态与 C 的失败现场完整保留，续跑至 C `completed`
- 服务端日志：run1 70 次 `opencode run spawn` / 69 次 `finished`（末次 spawn 在收尾 kill 前未落）、run2 74/74、
  run3 65/65；三次均 0 条 ERROR、0 次 `panicked at`（run3 另有 4 条已知 WARN：自定义 session 回退 2 条、
  一次上游流中断触发协商失败并由决策通道恢复 2 条，见非阻塞问题记录）
- 单元测试：`cargo test` 71 passed / 0 failed（2026-10-20 复跑）
- 运行目录：`/tmp/opencode/domo_m3_e2e.nW7qNY`

## 文件

| 文件 | 内容 |
|---|---|
| `run1-summary.md` | run1 头信息 + 逐条 PASS/FAIL |
| `run1-results.md` | run1 phase1 / phase2 逐条检查结果与备注 |
| `run1-domo.log` | run1 服务端运行日志（同份写入 `data/domo.log`） |
| `run2-summary.md` | run2（复跑）头信息 + 逐条 PASS/FAIL |
| `run2-results.md` | run2 phase1 / phase2 逐条检查结果与备注 |
| `run2-domo.log` | run2 服务端运行日志 |
| `run3-summary.md` | run3（`opencode-go/glm-5.3` 全量模型）头信息 + 逐条 PASS/FAIL |
| `run3-results.md` | run3 phase1 / phase2 逐条检查结果与备注 |
| `run3-domo.log` | run3 服务端运行日志 |
| `STABILITY.md` | 可重复性对比表（run1 / run2 / run3 检查集一致） |

滚动归档位于 `data/smoke_m3_e2e/`；用例清单见 `docs/SMOKE_M3_E2E.md`。
历史 M3 版（134 项、run1~run7）的明细在环境重置后未保留，相关历史小节仅作回放参考。
