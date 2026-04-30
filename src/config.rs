use crate::gpu_control::{CurvePoint, FanMode};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Persisted GPU settings — re-applied on startup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuConfig {
    /// Schema version for forward compatibility
    pub version: u32,
    /// PCI device path (e.g. "/sys/class/drm/card1/device") to verify same GPU
    pub gpu_id: Option<String>,
    pub fan: FanConfig,
    pub power_cap_watts: Option<f64>,
    pub voltage_offset_mv: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum FanConfig {
    Auto,
    Manual { speed_pct: f64 },
    Curve { points: Vec<CurvePoint> },
}

impl Default for GpuConfig {
    fn default() -> Self {
        Self {
            version: 1,
            gpu_id: None,
            fan: FanConfig::Auto,
            power_cap_watts: None,
            voltage_offset_mv: None,
        }
    }
}

impl GpuConfig {
    /// Build config from current device state
    pub fn from_control_state(
        fan_mode: Option<FanMode>,
        fan_speed_pct: Option<f64>,
        fan_curve: Option<Vec<CurvePoint>>,
        power_cap_watts: Option<f64>,
        voltage_offset_mv: Option<i32>,
        gpu_id: Option<String>,
    ) -> Self {
        let fan = match fan_mode {
            Some(FanMode::Manual) => FanConfig::Manual {
                speed_pct: fan_speed_pct.unwrap_or(50.0),
            },
            Some(FanMode::Curve) => FanConfig::Curve {
                points: fan_curve.unwrap_or_default(),
            },
            _ => FanConfig::Auto,
        };
        Self {
            version: 1,
            gpu_id,
            fan,
            power_cap_watts,
            voltage_offset_mv,
        }
    }
}

/// Resolve config file path.
/// Uses `$GPUPEEK_CONFIG` env var, or `$XDG_CONFIG_HOME/gpupeek/config.json`,
/// or `~/.config/gpupeek/config.json`.
pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("GPUPEEK_CONFIG") {
        return PathBuf::from(p);
    }
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".config")
        });
    config_dir.join("gpupeek").join("config.json")
}

/// Load config from disk. Returns None if file doesn't exist or is invalid.
pub fn load_config() -> Option<GpuConfig> {
    let path = config_path();
    let content = fs::read_to_string(&path).ok()?;
    match serde_json::from_str(&content) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            eprintln!("  ⚠ Failed to parse config {}: {}", path.display(), e);
            None
        }
    }
}

/// Save config atomically (write tmp + rename).
pub fn save_config(config: &GpuConfig) -> Result<(), String> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create config dir: {}", e))?;
    }
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| format!("failed to serialize config: {}", e))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &json)
        .map_err(|e| format!("failed to write temp config: {}", e))?;
    fs::rename(&tmp, &path)
        .map_err(|e| format!("failed to rename config: {}", e))?;
    Ok(())
}
