# PawnIO modules

Compiled, signed modules from PawnIO.Modules (https://github.com/namazso/PawnIO.Modules),
release 0.2.11 (2026-08-30), `release_0_2_11.zip`
(sha256 `43608cb89bc84247fef1368a139013f7d043e17db6d6c8dfc9b46bf0905a81f4`, the digest GitHub
publishes for that asset). Each is LGPL-2.1-or-later (see `COPYING.LGPL-2.1`, the licence text
shipped in that release); the source of each is the `.p` file of the same name in that repository
at tag `0.2.11`.

| File | Author | sha256 | What Roch Monitor calls |
| --- | --- | --- | --- |
| `IntelMSR.bin` | namazso | `d6ed85d65ab17a22f813ef98207d6d537155ee2ded5976a21cb48413c9b92e5f` | `ioctl_read_msr`: per-core temperature, clock, VID, effective clock |
| `AMDFamily17.bin` | namazso | `dae74615761b78bdf064dfb3e136252ddcc6fc727d88f14738d0e5800d427a91` | `ioctl_read_msr`: Zen effective clocks, per-core power |
| `IntelMCHBAR.bin` | Gen Li | `3f82b832d99b4aac37d2a20fdb7c9baa2a3bc0488612c9019c9484eb0e8a6eae` | `ioctl_get_mchbar_addr`, `ioctl_read_dword`: the memory controller's clocks, Core Max, RAPL, VCCSA, VDDQ TX |
| `LpcIO.bin` | namazso | `b3896a1cab0d808fca31fe2ebcae045d59dac690da87b17c858bb8da357eb45e` | `ioctl_select_slot`, `ioctl_find_bars`, `ioctl_pio_inb`, `ioctl_pio_outb`: the Super I/O |
| `LpcACPIEC.bin` | namazso | `c38fd116e7aff4d1fdb0a494e296be0a6708e5a22fc72f14587442fb7f8f7906` | `ioctl_pio_read`, `ioctl_pio_write`: the ACPI EC (ASUS boards) |
| `SmbusI801.bin` | Steve-Tech | `a0f7d066e7efda28c0e754c1f52dbd8dc280d388ba8575b514a80cb67490530c` | `ioctl_identity`, `ioctl_smbus_xfer`: Intel PCH SMBus (DIMM sensors) |
| `SmbusPIIX4.bin` | Steve-Tech | `91f9b4b1c39e3d399ce48477a89d8f6bd3e58a2241064a4daec3a1513dff56e5` | `ioctl_identity`, `ioctl_piix4_port_sel` (ports 0 and 1), `ioctl_smbus_xfer`: AMD FCH SMBus |

One more module is used but not bundled: `RyzenSMU.bin` (namazso), the SMU's PM table on Ryzen. It is
loaded from the PawnIO installation's own `modules` folder in Program Files, where the installer puts
it, and only its `ioctl_resolve_pm_table`, `ioctl_update_pm_table` and `ioctl_read_pm_table` (and
`ioctl_get_code_name`) are called; its raw SMU command and register writes are not. Where the
installation does not carry it, the table falls back to inpoutx64. Bundling it here, from the same
0.2.11 release as the others, would make that fallback unnecessary.

They run inside the PawnIO driver, which the user installs (https://pawnio.eu); Roch Monitor only
loads them into its own handles. The driver accepts only modules signed by their author, so these are
the release files byte for byte and must not be rebuilt or edited (`.gitattributes` marks them
binary so no line-ending conversion can touch them).

The port writes named above are the selector writes the read protocols need, documented at each
transport (`sources/superio.rs`, `sources/asus_ec.rs`); the one SMBus write is a DDR5 PMIC's ADC
channel selector (`sources/smbus.rs`). The MSR modules also export MSR writes, and nothing in Roch
Monitor calls them.

To update: download a newer `release_*.zip` from the releases page, check its sha256 against the
digest GitHub shows, copy the files over these, update this table, and update the sizes in
`src/hw/pawnio.rs`'s test.
