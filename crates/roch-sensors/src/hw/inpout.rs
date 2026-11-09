//! Port I/O and physical reads through inpoutx64 (Roch Viewer's `lowlevel_io` + `read`).
//!
//! The driver is not distributed with Roch Monitor, for the reasons Roch Viewer gives in its
//! third-party notices: it is a well-known low-level access component and, for the same reason,
//! one attackers have abused, so whether it goes on a machine is the owner's decision. It is looked
//! for beside the executable, where Roch Viewer's README tells people to put it, and then in
//! System32. Never on the DLL search path: the process runs elevated, and a DLL found by searching
//! is a DLL anyone who can write to the current directory chooses.
//!
//! Only primitives live here. Every caller writes nothing but the selectors its read protocol
//! needs (a Super I/O index, an SMBus command byte, a PCI config address); see each transport.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use libloading::Library;

pub const DLL_NAME: &str = "inpoutx64.dll";

type IsOpenFn = unsafe extern "system" fn() -> u32;
type ReadUcharFn = unsafe extern "system" fn(u16) -> u8;
type WriteUcharFn = unsafe extern "system" fn(u16, u8);
type ReadUlongFn = unsafe extern "system" fn(u32) -> u32;
type GetPhysLongFn = unsafe extern "system" fn(*mut u8, *mut u32) -> i32;

/// The loaded driver. Function pointers are copied out of the library, which stays loaded for the
/// life of the process (it lives in a `OnceLock`).
pub struct InpOut {
    _lib: Library,
    path: PathBuf,
    is_open: IsOpenFn,
    read_uchar: ReadUcharFn,
    write_uchar: WriteUcharFn,
    read_ulong: ReadUlongFn,
    get_phys_long: GetPhysLongFn,
}

// The DLL serialises its own IOCTLs; the function pointers are plain code addresses.
unsafe impl Send for InpOut {}
unsafe impl Sync for InpOut {}

static DRIVER: OnceLock<Result<InpOut, String>> = OnceLock::new();

/// The driver, loaded on first use. `Err` says why there is none, in words a person can act on.
pub fn driver() -> Result<&'static InpOut, &'static str> {
    DRIVER.get_or_init(load).as_ref().map_err(String::as_str)
}

/// Every place the DLL is looked for, nearest first.
pub fn search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        paths.push(dir.join(DLL_NAME));
    }
    let windir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows"));
    paths.push(windir.join("System32").join(DLL_NAME));
    paths
}

fn load() -> Result<InpOut, String> {
    let candidates = search_paths();
    let Some(path) = candidates.iter().find(|p| p.is_file()).cloned() else {
        let places: Vec<String> = candidates.iter().map(|p| p.display().to_string()).collect();
        return Err(format!(
            "{DLL_NAME} not found. It is a third-party driver and is not distributed with Roch Monitor; \
             download it from www.highrez.co.uk and put inpoutx64.dll and inpoutx64.sys in one of: {}",
            places.join("; ")
        ));
    };
    // SAFETY: loading the DLL runs its DllMain, which is the point; the path is absolute, so no
    // search order is involved.
    let lib =
        unsafe { Library::new(&path) }.map_err(|e| format!("Found {} but could not load it: {e}", path.display()))?;
    // SAFETY: the signatures are inpout32's documented exports.
    let driver = unsafe {
        let is_open = *lib.get::<IsOpenFn>(b"IsInpOutDriverOpen\0").map_err(|e| e.to_string())?;
        let read_uchar = *lib.get::<ReadUcharFn>(b"DlPortReadPortUchar\0").map_err(|e| e.to_string())?;
        let write_uchar = *lib.get::<WriteUcharFn>(b"DlPortWritePortUchar\0").map_err(|e| e.to_string())?;
        let read_ulong = *lib.get::<ReadUlongFn>(b"DlPortReadPortUlong\0").map_err(|e| e.to_string())?;
        let get_phys_long = *lib.get::<GetPhysLongFn>(b"GetPhysLong\0").map_err(|e| e.to_string())?;
        InpOut { _lib: lib, path: path.clone(), is_open, read_uchar, write_uchar, read_ulong, get_phys_long }
    };
    // SAFETY: no arguments; opens the driver's device on first call.
    if unsafe { (driver.is_open)() } == 0 {
        return Err(format!(
            "Loaded {} but the inpoutx64 driver did not open. Run Roch Monitor as administrator.",
            path.display()
        ));
    }
    Ok(driver)
}

impl InpOut {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn inb(&self, port: u16) -> u8 {
        // SAFETY: a port read through the driver; it cannot fault in this process.
        unsafe { (self.read_uchar)(port) }
    }

    pub fn outb(&self, port: u16, value: u8) {
        // SAFETY: as above. Callers only ever write selector registers.
        unsafe { (self.write_uchar)(port, value) }
    }

    pub fn inl(&self, port: u16) -> u32 {
        // SAFETY: as `inb`.
        unsafe { (self.read_ulong)(port as u32) }
    }

    /// One 32-bit physical read (the DLL maps the page, reads, and unmaps). `None` when the
    /// driver refused, which is different from a register that reads all ones.
    pub fn phys_read32(&self, address: u64) -> Option<u32> {
        let mut value = 0u32;
        // SAFETY: the DLL takes the physical address as a pointer-sized integer and writes one
        // DWORD to `value`.
        let ok = unsafe { (self.get_phys_long)(address as usize as *mut u8, &mut value) };
        (ok != 0).then_some(value)
    }

    /// Two dwords as one little-endian u64, for the fields that straddle a dword boundary.
    pub fn phys_read64(&self, address: u64) -> Option<u64> {
        let low = self.phys_read32(address)? as u64;
        let high = self.phys_read32(address + 4)? as u64;
        Some(low | (high << 32))
    }
}

/// A field out of a register: `width` bits from `start`.
pub fn bits(value: u64, start: u32, width: u32) -> u64 {
    if width >= 64 { value >> start } else { (value >> start) & ((1u64 << width) - 1) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_come_out_of_the_register() {
        // MCHBAR 0x5E04 on the Z790-P bench: ratio 38, 100 MHz reference, Gear 1.
        let raw = 0x79EC_0126u64;
        assert_eq!(bits(raw, 0, 8), 0x26);
        assert_eq!(bits(raw, 8, 4), 1);
        assert_eq!(bits(raw, 12, 2), 0);
        assert_eq!(bits(u64::MAX, 0, 64), u64::MAX);
    }

    #[test]
    fn the_exe_directory_is_searched_before_system32() {
        let paths = search_paths();
        assert!(paths.last().unwrap().ends_with("System32\\inpoutx64.dll"));
    }
}
