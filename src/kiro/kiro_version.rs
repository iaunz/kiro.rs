//! Kiro IDE 版本常量
//!
//! 用量类 REST 接口（getUsageLimits）不使用 `config.kiro_version`，而是固定使用
//! [`USAGE_API_KIRO_VERSION`] + [`USAGE_API_AWS_SDK_VERSION`]，并且必须携带
//! profileArn。详见那两个常量的说明。
//!
//! 推理链路（generateAssistantResponse / mcp）不受此约束，仍走 `config.kiro_version`，
//! 见 `crate::kiro::endpoint::ide`。

/// 用量类接口（getUsageLimits）固定使用的 Kiro IDE 版本。
///
/// 上游把 UA 里的版本号当准入条件，且 profileArn 已从可选变为必填。
/// 实测（2026-08-25，BuilderID 凭据）四格对照：
///
/// | | 无 profileArn | 带 profileArn |
/// |---|---|---|
/// | `KiroIDE-0.9.2` / sdk 1.0.0 | 403 not authorized | 200 |
/// | `KiroIDE-0.12.155` / sdk 1.0.34 | 400 Invalid profileArn | 200 |
///
/// 所以两个条件必须同时满足：版本号用下面这组，且请求带上 profileArn
/// （BuilderID 用占位符 ARN，见 `KiroCredentials::streaming_profile_arn`）。
/// 只改一个都不行。
///
/// 该门槛只作用于 BuilderID / IdC；Social（Github / Google）与 API Key 凭据两组 UA 都通，
/// 所以此前一直没暴露，直到上游收紧后 idc 凭据的余额集体查不到。
///
/// 升级时这两个常量要一起动，混搭（新版本号 + 旧 SDK）未验证过。
pub const USAGE_API_KIRO_VERSION: &str = "0.12.155";

/// 用量类接口 UA 里的 aws-sdk-js 版本，与 [`USAGE_API_KIRO_VERSION`] 配套。
pub const USAGE_API_AWS_SDK_VERSION: &str = "1.0.34";
