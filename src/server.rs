use crate::devices::{self, FoundDevice};
use crate::hardware::HardwareStatus;
use crate::profiles::Profile;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Serialize;
use std::sync::Arc;
use tower_http::cors::CorsLayer;

const INDEX_HTML: &str = include_str!("../webui/index.html");
const APP_JS: &str = include_str!("../webui/app.js");
const STYLE_CSS: &str = include_str!("../webui/style.css");
const LOGO_SVG: &str = include_str!("../webui/kenctl-logo.svg");

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/style.css", get(style_css))
        .route("/kenctl-logo.svg", get(logo_svg))
        .route("/api/state", get(get_state))
        .route("/api/keys", get(get_keys))
        .route("/api/buttons", get(get_buttons))
        .route("/api/hardware", get(get_hardware))
        .route("/api/profiles", post(upsert_profile))
        .route(
            "/api/profiles/:name",
            put(put_profile).delete(delete_profile),
        )
        .route("/api/profiles/:name/activate", post(activate_profile))
        .layer(CorsLayer::very_permissive())
        .with_state(state)
}

type ApiError = (StatusCode, String);

fn internal<E: std::fmt::Display>(e: E) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

// ---------------------------------------------------------------- static UI

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn app_js() -> impl IntoResponse {
    (
        [
            (axum::http::header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        APP_JS,
    )
}

async fn style_css() -> impl IntoResponse {
    (
        [
            (axum::http::header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        STYLE_CSS,
    )
}

async fn logo_svg() -> impl IntoResponse {
    (
        [
            (axum::http::header::CONTENT_TYPE, "image/svg+xml; charset=utf-8"),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        LOGO_SVG,
    )
}

// ------------------------------------------------------------------- status

#[derive(Serialize)]
struct Snapshot {
    active_name: String,
    active: Profile,
    profiles: Vec<Profile>,
    devices: Vec<FoundDevice>,
    hardware: HardwareStatus,
    store_path: String,
    engine_running: bool,
}

async fn get_state(State(state): State<Arc<AppState>>) -> Json<Snapshot> {
    let (profiles, active_name, active) = {
        let store = state.store.lock().expect("store lock poisoned");
        let names = store.profiles.clone();
        let name = store.active.clone();
        (names, name, state.active_snapshot())
    };

    // Dry-run mode never starts the supervisor, so keep the device list fresh
    // straight from enumeration instead of reporting a stale empty list.
    let devices = {
        let mut cached = state.devices.lock().expect("device lock poisoned");
        if cached.is_empty() {
            *cached = devices::scan();
        }
        cached.clone()
    };

    let hardware = state.hardware.lock().expect("hardware lock poisoned").clone();
    let engine_running = devices.iter().any(|d| d.managed);

    Json(Snapshot {
        active_name,
        active,
        profiles,
        devices,
        hardware,
        store_path: state.store_path.display().to_string(),
        engine_running,
    })
}

async fn get_hardware(State(state): State<Arc<AppState>>) -> Json<HardwareStatus> {
    let status = HardwareStatus::probe();
    *state.hardware.lock().expect("hardware lock poisoned") = status.clone();
    Json(status)
}

#[derive(Serialize)]
struct ButtonOption {
    /// Real evdev code, stored in profiles and used by the engine.
    code: String,
    /// Positional name shown in the UI (falls back to `code`).
    label: String,
}

/// Buttons that the currently connected trackballs actually have.
///
/// The generic `/api/keys` list contains every Linux button (`BTN_0` … gamepad
/// buttons). Known models use their physical layout so the UI offers exactly
/// the real buttons, in clockwise order, with positional labels.
async fn get_buttons() -> Json<Vec<ButtonOption>> {
    let mut out: Vec<ButtonOption> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for found in devices::scan() {
        let codes: Vec<String> = if let Some(layout) = devices::model_buttons(found.product_id) {
            layout.iter().map(|c| (*c).to_string()).collect()
        } else {
            let Ok(dev) = evdev::Device::open(&found.path) else {
                continue;
            };
            let Some(keys) = dev.supported_keys() else {
                continue;
            };
            keys.iter()
                .map(|c| format!("{c:?}"))
                .filter(|name| name.starts_with("BTN_") && !name.contains(' '))
                .collect()
        };

        for code in codes {
            if !seen.insert(code.clone()) {
                continue;
            }
            let label = devices::button_label(found.product_id, &code)
                .map(str::to_string)
                .unwrap_or_else(|| code.clone());
            out.push(ButtonOption { code, label });
        }
    }

    Json(out)
}

/// Every assignable key/button name, deduplicated and ordered.
async fn get_keys() -> Json<Vec<String>> {
    let mut keys = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for code in 0..=0x2ffu16 {
        let name = format!("{:?}", evdev::KeyCode(code));
        // `Debug` prints `unknown key: <n>` for holes in the table.
        if name.contains(' ') {
            continue;
        }
        if seen.insert(name.clone()) {
            keys.push(name);
        }
    }
    Json(keys)
}

// ----------------------------------------------------------------- profiles

async fn upsert_profile(
    State(state): State<Arc<AppState>>,
    Json(profile): Json<Profile>,
) -> Result<Json<String>, ApiError> {
    let name = {
        let mut store = state.store.lock().map_err(internal)?;
        store.upsert(profile).map_err(internal)?
    };
    state.commit().map_err(internal)?;
    tracing::info!("saved profile {name}");
    Ok(Json(name))
}

async fn put_profile(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(mut profile): Json<Profile>,
) -> Result<Json<String>, ApiError> {
    if profile.name != name {
        profile.name = name;
    }
    let saved = {
        let mut store = state.store.lock().map_err(internal)?;
        store.upsert(profile).map_err(internal)?
    };
    state.commit().map_err(internal)?;
    Ok(Json(saved))
}

async fn delete_profile(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<bool>, ApiError> {
    let removed = {
        let mut store = state.store.lock().map_err(internal)?;
        store.remove(&name)
    };
    if !removed {
        return Err((StatusCode::NOT_FOUND, format!("no such profile: {name}")));
    }
    state.commit().map_err(internal)?;
    Ok(Json(true))
}

async fn activate_profile(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Profile>, ApiError> {
    let profile = {
        let mut store = state.store.lock().map_err(internal)?;
        store.activate(&name).map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?
    };
    state.commit().map_err(internal)?;
    tracing::info!("activated profile {name}");
    Ok(Json(profile))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::ProfileStore;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_state() -> Arc<AppState> {
        // Each test gets its own directory: these tests run in parallel and
        // share a process id, so a pid-only name races on profiles.json.tmp.
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "kenctl-api-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        AppState::new(ProfileStore::default(), dir.join("profiles.json"))
    }

    #[tokio::test]
    async fn state_endpoint_returns_a_snapshot() {
        let state = test_state();
        let Json(snap) = get_state(State(state)).await;
        assert_eq!(snap.active_name, "Default");
        assert!(snap.profiles.iter().any(|p| p.name == "Default"));
        assert!(!snap.store_path.is_empty());
        assert!(!snap.hardware.message.is_empty());
    }

    #[tokio::test]
    async fn profiles_roundtrip_through_the_api() {
        let state = test_state();
        let mut p = Profile::default();
        p.name = "Work".into();
        p.pointer_speed = 1.25;

        let Json(saved) = upsert_profile(State(state.clone()), Json(p)).await.unwrap();
        assert_eq!(saved, "Work");

        let Json(prof) = activate_profile(State(state.clone()), Path("Work".into())).await.unwrap();
        assert_eq!(prof.name, "Work");
        assert_eq!(state.active_snapshot().pointer_speed, 1.25);

        let Json(removed) = delete_profile(State(state.clone()), Path("Work".into())).await.unwrap();
        assert!(removed);
        assert_eq!(state.active_snapshot().name, "Default");
    }

    #[tokio::test]
    async fn activating_a_missing_profile_is_a_404() {
        let state = test_state();
        let err = activate_profile(State(state), Path("nope".into())).await.unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn put_rewrites_the_name_to_match_the_url() {
        let state = test_state();
        let mut p = Profile::default();
        p.name = "ignored".into();
        let Json(saved) = put_profile(State(state.clone()), Path("Renamed".into()), Json(p))
            .await
            .unwrap();
        assert_eq!(saved, "Renamed");
        let store = state.store.lock().unwrap();
        assert!(store.get("Renamed").is_some());
        assert!(store.get("ignored").is_none());
    }

    #[tokio::test]
    async fn key_list_contains_real_names_and_no_debug_noise() {
        let Json(keys) = get_keys().await;
        assert!(keys.contains(&"BTN_LEFT".to_string()));
        assert!(keys.contains(&"KEY_A".to_string()));
        assert!(keys.iter().all(|k| !k.contains(' ')));
        let unique: std::collections::HashSet<&String> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len(), "key list must not repeat names");
    }
}
