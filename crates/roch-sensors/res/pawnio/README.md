# PawnIO modules

Compiled, signed modules from PawnIO.Modules by namazso (https://github.com/namazso/PawnIO.Modules),
release 0.2.11 (2026-08-30), `release_0_2_11.zip`
(sha256 `43608cb89bc84247fef1368a139013f7d043e17db6d6c8dfc9b46bf0905a81f4`, the digest GitHub
publishes for that asset). LGPL-2.1-or-later; see `COPYING.LGPL-2.1`. Their source is
`IntelMSR.p` and `AMDFamily17.p` in that repository.

| File | sha256 | Used for |
| --- | --- | --- |
| `IntelMSR.bin` | `d6ed85d65ab17a22f813ef98207d6d537155ee2ded5976a21cb48413c9b92e5f` | Intel per-core temperature, clock, VID and effective clock |
| `AMDFamily17.bin` | `dae74615761b78bdf064dfb3e136252ddcc6fc727d88f14738d0e5800d427a91` | AMD Zen effective clocks and per-core power |

They run inside the PawnIO driver, which the user installs (https://pawnio.eu); Roch Monitor only
loads them into its own handle. The driver accepts only modules signed by their author, so these are
the release files byte for byte and must not be rebuilt or edited.

Roch Monitor calls one function in each, `ioctl_read_msr`. The modules also export MSR writes; nothing
in Roch Monitor calls them.

To update: download a newer `release_*.zip` from the releases page, check its sha256 against the
digest GitHub shows, copy the two files over these, and update this table.
