use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct GpuSnapshot {
    pub gpu_name: String,
    pub gpu_vendor: GpuVendor,
    pub gpu_utilization: Option<f64>,
    pub vram_used_mb: Option<u64>,
    pub vram_total_mb: Option<u64>,
    pub temperatures: Temperatures,
    pub fan: Option<FanData>,
    pub power_watts: Option<f64>,
    pub power_cap_watts: Option<f64>,
    pub gpu_clock_mhz: Option<u32>,
    pub gpu_clock_max_mhz: Option<u32>,
    pub vram_clock_mhz: Option<u32>,
    pub vram_clock_max_mhz: Option<u32>,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FanData {
    pub speed_rpm: u32,
    pub speed_pct: f64,
    pub max_rpm: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Temperatures {
    pub edge: Option<f64>,
    pub hotspot: Option<f64>,
    pub memory: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Mock,
}

impl std::fmt::Display for GpuVendor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Nvidia => write!(f, "NVIDIA"),
            Self::Amd => write!(f, "AMD"),
            Self::Intel => write!(f, "Intel"),
            Self::Mock => write!(f, "Mock"),
        }
    }
}

pub trait DataSource: Send + 'static {
    fn snapshot(&mut self) -> Result<GpuSnapshot, Box<dyn std::error::Error>>;
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
