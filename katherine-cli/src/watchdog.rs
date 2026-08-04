// watchdog.rs — Kill Switch: 独立 watchdog 进程。
// 不依赖引擎代码。不依赖 tokio 调度器。进程级安全阀。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

/// Watchdog 配置。
struct Watchdog {
    health_url: String,
    check_interval: Duration,      // 健康检查间隔
    max_health_failures: u32,       // 连续失败阈值
    grace_period: Duration,         // stop 信号后的宽限期
    memory_limit_mb: u64,           // 内存上限
    log_path: PathBuf,              // neuro-kill.jsonl 路径
    engine_pid: Option<u32>,         // 被监控的引擎 PID
}

impl Watchdog {
    fn new(state_dir: PathBuf) -> Self {
        Watchdog {
            health_url: "http://127.0.0.1:9876/health".into(),
            check_interval: Duration::from_secs(5),
            max_health_failures: 3,
            grace_period: Duration::from_secs(3),
            memory_limit_mb: 4096,  // 4GB
            log_path: state_dir.join("neuro-kill.jsonl"),
            engine_pid: None,
        }
    }

    /// 主监控循环。
    async fn run(&mut self) {
        eprintln!("[watchdog] Started. Monitoring {}. Interval: {:?}.",
            self.health_url, self.check_interval);
        eprintln!("[watchdog] Limits: {} health failures, {}MB memory.",
            self.max_health_failures, self.memory_limit_mb);

        let mut failures = 0u32;

        loop {
            tokio::time::sleep(self.check_interval).await;

            // 1. 健康检查
            let healthy = self.check_health().await;
            if healthy {
                if failures > 0 {
                    eprintln!("[watchdog] Health restored after {failures} failures.");
                }
                failures = 0;
            } else {
                failures += 1;
                eprintln!("[watchdog] Health check failed ({failures}/{})",
                    self.max_health_failures);
            }

            if failures >= self.max_health_failures {
                self.kill("health_timeout", &format!("{failures} consecutive failures")).await;
                failures = 0; // 重置，等待引擎重启
            }

            // 2. 内存检查（仅 Windows）
            if let Some(pid) = self.engine_pid {
                if let Some(rss) = check_memory_windows(pid) {
                    let rss_mb = rss / 1024 / 1024;
                    if rss_mb > self.memory_limit_mb {
                        self.kill("memory_limit", &format!("RSS {rss_mb}MB > {}MB", self.memory_limit_mb)).await;
                    }
                }
            }
        }
    }

    async fn check_health(&self) -> bool {
        reqwest::get(&self.health_url)
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    async fn kill(&self, reason: &str, detail: &str) {
        let entry = serde_json::json!({
            "t": unix_now(),
            "reason": reason,
            "detail": detail,
        });

        // 写日志
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&self.log_path) {
            writeln!(file, "{}", serde_json::to_string(&entry).unwrap_or_default()).ok();
        }

        eprintln!("[watchdog] KILL: {reason} — {detail}");

        // 强制杀引擎进程
        if let Some(pid) = self.engine_pid {
            kill_process(pid);
            eprintln!("[watchdog] Killed PID {pid}");
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Windows 进程内存查询。
#[cfg(windows)]
fn check_memory_windows(pid: u32) -> Option<u64> {
    use std::process::Command;
    let output = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    // CSV format: "name.exe","pid","session","session#","mem usage"
    let parts: Vec<&str> = stdout.split(',').collect();
    if parts.len() >= 5 {
        let mem_str = parts[4].trim().trim_matches('"').replace(" K", "").replace(",", "");
        mem_str.parse::<u64>().ok().map(|kb| kb * 1024) // KB → bytes
    } else {
        None
    }
}

#[cfg(not(windows))]
fn check_memory_windows(_pid: u32) -> Option<u64> { None }

fn kill_process(pid: u32) {
    #[cfg(windows)]
    {
        std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .output()
            .ok();
    }
    #[cfg(not(windows))]
    {
        // Unix: kill -9
        unsafe { libc::kill(pid as i32, 9); }
    }
}

/// 启动 watchdog（独立运行）。
pub async fn start_watchdog(engine_pid: u32, port: u16, state_dir: PathBuf) {
    let mut w = Watchdog::new(state_dir);
    w.health_url = format!("http://127.0.0.1:{port}/health");
    w.engine_pid = Some(engine_pid);
    w.run().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_default_config() {
        let w = Watchdog::new(PathBuf::from("/tmp/kat_test"));
        assert_eq!(w.max_health_failures, 3);
        assert_eq!(w.memory_limit_mb, 4096);
    }
}
