//! Power model + report.
//!
//! Per instance: **leakage** (`cell_leakage_power`), **internal** switching
//! (representative per-transition energy × output toggle rate), and **net
//! switching** (½·C·V²·toggle_rate, C = summed sink input caps + a wire-cap
//! stand-in). The per-instance **average current** (`P/Vdd`) is the seam into
//! `vyges-em-ir`, which lands per-instance current on the nearest supply node.
//!
//! Energy/transition note: each transition (either edge) dissipates ½·C·V², so a
//! clock at frequency f (2f transitions/s) gives the textbook C·V²·f.

use std::collections::HashMap;

use crate::activity::Activity;
use crate::liberty::{Dir, Lib};
use crate::netlist::Netlist;
use crate::spef::Spef;

#[derive(Debug, Clone)]
pub struct InstPower {
    pub inst: String,
    pub cell: String,
    pub out_net: String,
    pub toggle_rate: f64, // transitions/sec on the output net
    pub leakage_w: f64,
    pub internal_w: f64,
    pub switch_w: f64,
    pub avg_current_a: f64, // (leakage+dynamic)/Vdd — what em-ir consumes
}

impl InstPower {
    pub fn dynamic_w(&self) -> f64 {
        self.internal_w + self.switch_w
    }
    pub fn total_w(&self) -> f64 {
        self.leakage_w + self.dynamic_w()
    }
}

#[derive(Debug, Clone)]
pub struct PowerReport {
    pub design: String,
    pub vdd: f64,
    pub mode: String,
    pub insts: Vec<InstPower>,
    pub unmatched: Vec<String>, // netlist cells with no lib match (no power counted)
    pub leakage_w: f64,
    pub internal_w: f64,
    pub switch_w: f64,
    pub spef: bool,       // were extracted wire caps used?
    pub spef_nets: usize, // how many output nets got a SPEF cap (vs the flat stand-in)
}

impl PowerReport {
    pub fn dynamic_w(&self) -> f64 {
        self.internal_w + self.switch_w
    }
    pub fn total_w(&self) -> f64 {
        self.leakage_w + self.dynamic_w()
    }

    /// The per-instance map `vyges-em-ir` consumes: average current (A) and the
    /// output toggle rate, one instance per line. This replaces em-ir's
    /// worst-case-simultaneous activity assumption with this analysis.
    pub fn activity_map(&self) -> String {
        let mut s = String::new();
        s.push_str("# vyges-power activity map for vyges-em-ir\n");
        s.push_str(&format!(
            "# design {}  vdd {:.4} V  mode {}\n",
            self.design, self.vdd, self.mode
        ));
        s.push_str("# columns: instance  avg_current_a  toggle_rate_hz\n");
        for i in &self.insts {
            s.push_str(&format!(
                "{}  {:.6e}  {:.6e}\n",
                i.inst, i.avg_current_a, i.toggle_rate
            ));
        }
        s
    }
}

/// Analyze a netlist against merged libs with an activity source. When a SPEF
/// (from `vyges-extract`) is supplied, each output net's switched wire cap comes
/// from its `*D_NET` total; nets absent from the SPEF fall back to `wire_cap_f`.
pub fn analyze(
    nl: &Netlist,
    lib: &Lib,
    act: &Activity,
    vdd: f64,
    wire_cap_f: f64,
    spef: Option<&Spef>,
) -> PowerReport {
    // Per-net switched load = Σ input-pin caps of its sinks.
    let mut net_load: HashMap<String, f64> = HashMap::new();
    for inst in &nl.insts {
        if let Some(cell) = lib.cell(&inst.cell) {
            for (pin, net) in &inst.conns {
                if let Some(p) = cell.pins.values().find(|p| &p.name == pin) {
                    if matches!(p.direction, Dir::In | Dir::Inout) {
                        *net_load.entry(net.clone()).or_insert(0.0) += p.cap_f;
                    }
                }
            }
        }
    }

    let mut insts = Vec::new();
    let mut unmatched = Vec::new();
    let mut spef_nets = 0usize;
    for inst in &nl.insts {
        let Some(cell) = lib.cell(&inst.cell) else {
            unmatched.push(inst.cell.clone());
            continue;
        };
        // output net = net on the cell's first output pin
        let out_net = cell
            .pins
            .values()
            .find(|p| p.direction == Dir::Out)
            .and_then(|op| inst.conns.iter().find(|(pin, _)| pin == &op.name))
            .map(|(_, n)| n.clone())
            .unwrap_or_default();

        let tr = if out_net.is_empty() {
            0.0
        } else {
            act.rate(&out_net)
        };
        let leakage_w = cell.leakage_w;
        let internal_w = cell.int_energy_j * tr;
        let pin_load = net_load.get(&out_net).copied().unwrap_or(0.0);
        // wire cap: extracted SPEF *D_NET total if present, else the flat stand-in
        let wire = match spef.and_then(|s| s.nets.get(&out_net)) {
            Some(rc) => {
                spef_nets += 1;
                rc.cap_ff * 1.0e-15 // fF -> F
            }
            None => wire_cap_f,
        };
        let cload = pin_load + wire;
        let switch_w = 0.5 * cload * vdd * vdd * tr;
        let total = leakage_w + internal_w + switch_w;
        let avg_current_a = if vdd > 0.0 { total / vdd } else { 0.0 };
        insts.push(InstPower {
            inst: inst.name.clone(),
            cell: inst.cell.clone(),
            out_net,
            toggle_rate: tr,
            leakage_w,
            internal_w,
            switch_w,
            avg_current_a,
        });
    }

    let leakage_w = insts.iter().map(|i| i.leakage_w).sum();
    let internal_w = insts.iter().map(|i| i.internal_w).sum();
    let switch_w = insts.iter().map(|i| i.switch_w).sum();
    unmatched.sort();
    unmatched.dedup();
    PowerReport {
        design: nl.module.clone(),
        vdd,
        mode: act.mode().to_string(),
        insts,
        unmatched,
        leakage_w,
        internal_w,
        switch_w,
        spef: spef.is_some(),
        spef_nets,
    }
}
