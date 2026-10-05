//! Gerçek zamanlı sensör verileri.
//!
//! - CPU: `GetSystemTimes` deltalarından toplam kullanım (%). SDK örneğinin
//!   `CalcCpuUsage` yöntemiyle aynı yaklaşım.
//! - GPU: PDH "GPU Engine(*)\\Utilization Percentage" sayaçları — Task
//!   Manager'ın kullandığı kaynak. Örnekler dinamiktir, her örneklemede
//!   yenilenir; bildirilen değer 3D motorunun toplamı (Task Manager'ın
//!   varsayılanı). RTSS bu veriyi kendi HAL'inde tutar ama paylaşımlı
//!   bellekten vermez, bu yüzden aynı veriyi doğrudan Windows'tan okuyoruz.
//! - FPS: RTSS paylaşımlı belleğindeki uygulama girişinden (bu modülde
//!   değil — `RtssClient::list_apps()` ile okunur).

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, GetPerTcpConnectionEStats, SetPerTcpConnectionEStats,
    MIB_TCPROW_LH, MIB_TCPROW_LH_0, MIB_TCPROW_OWNER_PID, MIB_TCP_STATE_ESTAB,
    TcpConnectionEstatsFineRtt, TCP_ESTATS_FINE_RTT_ROD_v0, TCP_ESTATS_FINE_RTT_RW_v0,
    TCP_TABLE_OWNER_PID_ALL,
};
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhExpandWildCardPathW,
    PdhGetFormattedCounterValue, PdhOpenQueryW, PdhRemoveCounter, PDH_FMT_COUNTERVALUE,
    PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
};
use windows::Win32::System::Threading::GetSystemTimes;

/// Toplam CPU kullanımı (%) — GetSystemTimes deltalarından.
pub struct CpuMonitor {
    idle: u64,
    total: u64,
}

impl CpuMonitor {
    pub fn new() -> Self {
        let (idle, total) = read_times();
        Self { idle, total }
    }

    pub fn sample(&mut self) -> f64 {
        let (idle, total) = read_times();
        let di = idle.wrapping_sub(self.idle) as f64;
        let dt = total.wrapping_sub(self.total) as f64;
        self.idle = idle;
        self.total = total;
        if dt <= 0.0 {
            return 0.0;
        }
        ((1.0 - di / dt) * 100.0).clamp(0.0, 100.0)
    }
}

fn read_times() -> (u64, u64) {
    unsafe {
        let mut idle = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        if GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).is_err() {
            return (0, 0);
        }
        let to64 = |ft: &FILETIME| ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
        (to64(&idle), to64(&kernel) + to64(&user))
    }
}

/// GPU motor kullanımı (%) — PDH sayaçlarından.
///
/// "GPU Engine(*)\Utilization Percentage" örnekleri DİNAMİKTİR: süreçler GPU
/// kullanmaya başladığında/bıraktığında belirir ve kaybolur. Sayaçları bir kez
/// oluşturup sabitlemek oyunun örneklerini kaçırdığı için (Task Manager %76
/// iken hud'da %3 kalması tam olarak buydu) örnek listesi her örneklemede
/// yeniden açılır. Bildirilen değer: tüm süreçlerin 3D motoru toplamı —
/// Task Manager'ın varsayılan metrikleri; 3D örneği yoksa tüm motorların toplamı.
pub struct GpuMonitor {
    query: PDH_HQUERY,
    /// Normalize edilmiş sayaç yolu → tanıtıcı (örnek kümesi zamanla değişir).
    counters: std::collections::HashMap<String, PDH_HCOUNTER>,
}

impl GpuMonitor {
    pub fn new() -> Option<Self> {
        unsafe {
            let mut query = PDH_HQUERY::default();
            if PdhOpenQueryW(None, 0, &mut query) != 0 {
                return None;
            }
            let mut monitor = Self {
                query,
                counters: std::collections::HashMap::new(),
            };
            if !monitor.refresh_instances() || monitor.counters.is_empty() {
                PdhCloseQuery(query);
                return None;
            }

            // Sayaç eklendikten sonra ilk koleksiyon yapıyı kurar;
            // ikincisi gerçek veri üretir.
            PdhCollectQueryData(query);
            Some(monitor)
        }
    }

    /// Joker yolu yeniden açar; yeni motor örnekleri ekler, ölmüş olanları
    /// çıkarır. Genişletme başarısızsa mevcut sayaçlarla devam edilir (false).
    fn refresh_instances(&mut self) -> bool {
        // Yüzlerce örnek olabilir — gerekli tamponu PDH_MORE_DATA ile öğren.
        const PDH_MORE_DATA: u32 = 0x8000_07D2;
        unsafe {
            let path = wide("\\GPU Engine(*)\\Utilization Percentage");
            let mut buf = Vec::new();
            let mut len: u32 = 0;
            let mut status = PdhExpandWildCardPathW(
                None,
                PCWSTR(path.as_ptr()),
                None,
                &mut len,
                0,
            );
            if status == PDH_MORE_DATA {
                buf = vec![0u16; len as usize];
                status = PdhExpandWildCardPathW(
                    None,
                    PCWSTR(path.as_ptr()),
                    Some(PWSTR(buf.as_mut_ptr())),
                    &mut len,
                    0,
                );
            }
            if status != 0 {
                eprintln!("[gpu] expand durum=0x{status:08X}");
                return false;
            }
            buf.truncate(len as usize);

            let mut seen = std::collections::HashSet::new();
            for p in split_nul_strings(&buf) {
                // PdhExpandWildCardPathW yerel makine adı öneki ekler
                // ("\\KADIR\GPU Engine..."); yerel yollar öneksiz eklenir.
                let s = String::from_utf16_lossy(&p);
                let s = match s.find("GPU Engine") {
                    Some(i) if i > 0 => s[i - 1..].to_string(),
                    _ => s,
                };
                if !seen.insert(s.clone()) || self.counters.contains_key(&s) {
                    continue;
                }
                let w = wide(&s);
                let mut h = PDH_HCOUNTER::default();
                if PdhAddEnglishCounterW(self.query, PCWSTR(w.as_ptr()), 0, &mut h) == 0 {
                    self.counters.insert(s, h);
                }
            }
            // Artık var olmayan örneklerin sayaçlarını çıkar.
            let stale: Vec<String> = self
                .counters
                .keys()
                .filter(|k| !seen.contains(*k))
                .cloned()
                .collect();
            for k in stale {
                if let Some(h) = self.counters.remove(&k) {
                    PdhRemoveCounter(h);
                }
            }
            true
        }
    }

    /// GPU kullanımı (%). Hata durumunda None (son değer korunur).
    pub fn sample(&mut self) -> Option<f64> {
        unsafe {
            // Örnekler süreç etkinliğiyle gelip gider; her seferinde yenile.
            self.refresh_instances();

            let cs = PdhCollectQueryData(self.query);
            if cs != 0 {
                eprintln!("[gpu] collect durum=0x{cs:08X}");
                return None;
            }

            // 3D motoru toplamı (Task Manager'ın varsayılanı); 3D örneği
            // yoksa tüm motorlar. Yeni eklenen sayaçların ilk örneklemede
            // CStatus'ı geçersizdir (ikinci koleksiyona kadar) — atlanır.
            let mut sum_3d = 0.0;
            let mut sum_all = 0.0;
            let mut n_3d = 0;
            for (path, &h) in &self.counters {
                let mut v = PDH_FMT_COUNTERVALUE::default();
                if PdhGetFormattedCounterValue(h, PDH_FMT_DOUBLE, None, &mut v) == 0
                    && v.CStatus == 0
                {
                    let val = v.Anonymous.doubleValue;
                    sum_all += val;
                    if is_3d_engine(path) {
                        sum_3d += val;
                        n_3d += 1;
                    }
                }
            }
            let value = if n_3d > 0 { sum_3d } else { sum_all };
            Some(value.clamp(0.0, 100.0))
        }
    }
}

/// Örnek adı 3D motoru gösteriyor mu? Win10+ örnek adları:
/// `pid_N_luid_..._phys_N_engtype_3D` (yeni) veya `..._eng_0` (eski).
fn is_3d_engine(path: &str) -> bool {
    path.contains("engtype_3D") || path.contains("eng_0)")
}

impl Drop for GpuMonitor {
    fn drop(&mut self) {
        unsafe {
            PdhCloseQuery(self.query);
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// PDH yol listeleri çift NUL ile biter; öğeler tek NUL ile ayrılır.
fn split_nul_strings(buf: &[u16]) -> Vec<Vec<u16>> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    let mut prev_nul = false;
    for &c in buf {
        if c == 0 {
            if cur.is_empty() {
                if prev_nul {
                    break;
                }
            } else {
                out.push(std::mem::take(&mut cur));
            }
            prev_nul = true;
        } else {
            cur.push(c);
            prev_nul = false;
        }
    }
    out
}

/// Oyunun TCP bağlantısının gidiş-dönüş gecikmesi (ms) — Windows TCP
/// stack'inin kendi ölçümünden (EStats FineRtt, `SumRtt` = yumuşatılmış
/// RTT, mikrosaniye). Oyun sunucu değiştirince bağlantı tuple'ı değişir;
/// monitör otomatik olarak yeni bağlantıya geçer.
pub struct PingMonitor {
    /// Son görülen bağlantı: (yerel addr, yerel port, uzak addr, uzak port).
    tuple: Option<[u32; 4]>,
    /// FineRtt koleksiyonunun etkinleştirildiği bağlantı.
    enabled: Option<[u32; 4]>,
    value: f64,
}

/// BDO oyun sunucu portu.
pub const GAME_REMOTE_PORT: u16 = 8889;

impl PingMonitor {
    pub fn new() -> Self {
        Self { tuple: None, enabled: None, value: 0.0 }
    }

    /// `pid`: BlackDesert64.exe'nin PID'si (RTSS uygulama girişinden).
    /// Bağlantı bulunamazsa None döner (son değer korunur).
    pub fn sample(&mut self, pid: u32) -> Option<f64> {
        let tuple = find_game_connection(pid)?;
        if self.tuple != Some(tuple) {
            // Sunucu/bağlantı değişti: FineRtt koleksiyonu yeni bağlantıda
            // yeniden etkinleştirilir.
            self.tuple = Some(tuple);
            self.enabled = None;
        }
        let row = MIB_TCPROW_LH {
            Anonymous: MIB_TCPROW_LH_0 { dwState: MIB_TCP_STATE_ESTAB.0 as u32 },
            dwLocalAddr: tuple[0],
            dwLocalPort: tuple[1],
            dwRemoteAddr: tuple[2],
            dwRemotePort: tuple[3],
        };

        unsafe {
            // FineRtt koleksiyonu bağlantı başına bir kez etkinleştirilir.
            if self.enabled != Some(tuple) {
                let rw = TCP_ESTATS_FINE_RTT_RW_v0 { EnableCollection: true };
                let rw_bytes = std::slice::from_raw_parts(
                    &rw as *const _ as *const u8,
                    std::mem::size_of_val(&rw),
                );
                SetPerTcpConnectionEStats(
                    &row,
                    TcpConnectionEstatsFineRtt,
                    rw_bytes,
                    0,
                    0,
                );
                self.enabled = Some(tuple);
            }

            let mut rod = TCP_ESTATS_FINE_RTT_ROD_v0::default();
            let rod_bytes = std::slice::from_raw_parts_mut(
                &mut rod as *mut _ as *mut u8,
                std::mem::size_of_val(&rod),
            );
            let status = GetPerTcpConnectionEStats(
                &row,
                TcpConnectionEstatsFineRtt,
                None,
                0,
                None,
                0,
                Some(rod_bytes),
                0,
            );
            if status != 0 {
                return None;
            }
            self.value = rod.SumRtt as f64 / 1000.0;
            Some(self.value)
        }
    }
}

/// PID'ye ait, uzak portu oyun portu olan ESTABLISHED IPv4 bağlantısını
/// TCP tablosundan bulur → (yerel addr, yerel port, uzak addr, uzak port)
/// — alanlar tablodaki ağ bayt sırasıyla aynen döner (EStats bu formu ister).
fn find_game_connection(pid: u32) -> Option<[u32; 4]> {
    unsafe {
        let mut size: u32 = 16 * 1024;
        let mut buf = vec![0u8; size as usize];
        loop {
            let status = GetExtendedTcpTable(
                Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
                &mut size,
                false,
                2, // AF_INET
                TCP_TABLE_OWNER_PID_ALL,
                0,
            );
            if status == 0 {
                break;
            }
            if status == windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER.0 {
                buf = vec![0u8; size as usize];
                continue;
            }
            return None;
        }

        let entries = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
        let row_size = std::mem::size_of::<MIB_TCPROW_OWNER_PID>();
        for i in 0..entries {
            let o = 4 + i * row_size;
            if o + row_size > buf.len() {
                break;
            }
            let read_u32 = |off: usize| u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
            let state = read_u32(o);
            let laddr = read_u32(o + 4);
            let lport = read_u32(o + 8);
            let raddr = read_u32(o + 12);
            let rport = read_u32(o + 16);
            let owner = read_u32(o + 20);

            if owner == pid
                && state == MIB_TCP_STATE_ESTAB.0 as u32
                && ntohs16(rport) == GAME_REMOTE_PORT
            {
                return Some([laddr, lport, raddr, rport]);
            }
        }
        None
    }
}

/// Tablodaki port alanları düşük 16 bitte ağ bayt sırasındadır.
fn ntohs16(v: u32) -> u16 {
    let lo = (v & 0xffff) as u16;
    lo.swap_bytes()
}
