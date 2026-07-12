//! vyges-power — gate-level power analysis.
//!
//! Closes the power-integrity loop with the other engines: `vyges-char` emits
//! the Liberty (`.lib`) with leakage + internal-power tables, and this engine
//! *consumes* it to compute per-instance and total power — then emits the
//! per-instance **activity map** that `vyges-em-ir` needs. Today em-ir assumes a
//! worst-case-simultaneous activity; `vyges-power` replaces that assumption with
//! a measured (or estimated) one, so `char -> power -> em-ir` is a real chain.
//!
//! Boundaries (per the Vyges flow architecture): inputs and outputs are files
//! (Verilog netlist + Liberty + optional VCD/SAIF in; a power report + an
//! `.activity` map out). The whole v0 is pure std and unit-tested offline —
//! there is no subprocess. The correlation baseline (OpenSTA `report_power`)
//! is not a runtime dependency.
//!
//! v0 scope: total + per-instance **leakage** (from `cell_leakage_power`),
//! **internal** switching energy (representative per-transition energy from the
//! Liberty `internal_power` groups), and **net switching** power (½·C·V²·f·α).
//! Activity comes from a **vectored** source (VCD or SAIF toggle counts) or a
//! **vectorless** default (a per-net activity factor × clock). Depth reserved
//! for later: probabilistic vectorless propagation, glitch power, and
//! state/path-dependent internal energy (`PowerError::NotModeled` hooks).

pub mod job;
// liberty + netlist + spef + the activity readers (vcd/saif/names) all come from the
// shared vyges-loom foundation — the parse-once/query-many readers live there, next to
// each other, not in an engine. loom's Liberty is a superset (leakage_w, int_energy_j,
// pin cap_f, lib voltage). Re-exported under the crate root so `crate::liberty` /
// `crate::netlist` / `crate::spef` / `crate::vcd` / `crate::saif` / `crate::names` keep
// resolving for the rest of the engine. The `fst` feature pulls loom's FST reader.
pub use vyges_loom::{fst, liberty, names, netlist, saif, spef, vcd};
pub mod activity;
pub mod power;
pub mod engine;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const COPYRIGHT: &str = "© 2026 Vyges. All Rights Reserved.  https://vyges.com";
