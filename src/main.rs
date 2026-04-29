mod gpu_data;
mod mock_data;
#[cfg(feature = "nvidia")]
mod nvidia;
#[cfg(target_os = "linux")]
mod amd;
#[cfg(target_os = "linux")]
mod intel;

use axum::{
    Router,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
};
use gpu_data::DataSource;
use std::sync::Arc;
use tokio::sync::{broadcast, watch};
use tower_http::services::ServeDir;

fn detect_gpu_source() -> Box<dyn DataSource> {
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

#[tokio::main]
async fn main() {
    println!("🔮 gpupeek — detecting GPU...");
    let mut source = detect_gpu_source();

    // Get initial snapshot to seed the cache
    let initial = source.snapshot().expect("Initial snapshot failed");
    let initial_json = serde_json::to_string(&initial).unwrap();

    let (tx, _) = broadcast::channel::<String>(16);
    let (latest_tx, latest_rx) = watch::channel(initial_json);
    let tx2 = tx.clone();

    // Background producer: one task generates snapshots for all clients
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        let mut last_good: Option<String> = None;
        loop {
            interval.tick().await;
            match source.snapshot() {
                Ok(snap) => {
                    let json = serde_json::to_string(&snap).unwrap();
                    last_good = Some(json.clone());
                    let _ = latest_tx.send(json.clone());
                    let _ = tx2.send(json);
                }
                Err(e) => {
                    eprintln!("Snapshot error: {}", e);
                    // Re-broadcast last good snapshot if available
                    if let Some(ref json) = last_good {
                        let _ = tx2.send(json.clone());
                    }
                }
            }
        }
    });

    let shared_tx = Arc::new(tx);
    let shared_latest = Arc::new(latest_rx);

    let app = Router::new()
        .route("/ws", get({
            let shared_tx = Arc::clone(&shared_tx);
            let shared_latest = Arc::clone(&shared_latest);
            move |ws| ws_handler(ws, shared_tx, shared_latest)
        }))
        .route("/health", get(|| async { "ok" }))
        .fallback_service(ServeDir::new("static"));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3333").await.unwrap();
    println!("🌐 Dashboard at http://localhost:3333");
    axum::serve(listener, app).await.unwrap();
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    tx: Arc<broadcast::Sender<String>>,
    latest: Arc<watch::Receiver<String>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, tx, latest))
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
