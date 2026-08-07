use mining_telemetry_core::TelemetryEnvelope;
use rolling_file::{BasicRollingFileAppender, RollingConditionBasic};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Buffered JSONL writer that maintains one file handle per output stem.
pub struct JsonlWriter {
    data_dir: PathBuf,
    handles: HashMap<String, std::io::BufWriter<BasicRollingFileAppender>>,
}

impl JsonlWriter {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            data_dir: data_dir.as_ref().to_path_buf(),
            handles: HashMap::new(),
        }
    }

    /// Append a schema v1 envelope to `{stem}.jsonl`.
    ///
    /// NOTE: We perform the write + possible roll inline here.
    /// Assumption: low-rate telemetry (≤ ~10 lines / 5 s); short buffered writes
    /// are expected to stay in page cache. Revisit if interval decreases or disk pressure appears.
    pub async fn write_envelope(&mut self, env: &TelemetryEnvelope) -> std::io::Result<()> {
        if !self.handles.contains_key(&env.stem) {
            std::fs::create_dir_all(&self.data_dir)?;
            // Setup the rolling file appender: {stem}.jsonl, rotating daily, keeping 7 days.
            let path_pattern = self.data_dir.join(format!("{}.jsonl", env.stem));

            // BasicRollingFileAppender will rotate files when the condition is met.
            // The active file is always {stem}.jsonl
            let appender = BasicRollingFileAppender::new(
                path_pattern,
                RollingConditionBasic::new().daily(),
                6,
            )
            .map_err(std::io::Error::other)?;

            self.handles
                .insert(env.stem.clone(), std::io::BufWriter::new(appender));
        }

        let handle = self.handles.get_mut(&env.stem).unwrap();

        let mut line = env
            .to_json_line()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');

        handle.write_all(line.as_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mining_telemetry_core::{SCHEMA_VERSION, host_hw};

    #[tokio::test]
    async fn writes_schema_v1_envelope() {
        let dir = std::env::temp_dir().join(format!("theseus_telem_writer_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut w = JsonlWriter::new(&dir);

        let env = host_hw("collector", "hwmon_telemetry", Some(61.0), None, None, None);
        w.write_envelope(&env).await.unwrap();
        drop(w);

        let found_text = std::fs::read_to_string(dir.join("hwmon_telemetry.jsonl")).unwrap();

        assert!(found_text.contains(&format!("\"schema_version\":{SCHEMA_VERSION}")));
        assert!(found_text.contains("host_hw") || found_text.contains("\"kind\":\"host_hw\""));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
