use crate::gpu_data::{now_ms, DataSource, FanData, GpuSnapshot, GpuVendor, Temperatures};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

pub struct MockGpu {
    load: f64,
    vram_used: f64,
    edge_temp: f64,
    hotspot_temp: f64,
    mem_temp: f64,
    rng: StdRng,
}

impl MockGpu {
    pub fn new() -> Self {
        Self {
            load: 0.3,
            vram_used: 2048.0,
            edge_temp: 42.0,
            hotspot_temp: 55.0,
            mem_temp: 40.0,
            rng: StdRng::from_os_rng(),
        }
    }

    fn drift(&mut self, val: f64, target: f64, speed: f64, noise: f64) -> f64 {
        let direction = (target - val) * speed;
        let jitter = self.rng.random_range(-noise..noise);
        val + direction + jitter
    }
}

impl DataSource for MockGpu {
    fn snapshot(&mut self) -> Result<GpuSnapshot, Box<dyn std::error::Error>> {
        let load_target = if self.rng.random_range(0.0..1.0) < 0.02 {
            self.rng.random_range(0.0..1.0)
        } else {
            self.load + self.rng.random_range(-0.05..0.05)
        };
        self.load = self.drift(self.load, load_target, 0.1, 0.02).clamp(0.0, 1.0);

        let vram_target = 1024.0 + self.load * 6500.0;
        self.vram_used = self.drift(self.vram_used, vram_target, 0.05, 50.0).clamp(256.0, 7800.0);

        let edge_target = 35.0 + self.load * 45.0;
        let hotspot_target = 45.0 + self.load * 50.0;
        let mem_target = 32.0 + self.load * 40.0;

        self.edge_temp = self.drift(self.edge_temp, edge_target, 0.08, 0.3).clamp(25.0, 95.0);
        self.hotspot_temp = self.drift(self.hotspot_temp, hotspot_target, 0.08, 0.4).clamp(30.0, 110.0);
        self.mem_temp = self.drift(self.mem_temp, mem_target, 0.06, 0.2).clamp(25.0, 95.0);

        let power = 15.0 + self.load * 230.0 + self.rng.random_range(-3.0..3.0);

        let max_rpm: u32 = 2400;
        let fan_pct = if self.hotspot_temp < 40.0 {
            25.0
        } else if self.hotspot_temp < 60.0 {
            25.0 + (self.hotspot_temp - 40.0) / 20.0 * 25.0
        } else if self.hotspot_temp < 80.0 {
            50.0 + (self.hotspot_temp - 60.0) / 20.0 * 35.0
        } else {
            85.0 + (self.hotspot_temp - 80.0) / 20.0 * 15.0
        }
        .clamp(0.0, 100.0);
        let fan_rpm = ((fan_pct / 100.0) * max_rpm as f64) as u32;

        Ok(GpuSnapshot {
            gpu_name: "Mock GPU (RX 7900 XTX)".into(),
            gpu_vendor: GpuVendor::Mock,
            gpu_utilization: Some((self.load * 1000.0).round() / 10.0),
            vram_used_mb: Some(self.vram_used as u64),
            vram_total_mb: Some(8192),
            temperatures: Temperatures {
                edge: Some((self.edge_temp * 10.0).round() / 10.0),
                hotspot: Some((self.hotspot_temp * 10.0).round() / 10.0),
                memory: Some((self.mem_temp * 10.0).round() / 10.0),
            },
            fan: Some(FanData {
                speed_rpm: fan_rpm,
                speed_pct: (fan_pct * 10.0).round() / 10.0,
                max_rpm,
            }),
            power_watts: Some((power * 10.0).round() / 10.0),
            power_cap_watts: Some(250.0),
            gpu_clock_mhz: Some((1200.0 + self.load * 1200.0 + self.rng.random_range(-20.0..20.0)) as u32),
            gpu_clock_max_mhz: Some(2500),
            vram_clock_mhz: Some((800.0 + self.load * 1500.0 + self.rng.random_range(-10.0..10.0)) as u32),
            vram_clock_max_mhz: Some(2500),
            timestamp_ms: now_ms(),
        })
    }
}
