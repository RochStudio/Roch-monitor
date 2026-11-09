# Roch Monitor 1.0.0

[![CI](https://github.com/RochStudio/Roch-monitor/actions/workflows/ci.yml/badge.svg)](https://github.com/RochStudio/Roch-monitor/actions/workflows/ci.yml)

Roch Monitor is a Windows hardware monitor in the spirit of HWiNFO's sensor window and HWMonitor:
every live reading on the machine, grouped by device, each with its **current, minimum, maximum and
average** since the last reset. It is built from the telemetry of the other two Roch tools:

- **Roch Viewer**'s sensor transports, ported from Python to Rust: the board's Super I/O, the PCH
  and FCH SMBus (DIMM thermal sensors, DDR5 hubs and PMICs), the Intel memory controller's MCHBAR
  (clocks, the hottest core, RAPL power, VCCSA and VDDQ TX), the ASUS embedded controller, the
  Windows performance counters and the WHEA error log.
- **Roch GPU**'s vendor backends, used as they are: NVAPI/NVML for NVIDIA, ADL/ADLX for AMD and
  IGCL for Intel Arc, with Roch GPU's telemetry table's rows, names and units.
- **Per-core readings the way HWiNFO shows them**, which neither Roch tool had: each core's
  temperature, clock and VID and each thread's effective clock, read from the CPU's own registers
  through PawnIO.

Like both, it is **read-only**. It changes no setting, and the only writes anywhere are selectors
that choose what the next read returns, each documented where it is made. The MSRs are only ever
read.

## What it shows

| Device | Readings | Source |
| --- | --- | --- |
| System | physical and virtual memory used, available, load | `GlobalMemoryStatusEx` |
| CPU | per-core temperatures, clocks (ratio × bus clock) and VIDs; per-thread effective clocks; CPU Package; the DTS throttling, critical-temperature and power-limit flags | MSRs through PawnIO (Intel) |
| CPU | per-thread effective clocks and per-core power (AMD Zen; not yet checked on AMD hardware) | MSRs through PawnIO |
| CPU | Core Max (sampled every 200 ms for its peaks), package and IA-core power, VCCSA, VDDQ TX, bus and ring clock | MCHBAR |
| CPU | total and per-thread usage, labelled P-core/E-core; average, per-thread and effective clocks when PawnIO is not installed | performance counters, `GetLogicalProcessorInformationEx` |
| Motherboard | temperatures, Vcore, SA, DRAM, CPU AUX, +12V/+5V/+3.3V, fan speeds and duty | Super I/O (Nuvoton NCT668x/NCT679x, ITE) |
| Motherboard | VRM temperature (ASUS boards) | ACPI embedded controller |
| Memory | data rate, MCLK, UCLK | MCHBAR |
| Each DIMM | DDR4: on-module thermal sensor. DDR5: SPD hub temperature, PMIC-measured VDD/VDDQ/VPP/VIN/1.8 V/1.0 V and module power | SMBus |
| Each GPU | temperatures, voltages, every clock domain, load, power, rail currents, fans, VRAM, PCIe link, limiters | Roch GPU's backends |
| Each drive | temperature, activity, read and write rate | storage IOCTL, performance counters |
| Each network adapter | download and upload rate, session totals | `GetIfTable2` |
| Windows hardware errors | WHEA events since boot | System event log |

A sensor the machine does not have gets no row rather than a column of dashes, and a reading that
falls outside what its rail or sensor can physically report is shown as missing rather than as a
number.

Every row that needs a kernel driver (the MSRs, MCHBAR, the Super I/O, the SMBus, the EC) reads
through **PawnIO** where it is installed, and through **inpoutx64** for anything PawnIO cannot serve.
With PawnIO installed, an Intel machine never loads inpoutx64; an AMD one still does for the SMU's
PM table, if inpoutx64 is there. **Sources** names the driver each source used.

### Validated on

MSI PRO Z790-P WIFI DDR4 (MS-7E06), Intel Core i9-14900KF, 2 × 16 GB G.Skill DDR4-3800,
NVIDIA GeForce RTX 5060 Ti. Every source answers on that machine, and two independent instruments
agree where they overlap: the MCHBAR's VCCSA (1.231 V) against the Super I/O's CPU SA (1.230 V), and
the die's Core Max against the board's CPU thermistor. Every board, memory and DIMM row reads the
same through PawnIO as through inpoutx64 (the same 33 rows, the same values). The per-core MSR
readings (PawnIO 2.2.0,
IntelMSR 0.2.11) agree the same way: the hottest core's reading matches MCHBAR's Core Max, each
core's VID (1.300 V) sits just under the Vcore the Super I/O measures (1.308 V), every core reads
57 × 100.00 MHz against MCHBAR's bus clock, and a busy thread's effective clock follows its usage
times that clock. A sweep of all 16 threads takes 0.3 ms.

The MSI Z790MPOWER (MS-7E01, Intel Core i5-14600KF, 2 × 16 GB DDR5-8000, RTX 5070 Ti) has a map of
its own, checked against HWiNFO 8.50 on the same boot: every board temperature, rail and fan Roch
Monitor shows reads the same there, the DIMMs' SPD hubs and PMICs agree, and HWiNFO's
"Unknown Rail (D4)" and "(D6)" powers are the "Rail 212" and "Rail 214" currents times the 12 V
input. It is the PRO Z790-P's layout except for one input, which carries CPU VDD2 on this DDR5
board where the DDR4 one has DRAM.

A board with a supported chip but no map of its own gets the nearest one, and its heading and
**Sources** say which board that map was confirmed on.

The Super I/O maps for the ASUS ROG MAXIMUS Z790 APEX (NCT6798D), the Gigabyte Z890 AORUS TACHYON
ICE (ITE IT8696E + IT87952E) and the MSI B850MPOWER (NCT6687D on AM5), the ASUS EC map and the DDR5
PMIC decode come from Roch Viewer, where each was confirmed on that board against HWiNFO. They have
not been re-run since being ported; treat them as needing confirmation on your own board.

## Using it

- **Click** a sensor to graph it; **Ctrl+click** adds it to the graphs already open. Sensors that
  share a unit share a chart.
- **Click a device heading** to fold it. Rows marked `+16` fold their per-thread breakdown; click the
  name to open it.
- **Right-click** a sensor to hide it, reset its statistics, or copy it.
- **Search** (Ctrl+F) filters by sensor, device or group.
- **Reset Stats** (Ctrl+R) clears every minimum, maximum, average and graph.
- **Log CSV** writes every visible sensor once per poll, the way HWiNFO logs, to
  `Documents\Roch Monitor`. **Save report** writes the whole table as text.
- **Poll** sets the interval (250 ms to 10 s). Each source reads on its own thread, so a slow one
  never holds the others up; **Sources** shows what each found and how long it takes.

### Tray

Like HWiNFO, Roch Monitor can live in the notification area:

- **Right-click a sensor → Show in tray** puts its live value in the tray as an icon of its own,
  digits drawn in (up to six). Hover for the full reading.
- The **Roch icon's tooltip** lists those readings, or the CPU and GPU temperatures and power when
  none are chosen. Left-click it to bring the window back; right-click for Show, Settings,
  Reset Stats and Exit.
- **Minimising** hides the window in the tray. Settings → Window can make the close button do the
  same, turn the value icons off, and **start Roch Monitor in the tray when you sign in** (a Task
  Scheduler entry with administrator rights, so the driver-based sensors are there without a
  prompt). `RochMonitor.exe --minimized` starts it in the tray by hand.

Windows 11 puts new tray icons behind the ^ arrow at first; drag them onto the taskbar to keep them
in view.

### Settings

**⚙ Settings** (beside the LIVE badge) opens Roch GPU's Settings gear, adapted for the monitor.
Every change shows on the main window as you make it:

| Page | What it changes |
| --- | --- |
| Appearance | Light, Dark or Follow Windows; colour schemes (Roch, High contrast, Graphite, Ocean, Forest, Amber, Purple, the same as Roch GPU's); contrast; row shading; corner roundness |
| Colours | Every colour, separately for light and dark: background, text, current value, accent, borders, dimmed text, row shading, hover, device headings. Pick from a swatch or type `#RRGGBB` |
| Text | Any installed font (fixed-width ones first; Consolas is the default), text size, bold headings, bold current readings, with a preview |
| Layout | Overall scale, row spacing, value column width, and whether the list shows group headings, sensor counts, device details and the machine summary |
| Window | Always on top, opacity |
| Sensors | Poll interval, graph history, DDR5 PMIC ADC, hidden sensors, the log folder |

**☀ Light / ☾ Dark** beside it switches the theme in one click. `RochMonitor.exe --settings colours`
opens with Settings at a page. Everything is saved in `%APPDATA%\RochMonitor\settings.json`.

### Command line

```console
RochMonitor.exe --dump                   sample for 3 s and print every sensor as a table
RochMonitor.exe --dump --json            the same as JSON
RochMonitor.exe --log out.csv --seconds 600
RochMonitor.exe --dump --driver inpoutx64  the same through inpoutx64 only (auto, pawnio, inpoutx64)
RochMonitor.exe --help
```

## Prerequisites

- Windows 10 or 11, 64-bit.
- **PawnIO** (recommended): install it from its author's site, https://pawnio.eu (the driver
  LibreHardwareMonitor uses). It is what the per-core temperatures, clocks and VIDs need, and the
  board, DIMM and memory-controller sensors use it too. Roch Monitor carries the seven signed
  modules it loads into it (`crates/roch-sensors/res/pawnio`), each of which admits only its own
  hardware: the MSRs on an allow-list, the memory controller's window, the Super I/O's ports, the
  SMBus controller.
- **inpoutx64** (optional fallback): for a machine without PawnIO, and for the AMD SMU's PM table,
  which is still read through it. It is not distributed here, for the reasons Roch Viewer gives:
  download `inpoutx64.dll` and `inpoutx64.sys` from Highresolution Enterprises (www.highrez.co.uk)
  and put both beside `RochMonitor.exe`. It hands any administrator process every I/O port and all
  of physical memory, which is why security software warns about it and why PawnIO is preferred.
  `--driver pawnio` makes sure it is never loaded.
- **Administrator rights**, which either driver needs. The window asks for them when it starts.
  Declining is allowed: CPU clocks and usage, the GPU, drives, network and WHEA work without the
  drivers, and the window says what is missing and why.

## Build

With Rust stable:

```console
cargo build --release
```

The executable is `target\release\RochMonitor.exe`. Run the tests with `cargo test`. Some tests read
this machine's own counters and firmware tables; none needs the driver.

### Layout

| Crate | What it is |
| --- | --- |
| `crates/roch-sensors` | The engine: the hardware transports, the sources, the polling threads and the statistics |
| `crates/roch-monitor` | The window (eframe/egui) and the command line |
| `crates/roch-core`, `roch-nvidia`, `roch-amd`, `roch-intel` | Roch GPU's crates, vendored unchanged; see `crates/VENDORED.md` |

## Licence

GPL-3.0-or-later, as Roch Viewer and Roch GPU are. See `LICENSE` and `THIRD-PARTY-NOTICES.md`.

This program is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without
even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. It reads hardware
registers directly through a kernel-level access driver; run it on hardware you are willing to
experiment with.

Created by **Roch Studio**.
