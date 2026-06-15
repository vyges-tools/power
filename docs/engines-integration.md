# vyges-power in the Vyges engine flow

> **Cross-engine guide moved.** How all the Vyges engines compose, and where each plugs into an
> OpenROAD / LibreLane / OpenLane 2 flow, is now maintained once at
> **<https://docs.vyges.com/engines/integration.html>** (incl. the "drop these in" pre-P&R vs
> post-layout split). This stub stays so existing links keep working; the cross-engine map no
> longer lives per-repo (to avoid copy-drift).

## Where `vyges-power` sits

`vyges-power` is the middle of the **power loop**: it consumes what `vyges-char` produces and
produces what `vyges-em-ir` needs.

```text
   gate netlist + .lib + VCD/activity ─► vyges-power ─► power report + per-instance activity map
                                              │                                   │
                                       (+ .spef from extract                       └─► vyges-em-ir (IR/EM)
                                        for switching power)
```

Pre-P&R it gives an early power number on the synth netlist; post-route it uses extracted wire
caps. Either way it emits the per-instance activity map that turns `vyges-em-ir` from a
worst-case estimate into a real per-instance IR-drop solve — closing `char → power → em-ir`.

## power-specific depth (code-coupled, stays in this repo)

- [`correlation/`](../correlation/) — OpenSTA `report_power` correlation (leakage match; dynamic-energy
  unit calibration).
