//! Firmware tables through `GetSystemFirmwareTable`: the ACPI MCFG (where PCI configuration space
//! is mapped) and the raw SMBIOS block. Both are readable without the driver and without WMI.

/// `'ACPI'` and `'RSMB'` as the provider signatures the API wants (big-endian four-character codes).
pub const PROVIDER_ACPI: u32 = u32::from_be_bytes(*b"ACPI");
pub const PROVIDER_RSMB: u32 = u32::from_be_bytes(*b"RSMB");
/// ACPI table ids are the signature bytes read little-endian.
pub const TABLE_MCFG: u32 = u32::from_le_bytes(*b"MCFG");

#[cfg(windows)]
pub fn table(provider: u32, id: u32) -> Option<Vec<u8>> {
    use windows_sys::Win32::System::SystemInformation::GetSystemFirmwareTable;
    // SAFETY: a size query with a null buffer, then a read into a buffer of that size.
    unsafe {
        let size = GetSystemFirmwareTable(provider, id, std::ptr::null_mut(), 0);
        if size == 0 {
            return None;
        }
        let mut buffer = vec![0u8; size as usize];
        let got = GetSystemFirmwareTable(provider, id, buffer.as_mut_ptr(), size);
        if got == 0 || got > size {
            return None;
        }
        buffer.truncate(got as usize);
        Some(buffer)
    }
}

#[cfg(not(windows))]
pub fn table(_provider: u32, _id: u32) -> Option<Vec<u8>> {
    None
}
