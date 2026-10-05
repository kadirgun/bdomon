//! RTSS (RivaTuner Statistics Server) OSD istemcisi.
//!
//! `RTSSSharedMemoryV2` paylaşımlı bellek eşlemesi üzerinden RTSS OSD'sine
//! hypertext yazar. Referans uygulama: RTSS SDK `RTSSSharedMemorySample` ve
//! OverlayEditor eklentisi. Minimum gereksinim: RTSS >= 7.3.4 (shared memory
//! v2.20 — `szOSDEx2` slotu).

use std::fmt;
use std::ptr;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
use windows::Win32::System::Memory::{
    MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_ALL_ACCESS,
    MEMORY_MAPPED_VIEW_ADDRESS,
};

/// RTSS ile paylaşılan bellek eşlemesinin adı (tüm bitness'lerde aynı):
/// `RTSSSharedMemoryV2` — aşağıda `w!()` literal olarak kullanılır.
/// 'RTSS' imzası — belleğin geçerli veri içerdiğini doğrular.
const SIGNATURE: u32 = 0x5254_5353;
/// v2.20 => RTSS >= 7.3.4 (szOSDEx2). Daha eskide 32KB hiper metin slotu yok.
const MIN_VERSION: u32 = 0x0002_0014;
/// dwBusy kilidini en fazla bu kadar bekleriz (çökmüş istemci kilidi tutabilir).
const BUSY_LOCK_TIMEOUT: Duration = Duration::from_millis(100);

/// OSD slot sahiplik kimliği.
pub const OSD_OWNER: &str = "BDOMonHUD";

// RTSS_SHARED_MEMORY başlık alanları (RTSSSharedMemory.h v2.x).
#[repr(C)]
struct Header {
    dw_signature: u32,          // 0x00
    dw_version: u32,            // 0x04 (major<<16 | minor)
    dw_app_entry_size: u32,     // 0x08
    dw_app_arr_offset: u32,     // 0x0C
    dw_app_arr_size: u32,       // 0x10
    dw_osd_entry_size: u32,     // 0x14
    dw_osd_arr_offset: u32,     // 0x18
    dw_osd_arr_size: u32,       // 0x1C
    dw_osd_frame: u32,          // 0x20 — artırınca tüm 3D uygulamalarda OSD yenilenir
    dw_busy: i32,               // 0x24 — bit 0: renderer yazıyor (v2.14+)
}

// RTSS_SHARED_MEMORY_OSD_ENTRY (v2.20+).
#[repr(C)]
struct OsdEntry {
    sz_osd: [u8; 256],          // 0x00000
    sz_osd_owner: [u8; 256],    // 0x00100
    sz_osd_ex: [u8; 4096],      // 0x00200
    buffer: [u8; 262_144],      // 0x01200
    sz_osd_ex2: [u8; 32_768],   // 0x41200 — 32KB hiper metin slotu
}

const OFF_OWNER: usize = 0x100;
const OFF_OSD_EX2: usize = 0x41_200;

#[derive(Debug)]
pub enum RtssError {
    /// RTSS çalışmıyor (eşleme açılamadı).
    NotRunning,
    /// Bellek imzası 'RTSS' değil.
    InvalidSignature(u32),
    /// Shared memory sürümü çok eski (7.3.4+ gerekli).
    VersionTooOld(u32),
    /// OSD slot yapısı beklenenden küçük (szOSDEx2 güvenli değil).
    EntryTooSmall { expected: u32, actual: u32 },
    /// Boş slot yok.
    NoFreeSlot,
    /// Hiper metin 32KB slotuna sığmıyor.
    TextTooLong(usize),
    /// Win32 hatası.
    Win32(u32),
}

impl fmt::Display for RtssError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RtssError::NotRunning => write!(
                f,
                "RTSS is not running. Start RivaTuner Statistics Server (>= 7.3.4)."
            ),
            RtssError::InvalidSignature(v) => {
                write!(f, "Invalid RTSS shared memory signature (0x{v:08X}).")
            }
            RtssError::VersionTooOld(v) => write!(
                f,
                "RTSS is too old: shared memory v{}.{}. Minimum v2.20 (RTSS 7.3.4+) is required.",
                v >> 16,
                v & 0xFFFF
            ),
            RtssError::EntryTooSmall { expected, actual } => write!(
                f,
                "OSD slot structure is smaller than expected ({actual} < {expected} bytes) — RTSS version is not supported."
            ),
            RtssError::NoFreeSlot => write!(f, "No free OSD slot found."),
            RtssError::TextTooLong(n) => {
                write!(f, "Hypertext is too long ({n} bytes; max 32767).")
            }
            RtssError::Win32(code) => write!(f, "Win32 error: {code}."),
        }
    }
}

pub struct RtssClient {
    map_addr: *mut u8,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    handle: HANDLE,
    slots: Vec<usize>,
    version: u32,
    entry_size: usize,
    arr_size: usize,
}

/// Hook edilmiş bir uygulamanın özeti.
pub struct AppInfo {
    pub pid: u32,
    pub name: String,
    pub flags: u32,
    pub framerate: f64,
}

unsafe impl Send for RtssClient {}

impl RtssClient {
    /// RTSS'e bağlanır, sürümü doğrular ve `count` adet OSD slotu sahiplenir.
    ///
    /// Çoklu slot gerekli: RTSS hiper metin ayrıştırıcısı `%...%` çiftlerini
    /// veri kaynağı placeholder'ı olarak yorumluyor; bir string'de ikinci bir
    /// `%` (örn. "80% <...> 80%") tüm OSD metnini çökertiyor. Her değeri ayrı
    /// slota koyarak bunu aşarız (slotlar aynı anda render olur).
    pub fn connect_with_slots(count: usize) -> Result<Self, RtssError> {
        let handle = unsafe {
            OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, w!("RTSSSharedMemoryV2")).map_err(|e| {
                let code = (e.code().0 & 0xFFFF) as u32;
                if code == 2 {
                    // ERROR_FILE_NOT_FOUND
                    RtssError::NotRunning
                } else {
                    RtssError::Win32(code)
                }
            })?
        };

        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0) };
        if view.Value.is_null() {
            let code = unsafe { GetLastError().0 };
            return Err(RtssError::Win32(code));
        }
        let map_addr = view.Value as *mut u8;

        let header = unsafe { &*(map_addr as *const Header) };
        let signature = unsafe { ptr::read_volatile(&header.dw_signature) };
        let version = unsafe { ptr::read_volatile(&header.dw_version) };
        let entry_size = unsafe { ptr::read_volatile(&header.dw_osd_entry_size) } as usize;
        let arr_size = unsafe { ptr::read_volatile(&header.dw_osd_arr_size) } as usize;

        if signature != SIGNATURE {
            return Err(RtssError::InvalidSignature(signature));
        }
        if version < MIN_VERSION {
            return Err(RtssError::VersionTooOld(version));
        }
        let expected = std::mem::size_of::<OsdEntry>();
        if entry_size < expected {
            return Err(RtssError::EntryTooSmall {
                expected: expected as u32,
                actual: entry_size as u32,
            });
        }

        let mut client = RtssClient {
            map_addr,
            view,
            handle,
            slots: Vec::new(),
            version,
            entry_size,
            arr_size,
        };
        client.claim_slots(count)?;
        Ok(client)
    }

    /// Tek slotlu kullanım için kısayol.
    pub fn connect() -> Result<Self, RtssError> {
        Self::connect_with_slots(1)
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn slots(&self) -> &[usize] {
        &self.slots
    }

    /// `count` adet slot sahiplenir. Her adımda `self.slots` güncellenir ki
    /// `claim_one` az önce sahiplenilen slotu tekrar seçmesin.
    fn claim_slots(&mut self, count: usize) -> Result<(), RtssError> {
        for _ in 0..count {
            let idx = self.claim_one()?;
            self.slots.push(idx);
        }
        Ok(())
    }

    /// Sahiplenilmiş slotu bulur (1. geçiş) veya boş slotu sahiplenir (2. geçiş).
    /// Slot 0 birincil OSD istemcilerine (Afterburner vb.) ayrılmıştır; 1'den başlanır.
    fn claim_one(&self) -> Result<usize, RtssError> {
        for pass in 0..2 {
            for idx in 1..self.arr_size {
                let entry = self.entry_ptr(idx);
                let owner = read_cstr(unsafe { entry.add(OFF_OWNER) }, 256);
                match pass {
                    0 if owner == OSD_OWNER && !self.slots.contains(&idx) => return Ok(idx),
                    1 if owner.is_empty() => {
                        write_cstr(unsafe { entry.add(OFF_OWNER) }, OSD_OWNER.as_bytes());
                        return Ok(idx);
                    }
                    _ => {}
                }
            }
        }
        Err(RtssError::NoFreeSlot)
    }

    /// Hiper metni belirtilen sahipli slota yazar ve OSD yenilemesini tetikler.
    pub fn update(&mut self, slot_idx: usize, hypertext: &str) -> Result<(), RtssError> {
        if !self.slots.contains(&slot_idx) {
            return Err(RtssError::NoFreeSlot);
        }
        let text = hypertext.as_bytes();
        if text.len() >= 32_768 {
            return Err(RtssError::TextTooLong(text.len()));
        }

        // dwBusy bit 0 kilidi (v2.14+; biz zaten 7.3.4+ istiyoruz).
        let busy = self.busy_atomic();
        let deadline = Instant::now() + BUSY_LOCK_TIMEOUT;
        loop {
            let prev = busy.fetch_or(1, Ordering::SeqCst);
            if prev & 1 == 0 {
                break; // kilit bizim
            }
            if Instant::now() >= deadline {
                // Çökmüş bir istemci OSD'yi kilitli bırakmasın: zorla bırak ve al.
                busy.fetch_and(!1, Ordering::SeqCst);
                busy.fetch_or(1, Ordering::SeqCst);
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }

        unsafe {
            let dst = self.entry_ptr(slot_idx).add(OFF_OSD_EX2) as *mut u8;
            ptr::write_bytes(dst, 0, 32_768);
            ptr::copy_nonoverlapping(text.as_ptr(), dst, text.len());
        }

        busy.store(0, Ordering::SeqCst);
        self.frame_atomic().fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    /// Sahipli tüm slotları temizler ve OSD'den kaldırır.
    pub fn release(&mut self) {
        let slots = std::mem::take(&mut self.slots);
        if !slots.is_empty() {
            for slot in slots {
                unsafe {
                    ptr::write_bytes(self.entry_ptr(slot), 0, self.entry_size);
                }
            }
            self.frame_atomic().fetch_add(1, Ordering::SeqCst);
        }
    }

    fn entry_ptr(&self, idx: usize) -> *mut u8 {
        unsafe {
            let header = &*(self.map_addr as *const Header);
            let arr_offset = ptr::read_volatile(&header.dw_osd_arr_offset) as usize;
            self.map_addr.add(arr_offset + idx * self.entry_size)
        }
    }

    /// Teşhis: sahipli bir slotun güncel hiper metnini döndürür.
    pub fn dump_slot(&self, idx: usize) -> String {
        read_cstr(unsafe { self.entry_ptr(idx).add(OFF_OSD_EX2) }, 32_768).to_string()
    }

    /// RTSS'ten, belirtilen çalışan uygulama(lar) için profil dosyasını
    /// yeniden yüklemesini ister (oyunu yeniden başlatmadan).
    ///
    /// Mekanizma: `APPFLAG_PROFILE_UPDATE_REQUESTED (0x10000000)` bayrağı
    /// uygulama girişinin `dwFlags` alanına yazılır; RTSS bayrağı görünce
    /// `Profiles\<exe>.cfg` dosyasını yeniden okur ve uygular, ardından
    /// bayrağı temizler. SDK'daki HotkeyHandler eklentisi aynı işi
    /// RTSSHooks.dll'in `UpdateProfiles` export'uyla yapar.
    ///
    /// `exe_name`: eşleşecek yürütülebilir adı (örn. "BlackDesert64.exe").
    /// Dönüş: bayrak yazılan uygulama sayısı.
    pub fn request_profile_update(&self, exe_name: &str) -> usize {
        unsafe {
            let header = &*(self.map_addr as *const Header);
            let app_entry_size = ptr::read_volatile(&header.dw_app_entry_size) as usize;
            let app_arr_offset = ptr::read_volatile(&header.dw_app_arr_offset) as usize;
            let app_arr_size = ptr::read_volatile(&header.dw_app_arr_size) as usize;

            let needle = exe_name.to_lowercase();
            let mut count = 0;

            for idx in 0..app_arr_size {
                let entry = self.map_addr.add(app_arr_offset + idx * app_entry_size);
                let pid = ptr::read_volatile(entry as *const u32);
                if pid == 0 {
                    continue;
                }
                let name = read_cstr(entry.add(0x04), 260).to_lowercase();
                if name.ends_with(&needle) {
                    let flags = &*(entry.add(0x108) as *const AtomicU32);
                    flags.fetch_or(0x1000_0000, Ordering::SeqCst);
                    count += 1;
                }
            }

            // İsteği RTSS'in fark etmesi için OSD yenilemesi de tetiklenir.
            self.frame_atomic().fetch_add(1, Ordering::SeqCst);
            count
        }
    }

    /// Hook edilmiş uygulama listesi (teşhis amaçlı).
    pub fn list_apps(&self) -> Vec<AppInfo> {
        unsafe {
            let header = &*(self.map_addr as *const Header);
            let app_entry_size = ptr::read_volatile(&header.dw_app_entry_size) as usize;
            let app_arr_offset = ptr::read_volatile(&header.dw_app_arr_offset) as usize;
            let app_arr_size = ptr::read_volatile(&header.dw_app_arr_size) as usize;

            let mut apps = Vec::new();
            for idx in 0..app_arr_size {
                let entry = self.map_addr.add(app_arr_offset + idx * app_entry_size);
                let pid = ptr::read_volatile(entry as *const u32);
                if pid == 0 {
                    continue;
                }
                let name = read_cstr(entry.add(0x04), 260).to_string();
                let flags = ptr::read_volatile(entry.add(0x108) as *const u32);
                let frame_time = ptr::read_volatile(entry.add(0x118) as *const u32);
                let framerate = if frame_time != 0 {
                    1_000_000.0 / frame_time as f64
                } else {
                    0.0
                };
                apps.push(AppInfo {
                    pid,
                    name,
                    flags,
                    framerate,
                });
            }
            apps
        }
    }

    fn busy_atomic(&self) -> &AtomicI32 {
        unsafe { &*(self.map_addr.add(0x24) as *const AtomicI32) }
    }

    fn frame_atomic(&self) -> &AtomicU32 {
        unsafe { &*(self.map_addr.add(0x20) as *const AtomicU32) }
    }
}

impl Drop for RtssClient {
    fn drop(&mut self) {
        // Not: slot bilinçli olarak bırakılmaz — overlay'in sürmesi için
        // sahiplik korunur ve bir sonraki çalıştırmada devralınır.
        unsafe {
            if !self.view.Value.is_null() {
                let _ = UnmapViewOfFile(self.view);
            }
            if !self.handle.is_invalid() {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

fn read_cstr(buf: *const u8, max: usize) -> &'static str {
    unsafe {
        let slice = std::slice::from_raw_parts(buf, max);
        let len = slice.iter().position(|&b| b == 0).unwrap_or(max);
        std::str::from_utf8(&slice[..len]).unwrap_or("")
    }
}

fn write_cstr(dst: *mut u8, bytes: &[u8]) {
    unsafe {
        ptr::write_bytes(dst, 0, 256);
        ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len().min(255));
    }
}
