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

use std::collections::{HashMap, HashSet};

use crate::activity::Activity;
use crate::liberty::{Dir, Lib};
use crate::netlist::Netlist;
use crate::spef::Spef;

/// Where an instance's power belongs in the three-way split every power report uses.
///
/// The split answers the first question a designer asks — *where did it go* — and it is what
/// makes **clock-gating efficiency** observable at all: clock power that is not matched by
/// register activity is a clock tree toggling for nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    /// In the clock network: the cell drives a net reachable from the clock port through
    /// combinational logic.
    Clock,
    /// Has a Liberty `ff`/`latch` group.
    Sequential,
    /// Everything else.
    Combinational,
}

impl Group {
    pub fn label(&self) -> &'static str {
        match self {
            Group::Clock => "clock",
            Group::Sequential => "sequential",
            Group::Combinational => "combinational",
        }
    }
    pub const ALL: [Group; 3] = [Group::Sequential, Group::Combinational, Group::Clock];
}

/// Totals for one [`Group`].
#[derive(Debug, Clone, Copy, Default)]
pub struct GroupPower {
    pub instances: usize,
    pub leakage_w: f64,
    pub internal_w: f64,
    pub switch_w: f64,
}

impl GroupPower {
    pub fn dynamic_w(&self) -> f64 {
        self.internal_w + self.switch_w
    }
    pub fn total_w(&self) -> f64 {
        self.leakage_w + self.dynamic_w()
    }
}

#[derive(Debug, Clone)]
pub struct InstPower {
    pub inst: String,
    pub cell: String,
    pub out_net: String,
    pub toggle_rate: f64, // transitions/sec on the output net
    pub group: Group,
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
    /// The library named no sequential cell and no clock pin, so the clock/sequential split
    /// was made on cell *shape* rather than on what the library declared. The numbers are
    /// still right; only their attribution to groups is a guess, and it says so.
    pub clock_grouping_approximate: bool,
}

impl PowerReport {
    pub fn dynamic_w(&self) -> f64 {
        self.internal_w + self.switch_w
    }
    pub fn total_w(&self) -> f64 {
        self.leakage_w + self.dynamic_w()
    }

    /// Totals for one group — the `Sequential` / `Combinational` / `Clock` rows.
    pub fn group(&self, g: Group) -> GroupPower {
        let mut t = GroupPower::default();
        for i in self.insts.iter().filter(|i| i.group == g) {
            t.instances += 1;
            t.leakage_w += i.leakage_w;
            t.internal_w += i.internal_w;
            t.switch_w += i.switch_w;
        }
        t
    }

    /// Clock power as a share of the design total, in percent.
    ///
    /// The one number every published clock-gating case study moves. It is a *ratio*, so it
    /// survives the parts of the model that are approximate: a design whose clock network burns
    /// a third of its power is telling you something true even where the absolute watts are v0.
    pub fn clock_share_pct(&self) -> f64 {
        let total = self.total_w();
        if total > 0.0 {
            100.0 * self.group(Group::Clock).total_w() / total
        } else {
            0.0
        }
    }

    /// The per-instance map `vyges-em-ir` consumes: average current (A) and the
    /// output toggle rate, one instance per line. This replaces em-ir's
    /// worst-case-simultaneous activity assumption with this analysis.
    pub fn activity_map(&self) -> String {
        self.activity_map_labelled(None)
    }

    /// [`activity_map`](PowerReport::activity_map), with a line naming the window it was
    /// measured over.
    ///
    /// The label is not decoration. IR drop and electromigration are driven by the *busiest*
    /// window, not by the whole run, so a map from a peak window and a map from a dump average
    /// are different claims that produce different droop — and the file itself has to say
    /// which one it is, because em-ir cannot tell by looking.
    pub fn activity_map_labelled(&self, window: Option<&str>) -> String {
        let mut s = String::new();
        s.push_str("# vyges-power activity map for vyges-em-ir\n");
        s.push_str(&format!(
            "# design {}  vdd {:.4} V  mode {}\n",
            self.design, self.vdd, self.mode
        ));
        s.push_str(&format!(
            "# measured over {}\n",
            window.unwrap_or("the whole activity source")
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

/// What to analyze the design against, beyond the netlist/library/activity themselves.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnalyzeOpts<'a> {
    pub vdd: f64,
    /// Wire capacitance (F) added per net where no SPEF covers it.
    pub wire_cap_f: f64,
    /// Extracted parasitics from `vyges-extract`.
    pub spef: Option<&'a Spef>,
    /// The design's clock port. Present → cells in the clock network are identified and
    /// reported as their own group; absent → no cell is classified `Clock`.
    pub clock_port: Option<&'a str>,
}

/// Does the library say enough to tell a flop from a buffer?
///
/// Clock-network identification rests on Liberty saying which cells are sequential (an
/// `ff`/`latch` group) and which pin is a clock (`clock : true`). Real libraries declare
/// both. A library that declares neither cannot be asked the question — and the failure is
/// silent and total: with nothing to stop it, clock-ness propagates through the flops and out
/// their `Q` pins until the entire design is "clock network", which is what this engine did
/// on its own demo library before the fallback below existed.
fn library_declares_sequential(nl: &Netlist, lib: &Lib) -> bool {
    nl.insts
        .iter()
        .filter_map(|i| lib.cell(&i.cell))
        .any(|c| c.is_seq || c.clock_pin.is_some())
}

/// Nets in the clock network: the clock port, and anything a *combinational* cell drives
/// from one.
///
/// **Stated limit.** Propagation stops at sequential cells and at declared clock pins, so a
/// divided clock — a flop whose `Q` is a clock — and everything below it are classified as
/// data, as is the output of a latch-based integrated clock gate. Identifying those requires
/// a `create_generated_clock` constraint this engine does not read, and inferring them from
/// structure ("a flop whose output fans out to clock pins") is how a checker starts inventing
/// clock domains. The grouping is therefore conservative: it under-reports clock power on a
/// design with generated clocks rather than over-reporting it on every design.
///
/// When the library is under-specified (see [`library_declares_sequential`]) propagation
/// additionally requires a **single-input** cell — the buffer/inverter shape a clock tree is
/// mostly made of. That keeps a repeater chain identified without walking through a flop that
/// the library never said was one.
fn clock_network(nl: &Netlist, lib: &Lib, root: &str, trust_lib: bool) -> HashSet<String> {
    let mut nets: HashSet<String> = HashSet::new();
    nets.insert(root.to_string());
    // Each pass can only add nets and there are finitely many, so this terminates; a clock
    // tree converges in as many passes as it is deep.
    loop {
        let mut added = false;
        for inst in &nl.insts {
            let Some(cell) = lib.cell(&inst.cell) else {
                continue;
            };
            if cell.is_seq {
                continue; // a flop's Q is data — see the note above
            }
            let inputs = || {
                cell.pins
                    .values()
                    .filter(|p| matches!(p.direction, Dir::In | Dir::Inout))
            };
            if !trust_lib && inputs().count() != 1 {
                continue;
            }
            let driven_by_clock = inst.conns.iter().any(|(pin, net)| {
                nets.contains(net)
                    && inputs().any(|p| &p.name == pin)
                    // A clock arriving at a pin declared `clock : true` is consumed there, not
                    // repeated: that is the pin of a sequential element, and its output is data.
                    && cell.clock_pin.as_deref() != Some(pin.as_str())
            });
            if !driven_by_clock {
                continue;
            }
            for (pin, net) in &inst.conns {
                let is_out = cell
                    .pins
                    .values()
                    .any(|p| p.name == *pin && p.direction == Dir::Out);
                if is_out && nets.insert(net.clone()) {
                    added = true;
                }
            }
        }
        if !added {
            return nets;
        }
    }
}

/// Analyze a netlist against merged libs with an activity source. When a SPEF
/// (from `vyges-extract`) is supplied, each output net's switched wire cap comes
/// from its `*D_NET` total; nets absent from the SPEF fall back to `wire_cap_f`.
pub fn analyze(nl: &Netlist, lib: &Lib, act: &Activity, opts: &AnalyzeOpts) -> PowerReport {
    let AnalyzeOpts {
        vdd,
        wire_cap_f,
        spef,
        clock_port,
    } = *opts;
    let trust_lib = library_declares_sequential(nl, lib);
    let clk_nets = clock_port
        .map(|c| clock_network(nl, lib, c, trust_lib))
        .unwrap_or_default();
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
        // Sequential wins over clock: a flop clocked by the tree is a register, not part of
        // the tree — the same split OpenSTA's `report_power` groups report.
        let group = if cell.is_seq {
            Group::Sequential
        } else if !out_net.is_empty() && clk_nets.contains(&out_net) {
            Group::Clock
        } else {
            Group::Combinational
        };
        insts.push(InstPower {
            inst: inst.name.clone(),
            cell: inst.cell.clone(),
            out_net,
            toggle_rate: tr,
            group,
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
        clock_grouping_approximate: clock_port.is_some() && !trust_lib,
    }
}
