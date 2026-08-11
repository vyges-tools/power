//! Engine: load a job → netlist + libs + activity → power report.
//!
//! Mirrors the other engines' shape: files in, a report (text or JSON) out, no
//! subprocess. `vyges-char` supplies the Liberty; an optional VCD or SAIF supplies
//! real activity; the report's `activity_map()` is the seam out to `vyges-em-ir`.

use crate::activity::Activity;
use crate::fst::Fst;
use crate::job::PwrJob;
use crate::liberty::Lib;
use crate::netlist;
use crate::power::{self, PowerReport};
use crate::saif::Saif;
use crate::spef::Spef;
use crate::vcd::Vcd;
use vyges_events::{Event, Severity};

pub fn analyze_job(job: &PwrJob) -> Result<PowerReport, String> {
    let nl = netlist::load(&job.resolve(&job.netlist)).map_err(|e| e.to_string())?;

    let mut lib: Option<Lib> = None;
    for l in &job.libs {
        // NLDM-only load (skip CCS) — and this is the *correct* model for power, not
        // just faster: switching energy is ½·C·V² where C is the physical charge on the
        // net = static input cap (cap_f) + wire cap. CCS `receiver_capacitance` is the
        // Miller-inflated *delay-effective* input load — using it here would over-count
        // energy — and `output_current` is a driver-delay/SI model, not a power input.
        // Dynamic energy comes from Liberty `internal_power` (int_energy_j). So CCS is
        // dropped at parse time for a smaller/faster load with a result that is correct
        // by design. (Revisit only if power adopts a genuinely CCS-based energy model.)
        let parsed = Lib::load_opts(&job.resolve(l), crate::liberty::LibOpts { skip_ccs: true })
            .map_err(|e| e.to_string())?;
        match &mut lib {
            Some(acc) => acc.merge(parsed),
            None => lib = Some(parsed),
        }
    }
    let lib = lib.ok_or_else(|| "no libraries loaded".to_string())?;

    let vdd = job.vdd.unwrap_or(lib.voltage);
    let freq = job.freq_hz();
    // Emit a structured vyges-events warning when leaf names collide across scopes and
    // no `scope:` disambiguates them — those nets fall back to the vectorless factor.
    // `objects` carries the ambiguous net leaves for cross-stage co-reference.
    let warn_collisions = |leaves: Vec<String>| {
        if !leaves.is_empty() && job.scope.is_none() {
            let objs: Vec<String> = leaves.iter().map(|l| format!("net:{l}")).collect();
            vyges_events::emit(
                &Event::new(
                    "vyges-power",
                    Severity::Warn,
                    format!(
                        "{} net name(s) appear in multiple scopes; set 'scope:' to disambiguate (ambiguous nets fall back to the vectorless factor)",
                        leaves.len()
                    ),
                )
                .with_code("POWER-SCOPE-COLLISION")
                .with_objects(objs),
            );
        }
    };
    let act = if let Some(s) = &job.saif {
        let saif =
            Saif::load_scoped(&job.resolve(s), job.scope.clone()).map_err(|e| e.to_string())?;
        warn_collisions(saif.idx.colliding_leaves());
        Activity::vectored(saif, "vectored (SAIF)", job.activity_factor, freq)
    } else if let Some(v) = &job.vcd {
        let vcd = Vcd::load_scoped(&job.resolve(v), job.activity_window, job.scope.clone())
            .map_err(|e| e.to_string())?;
        if job.activity_window.is_some() && vcd.sim_time_s <= 0.0 {
            vyges_events::emit(
                &Event::new(
                    "vyges-power",
                    Severity::Warn,
                    "activity_window is empty or outside the dump; nets fall back to the vectorless factor".to_string(),
                )
                .with_code("POWER-ACTIVITY-WINDOW-EMPTY"),
            );
        }
        warn_collisions(vcd.idx.colliding_leaves());
        Activity::vectored(vcd, "vectored (VCD)", job.activity_factor, freq)
    } else if let Some(fp) = &job.fst {
        let fst = Fst::load_scoped(&job.resolve(fp), job.activity_window, job.scope.clone())
            .map_err(|e| e.to_string())?;
        if job.activity_window.is_some() && fst.sim_time_s <= 0.0 {
            vyges_events::emit(
                &Event::new(
                    "vyges-power",
                    Severity::Warn,
                    "activity_window is empty or outside the dump; nets fall back to the vectorless factor".to_string(),
                )
                .with_code("POWER-ACTIVITY-WINDOW-EMPTY"),
            );
        }
        warn_collisions(fst.idx.colliding_leaves());
        Activity::vectored(fst, "vectored (FST)", job.activity_factor, freq)
    } else {
        Activity::vectorless(job.activity_factor, freq)
    };
    let wire_cap_f = job.default_wire_cap_pf * 1.0e-12;
    let spef = match &job.spef {
        Some(p) => Some(Spef::load(&job.resolve(p)).map_err(|e| e.to_string())?),
        None => None,
    };
    emit_input_coverage(&nl, &lib, &act, spef.as_ref());
    Ok(power::analyze(
        &nl,
        &lib,
        &act,
        &power::AnalyzeOpts {
            vdd,
            wire_cap_f,
            spef: spef.as_ref(),
            clock_port: Some(&job.clock_port),
        },
    ))
}

/// One window of a [`SweepReport`]: when it ran, how busy it was, and what it cost.
#[derive(Debug, Clone)]
pub struct WindowPower {
    pub index: usize,
    pub from_s: f64,
    pub to_s: f64,
    pub events: u64, // value changes in the window — 0 means nothing happened, not a failure
    pub leakage_w: f64,
    pub internal_w: f64,
    pub switch_w: f64,
    pub sequential_w: f64,
    pub combinational_w: f64,
    pub clock_w: f64,
    pub covered_nets: usize,
}

impl WindowPower {
    pub fn dynamic_w(&self) -> f64 {
        self.internal_w + self.switch_w
    }
    pub fn total_w(&self) -> f64 {
        self.leakage_w + self.dynamic_w()
    }
}

/// Power over the workload: one row per measurement window, plus the **peak** window kept as
/// a full [`PowerReport`].
///
/// The peak is the point of the sweep, not a summary of it. Average power over a whole dump is
/// the wrong input for IR drop and electromigration — those are driven by the busiest window —
/// and until now the choice was between a dump average (optimistic) and worst-case-simultaneous
/// switching (pessimistic). The peak window is the third option, and it is measured.
#[derive(Debug, Clone)]
pub struct SweepReport {
    pub design: String,
    pub vdd: f64,
    pub mode: String,
    pub dump_end_s: f64,
    pub windows: Vec<WindowPower>,
    pub peak: usize,              // index into `windows`
    pub peak_report: PowerReport, // the full per-instance report for the peak window
}

impl SweepReport {
    pub fn peak_window(&self) -> &WindowPower {
        &self.windows[self.peak]
    }
    /// Mean total power across the measured windows — what a whole-dump run reports.
    pub fn mean_total_w(&self) -> f64 {
        if self.windows.is_empty() {
            return 0.0;
        }
        self.windows.iter().map(|w| w.total_w()).sum::<f64>() / self.windows.len() as f64
    }
    /// Peak / mean. The factor a dump-average activity map hides from `vyges-em-ir`.
    pub fn peak_to_mean(&self) -> f64 {
        let mean = self.mean_total_w();
        if mean > 0.0 {
            self.peak_window().total_w() / mean
        } else {
            0.0
        }
    }
}

/// Analyze a job that carries an `activity_sweep:` — power over the workload, from **one**
/// parse of the dump.
///
/// The netlist, libraries and parasitics are loaded once and every window is evaluated against
/// them; only the toggle counts differ per window. Running the same sweep by re-invoking a
/// whole-design power analysis per window would re-read all of it N times, which is what makes
/// the obvious implementation of this slow rather than what makes it hard.
pub fn analyze_sweep_job(job: &PwrJob) -> Result<SweepReport, String> {
    let Some(sw) = job.activity_sweep else {
        return Err("job has no activity_sweep:".to_string());
    };
    let vcd_path = job
        .vcd
        .as_ref()
        .ok_or_else(|| "activity_sweep requires a 'vcd:' source".to_string())?;

    let nl = netlist::load(&job.resolve(&job.netlist)).map_err(|e| e.to_string())?;
    let mut lib: Option<Lib> = None;
    for l in &job.libs {
        let parsed = Lib::load_opts(&job.resolve(l), crate::liberty::LibOpts { skip_ccs: true })
            .map_err(|e| e.to_string())?;
        match &mut lib {
            Some(acc) => acc.merge(parsed),
            None => lib = Some(parsed),
        }
    }
    let lib = lib.ok_or_else(|| "no libraries loaded".to_string())?;
    let vdd = job.vdd.unwrap_or(lib.voltage);
    let freq = job.freq_hz();
    let spef = match &job.spef {
        Some(p) => Some(Spef::load(&job.resolve(p)).map_err(|e| e.to_string())?),
        None => None,
    };
    let opts = power::AnalyzeOpts {
        vdd,
        wire_cap_f: job.default_wire_cap_pf * 1.0e-12,
        spef: spef.as_ref(),
        clock_port: Some(&job.clock_port),
    };

    let sweep = Vcd::load_sweep(
        &job.resolve(vcd_path),
        sw.from_s,
        sw.to_s,
        sw.window_s,
        Some(sw.step_s),
        job.scope.clone(),
    )
    .map_err(|e| e.to_string())?;
    if sweep.windows.is_empty() {
        return Err("activity_sweep measured no windows — check its range against the dump".into());
    }

    let mut nets: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for i in &nl.insts {
        for (_pin, n) in &i.conns {
            nets.insert(n.as_str());
        }
    }

    let mut windows: Vec<WindowPower> = Vec::with_capacity(sweep.windows.len());
    let mut peak = 0usize;
    let mut peak_report: Option<PowerReport> = None;
    for i in 0..sweep.windows.len() {
        let act = Activity::vectored(
            sweep.window(i),
            "vectored (VCD sweep)",
            job.activity_factor,
            freq,
        );
        let rep = power::analyze(&nl, &lib, &act, &opts);
        let w = &sweep.windows[i];
        let (hit, _) = act.coverage(nets.iter().copied());
        let row = WindowPower {
            index: i,
            from_s: w.from_s,
            to_s: w.to_s,
            events: w.events,
            leakage_w: rep.leakage_w,
            internal_w: rep.internal_w,
            switch_w: rep.switch_w,
            sequential_w: rep.group(power::Group::Sequential).total_w(),
            combinational_w: rep.group(power::Group::Combinational).total_w(),
            clock_w: rep.group(power::Group::Clock).total_w(),
            covered_nets: hit,
        };
        // Strictly greater, so the earliest window wins a tie — a deterministic peak, and the
        // first time the design got that busy is the more useful one to look at.
        if peak_report.is_none() || row.total_w() > windows[peak].total_w() {
            peak = i;
            peak_report = Some(rep);
        }
        windows.push(row);
    }
    let peak_report = peak_report.expect("at least one window");

    vyges_events::emit(
        &Event::new(
            "vyges-power",
            Severity::Info,
            format!(
                "swept {} window(s) of {:.3} ns over one parse; peak window #{} [{:.3} ns, {:.3} ns) at {:.3} mW ({:.2}x the mean)",
                windows.len(),
                sw.window_s * 1e9,
                peak,
                windows[peak].from_s * 1e9,
                windows[peak].to_s * 1e9,
                windows[peak].total_w() * 1e3,
                {
                    let mean: f64 = windows.iter().map(|w| w.total_w()).sum::<f64>() / windows.len() as f64;
                    if mean > 0.0 { windows[peak].total_w() / mean } else { 0.0 }
                }
            ),
        )
        .with_code("POWER-SWEEP"),
    );

    Ok(SweepReport {
        design: nl.module.clone(),
        vdd,
        mode: "vectored (VCD sweep)".to_string(),
        dump_end_s: sweep.dump_end_s,
        windows,
        peak,
        peak_report,
    })
}

/// Report how much of the design each input actually covers.
///
/// The counting is `vyges_loom::coverage`, shared with the other engines so it cannot drift.
/// The verdicts are here, because they are not portable: what matters to a power run is not what
/// matters to a timing run.
///
/// The activity one is the reason this exists. In vectored mode a net the dump does not name
/// silently takes the vectorless fallback, so a dump covering a fraction of the design still
/// produces a report labelled "vectored (VCD)" whose numbers are mostly the uniform estimate.
/// Power figures get quoted, and nothing said so.
fn emit_input_coverage(nl: &netlist::Netlist, lib: &Lib, act: &Activity, spef: Option<&Spef>) {
    use std::collections::BTreeSet;
    use vyges_events::{Event, Severity};
    use vyges_loom::coverage;

    let emit = |attention: bool, code: &str, msg: String| {
        let sev = if attention {
            Severity::Warn
        } else {
            Severity::Info
        };
        vyges_events::emit(&Event::new("vyges-power", sev, msg).with_code(code));
    };

    let nc = coverage::netlist(nl, lib);
    // Unlike a timer, a power run does not error on an unknown master — it simply contributes
    // nothing, so an unresolved signal cell is missing power rather than a missing check.
    emit(
        nc.unresolved_signal > 0 || nc.instances == 0,
        "POWER-NETLIST",
        format!(
            "netlist: {} instance(s) of {} master(s); {} with a library cell, {} physical-only, \
             {} with signal pins have NO library cell (they contribute no power)",
            nc.instances,
            nc.masters,
            nc.analysable(),
            nc.physical_only,
            nc.unresolved_signal
        ),
    );

    // Whether the library says enough to attribute power to the clock network. Reported here
    // with the other input-coverage facts because that is what it is: a statement about what
    // the inputs support, not about the design.
    let declares_seq = nl
        .insts
        .iter()
        .filter_map(|i| lib.cell(&i.cell))
        .any(|c| c.is_seq || c.clock_pin.is_some());
    if !declares_seq {
        emit(
            true,
            "POWER-CLOCK-GROUPING",
            "liberty: no cell declares an ff/latch group or a `clock : true` pin — the \
             sequential/clock split falls back to cell shape (single-input repeaters only)"
                .to_string(),
        );
    }

    // Power needs leakage and internal-energy tables, not delay arcs, so the timing-oriented
    // arc check is deliberately not repeated here — it would be a warning about something this
    // engine does not use.
    let lc = coverage::liberty(nl, lib);
    emit(
        lc.resolved_masters == 0,
        "POWER-LIBERTY",
        format!(
            "liberty: {} cell(s) loaded, {} used by the design",
            lc.cells_in_lib, lc.resolved_masters
        ),
    );

    let mut nets: BTreeSet<&str> = BTreeSet::new();
    for i in &nl.insts {
        for (_pin, n) in &i.conns {
            nets.insert(n.as_str());
        }
    }
    for p in nl.inputs.iter().chain(nl.outputs.iter()) {
        nets.insert(p.as_str());
    }
    let (hit, total) = act.coverage(nets.iter().copied());
    let pct = if total == 0 {
        0.0
    } else {
        100.0 * hit as f64 / total as f64
    };
    if act.mode() == "vectorless" {
        emit(
            false,
            "POWER-ACTIVITY",
            format!("activity: vectorless — a uniform estimate over all {total} net(s)"),
        );
    } else {
        emit(
            pct < 50.0,
            "POWER-ACTIVITY",
            format!(
                "activity: {} names {hit} of {total} net(s) ({pct:.1}%); the remaining {} take \
                 the vectorless fallback",
                act.mode(),
                total - hit
            ),
        );
    }

    if let Some(sp) = spef {
        let sc = coverage::spef(nl, sp);
        // Parasitics set wire capacitance here. Missing them under-reports switching power
        // rather than invalidating the run, so this is quieter than the timing equivalent.
        emit(
            sc.read_but_unmatched() || sp.nets.is_empty(),
            "POWER-SPEF",
            format!(
                "SPEF covers {} of {} design net(s) ({:.1}%); {} net(s) in the file",
                sc.matched,
                sc.design_nets,
                sc.percent(),
                sc.file_nets
            ),
        );
    }
}

/// A tiny built-in design (no files needed) — `vyges-power demo`.
pub fn demo() -> PowerReport {
    let nl = netlist::parse(DEMO_V).expect("demo netlist");
    let lib = Lib::parse(DEMO_LIB).expect("demo lib");
    // vectorless: 20% activity at 100 MHz
    let act = Activity::vectorless(0.2, 1.0e8);
    let vdd = lib.voltage;
    power::analyze(
        &nl,
        &lib,
        &act,
        &power::AnalyzeOpts {
            vdd,
            clock_port: Some("clk"),
            ..Default::default()
        },
    )
}

// ---- rendering -----------------------------------------------------------------

pub fn render_report(rep: &PowerReport) -> String {
    let mut s = String::new();
    s.push_str(&format!("vyges-power — {}\n", rep.design));
    s.push_str(&format!("  supply (Vdd)     {:.3} V\n", rep.vdd));
    s.push_str(&format!("  activity         {}\n", rep.mode));
    s.push_str(&format!(
        "  wire caps        {}\n",
        if rep.spef {
            format!("SPEF ({} net(s) extracted)", rep.spef_nets)
        } else {
            "flat stand-in".to_string()
        }
    ));
    s.push_str(&format!(
        "  instances        {} ({} unmatched)\n\n",
        rep.insts.len(),
        rep.unmatched.len()
    ));
    s.push_str(&format!("  total power      {}\n", fmt_w(rep.total_w())));
    s.push_str(&format!("    leakage        {}\n", fmt_w(rep.leakage_w)));
    s.push_str(&format!("    internal       {}\n", fmt_w(rep.internal_w)));
    s.push_str(&format!("    net switching  {}\n", fmt_w(rep.switch_w)));
    let i_total: f64 = rep.insts.iter().map(|i| i.avg_current_a).sum();
    s.push_str(&format!("  avg current      {}\n\n", fmt_a(i_total)));

    s.push_str("  by group:        instances     total      leak       dyn\n");
    for g in power::Group::ALL {
        let t = rep.group(g);
        s.push_str(&format!(
            "    {:<14} {:>9} {:>9} {:>9} {:>9}\n",
            g.label(),
            t.instances,
            fmt_w(t.total_w()),
            fmt_w(t.leakage_w),
            fmt_w(t.dynamic_w()),
        ));
    }
    s.push_str(&format!(
        "  clock network is {:.1}% of total power\n",
        rep.clock_share_pct()
    ));
    if rep.clock_grouping_approximate {
        s.push_str(
            "  [warn] the library declares no sequential cell and no clock pin — the group\n\
             \x20        split is inferred from cell shape, and only repeater-shaped cells are\n\
             \x20        counted as clock network\n",
        );
    }
    s.push('\n');

    let mut top = rep.insts.clone();
    top.sort_by(|a, b| {
        b.total_w()
            .partial_cmp(&a.total_w())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    s.push_str("  top instances by power:\n");
    s.push_str("    instance              cell             total      leak      dyn       I_avg\n");
    for i in top.iter().take(10) {
        s.push_str(&format!(
            "    {:<20} {:<15} {:>9} {:>9} {:>9} {:>9}\n",
            trunc(&i.inst, 20),
            trunc(&i.cell, 15),
            fmt_w(i.total_w()),
            fmt_w(i.leakage_w),
            fmt_w(i.dynamic_w()),
            fmt_a(i.avg_current_a),
        ));
    }
    if !rep.unmatched.is_empty() {
        s.push_str(&format!(
            "\n  [warn] {} cell(s) not in any lib — no power counted: {}\n",
            rep.unmatched.len(),
            rep.unmatched.join(", ")
        ));
    }
    s
}

pub fn report_json(rep: &PowerReport) -> String {
    let i_total: f64 = rep.insts.iter().map(|i| i.avg_current_a).sum();
    let mut s = String::new();
    s.push_str("{\n");
    s.push_str(&format!("  \"design\": {},\n", jstr(&rep.design)));
    s.push_str(&format!("  \"vdd\": {:.6},\n", rep.vdd));
    s.push_str(&format!("  \"activity_mode\": {},\n", jstr(&rep.mode)));
    s.push_str(&format!("  \"spef\": {},\n", rep.spef));
    s.push_str(&format!("  \"spef_nets\": {},\n", rep.spef_nets));
    s.push_str(&format!("  \"instances\": {},\n", rep.insts.len()));
    s.push_str(&format!("  \"leakage_w\": {:.6e},\n", rep.leakage_w));
    s.push_str(&format!("  \"internal_w\": {:.6e},\n", rep.internal_w));
    s.push_str(&format!("  \"switch_w\": {:.6e},\n", rep.switch_w));
    s.push_str(&format!("  \"dynamic_w\": {:.6e},\n", rep.dynamic_w()));
    s.push_str(&format!("  \"total_w\": {:.6e},\n", rep.total_w()));
    s.push_str(&format!("  \"total_current_a\": {:.6e},\n", i_total));
    s.push_str(&format!(
        "  \"unmatched_cells\": [{}],\n",
        jlist(&rep.unmatched)
    ));
    s.push_str(&format!(
        "  \"clock_share_pct\": {:.3},\n",
        rep.clock_share_pct()
    ));
    s.push_str("  \"by_group\": {\n");
    for (k, g) in power::Group::ALL.iter().enumerate() {
        let t = rep.group(*g);
        let comma = if k + 1 < power::Group::ALL.len() {
            ","
        } else {
            ""
        };
        s.push_str(&format!(
            "    {}: {{\"instances\": {}, \"leakage_w\": {:.6e}, \"internal_w\": {:.6e}, \"switch_w\": {:.6e}, \"total_w\": {:.6e}}}{}\n",
            jstr(g.label()), t.instances, t.leakage_w, t.internal_w, t.switch_w, t.total_w(), comma
        ));
    }
    s.push_str("  },\n");
    s.push_str("  \"by_instance\": [\n");
    for (k, i) in rep.insts.iter().enumerate() {
        let comma = if k + 1 < rep.insts.len() { "," } else { "" };
        s.push_str(&format!(
            "    {{\"instance\": {}, \"cell\": {}, \"group\": {}, \"total_w\": {:.6e}, \"avg_current_a\": {:.6e}, \"toggle_rate_hz\": {:.6e}}}{}\n",
            jstr(&i.inst), jstr(&i.cell), jstr(i.group.label()), i.total_w(), i.avg_current_a, i.toggle_rate, comma
        ));
    }
    s.push_str("  ]\n}\n");
    s
}

/// The sweep as a person reads it: the peak first (it is the answer), then the curve.
pub fn render_sweep(rep: &SweepReport) -> String {
    let mut s = String::new();
    let peak = rep.peak_window();
    s.push_str(&format!("vyges-power — {} (sweep)\n", rep.design));
    s.push_str(&format!("  supply (Vdd)     {:.3} V\n", rep.vdd));
    s.push_str(&format!("  activity         {}\n", rep.mode));
    s.push_str(&format!(
        "  windows          {} over a {:.3} ns dump\n\n",
        rep.windows.len(),
        rep.dump_end_s * 1e9
    ));
    s.push_str(&format!(
        "  PEAK window #{} [{:.3} ns, {:.3} ns)\n",
        peak.index,
        peak.from_s * 1e9,
        peak.to_s * 1e9
    ));
    s.push_str(&format!("    total power    {}\n", fmt_w(peak.total_w())));
    s.push_str(&format!("    leakage        {}\n", fmt_w(peak.leakage_w)));
    s.push_str(&format!("    dynamic        {}\n", fmt_w(peak.dynamic_w())));
    s.push_str(&format!(
        "    seq/comb/clk   {} / {} / {}\n",
        fmt_w(peak.sequential_w),
        fmt_w(peak.combinational_w),
        fmt_w(peak.clock_w)
    ));
    s.push_str(&format!(
        "  mean power       {}  (peak is {:.2}x the mean)\n\n",
        fmt_w(rep.mean_total_w()),
        rep.peak_to_mean()
    ));

    s.push_str(
        "  window   start(ns)     end(ns)   events      total       seq      comb       clk\n",
    );
    for w in &rep.windows {
        s.push_str(&format!(
            "  {:>6} {:>11.3} {:>11.3} {:>8} {:>10} {:>9} {:>9} {:>9}{}\n",
            w.index,
            w.from_s * 1e9,
            w.to_s * 1e9,
            w.events,
            fmt_w(w.total_w()),
            fmt_w(w.sequential_w),
            fmt_w(w.combinational_w),
            fmt_w(w.clock_w),
            if w.index == rep.peak { "  <- peak" } else { "" },
        ));
    }
    s
}

/// The sweep as a machine reads it: the series, and the peak named explicitly so a consumer
/// does not have to re-derive which row mattered.
pub fn sweep_json(rep: &SweepReport) -> String {
    let mut s = String::new();
    s.push_str("{\n");
    s.push_str(&format!("  \"design\": {},\n", jstr(&rep.design)));
    s.push_str(&format!("  \"vdd\": {:.6},\n", rep.vdd));
    s.push_str(&format!("  \"activity_mode\": {},\n", jstr(&rep.mode)));
    s.push_str(&format!("  \"dump_end_s\": {:.6e},\n", rep.dump_end_s));
    s.push_str(&format!("  \"windows\": {},\n", rep.windows.len()));
    s.push_str(&format!("  \"peak_index\": {},\n", rep.peak));
    s.push_str(&format!(
        "  \"peak_total_w\": {:.6e},\n",
        rep.peak_window().total_w()
    ));
    s.push_str(&format!(
        "  \"mean_total_w\": {:.6e},\n",
        rep.mean_total_w()
    ));
    s.push_str(&format!("  \"peak_to_mean\": {:.6},\n", rep.peak_to_mean()));
    s.push_str("  \"series\": [\n");
    for (k, w) in rep.windows.iter().enumerate() {
        let comma = if k + 1 < rep.windows.len() { "," } else { "" };
        s.push_str(&format!(
            "    {{\"index\": {}, \"from_s\": {:.6e}, \"to_s\": {:.6e}, \"events\": {}, \
             \"internal_w\": {:.6e}, \"switch_w\": {:.6e}, \"leakage_w\": {:.6e}, \"total_w\": {:.6e}, \
             \"sequential_w\": {:.6e}, \"combinational_w\": {:.6e}, \"clock_w\": {:.6e}, \"covered_nets\": {}}}{}\n",
            w.index, w.from_s, w.to_s, w.events, w.internal_w, w.switch_w, w.leakage_w,
            w.total_w(), w.sequential_w, w.combinational_w, w.clock_w, w.covered_nets, comma
        ));
    }
    s.push_str("  ]\n}\n");
    s
}

// ---- formatting helpers --------------------------------------------------------

fn fmt_w(w: f64) -> String {
    let a = w.abs();
    if a >= 1e-3 {
        format!("{:.3} mW", w * 1e3)
    } else if a >= 1e-6 {
        format!("{:.3} uW", w * 1e6)
    } else if a >= 1e-9 {
        format!("{:.3} nW", w * 1e9)
    } else if a == 0.0 {
        "0".to_string()
    } else {
        format!("{:.3} pW", w * 1e12)
    }
}

fn fmt_a(amp: f64) -> String {
    let a = amp.abs();
    if a >= 1e-3 {
        format!("{:.3} mA", amp * 1e3)
    } else if a >= 1e-6 {
        format!("{:.3} uA", amp * 1e6)
    } else if a >= 1e-9 {
        format!("{:.3} nA", amp * 1e9)
    } else if a == 0.0 {
        "0".to_string()
    } else {
        format!("{:.3} pA", amp * 1e12)
    }
}

fn trunc(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n.saturating_sub(1)])
    }
}

fn jstr(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            _ => o.push(c),
        }
    }
    o.push('"');
    o
}

fn jlist(items: &[String]) -> String {
    items.iter().map(|s| jstr(s)).collect::<Vec<_>>().join(", ")
}

const DEMO_V: &str = r#"
module demo (clk, d, q);
  input clk, d;
  output q;
  wire n1;
  INV u_inv (.A(d), .Y(n1));
  DFF u_ff (.CK(clk), .D(n1), .Q(q));
endmodule
"#;

const DEMO_LIB: &str = r#"
library (demo) {
  leakage_power_unit : 1nW;
  time_unit : "1ns";
  capacitive_load_unit (1, pf);
  nom_voltage : 1.8;
  cell (INV) {
    cell_leakage_power : 2.0;
    pin (A) { direction : input; capacitance : 0.004; }
    pin (Y) { direction : output;
      internal_power () { rise_power(t){ values("0.010"); } fall_power(t){ values("0.010"); } } }
  }
  cell (DFF) {
    cell_leakage_power : 5.0;
    pin (CK) { direction : input; capacitance : 0.006; }
    pin (D)  { direction : input; capacitance : 0.004; }
    pin (Q)  { direction : output;
      internal_power () { rise_power(t){ values("0.030"); } fall_power(t){ values("0.030"); } } }
  }
}
"#;

/// The demo design against a library that declares what a real one declares: an `ff` group on
/// the flop and `clock : true` on its clock pin. Used to test the clock/sequential split on the
/// path a real PDK takes.
#[cfg(test)]
const DECLARED_LIB: &str = r#"
library (declared) {
  leakage_power_unit : 1nW;
  time_unit : "1ns";
  capacitive_load_unit (1, pf);
  nom_voltage : 1.8;
  cell (INV) {
    cell_leakage_power : 2.0;
    pin (A) { direction : input; capacitance : 0.004; }
    pin (Y) { direction : output; function : "!A";
      internal_power () { rise_power(t){ values("0.010"); } fall_power(t){ values("0.010"); } } }
  }
  cell (CLKBUF) {
    cell_leakage_power : 4.0;
    pin (A) { direction : input; capacitance : 0.005; }
    pin (X) { direction : output; function : "A";
      internal_power () { rise_power(t){ values("0.020"); } fall_power(t){ values("0.020"); } } }
  }
  cell (DFF) {
    cell_leakage_power : 5.0;
    ff (IQ, IQN) { clocked_on : "CK"; next_state : "D"; }
    pin (CK) { direction : input; clock : true; capacitance : 0.006; }
    pin (D)  { direction : input; capacitance : 0.004; }
    pin (Q)  { direction : output;
      internal_power () { rise_power(t){ values("0.030"); } fall_power(t){ values("0.030"); } } }
  }
}
"#;

/// A clock buffer feeding two flops, one of which drives an inverter — one instance of each
/// group, so a misclassification cannot hide in a total.
#[cfg(test)]
const GROUPED_V: &str = r#"
module grouped (clk_in, d, q0, q1, y);
  input clk_in, d;
  output q0, q1, y;
  wire clk, n1;
  CLKBUF u_clkbuf (.A(clk_in), .X(clk));
  DFF    u_ff0    (.CK(clk), .D(d),  .Q(q0));
  DFF    u_ff1    (.CK(clk), .D(n1), .Q(q1));
  INV    u_inv    (.A(q0),   .Y(n1));
  INV    u_out    (.A(n1),   .Y(y));
endmodule
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn grouped(lib_text: &str) -> PowerReport {
        let nl = netlist::parse(GROUPED_V).expect("netlist");
        let lib = Lib::parse(lib_text).expect("lib");
        let act = Activity::vectorless(0.2, 1.0e8);
        let vdd = lib.voltage;
        power::analyze(
            &nl,
            &lib,
            &act,
            &power::AnalyzeOpts {
                vdd,
                clock_port: Some("clk"),
                ..Default::default()
            },
        )
    }

    #[test]
    fn a_declaring_library_splits_clock_sequential_and_combinational() {
        let rep = grouped(DECLARED_LIB);
        assert!(!rep.clock_grouping_approximate);
        let by = |g| rep.group(g).instances;
        assert_eq!(by(power::Group::Clock), 1, "the clock buffer, and only it");
        assert_eq!(by(power::Group::Sequential), 2, "both flops");
        assert_eq!(by(power::Group::Combinational), 2, "both inverters");
        // The groups partition the design — nothing counted twice, nothing dropped.
        let sum: f64 = power::Group::ALL
            .iter()
            .map(|g| rep.group(*g).total_w())
            .sum();
        assert!((sum - rep.total_w()).abs() < 1e-18);
        assert!(rep.clock_share_pct() > 0.0 && rep.clock_share_pct() < 100.0);
    }

    #[test]
    fn clock_ness_stops_at_a_flops_clock_pin() {
        // THE FAILURE THIS EXISTS TO PREVENT. Propagating through the flops marks q0, then n1,
        // then y as clock nets, and the whole design reports as clock network — which is
        // exactly what happened on the demo library before the clock pin was honoured. A
        // designer reading "clock is 83% of your power" would go optimise a clock tree that was
        // mostly registers and logic.
        let rep = grouped(DECLARED_LIB);
        let group_of = |inst: &str| {
            rep.insts
                .iter()
                .find(|i| i.inst == inst)
                .map(|i| i.group)
                .expect("instance")
        };
        assert_eq!(group_of("u_clkbuf"), power::Group::Clock);
        assert_eq!(group_of("u_ff0"), power::Group::Sequential);
        assert_eq!(group_of("u_inv"), power::Group::Combinational);
        assert_eq!(group_of("u_out"), power::Group::Combinational);
    }

    #[test]
    fn an_under_specified_library_says_so_instead_of_guessing_wide() {
        // DEMO_LIB declares no ff group and no clock pin. The engine cannot tell the flop from
        // the buffer, so it falls back to shape — and, critically, flags that it did. Silence
        // here would be a made-up number with a straight face.
        let rep = grouped(DEMO_LIB);
        assert!(rep.clock_grouping_approximate);
        assert_eq!(rep.group(power::Group::Sequential).instances, 0);
        assert!(
            render_report(&rep).contains("[warn] the library declares no sequential cell"),
            "the caveat has to reach the person reading the report"
        );
        // The two-input flops are not repeaters, so clock-ness stops at them anyway.
        assert!(rep.group(power::Group::Clock).instances <= 1);
    }

    #[test]
    fn demo_has_power_and_current() {
        let rep = demo();
        assert_eq!(rep.insts.len(), 2);
        assert!(rep.leakage_w > 0.0); // 2nW + 5nW
        assert!(rep.total_w() > rep.leakage_w); // dynamic adds on top
        let i: f64 = rep.insts.iter().map(|x| x.avg_current_a).sum();
        assert!(i > 0.0);
        let txt = render_report(&rep);
        assert!(txt.contains("total power"));
        assert!(report_json(&rep).contains("\"total_w\""));
        assert!(rep.activity_map().contains("u_inv"));
    }
}
