# Correlation — vyges-power vs OpenSTA `report_power` (real sky130)

A reproducible correlation of `vyges-power` against the open baseline (OpenSTA
`report_power`) on a **real sky130** block: `counter.v` synthesized to
`sky130_fd_sc_hd` (yosys), then both tools run on the *same* mapped netlist + the
*same* global activity. `run.sh` drives it on a host with `yosys` + `sta` + a
sky130 hd `.lib` + a built `vyges-power`.

```sh
SKY130_LIB=.../sky130_fd_sc_hd__tt_025C_1v80.lib \
VYGES_POWER=.../target/debug/vyges-power  ./run.sh
```

## Result (sky130 hd, 23 cells, 100 MHz, global activity 0.2)

| Component | OpenSTA `report_power` | vyges-power | ratio |
| --- | --- | --- | --- |
| **Leakage**   | 1.30e-10 W | 1.297e-10 W | **1.00 (≈0.2 %)** ✓ |
| **Internal**  | 3.82e-05 W | 2.00e-05 W  | 0.52× (within ~2×) |
| Switching     | 1.68e-05 W | 3.71e-06 W  | 0.22× |
| Total         | 5.50e-05 W | 2.37e-05 W  | 0.43× |

## What it validates / what's next

- **Leakage — validated to ≈0.2 %.** Both read `cell_leakage_power` from the same
  `.lib`; `vyges-power` reads real sky130 cleanly (0 unmatched cells). This is the
  most reliable power component and it matches.
- **Internal — within ~2×.** This pass fixed a unit bug: Liberty dynamic energy =
  **voltage_unit × current_unit × time_unit** (sky130: 1 V × 1 mA × 1 ns = 1e-12 J),
  not `leakage_power_unit × time_unit` — which moved internal power from ~1e6× low
  to 0.52×. The residual ~2× is the v0 **representative-mean** energy model vs
  OpenSTA's per-arc, state-/path-dependent `internal_power` (the documented depth item).
- **Switching — low because pre-layout.** `vyges-power` here used only `.lib` pin
  caps (no extracted wire caps); OpenSTA added `set_load 0.05` on outputs. On a
  *routed* block, feed a `vyges-extract` SPEF (`spef:` job key — already supported)
  and the wire-cap path closes most of this gap.

Honest bound: not advanced-node certified sign-off; this is the open, scriptable
inner-loop correlated to OpenSTA, with leakage validated and the dynamic models on a
clear depth path.
