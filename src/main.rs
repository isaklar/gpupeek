mod config;
mod gpu_control;
mod gpu_data;
mod mock_data;
#[cfg(feature = "nvidia")]
mod nvidia;
#[cfg(target_os = "linux")]
mod amd;
#[cfg(target_os = "linux")]
mod intel;

use axum::{
    Json, Router,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use config::{FanConfig, GpuConfig, load_config, save_config};
use gpu_control::{ControlError, CurvePoint, FanMode, GpuControl};
use gpu_data::DataSource;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::{broadcast, watch, Mutex};
use tower_http::services::ServeDir;

/// Unified trait for sources that can both monitor and control
pub trait GpuDevice: DataSource + GpuControl {}
impl<T: DataSource + GpuControl> GpuDevice for T {}

fn detect_gpu_source() -> Box<dyn GpuDevice> {
    // 1. Try NVIDIA (dynamic loading, works on any platform with NVIDIA drivers)
    #[cfg(feature = "nvidia")]
    {
        match nvidia::NvidiaSource::new() {
            Ok(src) => {
                println!("  ✓ NVIDIA GPU detected");
                return Box::new(src);
            }
            Err(_) => {}
        }
    }

    // 2. Try AMD (sysfs, Linux only)
    #[cfg(target_os = "linux")]
    {
        match amd::AmdSource::new() {
            Ok(src) => {
                println!("  ✓ AMD GPU detected");
                return Box::new(src);
            }
            Err(_) => {}
        }
    }

    // 3. Try Intel (sysfs, Linux only)
    #[cfg(target_os = "linux")]
    {
        match intel::IntelSource::new() {
            Ok(src) => {
                println!("  ✓ Intel GPU detected");
                return Box::new(src);
            }
            Err(_) => {}
        }
    }

    // 4. Fallback to mock data
    println!("  ⚡ No supported GPU found — using mock data");
    Box::new(mock_data::MockGpu::new())
}

type SharedDevice = Arc<Mutex<Box<dyn GpuDevice>>>;

#[derive(Clone)]
struct AppState {
    device: SharedDevice,
    tx: Arc<broadcast::Sender<String>>,
    latest: Arc<watch::Receiver<String>>,
}

#[tokio::main]
async fn main() {
    let daemon_mode = std::env::args().any(|a| a == "--daemon" || a == "-d");

    println!("🔮 gpupeek — detecting GPU...");
    let device: SharedDevice = Arc::new(Mutex::new(detect_gpu_source()));

    // Apply saved config on startup
    apply_saved_config(&device).await;

    // Get initial snapshot to seed the cache
    let initial = {
        let mut dev = device.lock().await;
        dev.snapshot().expect("Initial snapshot failed")
    };
    let initial_json = serde_json::to_string(&initial).unwrap();

    let (tx, _) = broadcast::channel::<String>(16);
    let (latest_tx, latest_rx) = watch::channel(initial_json);
    let tx2 = tx.clone();
    let device_clone = Arc::clone(&device);

    // Background producer: one task generates snapshots for all clients
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        let mut last_good: Option<String> = None;
        loop {
            interval.tick().await;
            let mut dev = device_clone.lock().await;
            match dev.snapshot() {
                Ok(snap) => {
                    // Apply fan curve if active (uses hotspot or edge temp)
                    let curve_temp = snap.temperatures.hotspot
                        .or(snap.temperatures.edge);
                    dev.apply_curve_tick(curve_temp);

                    let json = serde_json::to_string(&snap).unwrap();
                    last_good = Some(json.clone());
                    let _ = latest_tx.send(json.clone());
                    let _ = tx2.send(json);
                }
                Err(e) => {
                    eprintln!("Snapshot error: {}", e);
                    // Safety: if we can't read temps, force full fan in curve mode
                    dev.apply_curve_tick(Some(100.0));
                    if let Some(ref json) = last_good {
                        let _ = tx2.send(json.clone());
                    }
                }
            }
        }
    });

    let state = AppState {
        device,
        tx: Arc::new(tx),
        latest: Arc::new(latest_rx),
    };

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(|| async { "ok" }))
        .route("/api/control", get(get_control_info))
        .route("/api/fan/mode", post(set_fan_mode))
        .route("/api/fan/speed", post(set_fan_speed))
        .route("/api/fan/curve", post(set_fan_curve))
        .route("/api/power-cap", post(set_power_cap))
        .route("/api/voltage-offset", post(set_voltage_offset))
        .fallback_service(ServeDir::new("static"))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3333").await.unwrap();
    println!("🌐 Dashboard at http://localhost:3333");

    if !daemon_mode {
        let _ = open::that("http://localhost:3333");
    }

    axum::serve(listener, app).await.unwrap();
}

/// Load saved config and apply settings to the GPU.
async fn apply_saved_config(device: &SharedDevice) {
    let cfg = match load_config() {
        Some(c) => c,
        None => return,
    };

    let mut dev = device.lock().await;

    // Verify GPU identity matches
    if let Some(ref saved_id) = cfg.gpu_id
        && let Some(current_id) = dev.gpu_id()
        && &current_id != saved_id
    {
        eprintln!("  ⚠ Config GPU ({}) doesn't match detected GPU ({}) — skipping", saved_id, current_id);
        return;
    }

    println!("  📂 Applying saved settings...");

    // Apply fan settings
    match &cfg.fan {
        FanConfig::Auto => {
            if let Err(e) = dev.set_fan_mode(FanMode::Auto) {
                eprintln!("    ⚠ Fan auto: {}", e);
            }
        }
        FanConfig::Manual { speed_pct } => {
            if let Err(e) = dev.set_fan_speed(*speed_pct) {
                eprintln!("    ⚠ Fan manual {}%: {}", speed_pct, e);
            }
        }
        FanConfig::Curve { points } => {
            if let Err(e) = dev.set_fan_curve(points.clone()) {
                eprintln!("    ⚠ Fan curve: {}", e);
            }
        }
    }

    // Apply power cap
    if let Some(watts) = cfg.power_cap_watts
        && let Err(e) = dev.set_power_cap(watts)
    {
        eprintln!("    ⚠ Power cap {}W: {}", watts, e);
    }

    // Apply voltage offset
    if let Some(mv) = cfg.voltage_offset_mv
        && let Err(e) = dev.set_voltage_offset(mv)
    {
        eprintln!("    ⚠ Voltage offset {}mV: {}", mv, e);
    }

    println!("  ✓ Settings applied");
}

/// Save current device state to config file.
fn persist_current_config(dev: &mut Box<dyn GpuDevice>) {
    let info = match dev.get_control_info() {
        Ok(i) => i,
        Err(_) => return,
    };
    let cfg = GpuConfig::from_control_state(
        info.fan_mode,
        info.fan_manual_speed_pct,
        info.fan_curve,
        info.power_cap_watts,
        info.voltage_offset_mv,
        dev.gpu_id(),
    );
    if let Err(e) = save_config(&cfg) {
        eprintln!("  ⚠ Failed to save config: {}", e);
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state.tx, state.latest))
}

async fn handle_socket(
    mut socket: WebSocket,
    tx: Arc<broadcast::Sender<String>>,
    latest: Arc<watch::Receiver<String>>,
) {
    // Send the cached latest snapshot immediately
    let initial = latest.borrow().clone();
    if socket.send(Message::Text(initial.into())).await.is_err() {
        return;
    }

    // Subscribe to the shared broadcast stream
    let mut rx = tx.subscribe();
    while let Ok(json) = rx.recv().await {
        if socket.send(Message::Text(json.into())).await.is_err() {
            break;
        }
    }
}

// ─── Control API ───

async fn get_control_info(State(state): State<AppState>) -> impl IntoResponse {
    let mut dev = state.device.lock().await;
    match dev.get_control_info() {
        Ok(info) => (StatusCode::OK, Json(serde_json::to_value(info).unwrap())).into_response(),
        Err(e) => control_error_response(e),
    }
}

#[derive(Deserialize)]
struct FanModeRequest {
    mode: FanMode,
}

async fn set_fan_mode(
    State(state): State<AppState>,
    Json(body): Json<FanModeRequest>,
) -> impl IntoResponse {
    let mut dev = state.device.lock().await;
    match dev.set_fan_mode(body.mode) {
        Ok(()) => {
            persist_current_config(&mut dev);
            (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
        }
        Err(e) => control_error_response(e),
    }
}

#[derive(Deserialize)]
struct FanSpeedRequest {
    speed_pct: f64,
}

async fn set_fan_speed(
    State(state): State<AppState>,
    Json(body): Json<FanSpeedRequest>,
) -> impl IntoResponse {
    let mut dev = state.device.lock().await;
    match dev.set_fan_speed(body.speed_pct) {
        Ok(()) => {
            persist_current_config(&mut dev);
            (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
        }
        Err(e) => control_error_response(e),
    }
}

#[derive(Deserialize)]
struct FanCurveRequest {
    points: Vec<CurvePoint>,
}

async fn set_fan_curve(
    State(state): State<AppState>,
    Json(body): Json<FanCurveRequest>,
) -> impl IntoResponse {
    let mut dev = state.device.lock().await;
    match dev.set_fan_curve(body.points) {
        Ok(()) => {
            persist_current_config(&mut dev);
            (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
        }
        Err(e) => control_error_response(e),
    }
}

#[derive(Deserialize)]
struct PowerCapRequest {
    watts: f64,
}

async fn set_power_cap(
    State(state): State<AppState>,
    Json(body): Json<PowerCapRequest>,
) -> impl IntoResponse {
    let mut dev = state.device.lock().await;
    match dev.set_power_cap(body.watts) {
        Ok(()) => {
            persist_current_config(&mut dev);
            (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
        }
        Err(e) => control_error_response(e),
    }
}

#[derive(Deserialize)]
struct VoltageOffsetRequest {
    mv: i32,
}

async fn set_voltage_offset(
    State(state): State<AppState>,
    Json(body): Json<VoltageOffsetRequest>,
) -> impl IntoResponse {
    let mut dev = state.device.lock().await;
    match dev.set_voltage_offset(body.mv) {
        Ok(()) => {
            persist_current_config(&mut dev);
            (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
        }
        Err(e) => control_error_response(e),
    }
}

fn control_error_response(e: ControlError) -> axum::response::Response {
    let (status, msg) = match &e {
        ControlError::Unsupported(_) => (StatusCode::CONFLICT, e.to_string()),
        ControlError::PermissionDenied(_) => (StatusCode::FORBIDDEN, e.to_string()),
        ControlError::InvalidValue(_) => (StatusCode::BAD_REQUEST, e.to_string()),
        ControlError::BackendError(_) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    (status, Json(serde_json::json!({"error": msg}))).into_response()
}
