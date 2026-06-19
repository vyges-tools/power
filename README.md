# vyges-power

Gate-level **power analysis**: a netlist + timing libraries + an activity source
in, per-instance leakage + dynamic power out — and the per-instance **activity map
that `vyges-em-ir` consumes**, closing `char → power → em-ir`.

> **Vyges open EDA tools.** Commercial-grade silicon sign-off capability, built on
> open standards and plain file formats — meant to be accessible to everyone, not
> only teams who can license a six-figure tool. `vyges-power` opens up power analysis.

**Docs:** [docs.vyges.com](https://docs.vyges.com) — this engine's chapter, the
[cross-engine integration guide](https://docs.vyges.com/engines/integration.html), and the
job-file formats. In-repo depth: [`docs/engines-integration.md`](docs/engines-integration.md). **Integrating at the binary
level and need help?** → <https://vyges.com/contact>.

## Why this exists

Power is a first-class sign-off concern — total power sets the package and thermal
budget, and the *per-instance* current map is what drives IR-drop and EM. In the
Vyges flow, `vyges-char` characterizes per-cell switching energy and `vyges-em-ir`
solves the power grid — but the **middle was missing**: nothing turned a design's
*activity* into per-instance power and the current map em-ir needs. Today em-ir
assumes a worst-case-simultaneous activity (why a small counter shows ~19 % droop);
`vyges-power` replaces that assumption with a measured or estimated one, so
`char → power → em-ir` is a real, end-to-end chain.

## How this is solved today

In production, power is done by the commercial power tools — vectored (VCD/FSDB) and
vectorless propagation, glitch power, state-dependent energy — gated behind major
licenses. The open baseline is thin: OpenSTA's `report_power` is rudimentary and there
is no strong open *dynamic* power tool. `vyges-power` is an open engine in that space,
behind the standard formats (Verilog, Liberty, VCD), correlated against `report_power`
and the commercial power tools as baselines.

**Describe the job, not the script.** A small **declarative `.pwr` file** — readable,
diffable — instead of hand-written Tcl.

## The job (`.pwr`)

```text
design:           block
netlist:          block.v        # gate-level structural Verilog
lib:              tiny.lib        # one or more (comma-separated; from vyges-char)
clock:            clk 10.0        # port + period (ns) -> frequency
vdd:              1.8             # supply (V); optional, else the lib's nominal
vcd:              block.vcd       # vectored activity (omit -> vectorless)
# saif:           block.saif      # vectored activity from SAIF instead (exclusive with vcd)
activity:         0.2             # vectorless toggle factor (and VCD/SAIF fallback)
spef:             block.spef      # extracted wire caps from vyges-extract (optional)
default_wire_cap: 0.001           # pF per net when no SPEF (crude stand-in)
power_budget_mw:  1.0             # --fail-on-budget CI gate
emit_activity:    block.activity  # per-instance map for vyges-em-ir
```

```sh
cargo build --release            # std-only, no external deps
vyges-power run   examples/block/block.pwr            # text report
vyges-power run   examples/block/block.pwr --json     # machine-readable
vyges-power run   examples/block/block.pwr --fail-on-budget   # exit 3 over budget
vyges-power demo                                      # built-in design, no files
# common flags: -o FILE · --json · -q/--quiet · -v/--verbose · -h/--help · -V/--version
```

## What it computes (v0)

- **Leakage** — per cell from `cell_leakage_power`.
- **Internal** — representative per-transition energy (from Liberty `internal_power`)
  × the output net's toggle rate.
- **Net switching** — ½·C·V²·toggle_rate (a clock at f gives the textbook C·V²·f).
  C = Σ sink input caps (from the `.lib`) **+ the net's real extracted wire cap** from a
  `vyges-extract` SPEF (`*D_NET` total) when a `spef:` is given, else a flat stand-in.
  See `examples/counter/` — `counter.spef` is a real extract output
  (`vyges-extract run counter.ext`); two of its nets (`clk`, `n0`) carry extracted caps.
- **Activity** — **vectored** (measured per-net toggle rates from a **VCD** or a
  **SAIF** — e.g. Verilator `--trace-saif`; `read_saif` turns `TC`/`DURATION` into a
  toggle rate) or **vectorless** (a uniform `activity` factor × clock; also the
  fallback for nets absent from the dump — never silently zeroed).
- **The em-ir seam** — `emit_activity:` writes a per-instance **average current** +
  toggle rate map; `vyges-em-ir` lands that current on the nearest supply node instead
  of assuming worst-case-simultaneous switching.

**Honest bounds (depth reserved).** v0's internal-energy model is a representative mean
(real `internal_power` is per-arc / state- & path-dependent), vectorless is a uniform
factor (not yet probabilistic propagation), and glitch power is not yet in. These are
the correlation/depth pass.

## Open core, certified fab plugins

`vyges-power` is open (Apache-2.0) and contains **no foundry-confidential data** — power
comes entirely from the (open or licensed) Liberty you supply. Any per-fab correlation
adjustments ship as separate plugins under that foundry's terms, never in this repository.

## Current state (v0)

Leakage + internal + net-switching power, per-instance and total; vectored (VCD or SAIF)
and vectorless activity; real extracted wire caps via a `vyges-extract` SPEF (`spef:`); the
em-ir activity map; text + JSON; a `--fail-on-budget` CI gate. Pure std, unit + example
tested offline, no subprocess.

**Correlated against OpenSTA `report_power` on a real sky130 block** (see
[`correlation/`](correlation/)): leakage matches to ≈0.2 %, internal power within ~2×.
**The loop is closed end-to-end** — `examples/counter/run_loop.sh` runs
`vyges-extract` → `vyges-power` → `vyges-em-ir` and shows measured per-instance activity
giving a realistic (lower) IR droop than the worst-case assumption.

Depth reserved (next): per-arc / state-dependent internal energy, routed-block switching
via extracted parasitics, probabilistic vectorless propagation, glitch power.
