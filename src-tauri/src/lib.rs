pub mod hud;
pub mod resources;
pub mod rtss;
pub mod sensors;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use hud::{build_static_slots, build_value_slots, HudLayout, LiveSource, Stats};
use rtss::RtssClient;
use tauri::Manager;

/// HUD'ın kullandığı OSD slotu sayısı:
/// [0] arka plan, [1] FPS, [2] GPU, [3] CPU, [4] Ping.
const SLOT_COUNT: usize = 5;
/// Dinamik (değer) slotlarının başlangıç indeksi.
const VALUE_SLOT_START: usize = 1;
/// HUD yenileme aralığı (saniyede bir).
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// Tasarım alanı: 1920x1080. HUD panelinin boyutu (hud.png).
const DESIGN_W: f64 = 1920.0;
const DESIGN_H: f64 = 1080.0;
const HUD_W: f64 = 141.0;
const HUD_H: f64 = 52.0;

/// Konumun kalıcı olarak saklandığı dosya.
fn position_file() -> std::path::PathBuf {
    std::env::var("APPDATA")
        .map(|d| std::path::PathBuf::from(d).join("bdomon").join("position.json"))
        .unwrap_or_else(|_| std::path::PathBuf::from("position.json"))
}

/// Konumu diskten okur; yoksa varsayılan (1, 300).
fn load_position() -> (f64, f64) {
    std::fs::read_to_string(position_file())
        .ok()
        .and_then(|s| serde_json::from_str::<[f64; 2]>(&s).ok())
        .map(|p| (p[0], p[1]))
        .unwrap_or((1.0, 300.0))
}

/// Konumu diske yazar.
fn save_position(pos: (f64, f64)) {
    if let Some(dir) = position_file().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(
        position_file(),
        serde_json::to_string(&[pos.0, pos.1]).unwrap_or_default(),
    );
}

/// Konumu geçerli aralığa kırpılır (min (1,1) — RTSS kısıtı; max ekran-hud).
fn clamp_position(x: f64, y: f64) -> (f64, f64) {
    (
        x.clamp(1.0, DESIGN_W - HUD_W - 1.0),
        y.clamp(1.0, DESIGN_H - HUD_H - 1.0),
    )
}

fn default_layout() -> HudLayout {
    // Görüntü: gömülü kaynaktan çıkarılan hud.png (%APPDATA%\bdomon\resources)
    let mut layout = HudLayout::default();
    layout.image_path = resources::hud_png_path();
    layout
}

struct OverlayState {
    client: Mutex<Option<RtssClient>>,
    running: AtomicBool,
    stats: Mutex<Stats>,
    /// HUD'ın ekrandaki sol-üst köşesi (tasarım pikseli).
    position: Mutex<(f64, f64)>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OverlayStatus {
    running: bool,
    rtss_version: String,
    slot: i64,
    fps: f64,
    gpu: f64,
    cpu: f64,
    ping: f64,
    x: f64,
    y: f64,
}

/// Slot aralığına hiper metinleri yazar (start'tan itibaren).
fn push_range(client: &mut RtssClient, start: usize, texts: &[String]) -> Result<(), String> {
    let ids: Vec<usize> = client.slots().to_vec();
    for (i, text) in texts.iter().enumerate() {
        client
            .update(ids[start + i], text)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// BDO'nun RTSS uygulama girişini bulur (0 = bulunamadı).
fn find_bdo(client: &RtssClient) -> Option<rtss::AppInfo> {
    client
        .list_apps()
        .into_iter()
        .find(|a| a.name.to_lowercase().ends_with("blackdesert64.exe"))
}

/// Overlay'i başlatır: RTSS'e bağlanır ve canlı veri döngüsünü başlatır.
#[tauri::command]
fn overlay_start(state: tauri::State<Arc<OverlayState>>) -> Result<String, String> {
    if state.running.swap(true, Ordering::SeqCst) {
        return Ok("Overlay is already running.".into());
    }

    {
        let mut guard = state.client.lock().unwrap();
        if guard.is_none() {
            let client = RtssClient::connect_with_slots(SLOT_COUNT).map_err(|e| e.to_string())?;
            *guard = Some(client);
        }
    }

    let (version, slot_ids) = {
        let guard = state.client.lock().unwrap();
        let client = guard.as_ref().unwrap();
        (client.version(), client.slots().to_vec())
    };

    // Statik slot (görüntü) güncel konumla bir kez yazılır.
    let position = *state.position.lock().unwrap();
    let statics = build_static_slots(&default_layout(), position, 1.0);
    {
        let mut guard = state.client.lock().unwrap();
        if let Some(client) = guard.as_mut() {
            if let Err(e) = push_range(client, 0, &statics) {
                state.running.store(false, Ordering::SeqCst);
                return Err(e);
            }
        }
    }

    let shared = Arc::clone(&state);
    thread::spawn(move || {
        let layout = default_layout();
        let mut live = LiveSource::new();

        while shared.running.load(Ordering::SeqCst) {
            // FPS/PID: RTSS uygulama girişinden (GPU/CPU/Ping LiveSource'ta).
            let (fps, pid) = {
                let guard = shared.client.lock().unwrap();
                guard
                    .as_ref()
                    .and_then(find_bdo)
                    .map(|a| (a.framerate, a.pid))
                    .unwrap_or((0.0, 0))
            };
            live.tick(fps, pid);

            let origin = *shared.position.lock().unwrap();
            let values = build_value_slots(&layout, origin, &live.stats(), 1.0);
            let mut guard = shared.client.lock().unwrap();
            if let Some(client) = guard.as_mut() {
                if let Err(e) = push_range(client, VALUE_SLOT_START, &values) {
                    eprintln!("Overlay güncelleme hatası: {e}");
                    shared.running.store(false, Ordering::SeqCst);
                    break;
                }
            }
            *shared.stats.lock().unwrap() = live.stats();
            thread::sleep(REFRESH_INTERVAL);
        }
    });

    let slot_str = slot_ids
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join(",");
    Ok(format!(
        "Overlay started (RTSS v{}.{} — slots {slot_str})",
        version >> 16,
        version & 0xFFFF
    ))
}

/// Overlay'i durdurur ve OSD slotlarını temizler.
#[tauri::command]
fn overlay_stop(state: tauri::State<Arc<OverlayState>>) -> Result<String, String> {
    state.running.store(false, Ordering::SeqCst);
    let mut guard = state.client.lock().unwrap();
    if let Some(client) = guard.as_mut() {
        client.release();
    }
    *guard = None;
    Ok("Overlay stopped, OSD slots cleared.".into())
}

/// HUD'ın konumunu değiştirir: statik slotu anında taşır, değer slotları
/// döngüde (veya aşağıdaki anlık yazımda) takip eder. Konum diske kaydedilir.
#[tauri::command]
fn overlay_set_position(
    x: f64,
    y: f64,
    state: tauri::State<Arc<OverlayState>>,
) -> Result<String, String> {
    let pos = clamp_position(x, y);
    let layout = default_layout();

    let mut guard = state.client.lock().unwrap();
    if guard.is_none() {
        *guard = Some(RtssClient::connect_with_slots(SLOT_COUNT).map_err(|e| e.to_string())?);
    }
    let client = guard.as_mut().unwrap();

    // Görüntü slotu anında taşınır.
    let statics = build_static_slots(&layout, pos, 1.0);
    push_range(client, 0, &statics)?;

    // Değer slotları da anında taşınır (döngü çalışmasa bile).
    let stats = *state.stats.lock().unwrap();
    let values = build_value_slots(&layout, pos, &stats, 1.0);
    push_range(client, VALUE_SLOT_START, &values)?;

    *state.position.lock().unwrap() = pos;
    save_position(pos);

    Ok(format!("Position: ({:.0}, {:.0})", pos.0, pos.1))
}

/// RTSS'ten BDO profilini oyunu yeniden başlatmadan yeniden yüklemesini ister.
#[tauri::command]
fn overlay_reload_profile(state: tauri::State<Arc<OverlayState>>) -> Result<String, String> {
    let mut guard = state.client.lock().unwrap();
    let client = match guard.as_mut() {
        Some(c) => c,
        None => {
            *guard = Some(RtssClient::connect().map_err(|e| e.to_string())?);
            guard.as_mut().unwrap()
        }
    };
    let n = client.request_profile_update("BlackDesert64.exe");
    if n == 0 {
        return Err("BlackDesert64.exe not found — the game must be running and hooked by RTSS."
            .into());
    }
    Ok(format!(
        "Profile reload requested ({n} app(s)); RTSS applies the profile live."
    ))
}

/// Güncel durum: çalışıyor mu, RTSS sürümü, slot ve son değerler.
#[tauri::command]
fn overlay_status(state: tauri::State<Arc<OverlayState>>) -> OverlayStatus {
    let (version, slot, position) = {
        let guard = state.client.lock().unwrap();
        (
            match guard.as_ref() {
                Some(client) => client.version(),
                None => 0,
            },
            match guard.as_ref() {
                Some(client) => client.slots().first().copied().map(|s| s as i64).unwrap_or(-1),
                None => -1,
            },
            *state.position.lock().unwrap(),
        )
    };
    let stats = *state.stats.lock().unwrap();
    OverlayStatus {
        running: state.running.load(Ordering::SeqCst),
        rtss_version: if version == 0 {
            "not connected".into()
        } else {
            format!("v{}.{}", version >> 16, version & 0xFFFF)
        },
        slot,
        fps: stats.fps,
        gpu: stats.gpu_pct,
        cpu: stats.cpu_pct,
        ping: stats.ping_ms,
        x: position.0,
        y: position.1,
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(Arc::new(OverlayState {
            client: Mutex::new(None),
            running: AtomicBool::new(false),
            stats: Mutex::new(Stats {
                fps: 0.0,
                gpu_pct: 0.0,
                cpu_pct: 0.0,
                ping_ms: 0.0,
            }),
            position: Mutex::new(load_position()),
        }))
        .setup(|app| {
            // Kaynakları çıkar (her açılışta üstüne yazar): hud.png + font.
            resources::install();

            // Açılışta overlay'i kayıtlı konum ve güncel hud.png ile tazele —
            // kullanıcı Başlat'a basmadan görüntü güncel olsun.
            let state = app.state::<Arc<OverlayState>>();
            match RtssClient::connect_with_slots(SLOT_COUNT) {
                Ok(mut client) => {
                    let layout = default_layout();
                    let origin = *state.position.lock().unwrap();
                    let stats = *state.stats.lock().unwrap();
                    let mut slots = build_static_slots(&layout, origin, 1.0);
                    slots.extend(build_value_slots(&layout, origin, &stats, 1.0));
                    if let Err(e) = push_range(&mut client, 0, &slots) {
                        eprintln!("Açılışta overlay yazılamadı: {e}");
                    } else {
                        *state.client.lock().unwrap() = Some(client);
                    }
                }
                Err(e) => eprintln!("RTSS'e bağlanılamadı: {e}"),
            }

            // Sistem tepsisi: sol tık = pencereyi göster; sağ tık menüsü
            // Göster / Çıkış. Çıkışta OSD slotları temizlenir.
            use tauri::menu::{Menu, MenuItem};
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

            let show = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;

            let tray = TrayIconBuilder::with_id("bdomon-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("BDOMon HUD")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "quit" => {
                        let state = app.state::<Arc<OverlayState>>();
                        state.running.store(false, Ordering::SeqCst);
                        if let Some(c) = state.client.lock().unwrap().as_mut() {
                            c.release();
                        }
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        if let Some(w) = tray.app_handle().get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                })
                .build(app)?;
            app.manage(tray);

            Ok(())
        })
        .on_window_event(|window, event| {
            // Pencerenin X'i = tepsiye küçült; overlay çalışmaya devam eder.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            overlay_start,
            overlay_stop,
            overlay_set_position,
            overlay_status,
            overlay_reload_profile
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
