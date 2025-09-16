use config::Source;
use std::path::PathBuf;
use std::time::Duration;
use std::{net::SocketAddr, path::Path};

pub use config::ConfigError;
use config::{Config, Environment, File};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub bind_addr: SocketAddr,
    pub require_sink: bool,
    pub supersede_on_register: bool,
    pub max_job_bytes: usize,
    #[serde(with = "serde_with::As::<serde_with::DurationSeconds<u64>>")]
    pub websocket_ping_interval: Duration,
    #[serde(with = "serde_with::As::<serde_with::DurationSeconds<u64>>")]
    pub websocket_pong_timeout: Duration,
    pub websocket_max_missed_pings: u32,
    #[serde(with = "serde_with::As::<serde_with::DurationSeconds<u64>>")]
    pub dispatch_timeout: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:8787".parse().unwrap(),
            require_sink: false,
            supersede_on_register: true,
            max_job_bytes: 128 * 1024, // 128 KiB
            websocket_ping_interval: Duration::from_secs(15),
            websocket_pong_timeout: Duration::from_secs(10),
            websocket_max_missed_pings: 3,
            dispatch_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub log_level: String,
    pub log_format: LogFormat,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct EnvConfig {
    pub server_bind_addr: Option<SocketAddr>,
    pub log_level: Option<String>,
    pub log_format: Option<LogFormat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Json,
    Pretty,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            log_level: "info".to_string(),
            log_format: LogFormat::Pretty,
        }
    }
}

impl AppConfig {
    pub fn from_file<P: AsRef<std::path::Path>>(
        config_path: Option<P>,
    ) -> Result<Self, ConfigError> {
        let mut sources: Vec<File<_, _>> = Vec::new();

        if config_path.is_none() {
            if let Some(pb) = Self::get_default_config_path() {
                sources.push(File::from(pb).required(false));
            }
            sources.push(File::from(Path::new("promptivd.yaml")).required(false));
        }

        if let Some(p) = config_path {
            sources.push(File::from(p.as_ref()).required(true));
        }

        Self::from_sources(sources)
    }

    pub fn get_default_config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("promptivd/config.yaml"))
    }

    pub fn create_default_config_file() -> Result<PathBuf, std::io::Error> {
        let config_path = Self::get_default_config_path().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Could not determine config directory",
            )
        })?;

        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let default_config = AppConfig::default();
        let config_yaml = serde_yaml::to_string(&default_config)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        std::fs::write(&config_path, config_yaml)?;
        Ok(config_path)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.server.max_job_bytes == 0 {
            return Err(ConfigError::Message(
                "max_job_bytes must be greater than 0".to_string(),
            ));
        }

        if self.server.websocket_max_missed_pings == 0 {
            return Err(ConfigError::Message(
                "websocket_max_missed_pings must be greater than 0".to_string(),
            ));
        }

        Ok(())
    }

    fn from_sources<S, I>(sources: I) -> Result<Self, ConfigError>
    where
        S: Source + Send + Sync + 'static,
        I: IntoIterator<Item = S>,
    {
        let mut builder = Config::builder().add_source(Config::try_from(&AppConfig::default())?);

        for src in sources {
            builder = builder.add_source(src);
        }

        let base = builder.build()?;
        let mut cfg: AppConfig = base.try_deserialize()?;

        let env_cfg: EnvConfig = Config::builder()
            .add_source(Environment::with_prefix("PROMPTIVD").try_parsing(true))
            .build()?
            .try_deserialize()
            .unwrap_or_default();
        cfg.apply_env_overrides(env_cfg);

        Ok(cfg)
    }

    fn apply_env_overrides(&mut self, e: EnvConfig) {
        if let Some(v) = e.server_bind_addr {
            self.server.bind_addr = v;
        }
        if let Some(v) = e.log_level {
            self.log_level = v;
        }
        if let Some(v) = e.log_format {
            self.log_format = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::io::Write;
    use tempfile::Builder;

    #[test]
    fn test_default_config() {
        let config = AppConfig::default();
        assert_eq!(config.server.bind_addr.port(), 8787);
        assert_eq!(config.log_level, "info");
    }

    #[test]
    fn test_config_validation() {
        let mut config = AppConfig::default();
        assert!(config.validate().is_ok());

        config.server.max_job_bytes = 0;
        assert!(config.validate().is_err());

        config.server.max_job_bytes = 1024;
    }

    #[test]
    #[serial]
    fn test_config_from_file() {
        let yaml_content = r#"
server:
  bind_addr: "127.0.0.1:9999"
log_level: "debug"
"#;

        let mut temp_file = Builder::new().suffix(".yaml").tempfile().unwrap();
        temp_file.write_all(yaml_content.as_bytes()).unwrap();

        let config = AppConfig::from_file(Some(temp_file.path())).unwrap();
        assert_eq!(config.server.bind_addr.port(), 9999);
        assert_eq!(config.log_level, "debug");
    }

    #[test]
    #[serial]
    fn test_environment_variables() {
        std::env::set_var("PROMPTIVD_SERVER_BIND_ADDR", "0.0.0.0:8080");
        std::env::set_var("PROMPTIVD_LOG_LEVEL", "trace");

        let config = AppConfig::from_file(None::<&str>).unwrap();
        assert_eq!(config.server.bind_addr.to_string(), "0.0.0.0:8080");
        assert_eq!(config.log_level, "trace");

        // Cleanup
        std::env::remove_var("PROMPTIVD_SERVER_BIND_ADDR");
        std::env::remove_var("PROMPTIVD_LOG_LEVEL");
    }
}
