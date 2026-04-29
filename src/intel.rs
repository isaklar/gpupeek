#![cfg(target_os = "linux")]

use crate::gpu_data::{now_ms, DataSource, GpuSnapshot, GpuVendor, Temperatures};
use std::fs;
use std::path::{Path, PathBuf};

pub struct IntelSource {
    card_path: PathBuf,
    hwmon_path: Option<PathBuf>,
    gpu_name: String,
}

impl IntelSource {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let card_path = find_intel_card()?;
        let hwmon_path = find_hwmon(&card_path).ok();
        let gpu_name = read_gpu_name(&card_path);

        // Validate: we need at least hwmon for temperatures
        if hwmon_path.is_none() {
            return Err("Intel GPU found but no hwmon metrics available".into());
        }

        Ok(Self {
            card_path,
            hwmon_path,
            gpu_name,
        })
    }
}

fn find_intel_card() -> Result<PathBuf, Box<dyn std::error::Error>> {
    for entry in fs::read_dir("/sys/class/drm")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let device_path = entry.path().join("device");
        let vendor_path = device_path.join("vendor");
        if let Ok(vendor) = fs::read_to_string(&vendor_path) {
            if vendor.trim() == "0x8086" {
                return Ok(device_path);
            }
        }
    }
    Err("No Intel GPU found in /sys/class/drm".into())
}

fn find_hwmon(card_path: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let hwmon_dir = card_path.join("hwmon");
    for entry in fs::read_dir(&hwmon_dir)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with("hwmon") {
            return Ok(entry.path());
        }
    }
    Err("No hwmon found".into())
}

fn read_gpu_name(card_path: &Path) -> String {
    if let Ok(uevent) = fs::read_to_string(card_path.join("uevent")) {
        for line in uevent.lines() {
            if let Some(val) = line.strip_prefix("PCI_SLOT_NAME=") {
                return format!("Intel GPU [{}]", val);
            }
        }
    }
    "Intel GPU".to_string()
}

fn read_sysfs_f64(path: &Path) -> Option<f64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_sysfs_u64(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

impl DataSource for IntelSource {
    fn snapshot(&mut self) -> Result<GpuSnapshot, Box<dyn std::error::Error>> {
        let mut edge_temp = None;
        let mut power_watts = None;
        let mut power_cap_watts = None;

        if let Some(hwmon) = &self.hwmon_path {
            // Temperature (millidegrees)
            edge_temp = read_sysfs_f64(&hwmon.join("temp1_input")).map(|t| t / 1000.0);

            // Power via hwmon (microwatts) — available on some Intel dGPUs
            power_watts =
                read_sysfs_f64(&hwmon.join("power1_average")).map(|uw| uw / 1_000_000.0);
            power_cap_watts =
                read_sysfs_f64(&hwmon.join("power1_cap")).map(|uw| uw / 1_000_000.0);
        }

        // Intel iGPUs typically share system memory; try to read if available
        let vram_used_mb = read_sysfs_u64(&self.card_path.join("mem_info_vram_used"))
            .map(|b| b / (1024 * 1024));
        let vram_total_mb = read_sysfs_u64(&self.card_path.join("mem_info_vram_total"))
            .map(|b| b / (1024 * 1024));

        // Intel doesn't expose gpu_busy_percent the same way;
        // i915 frequency ratio can approximate but we won't fake it
        let utilization = read_sysfs_f64(&self.card_path.join("gpu_busy_percent"));

        Ok(GpuSnapshot {
            gpu_name: self.gpu_name.clone(),
            gpu_vendor: GpuVendor::Intel,
            gpu_utilization: utilization,
            vram_used_mb,
            vram_total_mb,
            temperatures: Temperatures {
                edge: edge_temp,
                hotspot: None,
                memory: None,
            },
            fan: None, // iGPUs typically have no dedicated fan
            power_watts,
            power_cap_watts,
            timestamp_ms: now_ms(),
        })
    }
}
