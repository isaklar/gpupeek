#![cfg(target_os = "linux")]

use crate::gpu_data::{now_ms, DataSource, FanData, GpuSnapshot, GpuVendor, Temperatures};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub struct AmdSource {
    card_path: PathBuf,
    hwmon_path: PathBuf,
    gpu_name: String,
    temp_label_map: HashMap<String, PathBuf>,
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

        Ok(Self {
            card_path,
            hwmon_path,
            gpu_name,
            temp_label_map,
        })
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
