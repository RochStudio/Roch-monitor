//! The card's PCIe link, as far as Windows and the driver can say without reading configuration
//! space, and how the Graphics header words it.
//!
//! A discrete AMD GPU sits behind a PCIe switch on its own board. The driver's PMLog reports the
//! live link to the slot (sensors 40 and 41), which an RDNA 4 card narrows at idle: an RX 9070 XT
//! read x2 against the root port's own Link Status, and widens it again under load. None of AMD's
//! interfaces on that card reports the full-power width: ADLX's IADLXGPU1 lane width is the same
//! live x2, the Overdrive 5 and 6 activity calls are refused, and the WDDM performance caps carry a
//! placeholder. So the maximum width comes from Windows' PCI properties for the GPU function, which
//! read its Link Capabilities (x16 on that card), and the header shows both halves, as GPU-Z does:
//! "5.0 x16 @ 5.0 x2". In a slot narrower than the card, the live half is where that shows.

use roch_core::GpuIdentity;

/// "max @ live" when both halves are known and differ, the one that is known otherwise. Each half
/// is worded by [`GpuIdentity::pcie_bus_interface`], so a half missing its width or generation
/// reads as it does elsewhere.
pub fn bus_interface_text(
    maximum_generation: i32,
    maximum_width: i32,
    current_generation: i32,
    current_width: i32,
) -> String {
    let maximum = GpuIdentity::pcie_bus_interface(maximum_generation, maximum_width, 0, 0);
    let current = GpuIdentity::pcie_bus_interface(0, 0, current_generation, current_width);
    match (maximum.is_empty(), current.is_empty()) {
        (false, false) if maximum != current => format!("{maximum} @ {current}"),
        (false, _) => maximum,
        (true, _) => current,
    }
}

/// The instance ID ADL reports for an adapter, and the same ID without a trailing "&NN" display
/// suffix, which ADL appends for a card's further outputs ("...&00000009&02"). Only the
/// Windows reader uses it, and the tests that pin it.
#[cfg(any(windows, test))]
fn instance_ids(pnp: &str) -> Vec<String> {
    let mut ids = vec![pnp.trim().to_string()];
    if let Some((stem, tail)) = pnp.trim().rsplit_once('&')
        && tail.len() == 2
        && tail.bytes().all(|b| b.is_ascii_hexdigit())
    {
        ids.push(stem.to_string());
    }
    ids
}

/// The GPU function's maximum link width (DEVPKEY_PciDevice_MaxLinkWidth), from the instance ID
/// ADL reports for it. `None` where Windows does not say, or says something no link has.
#[cfg(windows)]
pub fn max_link_width(pnp: &str) -> Option<i32> {
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_DevNode_PropertyW, CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW, CR_SUCCESS,
    };
    use windows_sys::Win32::Devices::Properties::{DEVPROP_TYPE_UINT32, DEVPROPKEY, DEVPROPTYPE};
    use windows_sys::core::GUID;
    // DEVPKEY_PciDevice_MaxLinkWidth (devpkey.h): the PCI property set, pid 12.
    const MAX_LINK_WIDTH: DEVPROPKEY =
        DEVPROPKEY { fmtid: GUID::from_u128(0x3ab22e31_8264_4b4e_9af5_a8d2d8e33e62), pid: 12 };
    let read = |id: &str| -> Option<i32> {
        let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
        let mut node = 0u32;
        // SAFETY: `wide` is NUL-terminated and outlives the call; `node` is a live out parameter.
        if unsafe { CM_Locate_DevNodeW(&mut node, wide.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) } != CR_SUCCESS {
            return None;
        }
        let mut kind: DEVPROPTYPE = 0;
        let mut value = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: a four-byte buffer and its size, for a UINT32 property.
        let rc = unsafe {
            CM_Get_DevNode_PropertyW(node, &MAX_LINK_WIDTH, &mut kind, (&mut value as *mut u32).cast(), &mut size, 0)
        };
        (rc == CR_SUCCESS && kind == DEVPROP_TYPE_UINT32 && size == 4 && (1..=32).contains(&value))
            .then_some(value as i32)
    };
    instance_ids(pnp).iter().find_map(|id| read(id))
}

#[cfg(not(windows))]
pub fn max_link_width(_pnp: &str) -> Option<i32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_shows_the_maximum_and_the_live_link() {
        // The RX 9070 XT at idle: capable of 5.0 x16, running 5.0 x2.
        assert_eq!(bus_interface_text(5, 16, 5, 2), "5.0 x16 @ 5.0 x2");
        // Under load in a slot as wide as the card, one half says it all.
        assert_eq!(bus_interface_text(5, 16, 5, 16), "5.0 x16");
        // An x16 card in an x8 slot, under load.
        assert_eq!(bus_interface_text(5, 16, 5, 8), "5.0 x16 @ 5.0 x8");
        // Whatever is known on its own; nothing invented.
        assert_eq!(bus_interface_text(5, 0, 0, 0), "5.0");
        assert_eq!(bus_interface_text(5, 16, 0, 0), "5.0 x16");
        assert_eq!(bus_interface_text(0, 0, 4, 8), "4.0 x8");
        assert_eq!(bus_interface_text(0, 0, 0, 0), "");
    }

    #[test]
    fn a_display_suffix_is_tried_without_as_well() {
        let card = r"PCI\VEN_1002&DEV_7550&SUBSYS_2435148C&REV_C0\6&276DF33&0&00000009";
        assert_eq!(instance_ids(card), vec![card.to_string()]);
        assert_eq!(instance_ids(&format!("{card}&02")), vec![format!("{card}&02"), card.to_string()]);
    }
}
