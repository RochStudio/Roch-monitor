//! PCI configuration space through the firmware's ECAM window (Roch Viewer's `pci_mcfg`), and
//! AMD's System Management Network behind it (Roch Viewer's `smn` and `smn_mcfg`).
//!
//! Not the legacy CF8/CFC ports: measured on the Z790 target, they return 0xFFFFFFFF for every
//! device including the 00:00.0 host bridge, which always exists. The MCFG table says where the
//! memory-mapped window is, and a configuration read is then a physical read.
//!
//! Configuration space is only ever read, with one exception, which is how SMN works: AMD's root
//! complex (00:00.0) shows the SMN register whose address is in its index register (0x60) through
//! its data register (0x64), so an SMN read takes one selector write. It is made under the PCI
//! mutex, only after 00:00.0 has answered as AMD's, and the index the previous owner left there is
//! put back before the mutex is released, when the read fails as well as when it succeeds.
//!
//! The SMU's RSMU mailbox sits on SMN too, and the PM-table reader drives it through the same
//! window. Its response, argument and message registers are the only SMN addresses anything here
//! writes: [`Smn`] has no general write, and the message register takes only the four
//! [`RsmuCommand`]s Roch Viewer sends, all of them reads. The physical write underneath is closed
//! over 00:00.0's index and data registers; no function in this crate takes an address to write.

use std::sync::OnceLock;

use libloading::Library;

use super::inpout::InpOut;
use super::{firmware, inpout, mutex};

/// One MCFG allocation: the window for a range of buses on one segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ecam {
    pub base: u64,
    pub segment: u16,
    pub start_bus: u8,
    pub end_bus: u8,
}

const ACPI_HEADER: usize = 36;
const MCFG_RESERVED: usize = 8;
const ENTRY: usize = 16;

/// Parse an MCFG table into its allocations. Entries that cannot be a window (zero base, a base
/// not 1 MiB aligned, a bus range backwards) are dropped rather than trusted.
pub fn parse_mcfg(table: &[u8]) -> Vec<Ecam> {
    if table.len() < ACPI_HEADER + MCFG_RESERVED || &table[..4] != b"MCFG" {
        return Vec::new();
    }
    let declared = u32::from_le_bytes([table[4], table[5], table[6], table[7]]) as usize;
    let end = declared.min(table.len());
    let mut out = Vec::new();
    let mut at = ACPI_HEADER + MCFG_RESERVED;
    while at + ENTRY <= end {
        let base = u64::from_le_bytes(table[at..at + 8].try_into().unwrap_or_default());
        let segment = u16::from_le_bytes([table[at + 8], table[at + 9]]);
        let (start_bus, end_bus) = (table[at + 10], table[at + 11]);
        if base != 0 && base & 0xF_FFFF == 0 && start_bus <= end_bus {
            out.push(Ecam { base, segment, start_bus, end_bus });
        }
        at += ENTRY;
    }
    out
}

impl Ecam {
    /// Physical address of one configuration register.
    pub fn address(&self, bus: u8, device: u8, function: u8, register: u16) -> Option<u64> {
        if bus < self.start_bus || bus > self.end_bus || device > 31 || function > 7 || register > 0xFFC {
            return None;
        }
        Some(
            self.base
                + (((bus - self.start_bus) as u64) << 20)
                + ((device as u64) << 15)
                + ((function as u64) << 12)
                + (register as u64 & !3),
        )
    }
}

/// The window covering segment 0, bus 0, resolved once.
pub fn ecam() -> Option<Ecam> {
    static ECAM: OnceLock<Option<Ecam>> = OnceLock::new();
    *ECAM.get_or_init(|| {
        let table = firmware::table(firmware::PROVIDER_ACPI, firmware::TABLE_MCFG)?;
        parse_mcfg(&table).into_iter().find(|e| e.segment == 0 && e.start_bus == 0)
    })
}

/// Read one dword of configuration space under the PCI mutex. `None` when there is no driver, no
/// window, or another tool held the mutex too long.
pub fn read32(bus: u8, device: u8, function: u8, register: u16) -> Option<u32> {
    let driver = inpout::driver().ok()?;
    let address = ecam()?.address(bus, device, function, register)?;
    let _bus = mutex::pci().lock(mutex::WAIT_MS)?;
    driver.phys_read32(address)
}

// ---------------------------------------------------------------------------------------------
// SMN, through 00:00.0's index/data pair.

/// 00:00.0's vendor/device dword, and the SMN index and data registers.
pub const VENDOR_REGISTER: u16 = 0x00;
pub const SMN_INDEX: u16 = 0x60;
pub const SMN_DATA: u16 = 0x64;
pub const AMD_VENDOR_ID: u32 = 0x1022;

/// The RSMU mailbox on SMN: message, response, and the first of six argument registers.
/// ZenStates-Core's Zen4Settings, which Zen5Settings inherits unchanged; Roch Viewer's `smn`.
pub const RSMU_MSG: u32 = 0x03B1_0524;
pub const RSMU_RSP: u32 = 0x03B1_0570;
pub const RSMU_ARG0: u32 = 0x03B1_0A40;
pub const RSMU_ARGUMENTS: usize = 6;

/// Every message the RSMU message register can be given from here. All four are reads, and they
/// are Roch Viewer's `PERMITTED_COMMANDS`: the messages either side of the PBO-scalar getter in the
/// same block are setters, and no value but these four can reach the register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsmuCommand {
    /// TransferTableToDram: refresh the PM table in the buffer the SMU keeps in DRAM.
    TransferTable = 0x03,
    /// GetDramBaseAddress: where that buffer is.
    TableBase = 0x04,
    /// GetTableVersion: which layout the table has.
    TableVersion = 0x05,
    /// GetPBOScalar: the scalar, which has its own getter rather than a table offset.
    PboScalar = 0x6D,
}

impl RsmuCommand {
    pub const ALL: [RsmuCommand; 4] =
        [RsmuCommand::TableVersion, RsmuCommand::TableBase, RsmuCommand::TransferTable, RsmuCommand::PboScalar];

    pub fn code(self) -> u32 {
        self as u32
    }
}

/// Why an SMN session read nothing. Whatever it is, the index was put back if it was moved,
/// except for [`SmnError::RestoreFailed`], which says it could not be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmnError {
    /// No driver, no ECAM window, or no SetPhysLong; says which.
    Unavailable(&'static str),
    DriverClosed,
    /// Another tool held the PCI mutex for longer than the wait.
    Busy,
    /// 00:00.0 is not AMD's. Nothing was written.
    Vendor(u32),
    /// The driver refused to read this 00:00.0 configuration register.
    ConfigRead(u16),
    /// The driver refused to point the index at this SMN address.
    IndexWrite(u32),
    /// The driver refused a mailbox write to this SMN address.
    DataWrite(u32),
    /// An SMN register read all ones, which is nothing answering rather than a value.
    AllOnes(u32),
    /// The mailbox has six arguments; this asked for another.
    Argument(usize),
    /// The index the previous owner left could not be put back.
    RestoreFailed(u32),
}

impl std::fmt::Display for SmnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SmnError::Unavailable(why) => write!(f, "{why}"),
            SmnError::DriverClosed => write!(f, "the inpoutx64 driver is not open"),
            SmnError::Busy => write!(f, "another tool held {} for over {} ms", mutex::PCI, mutex::WAIT_MS),
            SmnError::Vendor(v) => write!(f, "PCI 00:00.0 vendor is 0x{v:04X}, expected AMD 0x{AMD_VENDOR_ID:04X}"),
            SmnError::ConfigRead(r) => write!(f, "the driver refused to read 00:00.0 config 0x{r:02X}"),
            SmnError::IndexWrite(a) => write!(f, "the driver refused to select SMN 0x{a:08X}"),
            SmnError::DataWrite(a) => write!(f, "the driver refused the mailbox write to SMN 0x{a:08X}"),
            SmnError::AllOnes(a) => write!(f, "SMN 0x{a:08X} returned invalid 0xFFFFFFFF"),
            SmnError::Argument(i) => write!(f, "the RSMU mailbox has no argument {i}"),
            SmnError::RestoreFailed(prior) => {
                write!(f, "CRITICAL: the SMN index could not be restored to 0x{prior:08X}")
            }
        }
    }
}

impl std::error::Error for SmnError {}

/// What the SMN protocol needs from 00:00.0, so that it can be exercised without hardware. The one
/// implementation that touches hardware, [`EcamRoot`], cannot be constructed or reached outside
/// this module except through [`amd_smn`], so its two writes are reachable only through [`Smn`].
pub trait RootComplex {
    type Guard;
    fn driver_open(&self) -> bool;
    /// The PCI bus mutex, or `None` when another tool held it past the wait.
    fn lock(&self) -> Option<Self::Guard>;
    /// One aligned configuration dword of 00:00.0.
    fn read_config(&self, register: u16) -> Option<u32>;
    /// 0x60: which SMN register the data window shows.
    fn write_index(&self, value: u32) -> bool;
    /// 0x64: the SMN register the index selects. Only [`Smn`]'s mailbox writes come here.
    fn write_data(&self, value: u32) -> bool;
}

/// One SMN session: the PCI mutex held, 00:00.0 confirmed AMD's, and the index the previous owner
/// left captured so it can be put back. Opened by [`session`] or [`amd_smn`].
pub struct Smn<'a, R: RootComplex + ?Sized> {
    root: &'a R,
    prior: u32,
    /// An index write was attempted. Marked before the write, because a driver can report failure
    /// after the write landed.
    selected: bool,
    restored: bool,
}

impl<R: RootComplex + ?Sized> Smn<'_, R> {
    fn select(&mut self, address: u32) -> Result<(), SmnError> {
        self.selected = true;
        if self.root.write_index(address) { Ok(()) } else { Err(SmnError::IndexWrite(address)) }
    }

    /// One SMN register.
    pub fn read(&mut self, address: u32) -> Result<u32, SmnError> {
        self.select(address)?;
        match self.root.read_config(SMN_DATA) {
            None => Err(SmnError::ConfigRead(SMN_DATA)),
            Some(0xFFFF_FFFF) => Err(SmnError::AllOnes(address)),
            Some(value) => Ok(value),
        }
    }

    /// Several registers in this one session; any failure fails them all, as Roch Viewer's
    /// `read_many` does, so a snapshot is never half one moment and half another.
    pub fn read_many(&mut self, addresses: &[u32]) -> Result<Vec<u32>, SmnError> {
        addresses.iter().map(|&a| self.read(a)).collect()
    }

    fn write_mailbox(&mut self, address: u32, value: u32) -> Result<(), SmnError> {
        self.select(address)?;
        if self.root.write_data(value) { Ok(()) } else { Err(SmnError::DataWrite(address)) }
    }

    /// The RSMU response register: 0 while a message is in flight, then the SMU's answer.
    pub fn response(&mut self) -> Result<u32, SmnError> {
        self.read(RSMU_RSP)
    }

    /// One of the six RSMU argument registers, where the SMU leaves what a message returns.
    pub fn argument(&mut self, index: usize) -> Result<u32, SmnError> {
        if index >= RSMU_ARGUMENTS {
            return Err(SmnError::Argument(index));
        }
        self.read(RSMU_ARG0 + 4 * index as u32)
    }

    /// Zero the response, so the answer to the next message can be told from the last one's.
    pub fn clear_response(&mut self) -> Result<(), SmnError> {
        self.write_mailbox(RSMU_RSP, 0)
    }

    /// The six arguments the next message takes.
    pub fn write_arguments(&mut self, values: &[u32; RSMU_ARGUMENTS]) -> Result<(), SmnError> {
        for (i, &value) in values.iter().enumerate() {
            self.write_mailbox(RSMU_ARG0 + 4 * i as u32, value)?;
        }
        Ok(())
    }

    /// Post one of the four permitted messages.
    pub fn issue(&mut self, command: RsmuCommand) -> Result<(), SmnError> {
        self.write_mailbox(RSMU_MSG, command.code())
    }

    /// Put the index back, if it was moved. Once only.
    fn restore(&mut self) -> bool {
        self.restored = true;
        !self.selected || self.root.write_index(self.prior)
    }
}

impl<R: RootComplex + ?Sized> Drop for Smn<'_, R> {
    fn drop(&mut self) {
        // Reached unrestored only when the session's work panicked; the PCI mutex is still held
        // here, because the session is dropped before the guard that took it.
        if !self.restored {
            let _ = self.restore();
        }
    }
}

/// Run `work` in one SMN session on `root`. Nothing is touched when the driver is closed; nothing
/// is written unless 00:00.0 is AMD's; the index is put back afterwards, and a failure to put it
/// back fails the whole session, whatever `work` returned.
pub fn session<R, T, E>(root: &R, work: impl FnOnce(&mut Smn<'_, R>) -> Result<T, E>) -> Result<T, E>
where
    R: RootComplex + ?Sized,
    E: From<SmnError>,
{
    if !root.driver_open() {
        return Err(SmnError::DriverClosed.into());
    }
    let _bus = root.lock().ok_or(SmnError::Busy)?;
    let id = root.read_config(VENDOR_REGISTER).ok_or(SmnError::ConfigRead(VENDOR_REGISTER))?;
    if id & 0xFFFF != AMD_VENDOR_ID {
        return Err(SmnError::Vendor(id & 0xFFFF).into());
    }
    let prior = root.read_config(SMN_INDEX).ok_or(SmnError::ConfigRead(SMN_INDEX))?;
    let mut smn = Smn { root, prior, selected: false, restored: false };
    let result = work(&mut smn);
    if !smn.restore() {
        return Err(SmnError::RestoreFailed(prior).into());
    }
    result
}

type IsOpenFn = unsafe extern "system" fn() -> u32;
type SetPhysLongFn = unsafe extern "system" fn(*mut u8, u32) -> i32;

/// 00:00.0 on this machine, through the ECAM window. Built once, by [`amd_smn`] only.
pub struct EcamRoot {
    driver: &'static InpOut,
    /// Physical address of 00:00.0's configuration space.
    config: u64,
    is_open: IsOpenFn,
    set_phys_long: SetPhysLongFn,
    /// Keeps the module the two functions above live in referenced.
    _lib: Library,
}

/// Physical address of one 00:00.0 configuration dword: aligned, and inside the function.
fn config_register(config: u64, register: u16) -> Option<u64> {
    (register & 3 == 0 && register <= 0xFFC).then(|| config + register as u64)
}

fn ecam_root() -> Result<&'static EcamRoot, &'static str> {
    static ROOT: OnceLock<Result<EcamRoot, String>> = OnceLock::new();
    ROOT.get_or_init(open_root).as_ref().map_err(String::as_str)
}

fn open_root() -> Result<EcamRoot, String> {
    let driver = inpout::driver().map_err(str::to_owned)?;
    let config = ecam()
        .and_then(|window| window.address(0, 0, 0, 0))
        .ok_or_else(|| "the ACPI MCFG table has no ECAM window for bus 0".to_owned())?;
    // SAFETY: the file inpout already loaded, by its absolute path, so no search order is involved
    // and the module is already mapped: this only takes another reference to it. The write is
    // looked up here rather than added to `InpOut`, so that no physical write primitive exists
    // anywhere in the crate, only these two destinations.
    let lib = unsafe { Library::new(driver.path()) }
        .map_err(|e| format!("could not reopen {}: {e}", driver.path().display()))?;
    // SAFETY: inpout32's documented exports, BOOL IsInpOutDriverOpen() and
    // BOOL SetPhysLong(PBYTE, DWORD).
    let (is_open, set_phys_long) = unsafe {
        let is_open = *lib.get::<IsOpenFn>(b"IsInpOutDriverOpen\0").map_err(|e| e.to_string())?;
        let set = *lib
            .get::<SetPhysLongFn>(b"SetPhysLong\0")
            .map_err(|e| format!("{} has no SetPhysLong: {e}", driver.path().display()))?;
        (is_open, set)
    };
    Ok(EcamRoot { driver, config, is_open, set_phys_long, _lib: lib })
}

impl EcamRoot {
    /// The only physical write in the crate, and its only two destinations.
    fn write(&self, register: u16, value: u32) -> bool {
        if register != SMN_INDEX && register != SMN_DATA {
            return false;
        }
        let Some(address) = config_register(self.config, register) else { return false };
        // SAFETY: SetPhysLong maps the page, writes one DWORD and unmaps it; the address is
        // 00:00.0's SMN index or data register and nothing else.
        unsafe { (self.set_phys_long)(address as usize as *mut u8, value) != 0 }
    }
}

impl RootComplex for EcamRoot {
    type Guard = mutex::Guard<'static>;

    fn driver_open(&self) -> bool {
        // SAFETY: no arguments; reports whether the driver's device handle is open.
        unsafe { (self.is_open)() != 0 }
    }

    fn lock(&self) -> Option<Self::Guard> {
        mutex::pci().lock(mutex::WAIT_MS)
    }

    fn read_config(&self, register: u16) -> Option<u32> {
        self.driver.phys_read32(config_register(self.config, register)?)
    }

    fn write_index(&self, value: u32) -> bool {
        self.write(SMN_INDEX, value)
    }

    fn write_data(&self, value: u32) -> bool {
        self.write(SMN_DATA, value)
    }
}

/// Run `work` in one SMN session on this machine's root complex (see [`session`]).
pub fn amd_smn<T, E: From<SmnError>>(work: impl FnOnce(&mut Smn<'_, EcamRoot>) -> Result<T, E>) -> Result<T, E> {
    let root = ecam_root().map_err(|why| E::from(SmnError::Unavailable(why)))?;
    session(root, work)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(entries: &[(u64, u16, u8, u8)]) -> Vec<u8> {
        let mut t = vec![0u8; ACPI_HEADER + MCFG_RESERVED];
        t[..4].copy_from_slice(b"MCFG");
        for &(base, seg, sb, eb) in entries {
            t.extend_from_slice(&base.to_le_bytes());
            t.extend_from_slice(&seg.to_le_bytes());
            t.push(sb);
            t.push(eb);
            t.extend_from_slice(&[0; 4]);
        }
        let len = t.len() as u32;
        t[4..8].copy_from_slice(&len.to_le_bytes());
        t
    }

    #[test]
    fn the_z790_window_parses_and_addresses_the_smbus_function() {
        // What this machine's MCFG says: 0xC0000000, segment 0, buses 0-255.
        let parsed = parse_mcfg(&table(&[(0xC000_0000, 0, 0, 255)]));
        assert_eq!(parsed, vec![Ecam { base: 0xC000_0000, segment: 0, start_bus: 0, end_bus: 255 }]);
        let smbus_bar4 = parsed[0].address(0, 0x1F, 4, 0x20).unwrap();
        assert_eq!(smbus_bar4, 0xC000_0000 + (0x1F << 15) + (4 << 12) + 0x20);
    }

    #[test]
    fn impossible_windows_are_dropped() {
        assert!(parse_mcfg(&table(&[(0, 0, 0, 255), (0xC000_1000, 0, 0, 255), (0xC000_0000, 0, 5, 1)])).is_empty());
        assert!(parse_mcfg(b"APIC").is_empty());
    }

    #[test]
    fn out_of_range_registers_have_no_address() {
        let e = Ecam { base: 0xE000_0000, segment: 0, start_bus: 0, end_bus: 63 };
        assert!(e.address(64, 0, 0, 0).is_none());
        assert!(e.address(0, 32, 0, 0).is_none());
        assert!(e.address(0, 0, 8, 0).is_none());
    }

    // The SMN transport, ported from Roch Viewer's test_amd_smn and test_amd_smn_mcfg. A fake
    // 00:00.0 emulates the index/data window over a map of SMN registers and records every access,
    // so the exact sequence, the vendor gate, the index restore and the failure handling can be
    // asserted without hardware. Roch Viewer's CF8 save/restore cases have no counterpart: ECAM
    // has no address port to save.

    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::rc::Rc;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Call {
        Open,
        Lock,
        Unlock,
        Read(u16),
        Index(u32),
        Data(u32),
    }
    use Call::*;

    /// What the index held before the session: the previous owner's selection.
    const PRIOR: u32 = 0xABCD_EF00;

    struct FakeRoot {
        vendor: u32,
        open: bool,
        busy: bool,
        smn: RefCell<HashMap<u32, u32>>,
        index: Cell<u32>,
        calls: Rc<RefCell<Vec<Call>>>,
        /// The first index write of this value lands, then reports failure.
        refuse_index: Cell<Option<u32>>,
        /// Putting the previous owner's index back fails.
        refuse_restore: bool,
    }

    struct FakeGuard(Rc<RefCell<Vec<Call>>>);

    impl Drop for FakeGuard {
        fn drop(&mut self) {
            self.0.borrow_mut().push(Unlock);
        }
    }

    impl FakeRoot {
        fn new(smn: &[(u32, u32)]) -> FakeRoot {
            FakeRoot {
                vendor: 0x1022,
                open: true,
                busy: false,
                smn: RefCell::new(smn.iter().copied().collect()),
                index: Cell::new(PRIOR),
                calls: Rc::new(RefCell::new(Vec::new())),
                refuse_index: Cell::new(None),
                refuse_restore: false,
            }
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.borrow().clone()
        }

        fn index_writes(&self) -> Vec<u32> {
            self.calls().into_iter().filter_map(|c| if let Index(v) = c { Some(v) } else { None }).collect()
        }

        fn count(&self, call: Call) -> usize {
            self.calls().into_iter().filter(|&c| c == call).count()
        }
    }

    impl RootComplex for FakeRoot {
        type Guard = FakeGuard;

        fn driver_open(&self) -> bool {
            self.calls.borrow_mut().push(Open);
            self.open
        }

        fn lock(&self) -> Option<FakeGuard> {
            self.calls.borrow_mut().push(Lock);
            (!self.busy).then(|| FakeGuard(self.calls.clone()))
        }

        fn read_config(&self, register: u16) -> Option<u32> {
            self.calls.borrow_mut().push(Read(register));
            match register {
                VENDOR_REGISTER => Some(0x1234_0000 | self.vendor),
                SMN_INDEX => Some(self.index.get()),
                SMN_DATA => Some(self.smn.borrow().get(&self.index.get()).copied().unwrap_or(0)),
                _ => None,
            }
        }

        fn write_index(&self, value: u32) -> bool {
            self.calls.borrow_mut().push(Index(value));
            if self.refuse_restore && value == PRIOR && self.index.get() != PRIOR {
                return false;
            }
            self.index.set(value);
            if self.refuse_index.get() == Some(value) {
                self.refuse_index.set(None);
                return false;
            }
            true
        }

        fn write_data(&self, value: u32) -> bool {
            self.calls.borrow_mut().push(Data(value));
            self.smn.borrow_mut().insert(self.index.get(), value);
            true
        }
    }

    fn read_one(root: &FakeRoot, address: u32) -> Result<u32, SmnError> {
        session(root, |s| s.read(address))
    }

    #[test]
    fn a_read_checks_the_vendor_captures_the_index_selects_reads_and_restores() {
        let root = FakeRoot::new(&[(0x50208, 0xCAFE)]);
        assert_eq!(read_one(&root, 0x50208), Ok(0xCAFE));
        assert_eq!(
            root.calls(),
            vec![
                Open,
                Lock,
                Read(VENDOR_REGISTER), // vendor check
                Read(SMN_INDEX),       // the previous owner's index
                Index(0x50208),        // select
                Read(SMN_DATA),        // read
                Index(PRIOR),          // restore, still under the mutex
                Unlock,
            ]
        );
        assert_eq!(root.index.get(), PRIOR);
    }

    #[test]
    fn zero_is_a_reading_not_a_failure() {
        let root = FakeRoot::new(&[(0x50204, 0)]);
        assert_eq!(read_one(&root, 0x50204), Ok(0));
    }

    #[test]
    fn a_batch_holds_the_mutex_once_checks_the_vendor_once_and_restores_once() {
        let root = FakeRoot::new(&[(0x50204, 0x1111), (0x50208, 0x2222)]);
        assert_eq!(session(&root, |s| s.read_many(&[0x50204, 0x50208])), Ok(vec![0x1111, 0x2222]));
        assert_eq!((root.count(Lock), root.count(Unlock)), (1, 1));
        assert_eq!(root.count(Read(VENDOR_REGISTER)), 1);
        assert_eq!(root.index_writes(), vec![0x50204, 0x50208, PRIOR]);
        assert_eq!(root.index.get(), PRIOR);
    }

    #[test]
    fn all_ones_invalidates_the_whole_batch() {
        let root = FakeRoot::new(&[(0x50204, 0x1111), (0x50208, 0xFFFF_FFFF)]);
        let err = session(&root, |s| s.read_many(&[0x50204, 0x50208])).unwrap_err();
        assert_eq!(err, SmnError::AllOnes(0x50208));
        assert!(err.to_string().contains("0xFFFFFFFF"));
        assert_eq!(root.index.get(), PRIOR);
    }

    #[test]
    fn a_foreign_vendor_is_never_selected() {
        let mut root = FakeRoot::new(&[(0x50208, 0xCAFE)]);
        root.vendor = 0x8086;
        let err = read_one(&root, 0x50208).unwrap_err();
        assert_eq!(err, SmnError::Vendor(0x8086));
        assert!(err.to_string().to_lowercase().contains("vendor"));
        assert!(!root.calls().iter().any(|c| matches!(c, Index(_) | Data(_))));
        assert_eq!(root.calls().last(), Some(&Unlock));
    }

    #[test]
    fn a_closed_driver_touches_nothing() {
        let mut root = FakeRoot::new(&[(0x50208, 0xCAFE)]);
        root.open = false;
        let err = read_one(&root, 0x50208).unwrap_err();
        assert!(err.to_string().contains("driver"));
        assert_eq!(root.calls(), vec![Open]);
    }

    #[test]
    fn a_busy_bus_is_not_touched() {
        let mut root = FakeRoot::new(&[(0x50208, 0xCAFE)]);
        root.busy = true;
        assert_eq!(read_one(&root, 0x50208), Err(SmnError::Busy));
        assert_eq!(root.calls(), vec![Open, Lock]);
    }

    #[test]
    fn a_failed_select_still_puts_the_index_back() {
        let root = FakeRoot::new(&[(0x50208, 0xCAFE)]);
        root.refuse_index.set(Some(0x50208));
        assert_eq!(read_one(&root, 0x50208), Err(SmnError::IndexWrite(0x50208)));
        assert_eq!(root.index_writes(), vec![0x50208, PRIOR]);
        assert_eq!(root.index.get(), PRIOR);
    }

    #[test]
    fn a_failed_restore_rejects_a_good_read() {
        let mut root = FakeRoot::new(&[(0x50204, 0x1111)]);
        root.refuse_restore = true;
        let err = read_one(&root, 0x50204).unwrap_err();
        assert_eq!(err, SmnError::RestoreFailed(PRIOR));
        assert!(err.to_string().contains("restored"));
    }

    #[test]
    fn reads_write_nothing_but_the_index() {
        let root = FakeRoot::new(&[(0x50208, 0xCAFE)]);
        read_one(&root, 0x50208).unwrap();
        assert!(!root.calls().iter().any(|c| matches!(c, Data(_))));
    }

    #[test]
    fn the_mailbox_is_written_through_its_fixed_registers_only() {
        let root = FakeRoot::new(&[]);
        session(&root, |s| {
            s.clear_response()?;
            s.write_arguments(&[1, 2, 3, 4, 5, 6])?;
            s.issue(RsmuCommand::TableVersion)
        })
        .unwrap();
        let writes: Vec<Call> = root.calls().into_iter().filter(|c| matches!(c, Index(_) | Data(_))).collect();
        let mut expected = vec![Index(RSMU_RSP), Data(0)];
        for i in 0..6u32 {
            expected.extend([Index(RSMU_ARG0 + 4 * i), Data(i + 1)]);
        }
        expected.extend([Index(RSMU_MSG), Data(0x05), Index(PRIOR)]);
        assert_eq!(writes, expected);
    }

    #[test]
    fn only_the_four_read_messages_exist() {
        let codes: Vec<u32> = RsmuCommand::ALL.iter().map(|c| c.code()).collect();
        assert_eq!(codes, vec![0x05, 0x04, 0x03, 0x6D]);
        // The getter's neighbours are setters in the same message block.
        for forbidden in [0x6C, 0x6E, 0x00, 0xFF] {
            assert!(!codes.contains(&forbidden));
        }
    }

    #[test]
    fn an_argument_past_the_sixth_does_not_exist() {
        let root = FakeRoot::new(&[]);
        assert_eq!(session(&root, |s| s.argument(6)), Err(SmnError::Argument(6)));
        assert!(root.index_writes().is_empty());
    }

    #[test]
    fn a_panic_mid_session_still_restores_the_index_before_the_bus_is_released() {
        let root = FakeRoot::new(&[(0x50208, 0xCAFE)]);
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<(), SmnError> = session(&root, |s| {
                s.read(0x50208)?;
                panic!("work panicked mid-session");
            });
        }));
        assert!(caught.is_err());
        assert_eq!(root.index.get(), PRIOR);
        let calls = root.calls();
        assert_eq!(&calls[calls.len() - 2..], &[Index(PRIOR), Unlock]);
    }

    #[test]
    fn config_reads_are_aligned_dwords_inside_the_function() {
        for register in [1, 2, 0x61, 0x1000] {
            assert_eq!(config_register(0xE000_0000, register), None, "register 0x{register:X}");
        }
        assert_eq!(config_register(0xE000_0000, 0xFFC), Some(0xE000_0FFC));
    }

    #[test]
    fn the_smn_window_is_00_00_0_config_0x60_and_0x64() {
        // Roch Viewer's MCFG fixture: one window at 0xE0000000 for buses 0-255.
        let window = parse_mcfg(&table(&[(0xE000_0000, 0, 0, 0xFF)]))[0];
        let config = window.address(0, 0, 0, 0).unwrap();
        assert_eq!(config_register(config, SMN_INDEX), Some(0xE000_0060));
        assert_eq!(config_register(config, SMN_DATA), Some(0xE000_0064));
    }
}
