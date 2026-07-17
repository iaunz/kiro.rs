//! 后台轮询任务

use std::sync::Arc;
use std::time::Duration;

use super::AlertService;

/// 启动后台轮询任务。首轮在一个间隔之后才评估。
pub fn spawn_poller(service: Arc<AlertService>) {
    tokio::spawn(async move {
        loop {
            let base = service.config_snapshot().poll_interval_secs;
            let jitter = fastrand::u64(300..=600); // 5-10 分钟
            tokio::time::sleep(Duration::from_secs(base + jitter)).await;

            if service.config_snapshot().enabled {
                service.evaluate_once().await;
            }
        }
    });
    tracing::info!("Credit 预警后台轮询已启动");
}
