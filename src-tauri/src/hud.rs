//! HUD yerleşimi ve RTSS hypertext şablonu.
//!
//! Konum modeli (RTSS profil semantiği, ampirik olarak doğrulandı):
//! - RTSS profili `PositionX=1, PositionY=1` olarak sabitlenmiştir (mutlak
//!   mod; 0 "ortala" sentinel'idir, negatif sağdan sarmalar). Böylece OSD
//!   origin'i ekranda (1,1) olur ve değişmez.
//! - HUD'ın konumu tamamen hipertext `<P>` offset'lerinden gelir:
//!   `P = (x-1, y-1)`. Minimum konum (1,1) — x=0'a RTSS ile ulaşılamaz.
//! - `HudLayout.text_pos` gibi tasarım alanları HUD'ın sol-üst köşesine
//!   göredir; `build_*` fonksiyonlarına origin parametresiyle verilir.

use std::path::PathBuf;

/// Gösterilecek değerler (mock ya da gerçek).
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub fps: f64,
    pub gpu_pct: f64,
    pub cpu_pct: f64,
    pub ping_ms: f64,
}

/// HUD tasarımı. Değerler 1920x1080 tasarım pikselidir.
#[derive(Debug, Clone)]
pub struct HudLayout {
    /// Her değerin sol-üst köşesi, HUD origin'ine göre. Sıra: FPS, GPU, CPU, Ping.
    pub text_pos: [(f64, f64); 4],
    /// Tek parça arka plan PNG'si (ikonlar + panel).
    pub image_path: PathBuf,
    /// PNG'nin piksel boyutu.
    pub image_size: (u32, u32),
    /// Değer metinlerinin boyutu: RTSS `<S>` etiketi yüzdesi.
    /// Pozitif = superscript (negatif >100 değerler RTSS 7.3.5'te bozuk,
    /// test edildi; pozitif >100 çalışıyor).
    pub text_size: f64,
}

impl Default for HudLayout {
    fn default() -> Self {
        Self {
            text_pos: [(9.0, 38.0), (43.0, 38.0), (76.0, 38.0), (107.0, 38.0)],
            image_path: PathBuf::from("../assets/overlay/hud.png"),
            image_size: (141, 52),
            // Zoom x1'de varsayılan metin ~8px; 8 x 1.37 ~= 11px tasarım boyutu.
            text_size: 137.0,
        }
    }
}

/// Görüntüyü RTSS için önbellek-güvenli bir yola hazırlar.
///
/// RTSS renderer'ı `<LI>` yolunu doku önbelleği anahtarı olarak kullanır;
/// aynı yol yeniden yazılsa bile dosya içeriğini tekrar okumaz. Bu yüzden
/// hud.png güncellendiğinde eski görüntü göstermeye devam eder. Çözüm:
/// dosya içeriğinin hash'iyle adlandırılmış bir kopya kullanmak — içerik
/// değişince yol değişir, renderer yeni görüntüyü yükler (oyun restartı
/// gerekmez).
fn rtss_image_path(p: &std::path::Path) -> String {
    let bytes = std::fs::read(p).unwrap_or_default();
    if bytes.is_empty() {
        return rtss_path(p);
    }
    let hash = fnv1a(&bytes);
    let cached = std::env::temp_dir().join(format!("bdomon_hud_{hash:016x}.png"));
    if !cached.exists() {
        let _ = std::fs::write(&cached, &bytes);
    }
    rtss_path(&cached)
}

/// FNV-1a 64 bit (basit, bağımlılıksız içerik özeti).
fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// RTSS `<LI>` etiketi için güvenli mutlak yol üretir.
///
/// `std::fs::canonicalize` Windows'ta `\\?\` verbatim öneki ekler; RTSS'in
/// dosya yükleyicisi bu öneki çözemez. Bu yüzden önek kırpılır ve ayraçlar
/// ileri eğik çizgiye çevrilir (ters eğik çizgi hiper metinde escape'tir).
fn rtss_path(p: &std::path::Path) -> String {
    let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let mut s = abs.to_string_lossy().replace('\\', "/");
    if let Some(rest) = s.strip_prefix("//?/") {
        s = rest.to_string();
    }
    s
}

/// Statik slot içeriklerini üretir (bir kez yazılır; değer içermez).
///
/// `origin`: HUD'ın ekrandaki sol-üst köşesi (tasarım pikseli, min (1,1)).
///
/// Dönüş: `[0]` arka plan PNG. Görüntü döngüde tekrar yazılmaz — her
/// yenilemede `<LI>` görüntüsünün renderer tarafından yeniden işlenmesi
/// RTSS 7.3.5'te çökmelere yol açabiliyor.
pub fn build_static_slots(layout: &HudLayout, origin: (f64, f64), unit_scale: f64) -> Vec<String> {
    let img = rtss_image_path(&layout.image_path);

    let (iw, ih) = layout.image_size;
    // P koordinatları profil origin'ine (1,1) göredir.
    vec![format!(
        "<LI=\"{img}\"><P={},{}><I={},{},0,0,{iw},{ih}>",
        origin.0 - 1.0,
        origin.1 - 1.0,
        iw as f64 * unit_scale,
        ih as f64 * unit_scale
    )]
}

/// Değer metni slotlarını üretir (her tick'te güncellenir).
///
/// RTSS renderer'ı `%...%` çiftlerini makro (veri kaynağı) olarak yorumlar;
/// bir string'de ikinci bir `%` (örn. "80% <...> 80%") geçersiz makroya yol
/// açar ve TÜM OSD metni reddedilir (SDK kaynaklarında doğrulandı: editör
/// makroları derleme aşamasında çözüp renderer'a tek `%` bırakıyor). Bu
/// yüzden her değer ayrı OSD slotuna yazılır.
///
/// Dönüş sırası: `[0]` FPS, `[1]` GPU, `[2]` CPU, `[3]` Ping.
pub fn build_value_slots(
    layout: &HudLayout,
    origin: (f64, f64),
    stats: &Stats,
    unit_scale: f64,
) -> Vec<String> {
    let values = [
        format!("{}", stats.fps.round() as i64),
        format!("{}%", stats.gpu_pct.round() as i64),
        format!("{}%", stats.cpu_pct.round() as i64),
        format!("{}ms", stats.ping_ms.round() as i64),
    ];

    let mut slots = Vec::with_capacity(4);
    for (i, value) in values.iter().enumerate() {
        let mut s = String::with_capacity(64);
        push_value(&mut s, layout, origin, i, value, unit_scale);
        slots.push(s);
    }
    slots
}

/// Tam HUD'ı üretir: statik slotlar + değer slotları.
pub fn build_slots(
    layout: &HudLayout,
    origin: (f64, f64),
    stats: &Stats,
    unit_scale: f64,
) -> Vec<String> {
    let mut slots = build_static_slots(layout, origin, unit_scale);
    slots.extend(build_value_slots(layout, origin, stats, unit_scale));
    slots
}

fn push_value(
    out: &mut String,
    layout: &HudLayout,
    origin: (f64, f64),
    index: usize,
    value: &str,
    unit_scale: f64,
) {
    let to_units = |px: f64| px * unit_scale;
    let (tx, ty) = layout.text_pos[index];
    out.push_str(&format!(
        "<P={},{}><C=FFFFFF><S={}>{value}<S>",
        to_units(origin.0 - 1.0 + tx),
        to_units(origin.1 - 1.0 + ty),
        layout.text_size
    ));
}

/// Gerçek veri kaynağı: FPS/GPU/CPU sistemden, ping TCP EStats'ten.
pub struct LiveSource {
    cpu: crate::sensors::CpuMonitor,
    gpu: Option<crate::sensors::GpuMonitor>,
    ping: crate::sensors::PingMonitor,
    stats: Stats,
}

impl LiveSource {
    pub fn new() -> Self {
        Self {
            cpu: crate::sensors::CpuMonitor::new(),
            gpu: crate::sensors::GpuMonitor::new(),
            ping: crate::sensors::PingMonitor::new(),
            stats: Stats {
                fps: 0.0,
                gpu_pct: 0.0,
                cpu_pct: 0.0,
                ping_ms: 0.0,
            },
        }
    }

    /// `fps`: RTSS uygulama girişinden okunan framerate (0 = okunamadı,
    /// son değer korunur). `pid`: oyun süreci (ping için TCP bağlantı araması).
    pub fn tick(&mut self, fps: f64, pid: u32) {
        if fps > 0.0 {
            self.stats.fps = fps;
        }
        self.stats.cpu_pct = self.cpu.sample();
        if let Some(gpu) = self.gpu.as_mut() {
            if let Some(v) = gpu.sample() {
                self.stats.gpu_pct = v;
            }
        }
        if pid != 0 {
            if let Some(p) = self.ping.sample(pid) {
                self.stats.ping_ms = p;
            }
        }
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }
}
