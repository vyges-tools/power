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
        let parsed = Lib::load(&job.resolve(l)).map_err(|e| e.to_string())?;
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
        let saif = Saif::load_scoped(&job.resolve(s), job.scope.clone()).map_err(|e| e.to_string())?;
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
    Ok(power::analyze(&nl, &lib, &act, vdd, wire_cap_f, spef.as_ref()))
}

/// A tiny built-in design (no files needed) — `vyges-power demo`.
pub fn demo() -> PowerReport {
    let nl = netlist::parse(DEMO_V).expect("demo netlist");
    let lib = Lib::parse(DEMO_LIB).expect("demo lib");
    // vectorless: 20% activity at 100 MHz
    let act = Activity::vectorless(0.2, 1.0e8);
    power::analyze(&nl, &lib, &act, lib.voltage, 0.0, None)
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

    let mut top = rep.insts.clone();
    top.sort_by(|a, b| b.total_w().partial_cmp(&a.total_w()).unwrap_or(std::cmp::Ordering::Equal));
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
    s.push_str(&format!("  \"unmatched_cells\": [{}],\n", jlist(&rep.unmatched)));
    s.push_str("  \"by_instance\": [\n");
    for (k, i) in rep.insts.iter().enumerate() {
        let comma = if k + 1 < rep.insts.len() { "," } else { "" };
        s.push_str(&format!(
            "    {{\"instance\": {}, \"cell\": {}, \"total_w\": {:.6e}, \"avg_current_a\": {:.6e}, \"toggle_rate_hz\": {:.6e}}}{}\n",
            jstr(&i.inst), jstr(&i.cell), i.total_w(), i.avg_current_a, i.toggle_rate, comma
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

#[cfg(test)]
mod tests {
    use super::*;

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
