use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

use crate::error::CoreError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTask {
    pub id: String,
    pub name: String,
    pub goal: String,
    pub interval_seconds: u64,
    pub last_run_timestamp: Option<u64>,
    pub enabled: bool,
}

impl ScheduledTask {
    pub fn should_run(&self, now: u64) -> bool {
        if !self.enabled {
            return false;
        }
        match self.last_run_timestamp {
            Some(last) => now.saturating_sub(last) >= self.interval_seconds,
            None => true,
        }
    }
}

/// Persistent schedule registry stored at `.dex/schedules.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScheduleRegistry {
    pub tasks: Vec<ScheduledTask>,
    #[serde(skip)]
    file_path: Option<PathBuf>,
}

impl ScheduleRegistry {
    pub const DEFAULT_PATH: &'static str = ".dex/schedules.json";

    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            file_path: Some(PathBuf::from(Self::DEFAULT_PATH)),
        }
    }

    pub fn with_path(mut self, path: impl AsRef<Path>) -> Self {
        self.file_path = Some(path.as_ref().to_path_buf());
        self
    }

    pub async fn load_or_default(path: impl AsRef<Path>) -> Self {
        let p = path.as_ref();
        if tokio::fs::try_exists(p).await.unwrap_or(false) {
            if let Ok(data) = tokio::fs::read_to_string(p).await {
                if let Ok(mut reg) = serde_json::from_str::<ScheduleRegistry>(&data) {
                    reg.file_path = Some(p.to_path_buf());
                    return reg;
                }
            }
        }
        Self::new().with_path(p)
    }

    pub async fn save(&self) -> Result<(), CoreError> {
        if let Some(ref path) = self.file_path {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    tokio::fs::create_dir_all(parent).await.map_err(|e| {
                        CoreError::Internal(format!("Failed to create schedules directory: {e}"))
                    })?;
                }
            }
            let json = serde_json::to_string_pretty(self)?;
            tokio::fs::write(path, json).await.map_err(|e| {
                CoreError::Internal(format!("Failed to write schedules file: {e}"))
            })?;
        }
        Ok(())
    }

    pub fn add(&mut self, name: impl Into<String>, goal: impl Into<String>, interval_seconds: u64) -> String {
        let id = format!("sched_{}", self.tasks.len() + 1);
        self.tasks.push(ScheduledTask {
            id: id.clone(),
            name: name.into(),
            goal: goal.into(),
            interval_seconds,
            last_run_timestamp: None,
            enabled: true,
        });
        id
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let prev_len = self.tasks.len();
        self.tasks.retain(|t| t.id != id);
        self.tasks.len() < prev_len
    }

    pub fn mark_executed(&mut self, id: &str, timestamp: u64) {
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            task.last_run_timestamp = Some(timestamp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schedule_should_run() {
        let mut task = ScheduledTask {
            id: "1".to_string(),
            name: "Test".to_string(),
            goal: "Do test".to_string(),
            interval_seconds: 60,
            last_run_timestamp: None,
            enabled: true,
        };

        // First run when last_run is None
        assert!(task.should_run(100));

        task.last_run_timestamp = Some(100);
        // Not enough time passed (only 30s)
        assert!(!task.should_run(130));
        // Enough time passed (60s)
        assert!(task.should_run(160));

        // Disabled tasks should never run
        task.enabled = false;
        assert!(!task.should_run(200));
    }
}
