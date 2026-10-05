//! Uygulamaya gömülü kaynaklar.
//!
//! Dosyalar derleme anında ikiliye paketlenir (`include_bytes!`); uygulama
//! HER açılışta hedef konuma çıkarılır — dosya zaten varsa bile üstüne
//! yazılır. Böylece tek .exe dağıtımı yeterlidir: hud.png ve Black Desert
//! fontu alıcının makinesinde otomatik hazır olur.

use std::path::PathBuf;

/// Gömülü HUD arka plan görüntüsü (derleme anında paketlenir).
pub const HUD_PNG: &[u8] = include_bytes!("../resources/hud.png");

/// Gömülü Black Desert fontu (derleme anında paketlenir).
pub const FONT_TTF: &[u8] = include_bytes!("../resources/black_desert.ttf");

/// Çıkarılan kaynakların kök dizini: %APPDATA%\bdomon\resources
fn resources_dir() -> PathBuf {
    std::env::var("APPDATA")
        .map(|d| PathBuf::from(d).join("bdomon").join("resources"))
        .unwrap_or_else(|_| PathBuf::from("resources"))
}

/// hud.png'nin çıkarıldığı konum (RTSS'e verilecek görüntünün kaynağı).
pub fn hud_png_path() -> PathBuf {
    resources_dir().join("hud.png")
}

/// Tüm kaynakları çıkarır/kurar. Uygulama her açılışta çağrılır; mevcut
/// dosyaların üstüne yazar. Hatalar konsola yazılır, uygulama çalışmaya
/// devam eder (font/kaynak eksikse HUD yine de çalışır, sadece görünüm
/// değişir).
pub fn install() {
    // 1) HUD görüntüsü
    let dir = resources_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("Kaynak klasörü oluşturulamadı: {e}");
    } else if let Err(e) = std::fs::write(dir.join("hud.png"), HUD_PNG) {
        eprintln!("hud.png çıkarılamadı: {e}");
    }

    // 2) Black Desert fontu: per-user font klasörü + HKCU kaydı
    install_font();
}

fn install_font() {
    use windows::core::w;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY_CURRENT_USER, KEY_WRITE,
        REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    let font_dir = std::env::var("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("Microsoft").join("Windows").join("Fonts"))
        .unwrap_or_else(|_| PathBuf::from("fonts"));
    if let Err(e) = std::fs::create_dir_all(&font_dir) {
        eprintln!("Font klasörü oluşturulamadı: {e}");
        return;
    }
    let font_path = font_dir.join("black_desert.ttf");
    if let Err(e) = std::fs::write(&font_path, FONT_TTF) {
        eprintln!("Font dosyası yazılamadı: {e}");
        return;
    }

    unsafe {
        let mut hkey = windows::Win32::System::Registry::HKEY::default();
        let res = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows NT\\CurrentVersion\\Fonts"),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        );
        if res.is_err() {
            eprintln!("Font registry anahtarı açılamadı: {res:?}");
            return;
        }
        let name: Vec<u16> = "Black Desert (TrueType)".encode_utf16().chain([0]).collect();
        let data: Vec<u16> = "black_desert.ttf".encode_utf16().chain([0]).collect();
        let res = RegSetValueExW(
            hkey,
            windows::core::PCWSTR(name.as_ptr()),
            None,
            REG_SZ,
            Some(bytemuck_like(&data)),
        );
        let _ = RegCloseKey(hkey);
        if res.is_err() {
            eprintln!("Font registry değeri yazılamadı: {res:?}");
        }
    }
}

/// &[u16] -> &[u8] (registry REG_SZ verisi için).
fn bytemuck_like(v: &[u16]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 2) }
}
