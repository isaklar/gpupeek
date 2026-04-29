# gpupeek

A sleek, real-time GPU monitoring dashboard served as a local web page. Built in Rust with a WebSocket backend and a dark-themed frontend inspired by modern design systems.

## Features

- **Real-time metrics** — GPU utilization, VRAM usage, temperatures (edge/hotspot/memory), power draw, fan speed
- **Auto-detection** — automatically detects NVIDIA, AMD, or Intel GPUs; falls back to mock data if none found
- **WebSocket streaming** — 1-second update interval with sparkline history (60 data points)
- **No build tools for frontend** — plain HTML/CSS/JS, served by the Rust binary
- **Single binary** — just build and run

## Supported GPUs

| Vendor | Method | Platform |
|--------|--------|----------|
| NVIDIA | NVML (dynamic loading) | Linux (with proprietary drivers) |
| AMD | sysfs (`/sys/class/drm/`) | Linux |
| Intel | sysfs hwmon | Linux |
| None / unsupported | Mock data | Any (macOS, Windows, etc.) |

## Prerequisites

### Rust toolchain

Install Rust via [rustup](https://rustup.rs/):

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Minimum supported Rust version: **1.85** (edition 2024)

### GPU-specific dependencies (Linux only)

**NVIDIA:**
- NVIDIA proprietary driver installed (provides `libnvidia-ml.so`)
- No extra packages needed — the library is loaded at runtime

**AMD:**
- No extra packages — reads directly from sysfs
- Requires read access to `/sys/class/drm/card*/device/` files
- Some metrics may require running as root or being in the `video` group

**Intel:**
- No extra packages — reads from sysfs hwmon
- Limited metrics (typically only temperature and power on discrete Intel GPUs)

### macOS / Windows / No GPU

No dependencies — gpupeek will automatically use realistic mock data.

## Building

```sh
# Clone and build (default: NVIDIA support enabled)
git clone <repo-url> && cd gpupeek
cargo build --release
```

The binary is at `target/release/gpupeek`.

### Build without NVIDIA support

If you don't have or want NVIDIA support (smaller binary, faster compile):

```sh
cargo build --release --no-default-features
```

### Feature flags

| Feature | Default | Description |
|---------|---------|-------------|
| `nvidia` | ✓ | NVIDIA GPU support via NVML |

AMD and Intel support are always compiled on Linux (zero extra dependencies).

## Running

```sh
# From the project directory (needs access to static/ folder)
cargo run --release

# Or run the binary directly (must be run from project root, or copy static/ alongside binary)
./target/release/gpupeek
```

Then open **http://localhost:3333** in your browser.

### Startup output

```
🔮 gpupeek — detecting GPU...
  ✓ NVIDIA GPU detected
🌐 Dashboard at http://localhost:3333
```

Or if no GPU is found:

```
🔮 gpupeek — detecting GPU...
  ⚡ No supported GPU found — using mock data
🌐 Dashboard at http://localhost:3333
```

## Configuration

The server listens on `0.0.0.0:3333` by default. To change the port, edit `src/main.rs` (environment variable support planned).

## Project Structure

```
gpupeek/
├── Cargo.toml          # Dependencies and feature flags
├── src/
│   ├── main.rs         # Server, WebSocket handler, GPU auto-detection
│   ├── gpu_data.rs     # Shared types (GpuSnapshot, DataSource trait)
│   ├── mock_data.rs    # Realistic mock data generator
│   ├── nvidia.rs       # NVIDIA backend (NVML)
│   ├── amd.rs          # AMD backend (sysfs, Linux only)
│   └── intel.rs        # Intel backend (sysfs, Linux only)
└── static/
    └── index.html      # Dashboard UI (HTML + CSS + JS, no build step)
```

## Troubleshooting

**"No supported GPU found — using mock data" on Linux with a GPU:**
- NVIDIA: ensure the proprietary driver is installed (`nvidia-smi` should work)
- AMD: check that `/sys/class/drm/card0/device/vendor` contains `0x1002`
- Permissions: try running with `sudo` or add your user to the `video` group

**Dashboard shows but no data updates:**
- Check browser console for WebSocket errors
- Ensure nothing else is using port 3333

