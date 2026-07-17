# Credit 预警功能设计

日期：2026-07-17
状态：已确认，待实现

## 背景

kiro.rs 是一个 Rust (axum) 代理，通过多个 Kiro/AWS 凭据对外提供 Anthropic 兼容 API。
后端已能查询单个凭据的余额（`getUsageLimits` API），并在 `AdminService::fetch_balance`
中计算 `remaining = usage_limit - current_usage`，缓存 5 分钟于 `kiro_balance_cache.json`。

本功能新增一个**总余额预警**子系统：后台定时轮询所有凭据、汇总剩余额度、
在总额低于阈值时通过 Telegram / Email 一次性告警。

## 需求（已确认）

1. 支持多个通知渠道（如 2 个 Telegram bot + 2 个 email）。渠道在 admin UI 配置，
   持久化到 JSON 文件。SMTP 连接参数通过环境变量设置。
2. 后台自动轮询汇总**所有**可上报凭据的**总**剩余额度。
3. 用户在 admin UI 设置预警阈值（如总剩余低于 1000 credit）。
4. 告警只提醒一次（单次触发），除非重新 arm。

## 关键决策

- **数据来源**：复用现有 `token_manager.get_usage_limits_for(id)`，无需新的上游 API
  或 token 刷新技巧。总额 = 对「启用 + 可上报（social/IdC）」凭据的 `remaining` 求和。
- **架构**：新增自包含 `src/alert/` 模块 + 后台轮询任务（Option A），与 `admin` 模块解耦。
- **轮询间隔**：admin UI 可配置，默认 base 间隔 + 每轮 5–10 分钟抖动，避免固定节拍冲击上游。
- **Re-arm 触发**：新增凭据 / 恢复到阈值以上（含迟滞）/ 用户修改阈值，三者任一都会重新 arm。
- **SMTP**：单一 SMTP 中继（环境变量），多个收件人。
- **Telegram 密钥**：bot token + chat ID 存 JSON，但 GET 时脱敏返回，不明文回传前端。
- **出站代理**：Telegram 复用现有 `proxy_url`；SMTP（lettre）直连，不走代理（已接受）。
- **部分失败**：查询失败的凭据从总额中排除（不计为 0）；若本轮所有可上报凭据都失败，
  跳过本轮评估，下轮重试。
- **Subject 前缀**：可配置 `subjectPrefix`，用于区分多实例。
- **测试端点**：保留 `POST /alerts/test` API，但**不在前端暴露**。

## 模块布局

新增 `src/alert/`：

- `mod.rs` — 公开接口 + `AlertService`（协调器）。
- `config.rs` — 持久化配置类型 + JSON 加载/保存。
- `state.rs` — arm/fire 状态机。
- `poller.rs` — 后台轮询循环（由 `main.rs` spawn）。
- `notify/mod.rs`、`notify/telegram.rs`、`notify/smtp.rs` — `Notifier` trait + 两个实现。
- `types.rs` — admin API 请求/响应 DTO（脱敏）。

## 数据模型

### 持久化配置 `alert_config.json`（与 `kiro_balance_cache.json` 同目录）

```jsonc
{
  "enabled": true,
  "thresholdRemaining": 1000.0,
  "pollIntervalSecs": 1800,          // base 间隔；poller 额外加 5–10 分钟抖动
  "subjectPrefix": "PROD-东京",       // 可空；用于区分多实例
  "channels": [
    { "id": "uuid-1", "kind": "telegram", "enabled": true,
      "botToken": "123:ABC...", "chatId": "-1001234567890", "name": "ops bot" },
    { "id": "uuid-2", "kind": "email", "enabled": true,
      "to": "alerts@example.com", "name": "on-call" }
  ]
}
```

### 运行时状态 `alert_state.json`（持久化，重启不重复告警）

```jsonc
{
  "fired": true,                     // 单次告警是否已触发
  "lastTotalRemaining": 842.5,
  "lastEvaluatedAt": 1752710400,
  "lastThreshold": 1000.0,
  "credentialFingerprint": "sha256(sorted credential ids)"  // 检测「新增凭据」
}
```

### SMTP 环境变量（启动时读取一次）

- `ALERT_SMTP_HOST`
- `ALERT_SMTP_PORT`
- `ALERT_SMTP_USERNAME`
- `ALERT_SMTP_PASSWORD`
- `ALERT_SMTP_FROM`
- `ALERT_SMTP_TLS`（`starttls`（默认）/ `implicit` / `none`）

若 host/from 未设置，email 渠道视为未配置并跳过（记录 warning）。

## 状态机（core，需求 #4）

每轮轮询产生一次决策。

**输入：**
- `total` = 对「启用 + 可上报（social/IdC）」凭据的 `remaining` 求和。
- `any_success` = 本轮是否至少一个可上报凭据查询成功（全失败则跳过本轮，不改状态）。
- `fingerprint_now` = 当前凭据 id 集合排序后的 sha256。
- 持久化状态：`fired`、`lastFingerprint`、`lastThreshold`。

**Re-arm 触发（任一将 `fired → false`）：**
1. 新增凭据：`fingerprint_now != lastFingerprint`。
2. 恢复：`total >= threshold + hysteresis`，`hysteresis = max(threshold * 0.05, 50)`。
3. 阈值变更：`threshold != lastThreshold`（PUT 阈值时也立即 re-arm，无需等一轮）。

**触发条件：** `!fired && total < threshold` → 发送所有启用渠道，成功后 `fired = true`。

**决策表：**

| `fired` | `total < threshold`? | 动作 |
|---|---|---|
| false | 是 | **触发告警**，置 `fired = true` |
| false | 否 | 无（保持 armed） |
| true | 是 | 无（已告警——单次） |
| true | 否（≥ threshold + hysteresis） | **Re-arm**（`fired = false`） |
| true | 否（在迟滞带内） | 无（避免抖动） |

Re-arm 触发 #1/#3 在触发检查之前执行，因此在仍低于阈值时新增凭据（或修改阈值）
会在同一轮重新告警。

每轮持久化 `fired`、`lastFingerprint`、`lastThreshold`、`lastTotalRemaining`、`lastEvaluatedAt`。

**部分失败：** 查询失败的凭据从求和中排除（不计为 0）。若本轮所有可上报凭据都失败，
跳过整轮评估，下轮重试。当求和为部分结果时，告警正文注明
`"基于 N/M 个凭据（跳过 K 个查询失败）"`。

## 后台轮询

由 `main.rs` 在构建 token manager 后 spawn，以 admin API 启用为前提
（无 UI 无法配置）。循环始终运行，`config.enabled` 控制是否评估，
使 UI 开关无需重启即可生效。

```
loop {
    let cfg = alert_service.config_snapshot();
    if cfg.enabled {
        alert_service.evaluate_once().await;   // 查询 + 决策 + 通知 + 持久化状态
    }
    let base = cfg.poll_interval_secs;         // 默认 1800
    let jitter = fastrand 300..=600 secs;      // 5–10 分钟
    tokio::time::sleep(base + jitter).await;
}
```

- **抖动**：`fastrand`（已有依赖），每轮应用。
- **余额查询**：复用 `token_manager.get_usage_limits_for(id)`，并发上限 `buffer_unordered(4)`。
  刻意绕过 5 分钟 admin 缓存——poller 按自身节奏取新鲜值，其间隔（≥30 分钟）已比缓存 TTL 温和。
- **可上报过滤**：`snapshot().entries` 中 `!disabled && auth_method != "api_key"`。
- **首轮**：启动后**等待一个间隔**再首评估，不在启动瞬间告警；`fired` 状态跨重启保留单次记忆。
- **关闭**：任务 detached，进程退出即结束（`main.rs` 当前无优雅关闭，故不新增）。
- **即时反馈**：保存配置时立即重新评估；另有 `POST /alerts/test` 手动测试。

## 通知器

```rust
#[async_trait]
trait Notifier {
    async fn send(&self, subject: &str, body: &str) -> anyhow::Result<()>;
}
```

**Telegram（`notify/telegram.rs`）：**
- `POST https://api.telegram.org/bot<token>/sendMessage`，body `{ chat_id, text, parse_mode: "HTML" }`。
- 用共享 `http_client` builder，继承 `proxy_url` + `tls_backend`。
- 每次发送 ~15s 超时。失败记录 warning 并继续下一渠道——单个坏渠道不阻塞其他。

**SMTP（`notify/smtp.rs`）：**
- 新增 `lettre` crate（Rust 标准 SMTP 客户端），tokio + rustls，**固定精确版本**。
- 启动时读取 `ALERT_SMTP_*` 环境变量到 `SmtpSettings`。TLS 模式由 `ALERT_SMTP_TLS` 决定。
- 每次发送建立一个连接，发往各 email 渠道的 `to`。按收件人记录失败，非致命。
- **代理限制**：`lettre` 不支持通过 HTTP/SOCKS 代理路由 SMTP，故 SMTP 直连，
  无视 `proxy_url`（已接受）。

**消息内容（中文，两渠道通用）：**
- Subject：`⚠️ {prefix} Kiro Credit 预警：剩余 {total} 低于阈值 {threshold}`
  （`{prefix}` 为空时干净省略，无多余空格）。
- Body：总剩余、阈值、纳入/跳过凭据计数、时间戳；正文头部含实例前缀。

**Fan-out：** `evaluate_once` 从启用渠道构建通知器列表，全部发送，
**仅当至少一个渠道成功**才置 `fired = true`。若全部失败，保持 armed，下轮重试
（瞬时 Telegram 故障不会静默消耗单次机会）。

## Admin API 与前端

### Admin API（nest 于 `/api/admin`，复用现有 auth 中间件）

- `GET /alerts/config` — 返回配置，密钥**脱敏**：`botToken` 显示为 `123456:AB••••••XY`
  （复用 `mask_api_key` 风格），永不返回完整 token。返回 SMTP *状态*
  （已配置/未配置，由 env 推导），永不返回 SMTP 密码。
- `PUT /alerts/config` — 更新 `enabled`、`thresholdRemaining`、`pollIntervalSecs`、
  `subjectPrefix`。修改阈值立即 re-arm（触发 #3）。持久化并触发即时重评估。
- `GET /alerts/channels` / `POST /alerts/channels` / `PUT /alerts/channels/{id}` /
  `DELETE /alerts/channels/{id}` — 渠道 CRUD。`PUT` 时若 `botToken` 回传的是脱敏占位符
  （未改动），保留已存 token——避免编辑时覆盖密钥。
- `POST /alerts/test` — 向所有启用渠道发测试消息，返回逐渠道成功/失败。忽略状态机。
  **仅 API，不在前端暴露。**
- `GET /alerts/status` — 运行时状态展示：`lastTotalRemaining`、`lastEvaluatedAt`、
  `fired`、上轮纳入/跳过凭据计数。

DTO 位于 `src/alert/types.rs`；admin router 仅调用 `AlertService`。
错误复用 `AdminServiceError` → HTTP 映射模式。

### 前端 — dashboard 新增「预警设置」区

- 遵循现有 shadcn/dialog + tanstack-query 约定（`api/alerts.ts`、`hooks/use-alerts.ts`、
  `types/api.ts` 补充）。
- 设置卡片：启用开关、阈值输入、轮询间隔、subject 前缀，
  加实时状态行（上次总额 / 上次检查时间 / fired-或-armed 徽章）。
- 渠道列表 + 增/改/删对话框（kind = telegram/email；脱敏 token 显示）。
  **不含「发送测试」按钮。**
- SMTP 状态只读展示（"SMTP 已配置 / 未配置——通过环境变量设置"）。

## 验证（Docker 环境）

- 后端：容器内 `cargo check --offline`（见 kiro-rs-docker-build 记忆）。
- 前端：`pnpm build`（重新生成 `admin-ui/dist` 供 rust-embed 编译期读取）。
- 单元测试：状态机（arm/fire/re-arm/迟滞/部分失败）+ fake `Notifier`。
- 清理临时文件。

## 非目标（YAGNI）

- 不做优雅关闭 / 信号处理（现有 `main.rs` 无此机制）。
- 不做每渠道独立代理覆盖（复用全局 `proxy_url`）。
- 不做 SMTP-over-proxy。
- 不做多 SMTP 服务器（单中继，多收件人）。
- 不做告警历史 / 审计日志。
