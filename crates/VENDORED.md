# Vendored crates

`roch-core`, `roch-nvidia`, `roch-amd` and `roch-intel` are copied unchanged from Roch GPU,
https://github.com/1xroch/Roch-GPU, at commit `0431c7a66c4ef3b8ce90f760bf4dc5209e0b13e1`
("Merge linux-package: an Arch package and a downloadable binary"), with two changes since, both
in `roch-amd` and both the same edits as in Roch GPU:

- `4fb8a51` ("RDNA 4: read the PCIe link generation as the card runs it"): `src/mapper.rs` and
  `src/backend.rs`.
- `ff295ce` ("AMD: show the PCIe link's width, as \"5.0 x16 @ 5.0 x2\""): the new `src/pcie.rs`,
  `src/backend.rs`, `src/lib.rs` and two windows-sys features in `Cargo.toml`.

Copying a Roch GPU checkout that includes both commits leaves nothing of them to carry.

They are copied rather than referenced so this repository builds on its own. Roch Monitor calls only
their read paths: `GpuBackend::initialize`, `devices`, `capabilities`, `read_telemetry` and
`measure_extra_clocks`. None of the setters is reachable from `roch-sensors`.

To update them, copy the four directories from a newer Roch GPU checkout over these, then run
`cargo test` and `RochMonitor.exe --dump` on a machine with the card.
