use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FanMode {
    Auto,
    Manual,
    Curve,
}

/// A single point on a fan curve: (temperature °C, fan speed %)
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CurvePoint {
    pub temp_c: f64,
    pub speed_pct: f64,
}

/// Evaluate a fan curve for a given temperature using linear interpolation.
/// Points must be sorted by temp_c. Clamps to first/last point outside range.
pub fn evaluate_fan_curve(curve: &[CurvePoint], temp_c: f64) -> f64 {
    if curve.is_empty() {
        return 50.0;
    }
    if curve.len() == 1 || temp_c <= curve[0].temp_c {
        return curve[0].speed_pct;
    }
    if temp_c >= curve[curve.len() - 1].temp_c {
        return curve[curve.len() - 1].speed_pct;
    }
    // Find the two points to interpolate between
    for i in 0..curve.len() - 1 {
        if temp_c >= curve[i].temp_c && temp_c <= curve[i + 1].temp_c {
            let range_t = curve[i + 1].temp_c - curve[i].temp_c;
            if range_t == 0.0 {
                return curve[i].speed_pct;
            }
            let t = (temp_c - curve[i].temp_c) / range_t;
            return curve[i].speed_pct + t * (curve[i + 1].speed_pct - curve[i].speed_pct);
        }
    }
    curve[curve.len() - 1].speed_pct
}

#[derive(Debug, Clone, Serialize)]
pub struct ControlInfo {
    pub fan_control_available: bool,
    pub fan_mode: Option<FanMode>,
    pub fan_manual_speed_pct: Option<f64>,
    pub fan_curve: Option<Vec<CurvePoint>>,
    pub power_cap_available: bool,
    pub power_cap_watts: Option<f64>,
    pub power_cap_min_watts: Option<f64>,
    pub power_cap_max_watts: Option<f64>,
    pub power_cap_default_watts: Option<f64>,
    pub voltage_offset_available: bool,
    pub voltage_offset_mv: Option<i32>,
}

#[derive(Debug)]
pub enum ControlError {
    Unsupported(String),
    PermissionDenied(String),
    InvalidValue(String),
    BackendError(String),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(msg) => write!(f, "unsupported: {}", msg),
            Self::PermissionDenied(msg) => write!(f, "permission denied: {}", msg),
            Self::InvalidValue(msg) => write!(f, "invalid value: {}", msg),
            Self::BackendError(msg) => write!(f, "backend error: {}", msg),
        }
    }
}

impl std::error::Error for ControlError {}

/// Trait for GPU control operations (fan speed, power cap).
/// Implementations should probe capabilities at construction time.
pub trait GpuControl: Send + 'static {
    fn get_control_info(&mut self) -> Result<ControlInfo, ControlError>;
    fn set_fan_mode(&mut self, mode: FanMode) -> Result<(), ControlError>;
    /// Set fan speed percentage (0-100). Automatically switches to manual mode.
    fn set_fan_speed(&mut self, pct: f64) -> Result<(), ControlError>;
    /// Set a custom fan curve. Automatically switches to curve mode.
    fn set_fan_curve(&mut self, curve: Vec<CurvePoint>) -> Result<(), ControlError>;
    fn set_power_cap(&mut self, watts: f64) -> Result<(), ControlError>;
    /// Set GPU voltage offset in millivolts (can be negative for undervolting).
    fn set_voltage_offset(&mut self, mv: i32) -> Result<(), ControlError>;
    /// Called each tick to apply curve-based fan control. Returns the applied speed if in curve mode.
    fn apply_curve_tick(&mut self, current_temp_c: Option<f64>) -> Option<f64>;
    /// A stable identifier for this GPU (e.g. PCI path). Used to verify config matches hardware.
    fn gpu_id(&self) -> Option<String> {
        None
    }
}
