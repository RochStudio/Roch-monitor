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

Like both, it is **read-only**. It changes no setting, and the only writes anywhere are selectors
that choose what the next read returns, each documented where it is made.

## What it shows

| Device | Readings | Source |
| --- | --- | --- |
| System | physical and virtual memory used, available, load | `GlobalMemoryStatusEx` |
| CPU | Core Max (sampled every 200 ms for its peaks), package and IA-core power, VCCSA, VDDQ TX, bus and ring clock | MCHBAR |
| CPU | average, per-thread and effective clocks, total and per-thread usage, labelled P-core/E-core | performance counters, `GetLogicalProcessorInformationEx` |
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

### Validated on

MSI PRO Z790-P WIFI DDR4 (MS-7E06), Intel Core i9-14900KF, 2 × 16 GB G.Skill DDR4-3800,
NVIDIA GeForce RTX 5060 Ti. Every source answers on that machine, and two independent instruments
agree where they overlap: the MCHBAR's VCCSA (1.231 V) against the Super I/O's CPU SA (1.230 V), and
the die's Core Max against the board's CPU thermistor.

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
RochMonitor.exe --help
```

## Prerequisites

- Windows 10 or 11, 64-bit.
- **The inpoutx64 driver** for the board, DIMM and memory-controller sensors. It is not distributed
  here, for the reasons Roch Viewer gives: download `inpoutx64.dll` and `inpoutx64.sys` from
  Highresolution Enterprises (www.highrez.co.uk) and put both beside `RochMonitor.exe`. Windows
  security software may warn about it; it is a well-known low-level access component and, for the
  same reason, one attackers have abused, so decide for yourself whether you want it on your
  machine.
- **Administrator rights**, which the driver needs. The window asks for them when it starts.
  Declining is allowed: CPU clocks and usage, the GPU, drives, network and WHEA work without the
  driver, and the window says what is missing and why.

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
