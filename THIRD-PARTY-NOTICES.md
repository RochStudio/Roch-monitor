# Third-party notices

Roch Monitor is GPL-3.0-or-later (see `LICENSE`).

Rust crate dependencies are listed in `Cargo.lock` and carry their own licences (MIT / Apache-2.0
for the most part); run `cargo about` or `cargo license` to produce the full list for a release.

## Roch GPU and Roch Viewer

`crates/roch-core`, `roch-nvidia`, `roch-amd` and `roch-intel` are Roch GPU's, vendored unchanged
(see `crates/VENDORED.md`), and carry Roch GPU's notices: the NVAPI layouts translated from
NvAPIWrapper (Soroush Falahati, LGPL-3.0, conveyed under GPL-3.0 as LGPL-3.0 §2(b) permits), and
independently written bindings to AMD's ADL/ADLX and Intel's IGCL, which load the vendors' own
DLLs from System32. No NVIDIA, AMD or Intel code or headers are included.

`crates/roch-sensors` is a Rust port of Roch Viewer's sensor transports, both projects being Roch
Studio's and GPL-3.0-or-later.

## inpoutx64.dll / inpoutx64.sys (NOT distributed with this project)

On a machine without PawnIO, the board, DIMM and memory-controller sensors fall back to a
low-level access driver that reads physical memory, PCI configuration space and I/O ports, and the
AMD SMU's PM table is read through it in any case. Roch Monitor uses inpout32/64 by Highresolution
Enterprises (www.highrez.co.uk), which reports:

    Copyright (c) 2008 Highresolution Enterprises.
    Portions Copyright (c) Logix4u.

Those files are deliberately not included in this repository or in any build produced from it, for
the reason Roch Viewer gives: they are third-party binaries under their own terms, and this project
is GPL-3.0. Obtain them from the vendor and place them beside the executable yourself.

## PawnIO modules (bundled) and the PawnIO driver (NOT distributed with this project)

Seven compiled, signed modules from PawnIO.Modules (https://github.com/namazso/PawnIO.Modules),
release 0.2.11, are included byte for byte in `crates/roch-sensors/res/pawnio/` and compiled into the
executable:

    IntelMSR, AMDFamily17, LpcIO, LpcACPIEC   Copyright (C) 2023-2025 namazso <admin@namazso.eu>
    SmbusI801, SmbusPIIX4                     Copyright (C) 2025 Steve-Tech <me@stevetech.au>
                                              (parts ported from the Linux kernel's i2c drivers)
    IntelMCHBAR                               Copyright (C) 2026 Gen Li

Each is licensed LGPL-2.1-or-later; the licence is in `crates/roch-sensors/res/pawnio/COPYING.LGPL-2.1`,
and each module's source is the `.p` file of the same name in that repository at tag `0.2.11`.
`crates/roch-sensors/res/pawnio/README.md` records each file's sha256 and which of its functions Roch
Monitor calls. They cannot be modified and still load: the driver accepts only modules signed by
their author.

The modules run inside the PawnIO driver (https://pawnio.eu), which the user installs from its
author's own signed installer. The driver is not included in this repository or in any build produced
from it. The calling convention in `crates/roch-sensors/src/hw/pawnio.rs` (the two IOCTLs and the
execute buffer's layout) was written from PawnIO's public `pawnio_um.h` and `PawnIOLib.cpp`, and each
module's functions from its published source; no PawnIO source file is included.

## ZenStates-Core and ZenTimings (followed, not bundled)

Copyright (c) irusanov, https://github.com/irusanov, GPL-3.0.

The DDR5 PMIC decode in `crates/roch-sensors/src/sources/dimm.rs` (the ADC channels, the current and
power modes, the identity registers) follows Roch Viewer's `ddr5_telemetry` and `ddr5_pmic`, which
follow ZenStates-Core's Ddr5PmicReader/Ddr5PmicDecoder and were checked against ZenTimings. The FCH
SMBus discovery follows Roch Viewer's `fch_smbus`, which follows the same work. No source file from
either project is included; copyright in the parts derived from it remains with its author.

## LibreHardwareMonitor (consulted, not bundled)

https://github.com/LibreHardwareMonitor/LibreHardwareMonitor, MPL-2.0.

The NCT6687D's fan tachometer and duty registers (0x140, 0x160) and the dividers on its +12 V, +5 V
and +3.3 V channels are the ones LibreHardwareMonitor documents for that chip. They were then read on
the MSI Z790-P bench, where they land on 12.02, 5.01 and 3.35 V and on fan speeds that match the
headers fitted. No LibreHardwareMonitor source is included. The per-core MSR source pins its thread to each logical
processor before a read, as LibreHardwareMonitor does with the same PawnIO modules.

## Reference tools

HWiNFO, HWMonitor, CPU-Z and the vendors' own utilities were used as reference instruments, through
Roch Viewer and Roch GPU, to confirm that a register had been located correctly. No source, header
or data file from any of them is included, and none is required to run Roch Monitor.

## Fonts

The window uses Consolas and Segoe UI Symbol from the Windows font folder at run time; no font file
is distributed. egui's built-in fonts are compiled in as the fallback for a machine without them;
their licences (Hack: MIT and the Bitstream Vera licence; Ubuntu Light: Ubuntu Font Licence 1.0;
Noto Emoji and emoji-icon-font: SIL OFL 1.1 and MIT) ship with the `epaint_default_fonts` crate.
