use std::collections::HashMap;
use std::path::{Path, PathBuf};
use mining_telemetry_core::TelemetryEnvelope;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

/// Buffered JSONL writer that maintains one file handle per output stem.
pub struct JsonlWriter {
    data_dir: PathBuf,
    handles: HashMap<String, tokio::fs::File>,
}

impl JsonlWriter {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            data_dir: data_dir.as_ref().to_path_buf(),
            handles: HashMap::new(),
        }
    }

    /// Append a schema v1 envelope to `{stem}.jsonl`.
    pub async fn write_envelope(&mut self, env: &TelemetryEnvelope) -> std::io::Result<()> {
        let handle = match self.handles.get_mut(&env.stem) {
            Some(h) => h,
            None => {
                tokio::fs::create_dir_all(&self.data_dir).await?;
                let path = self.data_dir.join(format!("{}.jsonl", env.stem));
                let h = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .await?;
                self.handles.insert(env.stem.clone(), h);
                self.handles.get_mut(&env.stem).unwrap()
            }
        };

        let mut line = env
            .to_json_line()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        handle.write_all(line.as_bytes()).await?;
        handle.flush().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mining_telemetry_core::{host_hw, SCHEMA_VERSION};

    #[tokio::test]
    async fn writes_schema_v1_envelope() {
        let dir = std::env::temp_dir().join(format!(
            "theseus_telem_writer_{}",
            std::process::id()
        ));
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let mut w = JsonlWriter::new(&dir);

        let env = host_hw(
            "collector",
            "hwmon_telemetry",
            Some(61.0),
            None,
            None,
            None,
        );
        w.write_envelope(&env).await.unwrap();
        drop(w);

        let text = tokio::fs::read_to_string(dir.join("hwmon_telemetry.jsonl"))
            .await
            .unwrap();
        assert!(text.contains(&format!("\"schema_version\":{SCHEMA_VERSION}")));
        assert!(text.contains("host_hw") || text.contains("\"kind\":\"host_hw\""));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
