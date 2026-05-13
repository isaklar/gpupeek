use crate::gpu_data::{now_ms, DataSource, FanData, GpuSnapshot, GpuVendor, Temperatures};
use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use nvml_wrapper::Nvml;
use std::ffi::c_void;

// ── NvAPI (undocumented NVIDIA API) ──────────────────────────────────────────
// libnvidia-api.so.1 is present on Linux wherever the NVIDIA driver is installed.
// We use the undocumented thermals call to get hotspot and VRAM temperatures,
// which are not exposed through NVML.

const NVAPI_LIB: &str = "libnvidia-api.so.1";
const NVAPI_QUERY_INTERFACE_SYM: &[u8] = b"nvapi_QueryInterface\0";

const NVAPI_INITIALIZE: u32 = 0x0150e828;
const NVAPI_ENUM_PHYSICAL_GPUS: u32 = 0xe5ac921f;
const NVAPI_GET_BUS_ID: u32 = 0x1be0b8e5;
const NVAPI_GET_THERMALS: u32 = 0x65fe3aad;

const NVAPI_MAX_GPUS: usize = 64;

// Sensor indices inside NvApiThermals::values (each raw value must be divided by 256)
const NVAPI_SENSOR_HOTSPOT: usize = 9;
const NVAPI_SENSOR_VRAM: usize = 15;

#[repr(C)]
struct NvApiThermals {
    version: u32,
    mask: i32,
    values: [i32; 40],
}

impl NvApiThermals {
    /// Extract a temperature (°C) from the raw sensor slot.
    /// Raw values are fixed-point (×256). Reject out-of-range readings.
    fn get_temp(&self, index: usize) -> Option<f64> {
        self.values
            .get(index)
            .copied()
            .map(|v| v / 256)
            .filter(|&v| v > 0 && v < 255)
            .map(|v| v as f64)
    }
}

/// Size-and-version tag used by NvAPI structs: lower 16 bits = sizeof, upper 16 bits = version.
const fn nvapi_version<T>(v: usize) -> u32 {
    (std::mem::size_of::<T>() | (v << 16)) as u32
}

type NvApiQueryFn = unsafe extern "C" fn(u32) -> *const ();
type NvApiInitFn = unsafe extern "C" fn() -> i32;
type NvApiEnumGpusFn =
    unsafe extern "C" fn(*mut [*mut c_void; NVAPI_MAX_GPUS], *mut u32) -> i32;
type NvApiGetBusIdFn = unsafe extern "C" fn(*mut c_void, *mut u32) -> i32;
type NvApiThermalsFn = unsafe extern "C" fn(*mut c_void, *mut NvApiThermals) -> i32;

struct NvApiState {
    _lib: libloading::Library, // must stay alive
    gpu_handle: *mut c_void,
    thermals_fn: NvApiThermalsFn,
    thermals_mask: i32,
}

// SAFETY: we only call NvAPI from a single monitoring task; the raw handle is
// tied to the NVIDIA driver which manages its own thread-safety internally.
unsafe impl Send for NvApiState {}

impl NvApiState {
    fn new(pci_bus: u32) -> Option<Self> {
        unsafe {
            let lib = libloading::Library::new(NVAPI_LIB).ok()?;

            let query_sym = lib
                .get::<NvApiQueryFn>(NVAPI_QUERY_INTERFACE_SYM)
                .ok()?;

            // Initialize the NvAPI runtime.
            let init_ptr = query_sym(NVAPI_INITIALIZE);
            if init_ptr.is_null() {
                return None;
            }
            let init: NvApiInitFn = std::mem::transmute(init_ptr);
            if init() != 0 {
                return None;
            }

            // Enumerate physical GPU handles.
            let enum_ptr = query_sym(NVAPI_ENUM_PHYSICAL_GPUS);
            if enum_ptr.is_null() {
                return None;
            }
            let enum_fn: NvApiEnumGpusFn = std::mem::transmute(enum_ptr);

            let mut handles = [std::ptr::null_mut::<c_void>(); NVAPI_MAX_GPUS];
            let mut count = 0u32;
            if enum_fn(&mut handles, &mut count) != 0 || count == 0 {
                return None;
            }

            // Find the handle whose PCI bus ID matches the NVML device.
            let bus_ptr = query_sym(NVAPI_GET_BUS_ID);
            if bus_ptr.is_null() {
                return None;
            }
            let bus_fn: NvApiGetBusIdFn = std::mem::transmute(bus_ptr);

            let mut gpu_handle = handles[0]; // fallback: first GPU
            for i in 0..count as usize {
                let mut id = 0u32;
                if bus_fn(handles[i], &mut id) == 0 && id == pci_bus {
                    gpu_handle = handles[i];
                    break;
                }
            }

            // Resolve the thermals function.
            let thermals_raw = query_sym(NVAPI_GET_THERMALS);
            if thermals_raw.is_null() {
                return None;
            }
            let thermals_fn: NvApiThermalsFn = std::mem::transmute(thermals_raw);

            // Calculate the valid sensor mask by iterating bit-by-bit until a
            // call fails, mirroring the approach used by LACT.
            let mut sensors = NvApiThermals {
                version: nvapi_version::<NvApiThermals>(2),
                mask: 1,
                values: [0; 40],
            };
            if thermals_fn(gpu_handle, &mut sensors) != 0 {
                return None;
            }
            let mut thermals_mask = 1i32;
            for bit in 0..32i32 {
                sensors.mask = 1 << bit;
                if thermals_fn(gpu_handle, &mut sensors) != 0 {
                    thermals_mask = sensors.mask - 1;
                    break;
                }
                thermals_mask = sensors.mask;
            }

            Some(Self {
                _lib: lib,
                gpu_handle,
                thermals_fn,
                thermals_mask,
            })
        }
    }

    fn read(&self) -> Option<NvApiThermals> {
        unsafe {
            let mut sensors = NvApiThermals {
                version: nvapi_version::<NvApiThermals>(2),
                mask: self.thermals_mask,
                values: [0; 40],
            };
            if (self.thermals_fn)(self.gpu_handle, &mut sensors) != 0 {
                return None;
            }
            Some(sensors)
        }
    }
}

// ── NvidiaSource ─────────────────────────────────────────────────────────────

pub struct NvidiaSource {
    nvml: Nvml,
    device_index: u32,
    nvapi: Option<NvApiState>,
}

impl NvidiaSource {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let nvml = Nvml::init()?;
        let count = nvml.device_count()?;
        if count == 0 {
            return Err("No NVIDIA GPUs found".into());
        }
        let device = nvml.device_by_index(0)?;
        let _ = device.name()?;

        // Try to obtain the PCI bus number for NvAPI handle matching.
        let pci_bus = device.pci_info().ok().map(|p| p.bus as u32).unwrap_or(0);

        let nvapi = NvApiState::new(pci_bus);
        if nvapi.is_some() {
            println!("  ✓ NvAPI thermals available (hotspot + VRAM temps enabled)");
        }

        Ok(Self {
            nvml,
            device_index: 0,
            nvapi,
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

        // Read hotspot and VRAM temps via NvAPI (indices 9 and 15).
        let (hotspot_temp, memory_temp) = match self.nvapi.as_ref().and_then(|a| a.read()) {
            Some(thermals) => (
                thermals.get_temp(NVAPI_SENSOR_HOTSPOT),
                thermals.get_temp(NVAPI_SENSOR_VRAM),
            ),
            None => (None, None),
        };

        let fan_data = device.num_fans().ok().and_then(|n| {
            if n == 0 {
                return None;
            }
            let pct = device.fan_speed(0).ok()? as f64;
            Some(FanData {
                speed_rpm: (pct / 100.0 * 3000.0) as u32,
                speed_pct: pct,
                max_rpm: 3000,
            })
        });

        let power_watts = device.power_usage().ok().map(|mw| mw as f64 / 1000.0);
        let power_cap_watts = device
            .enforced_power_limit()
            .ok()
            .map(|mw| mw as f64 / 1000.0);

        let gpu_clock_mhz = device
            .clock_info(nvml_wrapper::enum_wrappers::device::Clock::Graphics)
            .ok();
        let gpu_clock_max_mhz = device
            .max_clock_info(nvml_wrapper::enum_wrappers::device::Clock::Graphics)
            .ok();
        let vram_clock_mhz = device
            .clock_info(nvml_wrapper::enum_wrappers::device::Clock::Memory)
            .ok();
        let vram_clock_max_mhz = device
            .max_clock_info(nvml_wrapper::enum_wrappers::device::Clock::Memory)
            .ok();

        Ok(GpuSnapshot {
            gpu_name: name,
            gpu_vendor: GpuVendor::Nvidia,
            gpu_utilization: utilization,
            vram_used_mb,
            vram_total_mb,
            temperatures: Temperatures {
                edge: edge_temp,
                hotspot: hotspot_temp,
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
