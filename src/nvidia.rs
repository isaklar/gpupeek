use crate::gpu_data::{now_ms, DataSource, FanData, GpuSnapshot, GpuVendor, Temperatures};
use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use nvml_wrapper::enums::device::SampleValue;
use nvml_wrapper::structs::device::FieldId;
use nvml_wrapper::Nvml;

const NVML_FI_DEV_MEMORY_TEMP: u32 = 82;

pub struct NvidiaSource {
    nvml: Nvml,
    device_index: u32,
}

impl NvidiaSource {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let nvml = Nvml::init()?;
        let count = nvml.device_count()?;
        if count == 0 {
            return Err("No NVIDIA GPUs found".into());
        }
        // Validate we can read from the first device
        let device = nvml.device_by_index(0)?;
        let _ = device.name()?;
        Ok(Self {
            nvml,
            device_index: 0,
        })
    }
}

impl DataSource for NvidiaSource {
    fn snapshot(&mut self) -> Result<GpuSnapshot, Box<dyn std::error::Error>> {
        let device = self.nvml.device_by_index(self.device_index)?;

        let name = device.name()?;

        let utilization = device.utilization_rates().ok().map(|u| u.gpu as f64);

        let mem_info = device.memory_info().ok();
        let vram_used_mb = mem_info.as_ref().map(|m| m.used / (1024 * 1024));
        let vram_total_mb = mem_info.as_ref().map(|m| m.total / (1024 * 1024));

        let edge_temp = device
            .temperature(TemperatureSensor::Gpu)
            .ok()
            .map(|t| t as f64);

        let memory_temp = device
            .field_values_for(&[FieldId(NVML_FI_DEV_MEMORY_TEMP)])
            .ok()
            .and_then(|vals| vals.into_iter().next())
            .and_then(|r| r.ok())
            .and_then(|sample| match sample.value {
                Ok(SampleValue::U32(v)) => Some(v as f64),
                Ok(SampleValue::U64(v)) => Some(v as f64),
                Ok(SampleValue::I64(v)) => Some(v as f64),
                Ok(SampleValue::F64(v)) => Some(v),
                _ => None,
            });

        let fan_data = device.num_fans().ok().and_then(|n| {
            if n == 0 {
                return None;
            }
            let pct = device.fan_speed(0).ok()? as f64;
            // NVML doesn't expose max RPM directly; estimate from percentage
            Some(FanData {
                speed_rpm: (pct / 100.0 * 3000.0) as u32,
                speed_pct: pct,
                max_rpm: 3000,
            })
        });

        let power_watts = device
            .power_usage()
            .ok()
            .map(|mw| mw as f64 / 1000.0);
        let power_cap_watts = device
            .enforced_power_limit()
            .ok()
            .map(|mw| mw as f64 / 1000.0);

        let gpu_clock_mhz = device.clock_info(nvml_wrapper::enum_wrappers::device::Clock::Graphics).ok();
        let gpu_clock_max_mhz = device.max_clock_info(nvml_wrapper::enum_wrappers::device::Clock::Graphics).ok();
        let vram_clock_mhz = device.clock_info(nvml_wrapper::enum_wrappers::device::Clock::Memory).ok();
        let vram_clock_max_mhz = device.max_clock_info(nvml_wrapper::enum_wrappers::device::Clock::Memory).ok();

        Ok(GpuSnapshot {
            gpu_name: name,
            gpu_vendor: GpuVendor::Nvidia,
            gpu_utilization: utilization,
            vram_used_mb,
            vram_total_mb,
            temperatures: Temperatures {
                edge: edge_temp,
                hotspot: None,
                memory: memory_temp,
            },
            fan: fan_data,
            power_watts,
            power_cap_watts,
            gpu_clock_mhz,
            gpu_clock_max_mhz,
            vram_clock_mhz,
            vram_clock_max_mhz,
            timestamp_ms: now_ms(),
        })
    }
}
