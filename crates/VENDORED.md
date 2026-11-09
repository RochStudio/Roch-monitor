# Vendored crates

`roch-core`, `roch-nvidia`, `roch-amd` and `roch-intel` are copied unchanged from Roch GPU,
https://github.com/RochStudio/Roch-GPU, at commit `0431c7a66c4ef3b8ce90f760bf4dc5209e0b13e1`
("Merge linux-package: an Arch package and a downloadable binary").

They are copied rather than referenced so this repository builds on its own. Roch Monitor calls only
their read paths: `GpuBackend::initialize`, `devices`, `capabilities`, `read_telemetry` and
`measure_extra_clocks`. None of the setters is reachable from `roch-sensors`.

To update them, copy the four directories from a newer Roch GPU checkout over these, then run
`cargo test` and `RochMonitor.exe --dump` on a machine with the card.
