#![cfg(target_os = "linux")]

use crate::gpu_control::{ControlError, ControlInfo, CurvePoint, FanMode, GpuControl, evaluate_fan_curve};
use crate::gpu_data::{now_ms, DataSource, FanData, GpuSnapshot, GpuVendor, Temperatures};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub struct AmdSource {
    card_path: PathBuf,
    hwmon_path: PathBuf,
    gpu_name: String,
    temp_label_map: HashMap<String, PathBuf>,
    fan_control_writable: bool,
    power_cap_writable: bool,
    voltage_offset_writable: bool,
    fan_curve: Vec<CurvePoint>,
    fan_mode_override: Option<FanMode>,
}

impl AmdSource {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let card_path = find_amd_card()?;
        let hwmon_path = find_hwmon(&card_path)?;
        let gpu_name = read_gpu_name(&card_path);
        let temp_label_map = build_temp_label_map(&hwmon_path);

        // Validate we can read at least one metric
        let has_util = card_path.join("gpu_busy_percent").exists();
        let has_vram = card_path.join("mem_info_vram_used").exists();
        if !has_util && !has_vram {
            return Err("AMD GPU found but no readable metrics".into());
        }

        // Probe writability of control interfaces
        let pwm_enable_path = hwmon_path.join("pwm1_enable");
        let fan_control_writable = is_writable(&pwm_enable_path);
        let power_cap_path = hwmon_path.join("power1_cap");
        let power_cap_writable = is_writable(&power_cap_path);
        let od_voltage_path = card_path.join("pp_od_clk_voltage");
        let voltage_offset_writable = is_writable(&od_voltage_path);

        Ok(Self {
            card_path,
            hwmon_path,
            gpu_name,
            temp_label_map,
            fan_control_writable,
            power_cap_writable,
            voltage_offset_writable,
            // Default curve — user overrides via the UI
            fan_curve: vec![
                CurvePoint { temp_c: 30.0, speed_pct: 25.0 },
                CurvePoint { temp_c: 50.0, speed_pct: 35.0 },
                CurvePoint { temp_c: 70.0, speed_pct: 70.0 },
                CurvePoint { temp_c: 85.0, speed_pct: 100.0 },
            ],
            fan_mode_override: None,
        })
    }

    /// Parse the current voltage offset from pp_od_clk_voltage OD table.
    /// Looks for the OD_VDDGFX_OFFSET section or a "VDDGFX_OFFSET" line.
    fn read_voltage_offset(&self) -> Option<i32> {
        let path = self.card_path.join("pp_od_clk_voltage");
        let content = fs::read_to_string(&path).ok()?;
        // Look for "OD_VDDGFX_OFFSET:" section or direct offset value
        let mut in_vddgfx_section = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("OD_VDDGFX_OFFSET:") {
                in_vddgfx_section = true;
                continue;
            }
            if in_vddgfx_section {
                // Next line after header contains the offset in mV, e.g. "0mV" or "-50mV"
                let cleaned = trimmed.trim_end_matches("mV").trim_end_matches("mv");
                if let Ok(val) = cleaned.parse::<i32>() {
                    return Some(val);
                }
                // If it starts with another "OD_" section, stop
                if trimmed.starts_with("OD_") {
                    break;
                }
            }
        }
        None
    }
}

fn find_amd_card() -> Result<PathBuf, Box<dyn std::error::Error>> {
    for entry in fs::read_dir("/sys/class/drm")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let device_path = entry.path().join("device");
        let vendor_path = device_path.join("vendor");
        if let Ok(vendor) = fs::read_to_string(&vendor_path) {
            if vendor.trim() == "0x1002" {
                return Ok(device_path);
            }
        }
    }
    Err("No AMD GPU found in /sys/class/drm".into())
}

fn find_hwmon(card_path: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let hwmon_dir = card_path.join("hwmon");
    for entry in fs::read_dir(&hwmon_dir)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with("hwmon") {
            return Ok(entry.path());
        }
    }
    Err("No hwmon directory found for AMD GPU".into())
}

fn read_gpu_name(card_path: &Path) -> String {
    // Try product_name (available on some kernels/cards)
    let product = card_path.join("product_name");
    if let Ok(name) = fs::read_to_string(&product) {
        let name = name.trim().to_string();
        if !name.is_empty() {
            return name;
        }
    }

    // Try the PCI device's label
    if let Ok(name) = fs::read_to_string(card_path.join("label")) {
        let name = name.trim().to_string();
        if !name.is_empty() {
            return name;
        }
    }

    // Try lspci for the proper marketing name
    if let Ok(uevent) = fs::read_to_string(card_path.join("uevent")) {
        let mut slot = None;
        for line in uevent.lines() {
            if let Some(val) = line.strip_prefix("PCI_SLOT_NAME=") {
                slot = Some(val.to_string());
            }
        }
        if let Some(slot) = slot {
            // Try lspci for a proper name
            if let Ok(output) = std::process::Command::new("lspci")
                .args(["-s", &slot, "-m"])
                .output()
            {
                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    // lspci -m format: fields are quoted, 4th field is the device name
                    let fields: Vec<&str> = stdout
                        .lines()
                        .next()
                        .unwrap_or("")
                        .split('"')
                        .collect();
                    // Fields: [slot, "", class, "", vendor, "", device, "", ...]
                    if fields.len() >= 7 {
                        let device_name = fields[5].trim();
                        let vendor_name = fields[3].trim();
                        if !device_name.is_empty() {
                            // Return just the device name (vendor is already implied)
                            return format!("{} {}", vendor_name, device_name);
                        }
                    }
                }
            }
        }
    }

    // Final fallback with PCI slot
    if let Ok(uevent) = fs::read_to_string(card_path.join("uevent")) {
        for line in uevent.lines() {
            if let Some(val) = line.strip_prefix("PCI_SLOT_NAME=") {
                return format!("AMD GPU [{}]", val);
            }
        }
    }
    "AMD GPU".to_string()
}

/// Map temp label names (e.g. "edge", "junction", "mem") to their input files
fn build_temp_label_map(hwmon_path: &Path) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    for i in 1..=10 {
        let label_path = hwmon_path.join(format!("temp{}_label", i));
        let input_path = hwmon_path.join(format!("temp{}_input", i));
        if let Ok(label) = fs::read_to_string(&label_path) {
            let label = label.trim().to_lowercase();
            if input_path.exists() {
                map.insert(label, input_path);
            }
        }
    }
    map
}

fn read_sysfs_u64(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_sysfs_f64(path: &Path) -> Option<f64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn is_writable(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    if let Ok(meta) = fs::metadata(path) {
        let mode = meta.mode();
        let uid = meta.uid();
        let gid = meta.gid();
        let my_uid = unsafe { libc::getuid() };
        let my_gid = unsafe { libc::getgid() };
        if my_uid == 0 {
            return true;
        }
        if my_uid == uid && (mode & 0o200) != 0 {
            return true;
        }
        if my_gid == gid && (mode & 0o020) != 0 {
            return true;
        }
        if (mode & 0o002) != 0 {
            return true;
        }
    }
    false
}

fn read_temp_by_label(map: &HashMap<String, PathBuf>, label: &str) -> Option<f64> {
    let path = map.get(label)?;
    // sysfs temps are in millidegrees
    read_sysfs_f64(path).map(|t| t / 1000.0)
}

/// Read the currently active clock from pp_dpm_sclk/pp_dpm_mclk.
/// Active line is marked with a trailing '*'.
fn read_amd_clock(card_path: &Path, file: &str) -> Option<u32> {
    let content = fs::read_to_string(card_path.join(file)).ok()?;
    for line in content.lines() {
        if line.ends_with('*') {
            // Format: "1: 2500Mhz *" — extract the MHz value
            let mhz_str = line.split_whitespace()
                .find(|s| s.ends_with("Mhz") || s.ends_with("MHz"))?;
            return mhz_str.trim_end_matches("Mhz")
                .trim_end_matches("MHz")
                .parse().ok();
        }
    }
    None
}

/// Read the maximum clock (last line) from pp_dpm_sclk/pp_dpm_mclk.
fn read_amd_clock_max(card_path: &Path, file: &str) -> Option<u32> {
    let content = fs::read_to_string(card_path.join(file)).ok()?;
    let last_line = content.lines().last()?;
    let mhz_str = last_line.split_whitespace()
        .find(|s| s.ends_with("Mhz") || s.ends_with("MHz"))?;
    mhz_str.trim_end_matches("Mhz")
        .trim_end_matches("MHz")
        .parse().ok()
}

impl DataSource for AmdSource {
    fn snapshot(&mut self) -> Result<GpuSnapshot, Box<dyn std::error::Error>> {
        let utilization = read_sysfs_f64(&self.card_path.join("gpu_busy_percent"));

        let vram_used_mb =
            read_sysfs_u64(&self.card_path.join("mem_info_vram_used")).map(|b| b / (1024 * 1024));
        let vram_total_mb =
            read_sysfs_u64(&self.card_path.join("mem_info_vram_total")).map(|b| b / (1024 * 1024));

        let edge = read_temp_by_label(&self.temp_label_map, "edge");
        let hotspot = read_temp_by_label(&self.temp_label_map, "junction")
            .or_else(|| read_temp_by_label(&self.temp_label_map, "hotspot"));
        let memory = read_temp_by_label(&self.temp_label_map, "mem");

        // Power in microwatts
        let power_watts =
            read_sysfs_f64(&self.hwmon_path.join("power1_average")).map(|uw| uw / 1_000_000.0);
        let power_cap_watts =
            read_sysfs_f64(&self.hwmon_path.join("power1_cap")).map(|uw| uw / 1_000_000.0);

        let fan_rpm = read_sysfs_u64(&self.hwmon_path.join("fan1_input")).map(|r| r as u32);
        let fan_max = read_sysfs_u64(&self.hwmon_path.join("fan1_max"))
            .map(|r| r as u32)
            .unwrap_or(3000);

        let fan = fan_rpm.map(|rpm| {
            let pct = if fan_max > 0 {
                (rpm as f64 / fan_max as f64) * 100.0
            } else {
                0.0
            };
            FanData {
                speed_rpm: rpm,
                speed_pct: (pct * 10.0).round() / 10.0,
                max_rpm: fan_max,
            }
        });

        Ok(GpuSnapshot {
            gpu_name: self.gpu_name.clone(),
            gpu_vendor: GpuVendor::Amd,
            gpu_utilization: utilization,
            vram_used_mb,
            vram_total_mb,
            temperatures: Temperatures {
                edge,
                hotspot,
                memory,
            },
            fan,
            power_watts,
            power_cap_watts,
            gpu_clock_mhz: read_amd_clock(&self.card_path, "pp_dpm_sclk"),
            gpu_clock_max_mhz: read_amd_clock_max(&self.card_path, "pp_dpm_sclk"),
            vram_clock_mhz: read_amd_clock(&self.card_path, "pp_dpm_mclk"),
            vram_clock_max_mhz: read_amd_clock_max(&self.card_path, "pp_dpm_mclk"),
            timestamp_ms: now_ms(),
        })
    }
}

impl GpuControl for AmdSource {
    fn get_control_info(&mut self) -> Result<ControlInfo, ControlError> {
        let hw_fan_mode = read_sysfs_u64(&self.hwmon_path.join("pwm1_enable")).map(|v| match v {
            1 => FanMode::Manual,
            _ => FanMode::Auto,
        });

        // Report curve mode if we're managing it, otherwise report hw state
        let fan_mode = self.fan_mode_override.or(hw_fan_mode);

        let fan_manual_speed_pct = if hw_fan_mode == Some(FanMode::Manual) {
            read_sysfs_u64(&self.hwmon_path.join("pwm1")).map(|pwm| {
                let max = read_sysfs_u64(&self.hwmon_path.join("pwm1_max")).unwrap_or(255);
                (pwm as f64 / max as f64 * 100.0 * 10.0).round() / 10.0
            })
        } else {
            None
        };

        let power_cap_watts =
            read_sysfs_f64(&self.hwmon_path.join("power1_cap")).map(|uw| uw / 1_000_000.0);
        let power_cap_min_watts =
            read_sysfs_f64(&self.hwmon_path.join("power1_cap_min")).map(|uw| uw / 1_000_000.0);
        let power_cap_max_watts =
            read_sysfs_f64(&self.hwmon_path.join("power1_cap_max")).map(|uw| uw / 1_000_000.0);
        let power_cap_default_watts =
            read_sysfs_f64(&self.hwmon_path.join("power1_cap_default")).map(|uw| uw / 1_000_000.0);

        Ok(ControlInfo {
            fan_control_available: self.fan_control_writable,
            fan_mode,
            fan_manual_speed_pct,
            fan_curve: Some(self.fan_curve.clone()),
            power_cap_available: self.power_cap_writable,
            power_cap_watts,
            power_cap_min_watts,
            power_cap_max_watts,
            power_cap_default_watts,
            voltage_offset_available: self.voltage_offset_writable,
            voltage_offset_mv: self.read_voltage_offset(),
        })
    }

    fn set_fan_mode(&mut self, mode: FanMode) -> Result<(), ControlError> {
        if !self.fan_control_writable {
            return Err(ControlError::PermissionDenied(
                "fan control not writable (need root?)".into(),
            ));
        }
        match mode {
            FanMode::Auto => {
                self.fan_mode_override = None;
                fs::write(self.hwmon_path.join("pwm1_enable"), "2")
                    .map_err(|e| ControlError::BackendError(format!("failed to write pwm1_enable: {}", e)))
            }
            FanMode::Manual => {
                self.fan_mode_override = Some(FanMode::Manual);
                fs::write(self.hwmon_path.join("pwm1_enable"), "1")
                    .map_err(|e| ControlError::BackendError(format!("failed to write pwm1_enable: {}", e)))
            }
            FanMode::Curve => {
                self.fan_mode_override = Some(FanMode::Curve);
                // Put hardware in manual mode so we can drive it from the curve
                fs::write(self.hwmon_path.join("pwm1_enable"), "1")
                    .map_err(|e| ControlError::BackendError(format!("failed to write pwm1_enable: {}", e)))
            }
        }
    }

    fn set_fan_speed(&mut self, pct: f64) -> Result<(), ControlError> {
        if !self.fan_control_writable {
            return Err(ControlError::PermissionDenied(
                "fan control not writable (need root?)".into(),
            ));
        }
        if !(0.0..=100.0).contains(&pct) {
            return Err(ControlError::InvalidValue(
                "fan speed must be 0-100%".into(),
            ));
        }
        self.set_fan_mode(FanMode::Manual)?;

        let max = read_sysfs_u64(&self.hwmon_path.join("pwm1_max")).unwrap_or(255);
        let pwm_value = ((pct / 100.0) * max as f64).round() as u64;
        fs::write(self.hwmon_path.join("pwm1"), pwm_value.to_string())
            .map_err(|e| ControlError::BackendError(format!("failed to write pwm1: {}", e)))
    }

    fn set_fan_curve(&mut self, mut curve: Vec<CurvePoint>) -> Result<(), ControlError> {
        if !self.fan_control_writable {
            return Err(ControlError::PermissionDenied(
                "fan control not writable (need root?)".into(),
            ));
        }
        if curve.is_empty() {
            return Err(ControlError::InvalidValue("curve must have at least one point".into()));
        }
        // Sort by temperature
        curve.sort_by(|a, b| a.temp_c.partial_cmp(&b.temp_c).unwrap_or(std::cmp::Ordering::Equal));
        // Validate ranges
        for p in &curve {
            if !(0.0..=110.0).contains(&p.temp_c) {
                return Err(ControlError::InvalidValue("temperature must be 0-110°C".into()));
            }
            if !(0.0..=100.0).contains(&p.speed_pct) {
                return Err(ControlError::InvalidValue("fan speed must be 0-100%".into()));
            }
        }
        self.fan_curve = curve;
        self.set_fan_mode(FanMode::Curve)?;
        Ok(())
    }

    fn set_power_cap(&mut self, watts: f64) -> Result<(), ControlError> {
        if !self.power_cap_writable {
            return Err(ControlError::PermissionDenied(
                "power cap not writable (need root?)".into(),
            ));
        }

        let min_w = read_sysfs_f64(&self.hwmon_path.join("power1_cap_min"))
            .map(|uw| uw / 1_000_000.0)
            .unwrap_or(0.0);
        let max_w = read_sysfs_f64(&self.hwmon_path.join("power1_cap_max"))
            .map(|uw| uw / 1_000_000.0)
            .unwrap_or(f64::MAX);

        if watts < min_w || watts > max_w {
            return Err(ControlError::InvalidValue(format!(
                "power cap must be between {} and {} W",
                min_w, max_w
            )));
        }

        let microwatts = (watts * 1_000_000.0) as u64;
        fs::write(self.hwmon_path.join("power1_cap"), microwatts.to_string())
            .map_err(|e| ControlError::BackendError(format!("failed to write power1_cap: {}", e)))
    }

    fn set_voltage_offset(&mut self, mv: i32) -> Result<(), ControlError> {
        if !self.voltage_offset_writable {
            return Err(ControlError::PermissionDenied(
                "voltage offset not writable (need root and amdgpu.ppfeaturemask?)".into(),
            ));
        }
        // Safety: clamp to a reasonable range to prevent hardware damage
        if !(-250..=250).contains(&mv) {
            return Err(ControlError::InvalidValue(
                "voltage offset must be between -250 and +250 mV".into(),
            ));
        }
        let path = self.card_path.join("pp_od_clk_voltage");
        // Write "vo <offset_mV>" then "c" to commit
        fs::write(&path, format!("vo {}", mv))
            .map_err(|e| ControlError::BackendError(format!("failed to write voltage offset: {}", e)))?;
        fs::write(&path, "c")
            .map_err(|e| ControlError::BackendError(format!("failed to commit voltage offset: {}", e)))?;
        Ok(())
    }

    fn apply_curve_tick(&mut self, current_temp_c: Option<f64>) -> Option<f64> {
        if self.fan_mode_override != Some(FanMode::Curve) {
            return None;
        }
        let temp = current_temp_c?;

        // Safety: force 100% fan if temperature exceeds critical threshold
        const CRITICAL_TEMP_C: f64 = 95.0;
        let target_pct = if temp >= CRITICAL_TEMP_C {
            100.0
        } else {
            evaluate_fan_curve(&self.fan_curve, temp)
        };

        let max = read_sysfs_u64(&self.hwmon_path.join("pwm1_max")).unwrap_or(255);
        let pwm_value = ((target_pct / 100.0) * max as f64).round() as u64;
        let _ = fs::write(self.hwmon_path.join("pwm1"), pwm_value.to_string());
        Some(target_pct)
    }

    fn gpu_id(&self) -> Option<String> {
        Some(self.card_path.to_string_lossy().into_owned())
    }
}
