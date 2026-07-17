//! Alert 运行时状态与决策状态机

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 持久化的运行时状态
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertState {
    /// 单次告警是否已触发
    #[serde(default)]
    pub fired: bool,
    #[serde(default)]
    pub last_total_remaining: Option<f64>,
    #[serde(default)]
    pub last_evaluated_at: Option<i64>,
    #[serde(default)]
    pub last_threshold: Option<f64>,
    #[serde(default)]
    pub credential_fingerprint: Option<String>,
}

impl AlertState {
    pub fn load(path: &Path) -> Self {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Self::default(),
        };
        serde_json::from_str(&content).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }
}

/// 对凭据 id 集合计算指纹（排序去重后 sha256）
pub fn fingerprint(ids: &[u64]) -> String {
    let mut sorted: Vec<u64> = ids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let joined = sorted
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let mut hasher = Sha256::new();
    hasher.update(joined.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// 一轮评估的决策结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// 触发告警并置 fired=true
    Fire,
    /// 重新 arm（fired=false），不发送
    Rearm,
    /// 无动作
    Nothing,
}

/// 迟滞裕度
fn hysteresis(threshold: f64) -> f64 {
    (threshold * 0.05).max(50.0)
}

/// 决策状态机（纯函数，不修改状态）
pub fn decide(state: &AlertState, total: f64, threshold: f64, fingerprint_now: &str) -> Decision {
    let below = total < threshold;

    // re-arm 触发：新增凭据 / 阈值变化（在 fire 判定之前评估）
    let fingerprint_changed = state
        .credential_fingerprint
        .as_deref()
        .map(|f| f != fingerprint_now)
        .unwrap_or(true);
    let threshold_changed = state
        .last_threshold
        .map(|t| (t - threshold).abs() > f64::EPSILON)
        .unwrap_or(true);

    // 已 fired 时的处理
    if state.fired {
        // 触发 re-arm 条件之一：指纹变化或阈值变化
        if fingerprint_changed || threshold_changed {
            // re-arm 后若仍低于阈值，立即重新 Fire
            return if below { Decision::Fire } else { Decision::Rearm };
        }
        // 恢复到阈值 + 迟滞以上 => re-arm
        if total >= threshold + hysteresis(threshold) {
            return Decision::Rearm;
        }
        return Decision::Nothing;
    }

    // 未 fired：低于阈值则 Fire
    if below {
        Decision::Fire
    } else {
        Decision::Nothing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> AlertState {
        AlertState::default()
    }

    #[test]
    fn test_fingerprint_stable_and_order_independent() {
        assert_eq!(fingerprint(&[3, 1, 2]), fingerprint(&[1, 2, 3]));
        assert_ne!(fingerprint(&[1, 2]), fingerprint(&[1, 2, 3]));
    }

    #[test]
    fn test_fire_when_armed_and_below() {
        let mut s = base();
        s.last_threshold = Some(1000.0);
        s.credential_fingerprint = Some(fingerprint(&[1]));
        let d = decide(&s, 800.0, 1000.0, &fingerprint(&[1]));
        assert_eq!(d, Decision::Fire);
    }

    #[test]
    fn test_nothing_when_armed_and_above() {
        let mut s = base();
        s.last_threshold = Some(1000.0);
        s.credential_fingerprint = Some(fingerprint(&[1]));
        let d = decide(&s, 1200.0, 1000.0, &fingerprint(&[1]));
        assert_eq!(d, Decision::Nothing);
    }

    #[test]
    fn test_nothing_when_fired_and_still_below() {
        let mut s = base();
        s.fired = true;
        s.last_threshold = Some(1000.0);
        s.credential_fingerprint = Some(fingerprint(&[1]));
        let d = decide(&s, 800.0, 1000.0, &fingerprint(&[1]));
        assert_eq!(d, Decision::Nothing);
    }

    #[test]
    fn test_rearm_on_recovery_above_hysteresis() {
        let mut s = base();
        s.fired = true;
        s.last_threshold = Some(1000.0);
        s.credential_fingerprint = Some(fingerprint(&[1]));
        // 迟滞 = max(1000*0.05, 50) = 50，需 >= 1050 才 re-arm
        let d = decide(&s, 1060.0, 1000.0, &fingerprint(&[1]));
        assert_eq!(d, Decision::Rearm);
    }

    #[test]
    fn test_no_rearm_inside_hysteresis_band() {
        let mut s = base();
        s.fired = true;
        s.last_threshold = Some(1000.0);
        s.credential_fingerprint = Some(fingerprint(&[1]));
        let d = decide(&s, 1020.0, 1000.0, &fingerprint(&[1]));
        assert_eq!(d, Decision::Nothing);
    }

    #[test]
    fn test_new_credential_rearms_and_fires_when_below() {
        let mut s = base();
        s.fired = true;
        s.last_threshold = Some(1000.0);
        s.credential_fingerprint = Some(fingerprint(&[1]));
        // 指纹变化（新增凭据）+ 仍低于阈值 => 重新 Fire
        let d = decide(&s, 800.0, 1000.0, &fingerprint(&[1, 2]));
        assert_eq!(d, Decision::Fire);
    }

    #[test]
    fn test_threshold_change_rearms_and_fires_when_below() {
        let mut s = base();
        s.fired = true;
        s.last_threshold = Some(1000.0);
        s.credential_fingerprint = Some(fingerprint(&[1]));
        // 阈值调高到 2000，total=1500 仍低于新阈值 => 重新 Fire
        let d = decide(&s, 1500.0, 2000.0, &fingerprint(&[1]));
        assert_eq!(d, Decision::Fire);
    }
}
