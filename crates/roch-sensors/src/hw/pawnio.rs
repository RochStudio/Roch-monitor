//! PawnIO (https://pawnio.eu): the signed, scriptable kernel driver LibreHardwareMonitor moved to,
//! and the driver every low-level transport here tries first.
//!
//! The interface, from PawnIO's own `pawnio_um.h` and `PawnIOLib.cpp`: open `\Device\PawnIO`, load a
//! module blob into the handle with one IOCTL, then call the module's functions by name with another,
//! the input being a 32-byte NUL-padded name followed by u64 arguments and the output u64 values.
//! Each handle carries its own copy of its module and that module's state.
//!
//! Where inpoutx64 hands any administrator process every I/O port and all of physical memory, a
//! PawnIO module exposes only what it was written for, and checks it:
//!
//!   IntelMSR, AMDFamily17  an allow-list of MSRs, read on the calling thread's processor
//!   IntelMCHBAR            dword reads inside the memory controller's window, which it finds itself
//!   LpcIO                  one slot's Super I/O config pair, and the ports of the BARs its logical
//!                          devices decode, found while the chip is in configuration mode
//!   LpcACPIEC              ports 0x62 and 0x66, the ACPI embedded controller
//!   SmbusI801, SmbusPIIX4  SMBus transfers on the chipset's controller, which it finds itself
//!
//! What Roch Monitor calls: reads, and the selector writes each read protocol needs, documented at
//! each transport. The MSR modules also export MSR writes; nothing here names them.
//!
//! An MSR read happens on whichever logical processor the calling thread is running on, so a reader
//! pins its thread to the processor it wants first ([`pin_to`]), as LibreHardwareMonitor does.

pub const INTEL_MSR_MODULE: &[u8] = include_bytes!("../../res/pawnio/IntelMSR.bin");
pub const AMD_FAMILY17_MODULE: &[u8] = include_bytes!("../../res/pawnio/AMDFamily17.bin");
pub const INTEL_MCHBAR_MODULE: &[u8] = include_bytes!("../../res/pawnio/IntelMCHBAR.bin");
pub const LPC_IO_MODULE: &[u8] = include_bytes!("../../res/pawnio/LpcIO.bin");
pub const LPC_ACPI_EC_MODULE: &[u8] = include_bytes!("../../res/pawnio/LpcACPIEC.bin");
pub const SMBUS_I801_MODULE: &[u8] = include_bytes!("../../res/pawnio/SmbusI801.bin");
pub const SMBUS_PIIX4_MODULE: &[u8] = include_bytes!("../../res/pawnio/SmbusPIIX4.bin");

/// `CTL_CODE(41394, function, METHOD_BUFFERED, FILE_ANY_ACCESS)`.
const fn ctl_code(function: u32) -> u32 {
    (41394 << 16) | (function << 2)
}
pub const IOCTL_LOAD_BINARY: u32 = ctl_code(0x821);
pub const IOCTL_EXECUTE: u32 = ctl_code(0x841);
const NAME_BYTES: usize = 32;

/// Linux's `i2c_smbus_xfer` protocol numbers, which the SMBus modules take.
pub const SMBUS_BYTE_DATA: u64 = 2;
pub const SMBUS_WORD_DATA: u64 = 3;
const SMBUS_READ: u64 = 1;
const SMBUS_WRITE: u64 = 0;

/// The execute IOCTL's input: the function name in 32 NUL-padded bytes, then the arguments.
pub fn execute_input(name: &str, args: &[u64]) -> Vec<u8> {
    let mut buf = vec![0u8; NAME_BYTES + 8 * args.len()];
    let bytes = name.as_bytes();
    let n = bytes.len().min(NAME_BYTES - 1);
    buf[..n].copy_from_slice(&bytes[..n]);
    for (i, a) in args.iter().enumerate() {
        buf[NAME_BYTES + 8 * i..NAME_BYTES + 8 * i + 8].copy_from_slice(&a.to_le_bytes());
    }
    buf
}

/// Why the device would not open, in words a person can act on. No trailing full stop: callers
/// put it inside their own sentence.
pub fn open_error(err: u32) -> String {
    match err {
        2 | 3 => "PawnIO is not installed (pawnio.eu)".into(),
        5 => "PawnIO refused this process; run Roch Monitor as administrator".into(),
        e => format!("PawnIO could not be opened (error {e})"),
    }
}

/// Why a module would not load. A module's own start-up check failing (ERROR_NOT_SUPPORTED) means
/// it found nothing it drives on this machine.
pub fn load_error(module: &str, err: u32) -> String {
    match err {
        50 => format!("PawnIO's {module} module found nothing it supports on this machine"),
        e => format!("PawnIO refused its {module} module (error {e}); the driver may be older than the module"),
    }
}

/// Where the PawnIO installer puts its own copies of the signed modules: `modules` under the
/// install folder in Program Files. Only that folder is searched, never one a user can write to;
/// the driver checks each module's signature as it loads it either way.
pub fn installed_module_path(name: &str, program_files: Option<&std::ffi::OsStr>) -> Option<std::path::PathBuf> {
    let root = std::path::PathBuf::from(program_files?);
    root.is_absolute().then(|| root.join("PawnIO").join("modules").join(format!("{name}.bin")))
}

/// A module the PawnIO installation carries, for one this crate does not bundle (RyzenSMU).
pub fn installed_module(name: &str) -> Result<Vec<u8>, String> {
    let program_files = std::env::var_os("ProgramW6432").or_else(|| std::env::var_os("ProgramFiles"));
    let path = installed_module_path(name, program_files.as_deref())
        .ok_or_else(|| "the Program Files folder could not be found".to_owned())?;
    std::fs::read(&path).map_err(|_| {
        format!("PawnIO's {name} module is not in {}; this PawnIO installation does not carry it", path.display())
    })
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::IO::DeviceIoControl;

    fn open_device() -> Result<HANDLE, u32> {
        let path: Vec<u16> = r"\\?\GLOBALROOT\Device\PawnIO".encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: a plain CreateFileW on a device path.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            // SAFETY: no arguments.
            return Err(unsafe { GetLastError() });
        }
        Ok(handle)
    }

    /// Whether the driver is there and lets this process in, without loading anything.
    pub fn available() -> Result<(), String> {
        let handle = open_device().map_err(open_error)?;
        // SAFETY: the handle was just opened here.
        unsafe {
            CloseHandle(handle);
        }
        Ok(())
    }

    /// One open handle with one module loaded into it.
    pub struct PawnIo {
        handle: HANDLE,
    }

    // A handle is used from the one source thread that owns it.
    unsafe impl Send for PawnIo {}

    impl PawnIo {
        /// Open the driver and load `module`, called `name` in any error.
        pub fn open(name: &str, module: &[u8]) -> Result<PawnIo, String> {
            let io = PawnIo { handle: open_device().map_err(open_error)? };
            let mut returned = 0u32;
            // SAFETY: the blob outlives the call; no output buffer.
            let ok = unsafe {
                DeviceIoControl(
                    io.handle,
                    IOCTL_LOAD_BINARY,
                    module.as_ptr().cast(),
                    module.len() as u32,
                    std::ptr::null_mut(),
                    0,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                // SAFETY: no arguments.
                return Err(load_error(name, unsafe { GetLastError() }));
            }
            Ok(io)
        }

        /// Call one of the module's functions; up to `out` values come back. `Err` is the Win32
        /// error the module's NTSTATUS became.
        pub fn execute(&self, name: &str, args: &[u64], out: usize) -> Result<Vec<u64>, u32> {
            let input = execute_input(name, args);
            let mut output = vec![0u64; out.max(1)];
            let mut returned = 0u32;
            // SAFETY: both buffers are ours and sized as passed.
            let ok = unsafe {
                DeviceIoControl(
                    self.handle,
                    IOCTL_EXECUTE,
                    input.as_ptr().cast(),
                    input.len() as u32,
                    output.as_mut_ptr().cast(),
                    (output.len() * 8) as u32,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                // SAFETY: no arguments.
                return Err(unsafe { GetLastError() });
            }
            output.truncate((returned as usize / 8).min(out));
            Ok(output)
        }

        /// One value back from a function that returns one.
        fn value(&self, name: &str, args: &[u64]) -> Result<u64, u32> {
            self.execute(name, args, 1)?.first().copied().ok_or(0)
        }

        /// Read one MSR on the processor this thread is running on.
        pub fn read_msr(&self, msr: u32) -> Option<u64> {
            self.value("ioctl_read_msr", &[msr as u64]).ok()
        }
    }

    impl Drop for PawnIo {
        fn drop(&mut self) {
            // SAFETY: our own handle.
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }

    /// A module that does byte port I/O by name: LpcIO, for one Super I/O slot and the BARs behind
    /// it, or LpcACPIEC, for the ACPI EC's two ports. A port the module does not admit is an error,
    /// never a quiet 0xFF.
    pub struct PortModule {
        io: PawnIo,
        read: &'static str,
        write: &'static str,
        pub module: &'static str,
    }

    impl PortModule {
        /// LpcIO with the slot of `config_port` selected: 0x2E is slot 0, 0x4E slot 1.
        pub fn lpc(config_port: u16) -> Result<PortModule, String> {
            let slot = match config_port {
                0x2E => 0,
                0x4E => 1,
                p => return Err(format!("LpcIO has no slot for config port 0x{p:02X}")),
            };
            let io = PawnIo::open("LpcIO", LPC_IO_MODULE)?;
            io.execute("ioctl_select_slot", &[slot], 0)
                .map_err(|e| format!("PawnIO's LpcIO module refused slot {slot} (error {e})"))?;
            Ok(PortModule { io, read: "ioctl_pio_inb", write: "ioctl_pio_outb", module: "LpcIO" })
        }

        /// LpcACPIEC: ports 0x62 and 0x66 only.
        pub fn acpi_ec() -> Result<PortModule, String> {
            let io = PawnIo::open("LpcACPIEC", LPC_ACPI_EC_MODULE)?;
            Ok(PortModule { io, read: "ioctl_pio_read", write: "ioctl_pio_write", module: "LpcACPIEC" })
        }

        /// LpcIO only: with the chip in configuration mode and identified, admit the 8-port range
        /// of every base address its logical devices decode. The module selects each logical
        /// device in turn to read its bases, a selector write and nothing else.
        pub fn find_bars(&self) -> bool {
            self.module == "LpcIO" && self.io.execute("ioctl_find_bars", &[], 0).is_ok()
        }

        pub fn inb(&self, port: u16) -> Option<u8> {
            self.io.value(self.read, &[port as u64]).ok().map(|v| v as u8)
        }

        pub fn outb(&self, port: u16, value: u8) -> Option<()> {
            self.io.execute(self.write, &[port as u64, value as u64], 0).ok().map(|_| ())
        }
    }

    /// IntelMCHBAR: dword reads inside the memory controller's register window, which the module
    /// locates from the host bridge and maps itself. It loads only on the CPUs in its own table.
    pub struct Mchbar {
        io: PawnIo,
    }

    impl Mchbar {
        pub fn open() -> Result<Mchbar, String> {
            Ok(Mchbar { io: PawnIo::open("IntelMCHBAR", INTEL_MCHBAR_MODULE)? })
        }

        /// The physical base the module mapped.
        pub fn base(&self) -> Option<u64> {
            self.io.value("ioctl_get_mchbar_addr", &[]).ok()
        }

        /// One dword at `offset` into the window; offsets must be dword-aligned.
        pub fn read32(&self, offset: u64) -> Option<u32> {
            self.io.value("ioctl_read_dword", &[offset]).ok().map(|v| v as u32)
        }
    }

    /// SmbusI801 (Intel) or SmbusPIIX4 (AMD): SMBus transfers in Linux's `i2c_smbus_xfer` terms on
    /// the controller the module found. The I801 module also waits out the firmware's hold on the
    /// controller (INUSE_STS) before a transfer, and, if the firmware left the controller's memory
    /// decoding off, turns it on for the transfer and back off after.
    pub struct SmbusModule {
        io: PawnIo,
        pub module: &'static str,
        /// PIIX4: the port last selected (0 the main controller, 1 the auxiliary one).
        port: std::cell::Cell<Option<u64>>,
    }

    impl SmbusModule {
        pub fn i801() -> Result<SmbusModule, String> {
            Ok(SmbusModule {
                io: PawnIo::open("SmbusI801", SMBUS_I801_MODULE)?,
                module: "SmbusI801",
                port: None.into(),
            })
        }
    }

    /// PawnIO's RyzenSMU module: the SMU's PM table on Ryzen. The module runs the RSMU mailbox
    /// itself (version, table address, transfer) and copies the table out of DRAM, so no physical
    /// address ever passes through this process. Only its four read functions are named here; it
    /// also exports a raw SMU command and register writes, which nothing in Roch Monitor calls.
    pub struct RyzenSmu {
        io: PawnIo,
    }

    impl RyzenSmu {
        pub fn open() -> Result<RyzenSmu, String> {
            let blob = super::installed_module("RyzenSMU")?;
            Ok(RyzenSmu { io: PawnIo::open("RyzenSMU", &blob)? })
        }

        /// The module's name for this processor's code name (17 on a Granite Ridge).
        pub fn code_name(&self) -> Option<u64> {
            self.io.value("ioctl_get_code_name", &[]).ok()
        }

        /// (table version, table base), from the SMU's own answers.
        pub fn resolve_pm_table(&self) -> Result<(u32, u64), u32> {
            let v = self.io.execute("ioctl_resolve_pm_table", &[], 2)?;
            match v.as_slice() {
                [version, base, ..] => Ok((*version as u32, *base)),
                _ => Err(0),
            }
        }

        /// Ask the SMU to refresh the table in DRAM.
        pub fn update_pm_table(&self) -> Result<(), u32> {
            self.io.execute("ioctl_update_pm_table", &[], 0).map(|_| ())
        }

        /// The table's first `bytes` bytes, as little-endian qwords.
        pub fn read_pm_table(&self, bytes: usize) -> Result<Vec<u64>, u32> {
            self.io.execute("ioctl_read_pm_table", &[], bytes.div_ceil(8))
        }
    }

    impl SmbusModule {
        pub fn piix4() -> Result<SmbusModule, String> {
            let io = PawnIo::open("SmbusPIIX4", SMBUS_PIIX4_MODULE)?;
            // -1 asks which port is selected without changing it.
            let port = io.value("ioctl_piix4_port_sel", &[u64::MAX]).ok();
            Ok(SmbusModule { io, module: "SmbusPIIX4", port: port.into() })
        }

        /// The controller's I/O base, for the status line: (kind, base, PCI ids) from the module.
        pub fn io_base(&self) -> Option<u16> {
            let v = self.io.execute("ioctl_identity", &[], 3).ok()?;
            v.get(1).map(|&b| b as u16).filter(|&b| b != 0)
        }

        /// PIIX4: address the main controller (offset 0) or the auxiliary one (offset 0x20), the
        /// two Roch Viewer's FCH path reads. The I801 has one.
        fn select(&self, controller: u16) -> Result<(), u32> {
            if self.module != "SmbusPIIX4" {
                return if controller == 0 { Ok(()) } else { Err(0) };
            }
            let wanted = if controller == 0 { 0 } else { 1 };
            if self.port.get() != Some(wanted) {
                self.io.value("ioctl_piix4_port_sel", &[wanted])?;
                self.port.set(Some(wanted));
            }
            Ok(())
        }

        pub fn read(&self, controller: u16, address: u8, command: u8, protocol: u64) -> Result<u64, u32> {
            self.select(controller)?;
            self.io.value("ioctl_smbus_xfer", &[address as u64, SMBUS_READ, command as u64, protocol])
        }

        /// Byte Data write. Its one caller is the PMIC ADC selector (see `smbus::is_writable`).
        pub fn write_byte(&self, controller: u16, address: u8, command: u8, value: u8) -> Result<(), u32> {
            self.select(controller)?;
            self.io
                .execute(
                    "ioctl_smbus_xfer",
                    &[address as u64, SMBUS_WRITE, command as u64, SMBUS_BYTE_DATA, value as u64],
                    0,
                )
                .map(|_| ())
        }
    }

    /// Pin the calling thread to one logical processor. The returned guard puts the thread's
    /// previous affinity back when dropped.
    pub fn pin_to(group: u16, number: u8) -> Option<Pinned> {
        use windows_sys::Win32::System::SystemInformation::GROUP_AFFINITY;
        use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadGroupAffinity};
        let wanted = GROUP_AFFINITY { Mask: 1usize << number, Group: group, Reserved: [0; 3] };
        let mut previous = GROUP_AFFINITY { Mask: 0, Group: 0, Reserved: [0; 3] };
        // SAFETY: the pseudo-handle of this thread, and two plain structs.
        let ok = unsafe { SetThreadGroupAffinity(GetCurrentThread(), &wanted, &mut previous) };
        if ok == 0 {
            return None;
        }
        // Let the scheduler move the thread there before the read runs.
        std::thread::yield_now();
        Some(Pinned { previous })
    }

    pub struct Pinned {
        previous: windows_sys::Win32::System::SystemInformation::GROUP_AFFINITY,
    }

    impl Drop for Pinned {
        fn drop(&mut self) {
            use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadGroupAffinity};
            // SAFETY: as in pin_to.
            unsafe {
                SetThreadGroupAffinity(GetCurrentThread(), &self.previous, std::ptr::null_mut());
            }
        }
    }

    /// Raise the calling thread's priority until the guard drops. A thread pinned to a busy core
    /// otherwise waits out the running thread's time slice before its read: on the i9-14900KF a
    /// 16-thread sweep with a median of 0.28 ms took 74 ms now and then, and over 60 sweeps the
    /// worst was 7.0 ms at normal priority against 0.9 ms raised. Each read is microseconds, so
    /// taking the core ahead of normal-priority work costs that work nothing it would notice.
    pub fn raise_priority() -> Raised {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, GetThreadPriority, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
        };
        // SAFETY: the pseudo-handle of this thread.
        unsafe {
            let previous = GetThreadPriority(GetCurrentThread());
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
            Raised { previous }
        }
    }

    pub struct Raised {
        previous: i32,
    }

    impl Drop for Raised {
        fn drop(&mut self) {
            use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_NORMAL};
            // What GetThreadPriority returns when it fails (winbase.h).
            const THREAD_PRIORITY_ERROR_RETURN: i32 = 0x7FFF_FFFF;
            let previous =
                if self.previous == THREAD_PRIORITY_ERROR_RETURN { THREAD_PRIORITY_NORMAL } else { self.previous };
            // SAFETY: as in raise_priority.
            unsafe {
                SetThreadPriority(GetCurrentThread(), previous);
            }
        }
    }
}

#[cfg(windows)]
pub use imp::{Mchbar, PawnIo, Pinned, PortModule, Raised, RyzenSmu, SmbusModule, available, pin_to, raise_priority};

#[cfg(not(windows))]
pub fn available() -> Result<(), String> {
    Err("PawnIO is a Windows driver".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn an_installed_module_is_looked_for_in_program_files_only() {
        use std::ffi::OsStr;
        let path = installed_module_path("RyzenSMU", Some(OsStr::new(r"C:\Program Files"))).unwrap();
        assert_eq!(path, std::path::Path::new(r"C:\Program Files\PawnIO\modules\RyzenSMU.bin"));
        assert!(installed_module_path("RyzenSMU", None).is_none());
        // A relative "Program Files" would resolve against the working directory.
        assert!(installed_module_path("RyzenSMU", Some(OsStr::new("Program Files"))).is_none());
    }

    #[test]
    fn the_ioctls_are_pawnios_own() {
        // CTL_CODE(41394, 0x821/0x841, METHOD_BUFFERED, FILE_ANY_ACCESS), from pawnio_um.h.
        assert_eq!(IOCTL_LOAD_BINARY, 0xA1B2_2084);
        assert_eq!(IOCTL_EXECUTE, 0xA1B2_2104);
    }

    #[test]
    fn execute_input_is_a_padded_name_then_the_arguments() {
        let buf = execute_input("ioctl_read_msr", &[0x19C]);
        assert_eq!(buf.len(), 40);
        assert_eq!(&buf[..14], b"ioctl_read_msr");
        assert!(buf[14..32].iter().all(|&b| b == 0));
        assert_eq!(u64::from_le_bytes(buf[32..40].try_into().unwrap()), 0x19C);
        // A name longer than the field keeps its terminating NUL.
        let long = execute_input(&"x".repeat(40), &[]);
        assert_eq!(long[31], 0);
        // An SMBus write: address, direction, command, protocol, data.
        let xfer = execute_input("ioctl_smbus_xfer", &[0x48, SMBUS_WRITE, 0x30, SMBUS_BYTE_DATA, 0x80]);
        assert_eq!(xfer.len(), 32 + 5 * 8);
        assert_eq!(xfer[32 + 3 * 8], 2);
    }

    #[test]
    fn the_modules_are_the_signed_release_files() {
        // Sizes of the 0.2.11 release; a rebuilt module would not carry the author's signature.
        assert_eq!(INTEL_MSR_MODULE.len(), 5324);
        assert_eq!(AMD_FAMILY17_MODULE.len(), 10652);
        assert_eq!(INTEL_MCHBAR_MODULE.len(), 13332);
        assert_eq!(LPC_IO_MODULE.len(), 18076);
        assert_eq!(LPC_ACPI_EC_MODULE.len(), 2612);
        assert_eq!(SMBUS_I801_MODULE.len(), 55540);
        assert_eq!(SMBUS_PIIX4_MODULE.len(), 42404);
    }

    #[test]
    fn errors_say_what_to_do() {
        assert!(open_error(2).contains("not installed"));
        assert!(open_error(5).contains("administrator"));
        assert!(load_error("IntelMCHBAR", 50).contains("nothing it supports"));
    }
}
