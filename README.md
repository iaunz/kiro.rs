# kiro-rs

一个用 Rust 编写的 Kiro API 代理，提供 Anthropic Messages 和 OpenAI Responses 兼容接口，支持 Claude 与 GPT-5.6 系列模型。

---

<table>
<tr>
<td>
<b>特别感谢</b>：<a href="https://co.yes.vg/register?ref=hank9999">YesCode</a> 为本项目提供了 AI API 额度赞助, YesCode 作为一家低调务实的 AI API 中转服务商 <br>
长期以来提供稳定高可用的服务, 如您有意体验, 请点击链接注册体验 → <a href="https://co.yes.vg/register?ref=hank9999">立即访问</a>
</td>
</tr>
</table>

---

#### [LINUX DO 讨论帖](https://linux.do/t/topic/1571986)

## 免责声明

本项目仅供研究使用, Use at your own risk, 使用本项目所导致的任何后果由使用人承担, 与本项目无关。
本项目与 AWS/KIRO/Anthropic/Claude 等官方无关, 本项目不代表官方立场。

## 注意！

因 TLS 默认从 native-tls 切换至 rustls，你可能需要专门安装证书后才能配置 HTTP 代理。可通过 `config.json` 的 `tlsBackend` 切回 `native-tls`。
如果遇到请求报错, 尤其是无法刷新 token, 或者是直接返回 error request, 请尝试切换 tls 后端为 `native-tls`, 一般即可解决。

**Write Failed/会话卡死**: 如果遇到持续的 Write File / Write Failed 并导致会话不可用，参考 Issue [#22](https://github.com/hank9999/kiro.rs/issues/22) 和 [#49](https://github.com/hank9999/kiro.rs/issues/49) 的说明与临时解决方案（通常与输出过长被截断有关，可尝试调低输出相关 token 上限）

## 功能特性

- **兼容接口**: 支持 Anthropic Messages 和 OpenAI Responses，便于接入 Claude Code、Codex 等客户端
- **流式响应**: 支持 SSE (Server-Sent Events) 流式输出
- **Token 自动刷新**: 自动管理和刷新 OAuth Token
- **多凭据支持**: 支持配置多个凭据，按优先级自动故障转移
- **负载均衡**: 支持 `priority`（按优先级）和 `balanced`（均衡分配）两种模式
- **智能重试**: 单凭据最多重试 3 次，单请求最多重试 9 次
- **凭据回写**: 多凭据格式下自动回写刷新后的 Token
- **Thinking 模式**: 支持 Claude 的 extended thinking 功能
- **工具调用**: 支持 Anthropic tool use、Responses function/custom 工具，保留工具结果中的 Base64 图片
- **WebSearch**: 内置 WebSearch 工具转换逻辑
- **多模型支持**: 支持 Sonnet、Opus、Haiku、Fable 与 GPT-5.6 Sol/Terra/Luna，以及 `sonnet`、`opus`、`haiku` 简写
- **自动模型发现**: 从 Kiro 获取模型目录并缓存，查询失败时继续使用已有目录或静态列表
- **Admin 管理**: 可选的 Web 管理界面和 API，支持凭据管理、余额查询等
- **Credit 预警**: 后台定时汇总所有凭据的剩余额度，低于阈值时通过 Telegram / 邮件一次性告警
- **多级 Region 配置**: 支持全局和凭据级别的 Auth Region / API Region 配置
- **凭据级代理**: 支持为每个凭据单独配置 HTTP/SOCKS5 代理，优先级：凭据代理 > 全局代理 > 无代理

---

- [开始](#开始)
  - [1. 编译](#1-编译)
  - [2. 最小配置](#2-最小配置)
  - [3. 启动](#3-启动)
  - [4. 验证](#4-验证)
  - [Docker](#docker)
- [配置详解](#配置详解)
  - [config.json](#configjson)
  - [credentials.json](#credentialsjson)
  - [Region 配置](#region-配置)
  - [代理配置](#代理配置)
  - [认证方式](#认证方式)
  - [环境变量](#环境变量)
- [Credit 预警](#credit-预警)
- [API 端点](#api-端点)
  - [标准端点 (/v1)](#标准端点-v1)
  - [Claude Code 兼容端点 (/cc/v1)](#claude-code-兼容端点-ccv1)
  - [OpenAI Responses](#openai-responses)
  - [Thinking 模式](#thinking-模式)
  - [工具调用](#工具调用)
- [模型映射](#模型映射)
  - [自动获取 Kiro 模型](#自动获取-kiro-模型)
  - [功能来源与范围](#功能来源与范围)
- [Admin（可选）](#admin可选)
- [注意事项](#注意事项)
- [项目结构](#项目结构)
- [技术栈](#技术栈)
- [License](#license)
- [致谢](#致谢)

## 开始

### 1. 编译

> PS: 如果不想编辑可以直接前往 Release 下载二进制文件

> **前置步骤**：编译前需要先构建前端 Admin UI（用于嵌入到二进制中）：
> ```bash
> cd admin-ui && pnpm install && pnpm build
> ```

```bash
cargo build --release
```

### 2. 最小配置

创建 `config.json`：

```json
{
   "host": "127.0.0.1",
   "port": 8990,
   "apiKey": "sk-kiro-rs-qazWSXedcRFV123456",
   "region": "us-east-1"
}
```
> PS: 如果你需要 Web 管理面板, 请注意配置 `adminApiKey`

创建 `credentials.json`（从 Kiro IDE 等中获取凭证信息）：
> PS: 可以前往 Web 管理面板配置跳过本步骤
> 如果你对凭据地域有疑惑, 请查看 [Region 配置](#region-配置)

Social 认证：
```json
{
   "refreshToken": "你的刷新token",
   "expiresAt": "2025-12-31T02:32:45.144Z",
   "authMethod": "social"
}
```

IdC 认证：
```json
{
   "refreshToken": "你的刷新token",
   "expiresAt": "2025-12-31T02:32:45.144Z",
   "authMethod": "idc",
   "clientId": "你的clientId",
   "clientSecret": "你的clientSecret"
}
```

### 3. 启动

```bash
./target/release/kiro-rs
```

或指定配置文件路径：

```bash
./target/release/kiro-rs -c /path/to/config.json --credentials /path/to/credentials.json
```

### 4. 验证

```bash
curl http://127.0.0.1:8990/v1/messages \
  -H "Content-Type: application/json" \
  -H "x-api-key: sk-kiro-rs-qazWSXedcRFV123456" \
  -d '{
    "model": "sonnet",
    "max_tokens": 1024,
    "stream": true,
    "messages": [
      {"role": "user", "content": "Hello, Claude!"}
    ]
  }'
```

### Docker

也可以通过 Docker 启动：

```bash
docker-compose up
```

需要将 `config.json` 和 `credentials.json` 挂载到容器中，具体参见 `docker-compose.yml`。

## 配置详解

### config.json

| 字段 | 类型 | 默认值 | 描述 |
|------|------|--------|------|
| `host` | string | `127.0.0.1` | 服务监听地址 |
| `port` | number | `8080` | 服务监听端口 |
| `apiKey` | string | - | 自定义 API Key（用于客户端认证，必配） |
| `region` | string | `us-east-1` | AWS 区域 |
| `authRegion` | string | - | Auth Region（用于 Token 刷新），未配置时回退到 region |
| `apiRegion` | string | - | API Region（用于 API 请求），未配置时回退到 region |
| `kiroVersion` | string | `0.9.2` | Kiro 版本号 |
| `machineId` | string | - | 自定义机器码（64位十六进制），不定义则自动生成 |
| `systemVersion` | string | 随机 | 系统版本标识 |
| `nodeVersion` | string | `22.21.1` | Node.js 版本标识 |
| `tlsBackend` | string | `rustls` | TLS 后端：`rustls` 或 `native-tls` |
| `countTokensApiUrl` | string | - | 外部 count_tokens API 地址 |
| `countTokensApiKey` | string | - | 外部 count_tokens API 密钥 |
| `countTokensAuthType` | string | `x-api-key` | 外部 API 认证类型：`x-api-key` 或 `bearer` |
| `proxyUrl` | string | - | HTTP/SOCKS5 代理地址 |
| `proxyUsername` | string | - | 代理用户名 |
| `proxyPassword` | string | - | 代理密码 |
| `adminApiKey` | string | - | Admin API 密钥，配置后启用凭据管理 API 和 Web 管理界面 |
| `loadBalancingMode` | string | `priority` | 负载均衡模式：`priority`（按优先级）或 `balanced`（均衡分配） |
| `extractThinking` | boolean | `true` | 非流式响应的 thinking 块提取。启用后 `<thinking>` 标签会被解析为独立的 `thinking` 内容块 |
| `defaultEndpoint` | string | `ide` | 默认 Kiro 端点。凭据未显式指定 `endpoint` 时使用。当前支持：`ide` |

完整配置示例：

```json
{
   "host": "127.0.0.1",
   "port": 8990,
   "apiKey": "sk-kiro-rs-qazWSXedcRFV123456",
   "region": "us-east-1",
   "tlsBackend": "rustls",
   "kiroVersion": "0.9.2",
   "machineId": "64位十六进制机器码",
   "systemVersion": "darwin#24.6.0",
   "nodeVersion": "22.21.1",
   "authRegion": "us-east-1",
   "apiRegion": "us-east-1",
   "countTokensApiUrl": "https://api.example.com/v1/messages/count_tokens",
   "countTokensApiKey": "sk-your-count-tokens-api-key",
   "countTokensAuthType": "x-api-key",
   "proxyUrl": "http://127.0.0.1:7890",
   "proxyUsername": "user",
   "proxyPassword": "pass",
   "adminApiKey": "sk-admin-your-secret-key",
   "loadBalancingMode": "priority",
   "extractThinking": true
}
```

### credentials.json

支持单对象格式（向后兼容）或数组格式（多凭据）。

#### 字段说明

| 字段             | 类型     | 描述                                          |
|----------------|--------|---------------------------------------------|
| `id`           | number | 凭据唯一 ID（可选，仅用于 Admin API 管理；手写文件可不填）        |
| `accessToken`  | string | OAuth 访问令牌（可选，可自动刷新）                        |
| `refreshToken` | string | OAuth 刷新令牌                                  |
| `profileArn`   | string | AWS Profile ARN（可选，登录时返回）                   |
| `expiresAt`    | string | Token 过期时间 (RFC3339)                        |
| `authMethod`   | string | 认证方式：`social` 或 `idc`                       |
| `clientId`     | string | IdC 登录的客户端 ID（IdC 认证必填）                     |
| `clientSecret` | string | IdC 登录的客户端密钥（IdC 认证必填）                      |
| `priority`     | number | 凭据优先级，数字越小越优先，默认为 0                         |
| `region`       | string | 凭据级 Auth Region, 兼容字段                       |
| `authRegion`   | string | 凭据级 Auth Region，用于 Token 刷新, 未配置时回退到 region |
| `apiRegion`    | string | 凭据级 API Region，用于 API 请求                    |
| `machineId`    | string | 凭据级机器码（64位十六进制）                             |
| `email`        | string | 用户邮箱（可选，从 API 获取）                           |
| `proxyUrl`     | string | 凭据级代理 URL（可选，特殊值 `direct` 表示不使用代理）       |
| `proxyUsername`| string | 凭据级代理用户名（可选）                                |
| `proxyPassword`| string | 凭据级代理密码（可选）                                 |
| `endpoint`     | string | 凭据级端点名称（可选，未配置时使用 `config.defaultEndpoint`）|

说明：
- IdC / Builder-ID / IAM 在本项目里属于同一种登录方式，配置时统一使用 `authMethod: "idc"`
- 为兼容旧配置，`builder-id` / `iam` 仍可被识别，但会按 `idc` 处理

#### 单凭据格式（旧格式，向后兼容）

```json
{
   "accessToken": "请求token，一般有效期一小时，可选",
   "refreshToken": "刷新token，一般有效期7-30天不等",
   "profileArn": "arn:aws:codewhisperer:us-east-1:111112222233:profile/QWER1QAZSDFGH",
   "expiresAt": "2025-12-31T02:32:45.144Z",
   "authMethod": "social",
   "clientId": "IdC 登录需要",
   "clientSecret": "IdC 登录需要"
}
```

#### 多凭据格式（支持故障转移和自动回写）

```json
[
   {
      "refreshToken": "第一个凭据的刷新token",
      "expiresAt": "2025-12-31T02:32:45.144Z",
      "authMethod": "social",
      "priority": 0
   },
   {
      "refreshToken": "第二个凭据的刷新token",
      "expiresAt": "2025-12-31T02:32:45.144Z",
      "authMethod": "idc",
      "clientId": "xxxxxxxxx",
      "clientSecret": "xxxxxxxxx",
      "region": "us-east-2",
      "priority": 1,
      "proxyUrl": "socks5://proxy.example.com:1080",
      "proxyUsername": "user",
      "proxyPassword": "pass"
   },
   {
      "refreshToken": "第三个凭据（显式不走代理）",
      "expiresAt": "2025-12-31T02:32:45.144Z",
      "authMethod": "social",
      "priority": 2,
      "proxyUrl": "direct"
   }
]
```

多凭据特性：
- 按 `priority` 字段排序，数字越小优先级越高（默认为 0）
- 单凭据最多重试 3 次，单请求最多重试 9 次
- 自动故障转移到下一个可用凭据
- 多凭据格式下 Token 刷新后自动回写到源文件

### Region 配置

支持多级 Region 配置，分别控制 Token 刷新和 API 请求使用的区域。

**Auth Region**（Token 刷新）优先级：
`凭据.authRegion` > `凭据.region` > `config.authRegion` > `config.region`

**API Region**（API 请求）优先级：
`凭据.apiRegion` > `config.apiRegion` > `config.region`

### 代理配置

支持全局代理和凭据级代理，凭据级代理用于该凭据的 API 请求、Token 刷新、额度查询和模型目录查询。

**代理优先级**：`凭据.proxyUrl` > `config.proxyUrl` > 无代理

| 凭据 `proxyUrl` 值 | 行为 |
|---|---|
| 具体 URL（如 `http://proxy:8080`、`socks5://proxy:1080`） | 使用凭据指定的代理 |
| `direct` | 显式不使用代理（即使全局配置了代理） |
| 未配置（留空） | 回退到全局代理配置 |

凭据级代理示例：

```json
[
   {
      "refreshToken": "凭据A：使用自己的代理",
      "authMethod": "social",
      "proxyUrl": "socks5://proxy-a.example.com:1080",
      "proxyUsername": "user_a",
      "proxyPassword": "pass_a"
   },
   {
      "refreshToken": "凭据B：显式不走代理（直连）",
      "authMethod": "social",
      "proxyUrl": "direct"
   },
   {
      "refreshToken": "凭据C：使用全局代理（或直连，取决于 config.json）",
      "authMethod": "social"
   }
]
```

### 认证方式

客户端请求本服务时，支持两种认证方式：

1. **x-api-key Header**
   ```
   x-api-key: sk-your-api-key
   ```

2. **Authorization Bearer**
   ```
   Authorization: Bearer sk-your-api-key
   ```

### 环境变量

可通过环境变量配置日志级别：

```bash
RUST_LOG=debug ./target/release/kiro-rs
```

Credit 预警的 SMTP（邮件通知）连接参数也通过环境变量配置。仅当 `ALERT_SMTP_HOST`
与 `ALERT_SMTP_FROM` 均已设置时，邮件渠道才会启用；否则邮件渠道会被跳过（Telegram 渠道不受影响）。

| 变量 | 必填 | 说明 |
| --- | --- | --- |
| `ALERT_SMTP_HOST` | 是 | SMTP 服务器地址；缺失则邮件渠道禁用 |
| `ALERT_SMTP_FROM` | 是 | 发件人地址；缺失则邮件渠道禁用 |
| `ALERT_SMTP_PORT` | 否 | 端口；缺省按 TLS 推断（`implicit`=465，其它=587）|
| `ALERT_SMTP_USERNAME` | 否 | SMTP 认证用户名 |
| `ALERT_SMTP_PASSWORD` | 否 | SMTP 认证密码 |
| `ALERT_SMTP_TLS` | 否 | `starttls`（默认）/ `implicit` / `none` |

> 说明：Telegram 通知会复用 `config.json` 的 `proxyUrl` 出站代理；SMTP 邮件为直连，不走代理。

## Credit 预警

在启用 Admin API（配置了 `adminApiKey`）后，系统会在后台定时轮询所有凭据的剩余额度，
汇总后与用户设定的阈值比较，低于阈值时通过通知渠道告警。

- **数据来源**：复用各凭据的 `getUsageLimits` 查询，汇总「已启用且可上报（social / IdC）」
  凭据的剩余额度；API Key 凭据与已禁用凭据不计入。查询失败的凭据本轮从汇总中排除，
  若本轮全部失败则跳过评估。
- **通知渠道**：支持多个 Telegram bot 与多个邮件收件人，在 Web 管理界面配置，
  持久化到缓存目录下的 `alert_config.json`。Telegram 的 `botToken` 在读取时会脱敏返回，
  不会明文回传前端。SMTP 连接参数见上文[环境变量](#环境变量)。
- **阈值与轮询**：阈值、轮询间隔、主题前缀均在 Web 管理界面设置。轮询在基础间隔上叠加
  5–10 分钟随机抖动，避免固定节拍冲击上游。
- **单次告警语义**：低于阈值时只告警一次。以下任一情况会重新「布防」（使其可再次告警）：
  新增凭据、总剩余恢复到阈值 + 迟滞裕度以上、用户修改阈值。运行状态持久化到
  `alert_state.json`，重启不会重复告警。
- **主题前缀**：可为告警主题设置前缀（如 `PROD-东京`），便于区分多个部署实例。

> 提示：`POST /api/admin/alerts/test` 可向所有启用渠道发送测试消息（仅 API，未在界面暴露）。

## API 端点

以下端点均使用 `apiKey` 认证，支持 `x-api-key` 或 `Authorization: Bearer`。JSON 请求体上限为 **50 MiB（52,428,800 字节）**，包括 `/v1/responses`、`/v1/messages` 和 `/cc/v1/messages`；超过上限返回 HTTP 413。此限制包含 Base64 图片等整个请求体，与模型的 Token 上下文上限分别计算。

### 标准端点 (/v1)

| 端点 | 方法 | 描述 |
|------|------|------|
| `/v1/models` | GET | 返回动态模型目录与静态兼容模型列表 |
| `/v1/messages` | POST | Anthropic Messages，支持流式及非流式输出 |
| `/v1/messages/count_tokens` | POST | 估算 Token 数量 |
| `/v1/responses` | POST | OpenAI Responses，支持流式及非流式输出 |

### Claude Code 兼容端点 (/cc/v1)

| 端点 | 方法 | 描述 |
|------|------|------|
| `/cc/v1/models` | GET | 与 `/v1/models` 共用模型目录和缓存 |
| `/cc/v1/messages` | POST | 创建消息（缓冲模式，确保 `input_tokens` 准确） |
| `/cc/v1/messages/count_tokens` | POST | 估算 Token 数量（与 `/v1` 相同） |

> **`/cc/v1/messages` 与 `/v1/messages` 的区别**：
> - `/v1/messages`：实时流式返回，`message_start` 中的 `input_tokens` 是估算值
> - `/cc/v1/messages`：缓冲模式，等待上游流完成后，用从 `contextUsageEvent` 计算的准确 `input_tokens` 更正 `message_start`，然后一次性返回所有事件
> - 等待期间会每 25 秒发送 `ping` 事件保活

### OpenAI Responses

已有的 `/v1/responses` 兼容层接收字符串或消息数组形式的 `input`，支持 `instructions`、`max_output_tokens`、`stream` 和工具调用。客户端可将 API 地址设为 `http://127.0.0.1:8990/v1` 并选择 Responses 协议。本项目没有 `/v1/chat/completions` 端点。

```bash
curl http://127.0.0.1:8990/v1/responses \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-kiro-rs-qazWSXedcRFV123456" \
  -d '{
    "model": "gpt-5.6-sol",
    "input": "用一句话介绍 Rust。",
    "max_output_tokens": 1024,
    "stream": true,
    "store": false
  }'
```

- `type: "function"` 工具使用 JSON 参数；`type: "custom"` 工具保留自由文本输入，回传 `custom_tool_call` 和对应输入增量事件，历史中的 `custom_tool_call_output` 可继续用于后续请求。
- 支持展开 `namespace` 中的工具定义；自由文本工具的 `format` 会作为输入格式提示传给模型，不在本地执行语法校验。
- Responses 按无状态方式工作，每次请求需携带必要的对话和工具历史。`previous_response_id`、`conversation`、`background: true` 和 `store: true` 会返回参数错误；`include` 提示可接收但不会生成额外的加密推理内容。
- GPT-5.6 的推理由上游隐藏处理：`reasoning.effort` 不转换为 Claude thinking 标签，也不输出显式推理块。

### Thinking 模式

Anthropic Messages 支持显式 `thinking.type: "enabled"` 或 `"adaptive"`。例如：

```json
{
  "model": "claude-sonnet-4-5",
  "max_tokens": 16000,
  "thinking": {
    "type": "enabled",
    "budget_tokens": 10000
  },
  "messages": [{"role": "user", "content": "解释这个问题的解法。"}]
}
```

模型名附加 `-thinking` 会覆写 thinking 配置：Opus 4.6、Opus 5、Sonnet 5、Fable 5.1 使用 `adaptive` 和 `high` effort，其余 Claude 模型使用 `enabled`，预算为 20,000 tokens。

Sonnet 5、Opus 5（包括裸别名 `sonnet`、`opus`）默认拆分上游返回的 thinking 标签；无需为裸别名主动注入 thinking 参数。显式 `thinking.type: "disabled"` 可关闭默认拆分。非流式 Messages 的提取还受 `extractThinking` 配置控制。Fable 的 `-thinking` 版本使用 adaptive 模式。

GPT-5.6 使用 hidden chain-of-thought：即使提供 `thinking` 或 `-thinking` 后缀，也不会注入 Claude thinking 标签或将响应拆分成 thinking 内容块。

### 工具调用

Anthropic Messages 接收工具定义与 `tool_use` / `tool_result` 历史：

```json
{
  "model": "sonnet",
  "max_tokens": 1024,
  "tools": [
    {
      "name": "get_weather",
      "description": "获取指定城市的天气",
      "input_schema": {
        "type": "object",
        "properties": {
          "city": {"type": "string"}
        },
        "required": ["city"]
      }
    }
  ],
  "messages": [{"role": "user", "content": "查询新加坡天气。"}]
}
```

`tool_result.content` 可同时包含 `text` 和 `image` 块。PNG、JPEG、GIF、WebP 的 Base64 图片会加入对应的 Kiro 用户消息，文本与工具执行状态继续保留；当前轮与历史轮的工具结果均适用。图片来源需使用 `source.type: "base64"`，不自动下载 URL 图片。

图片限制以 Kiro 实际响应为准，不套用 Claude 的图片数量门槛。首次请求保留原图；仅在 Kiro 返回 HTTP 400、`IMAGE_DIMENSION_EXCEEDED` 且错误中明确给出像素上限时（例如 `max allowed size for many-image requests: 2000 pixels`），才检查当前及历史消息中的全部图片，将超限图片等比例缩小至该上限，再使用同一凭据和端点重试一次。合规图片保留原始编码，历史图片不会丢弃；未知错误或未给出明确尺寸时不猜测限制。PNG、JPEG、WebP 缩放后保留格式；超限 GIF 使用首帧并转为 PNG。图片处理在后台工作线程执行，无法处理或重试仍失败会明确返回错误，不无限重试。文件字节大小限制与像素尺寸限制分别处理，本功能不自动压缩文件大小。

## 模型映射

Messages 与 Responses 共用模型映射。名称忽略大小写和两端空白；Claude 的显式版本可使用点号或连字符形式。

| 客户端模型名 | Kiro 模型 ID | 静态上下文窗口 |
|---|---|---|
| `sonnet`、`claude-sonnet-5` | `claude-sonnet-5` | 1,000,000 |
| `opus`、`claude-opus-5` | `claude-opus-5` | 1,000,000 |
| `haiku`、`claude-haiku-4-5` | `claude-haiku-4.5` | 200,000 |
| `claude-fable-5-1`、`claude-fable-5.1` | `claude-fable-5.1` | 1,000,000 |
| `claude-sonnet-4-6` | `claude-sonnet-4.6` | 1,000,000 |
| `claude-sonnet-4-5` | `claude-sonnet-4.5` | 200,000 |
| `claude-opus-4-6` / `4-7` / `4-8` | 对应的 `claude-opus-4.6` / `4.7` / `4.8` | 1,000,000 |
| `claude-opus-4-5` | `claude-opus-4.5` | 200,000 |
| `gpt-5.6-sol` / `gpt-5.6-terra` / `gpt-5.6-luna` | 同名模型 | 272,000 |

GPT-5.6 也接受 `gpt-5-6-*` 和 `openai.gpt-5.6-*` 写法。Claude 历史模糊别名仍保留兼容逻辑：未指定可识别版本的 Sonnet、Opus 分别回退到 4.5、4.6；仅精确裸别名 `sonnet`、`opus` 指向 5。因此 `claude-opus-4-20250514` 不会因为日期包含 `5` 而映射到 Opus 5。

表中的上下文窗口是本地兜底值；若动态目录提供有效的 `maxInputTokens`，优先使用上游值。`GET /v1/models` 的 `max_tokens` 是输出上限，与此表的上下文窗口不同；新模型未公布输出上限且没有静态兼容值时，省略该字段。

### 自动获取 Kiro 模型

服务启动后在后台获取 Kiro 的 `ListAvailableModels` 目录，无需新增配置。`GET /v1/models` 与 `GET /cc/v1/models` 按需刷新并共用缓存：成功结果缓存 5 分钟，同一时间的查询合并为一次刷新；刷新失败保留最后一次成功结果，30 秒后允许重试。首次查询失败或没有可用 provider 时，接口仍返回静态兼容列表。这里的 5 分钟是缓存有效期，不是后台定时轮询间隔。

目录查询会合并上游分页，复用当前凭据的 Token、API Region、代理和 profile ARN 处理；HTTP 查询使用 `profileArn` 参数，推理请求保留本地 profile ARN 解析与注入规则。查询优先使用配置的 API Region，遇到 403 时尝试兼容区域回退。

成功获取后，客户端可以使用目录公布的新模型 ID。Fable、Sonnet、Opus、Haiku 系列在列表中统一使用 `claude-` 前缀：例如上游的 `opus-5.5` 展示为 `claude-opus-5.5`，两种写法同时存在时合并为一项并优先采用带前缀条目的元数据，版本号保持不变。请求可使用带前缀或不带前缀的名称，先精确匹配上游 ID，再查找对应的另一种写法；上下文限额使用同一映射。查询忽略大小写，发送上游时保留实际 ID。其他系列不追加前缀，未发现且不满足静态兼容映射的名称会被拒绝。静态 Claude 历史别名规则仍然生效。

目录中的 Fable、Sonnet、Opus、Haiku 模型会同时提供 `-thinking` 条目，例如 `claude-opus-5.5` 与 `claude-opus-5.5-thinking`。新增条目沿用基础模型的元数据和输出上限，已有条目不会重复添加；请求的 thinking 处理逻辑保持不变。

动态目录与静态兼容项合并展示，静态项不代表当前账户一定有权限。目录来自本次选中的凭据，并非所有凭据的权限并集，也不对多凭据建立按模型分配策略。缓存只保存在内存，重启后重新获取；刚启动时可先调用 `/v1/models`，待发现完成后再使用新模型 ID。

### 功能来源与范围

本次功能按当前项目的路由、协议转换和凭据逻辑整合，以上说明依据整合后的实现重新整理：

- [d0zingcat / 761e01b](https://github.com/d0zingcat/kiro.rs/commit/761e01bcea53abdc6bd1d52b441327c3fb0b0233)：Responses 的 50 MiB 请求体限制；本项目保留现有路由并补充回归验证。
- [d0zingcat / 6e5553f](https://github.com/d0zingcat/kiro.rs/commit/6e5553f4afab4010410bf4eccd997cfde5a297fe)：GPT-5.6 hidden CoT 与 Responses 自由文本工具兼容。
- [d0zingcat / 4e7e5a8](https://github.com/d0zingcat/kiro.rs/commit/4e7e5a8394ff92634bb810bceed0efed326997b6)：Claude Code 家族别名及默认 thinking 行为。
- [hank9999 / PR #199](https://github.com/hank9999/kiro.rs/pull/199)：工具结果图片提取及 Claude Fable 5.1 支持。
- [liuran001 / 2c581f6](https://github.com/liuran001/kiro.rs-admin/commit/2c581f6f522a324656fdf20b0341f813650765da)：仅提取自动获取 Kiro 模型目录的能力，缓存与当前项目集成逻辑在本地实现。

## Admin（可选）

当 `config.json` 配置了非空 `adminApiKey` 时，会启用：

- **Admin API（认证同 API Key）**
  - `GET /api/admin/credentials` - 获取所有凭据状态
  - `POST /api/admin/credentials` - 添加新凭据
  - `DELETE /api/admin/credentials/:id` - 删除凭据
  - `POST /api/admin/credentials/:id/disabled` - 设置凭据禁用状态
  - `POST /api/admin/credentials/:id/priority` - 设置凭据优先级
  - `POST /api/admin/credentials/:id/reset` - 重置失败计数
  - `GET /api/admin/credentials/:id/balance` - 获取凭据余额
  - `GET /api/admin/credentials/:id/models` - 使用指定凭据实时获取该账号的模型 ID

- **Admin UI**
  - `GET /admin` - 访问管理页面（需要在编译前构建 `admin-ui/dist`）

每个凭据卡片的“获取模型”会单独查询该账号，展示上游实际返回的模型 ID，以及 Fable、Sonnet、Opus、Haiku 对应的 `-thinking` ID。这里保留上游 ID 的原始拼写，不混入 `/v1/models` 的静态兼容项，也不使用其他账号或全局目录的缓存结果。查询失败时显示错误；已禁用的凭据也可查询，操作不会重新启用凭据。

## 注意事项

1. **凭证安全**: 请妥善保管 `credentials.json` 文件，不要提交到版本控制
2. **Token 刷新**: 服务会自动刷新过期的 Token，无需手动干预
3. **WebSearch 工具**: 当 `tools` 列表仅包含一个 `web_search` 工具时，会走内置 WebSearch 转换逻辑

## 项目结构

```
kiro-rs/
├── src/
│   ├── main.rs                 # 程序入口
│   ├── http_client.rs          # HTTP 客户端构建
│   ├── token.rs                # Token 计算模块
│   ├── debug.rs                # 调试工具
│   ├── test.rs                 # 测试
│   ├── model/                  # 配置和参数模型
│   │   ├── config.rs           # 应用配置
│   │   └── arg.rs              # 命令行参数
│   ├── anthropic/              # Anthropic API 兼容层
│   │   ├── router.rs           # 路由配置
│   │   ├── handlers.rs         # 请求处理器
│   │   ├── middleware.rs       # 认证中间件
│   │   ├── types.rs            # 类型定义
│   │   ├── converter.rs        # 协议转换器
│   │   ├── responses.rs        # OpenAI Responses 兼容层
│   │   ├── stream.rs           # 流式响应处理
│   │   └── websearch.rs        # WebSearch 工具处理
│   ├── kiro/                   # Kiro API 客户端
│   │   ├── provider.rs         # API 提供者
│   │   ├── model_catalog.rs    # 自动模型发现与缓存
│   │   ├── token_manager.rs    # Token 管理
│   │   ├── machine_id.rs       # 设备指纹生成
│   │   ├── model/              # 数据模型
│   │   │   ├── credentials.rs  # OAuth 凭证
│   │   │   ├── available_models.rs # 上游模型目录与 Token 限额
│   │   │   ├── events/         # 响应事件类型
│   │   │   ├── requests/       # 请求类型
│   │   │   ├── common/         # 共享类型
│   │   │   ├── token_refresh.rs # Token 刷新模型
│   │   │   └── usage_limits.rs # 使用额度模型
│   │   └── parser/             # AWS Event Stream 解析器
│   │       ├── decoder.rs      # 流式解码器
│   │       ├── frame.rs        # 帧解析
│   │       ├── header.rs       # 头部解析
│   │       ├── error.rs        # 错误类型
│   │       └── crc.rs          # CRC 校验
│   ├── admin/                  # Admin API 模块
│   │   ├── router.rs           # 路由配置
│   │   ├── handlers.rs         # 请求处理器
│   │   ├── service.rs          # 业务逻辑服务
│   │   ├── types.rs            # 类型定义
│   │   ├── middleware.rs       # 认证中间件
│   │   └── error.rs            # 错误处理
│   ├── admin_ui/               # Admin UI 静态文件嵌入
│   │   └── router.rs           # 静态文件路由
│   └── common/                 # 公共模块
│       └── auth.rs             # 认证工具函数
├── admin-ui/                   # Admin UI 前端工程（构建产物会嵌入二进制）
├── tools/                      # 辅助工具
├── Cargo.toml                  # 项目配置
├── config.example.json         # 配置示例
├── docker-compose.yml          # Docker Compose 配置
└── Dockerfile                  # Docker 构建文件
```

## 技术栈

- **Web 框架**: [Axum](https://github.com/tokio-rs/axum) 0.8
- **异步运行时**: [Tokio](https://tokio.rs/)
- **HTTP 客户端**: [Reqwest](https://github.com/seanmonstar/reqwest)
- **序列化**: [Serde](https://serde.rs/)
- **日志**: [tracing](https://github.com/tokio-rs/tracing)
- **命令行**: [Clap](https://github.com/clap-rs/clap)

## License

MIT

## 致谢

本项目的实现离不开前辈的努力:  
 - [kiro2api](https://github.com/caidaoli/kiro2api)
 - [proxycast](https://github.com/aiclientproxy/proxycast)

本项目部分逻辑参考了以上的项目, 再次由衷的感谢!
