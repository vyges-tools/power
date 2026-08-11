//! Power job: the declarative description of what to analyze.
//!
//! A `.pwr` job is a tiny `key: value` file (std-only parser — no deps):
//!
//! ```text
//! design:        block
//! netlist:       block.v            # gate-level structural Verilog
//! lib:           sky130_hd.lib      # one or more (comma-separated)
//! clock:         clk 10.0           # clock port + period (ns) -> frequency
//! vdd:           1.8                # supply voltage (V); optional, else from the lib
//! vcd:           block.vcd          # optional: vectored activity (VCD toggle counts)
//! saif:          block.saif         # optional: vectored activity (SAIF); exclusive with vcd
//! fst:           block.fst          # optional: vectored activity (FST binary); exclusive with vcd/saif
//! scope:         tb.dut             # optional: instance path where the design's nets live
//! activity_window: 200ns 1200ns     # optional (VCD only): count toggles in [from,to) only
//! activity_sweep: 0ns end 50ns      # optional (VCD only): power per 50ns window, over time
//! activity:      0.2                # vectorless default toggle factor (used when no vcd/saif)
//! default_wire_cap: 0.0             # pF added to every net's switched cap (optional)
//! power_budget_mw:  5.0             # optional CI gate (--fail-on-budget)
//! emit_activity: block.activity     # optional: write the per-instance map em-ir consumes
//! emit_power_vcd: block-power.vcd   # optional (sweep only): the power curve, as a VCD copy
//! ```
//!
//! Activity has two vectored sources — a **VCD** or a **SAIF** (`saif`, e.g.
//! Verilator `--trace-saif`); give at most one. Both yield measured per-net toggle
//! rates; without either, the engine falls back to the vectorless `activity` factor
//! × clock for every net. The `emit_activity` output is the seam into `vyges-em-ir`
//! — see `docs/engines-integration.md`.

use std::collections::BTreeMap;
use std::path::Path;

/// A uniform sweep of measurement windows over the dump — the `activity_sweep:` key.
///
/// `from to window [step]`, each with a unit; `to` may be the word `end`. `step` defaults to
/// `window` (consecutive, non-overlapping). A smaller step overlaps the windows, a larger one
/// samples the dump instead of covering it.
#[derive(Debug, Clone, Copy)]
pub struct ActivitySweep {
    pub from_s: f64,
    pub to_s: Option<f64>, // None = to the end of the dump
    pub window_s: f64,
    pub step_s: f64,
}

impl ActivitySweep {
    /// Windows this sweep would measure over a dump ending at `dump_end_s`. Used to refuse an
    /// unreasonable request before reading the dump rather than after allocating for it.
    pub fn window_count(&self, dump_end_s: f64) -> f64 {
        let to = self.to_s.unwrap_or(dump_end_s);
        ((to - self.from_s) / self.step_s).ceil().max(0.0)
    }
}

#[derive(Debug, Clone)]
pub struct PwrJob {
    pub design: String,
    pub netlist: String,
    pub libs: Vec<String>,
    pub clock_port: String,
    pub period_ns: f64,
    pub vdd: Option<f64>,     // supply (V); None -> take the lib's nominal
    pub vcd: Option<String>,  // vectored activity source (VCD)
    pub saif: Option<String>, // vectored activity source (SAIF); exclusive with vcd
    pub fst: Option<String>,  // vectored activity source (FST binary); exclusive with vcd/saif
    pub activity_window: Option<(f64, Option<f64>)>, // VCD-only: count [from, to) seconds; None=full dump
    pub activity_sweep: Option<ActivitySweep>, // VCD-only: power over time; exclusive with activity_window
    pub scope: Option<String>, // design instance path in the VCD/SAIF (disambiguates leaf names)
    pub spef: Option<String>,  // extracted wire parasitics (from vyges-extract)
    pub activity_factor: f64,  // vectorless default toggle factor (0..1), default 0.2
    pub default_wire_cap_pf: f64, // pF added per net (crude wire-cap stand-in)
    pub power_budget_mw: Option<f64>, // CI gate with --fail-on-budget
    pub emit_activity: Option<String>,
    /// Write a copy of the activity VCD carrying the swept power curve as extra signals, so
    /// the result can be read in a waveform viewer against the workload that produced it.
    /// Sweep-only — a single measurement is one number, and a number is not a curve.
    pub emit_power_vcd: Option<String>,
    pub base_dir: String,
}

impl PwrJob {
    /// Human-readable activity source for status lines (`saif:…`, the VCD path, or
    /// `vectorless`).
    pub fn activity_desc(&self) -> String {
        let base = if let Some(s) = &self.saif {
            format!("saif:{s}")
        } else if let Some(f) = &self.fst {
            format!("fst:{f}")
        } else if let Some(v) = &self.vcd {
            v.clone()
        } else {
            "vectorless".to_string()
        };
        if let Some(s) = self.activity_sweep {
            let to = match s.to_s {
                Some(t) => format!("{:.3}ns", t * 1e9),
                None => "end".to_string(),
            };
            let step = if (s.step_s - s.window_s).abs() < f64::EPSILON {
                String::new()
            } else {
                format!(" step {:.3}ns", s.step_s * 1e9)
            };
            return format!(
                "{base} sweep [{:.3}ns,{to}) window {:.3}ns{step}",
                s.from_s * 1e9,
                s.window_s * 1e9
            );
        }
        match self.activity_window {
            Some((f, Some(t))) => format!("{base} [{:.3}ns,{:.3}ns)", f * 1e9, t * 1e9),
            Some((f, None)) => format!("{base} [{:.3}ns,end)", f * 1e9),
            None => base,
        }
    }

    /// Clock frequency in Hz, derived from the period (ns).
    pub fn freq_hz(&self) -> f64 {
        if self.period_ns > 0.0 {
            1.0e9 / self.period_ns
        } else {
            0.0
        }
    }

    /// Resolve a job-relative path against the job's directory.
    pub fn resolve(&self, rel: &str) -> String {
        let p = Path::new(rel);
        if p.is_absolute() || self.base_dir.is_empty() {
            rel.to_string()
        } else {
            Path::new(&self.base_dir)
                .join(rel)
                .to_string_lossy()
                .into_owned()
        }
    }

    pub fn parse(text: &str, base_dir: &str) -> Result<PwrJob, JobError> {
        let mut kv: BTreeMap<String, String> = BTreeMap::new();
        let mut clock: Option<(String, f64)> = None;
        for raw in text.lines() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            let (k, v) = line
                .split_once(':')
                .ok_or_else(|| JobError(format!("expected 'key: value', got {line:?}")))?;
            let key = k.trim().to_lowercase();
            let val = v.trim().to_string();
            if key == "clock" {
                let toks: Vec<&str> = val.split_whitespace().collect();
                let [port, per] = toks.as_slice() else {
                    return Err(JobError("clock needs 'port period_ns'".into()));
                };
                let period: f64 = per
                    .parse()
                    .map_err(|_| JobError(format!("bad clock period: {per:?}")))?;
                clock = Some((port.to_string(), period));
                continue;
            }
            kv.insert(key, val);
        }
        let get = |k: &str| {
            kv.get(k)
                .cloned()
                .ok_or_else(|| JobError(format!("missing key: {k}")))
        };
        let num = |k: &str, dflt: f64| -> Result<f64, JobError> {
            match kv.get(k) {
                Some(s) => s
                    .parse()
                    .map_err(|_| JobError(format!("bad number for {k}: {s:?}"))),
                None => Ok(dflt),
            }
        };
        let libs: Vec<String> = get("lib")?
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        if libs.is_empty() {
            return Err(JobError("at least one lib is required".into()));
        }
        let (clock_port, period_ns) = clock.ok_or_else(|| JobError("missing key: clock".into()))?;

        let vcd = kv.get("vcd").filter(|s| !s.is_empty()).cloned();
        let saif = kv.get("saif").filter(|s| !s.is_empty()).cloned();
        let fst = kv.get("fst").filter(|s| !s.is_empty()).cloned();
        if [&vcd, &saif, &fst].iter().filter(|s| s.is_some()).count() > 1 {
            return Err(JobError(
                "specify only one vectored source: 'vcd', 'saif', or 'fst'".into(),
            ));
        }

        // activity_window: VCD-only steady-state window `from [to]`, each with a unit.
        let activity_window = match kv.get("activity_window").filter(|s| !s.is_empty()) {
            None => None,
            Some(s) => {
                let toks: Vec<&str> = s.split_whitespace().collect();
                let win = match toks.as_slice() {
                    [f] => (parse_time(f)?, None),
                    [f, t] => (parse_time(f)?, Some(parse_time(t)?)),
                    _ => {
                        return Err(JobError(
                            "activity_window needs 'from [to]' with units, e.g. '200ns 1200ns'"
                                .into(),
                        ))
                    }
                };
                if let (from, Some(to)) = win {
                    if to <= from {
                        return Err(JobError(
                            "activity_window 'to' must be greater than 'from'".into(),
                        ));
                    }
                }
                Some(win)
            }
        };
        // Windowing needs a per-transition timeline: VCD or FST (both have one). SAIF is
        // cumulative and vectorless has nothing to window — fail fast rather than no-op.
        if activity_window.is_some() && vcd.is_none() && fst.is_none() {
            return Err(JobError(
                "activity_window requires a 'vcd:' or 'fst:' source (SAIF is cumulative — re-dump a windowed sim instead)".into(),
            ));
        }

        // activity_sweep: `from to window [step]`, `to` may be `end`.
        let activity_sweep = match kv.get("activity_sweep").filter(|s| !s.is_empty()) {
            None => None,
            Some(s) => {
                let toks: Vec<&str> = s.split_whitespace().collect();
                let (f, t, w, st) = match toks.as_slice() {
                    [f, t, w] => (*f, *t, *w, None),
                    [f, t, w, st] => (*f, *t, *w, Some(*st)),
                    _ => {
                        return Err(JobError(
                            "activity_sweep needs 'from to window [step]' with units, e.g. '0ns end 50ns'"
                                .into(),
                        ))
                    }
                };
                let from_s = parse_time(f)?;
                // `end` sweeps to the end of the dump, so a job does not have to hard-code how
                // long the simulation happened to run — the common case, and the one a
                // hard-coded number silently truncates when the testbench grows.
                let to_s = if t.eq_ignore_ascii_case("end") {
                    None
                } else {
                    Some(parse_time(t)?)
                };
                let window_s = parse_time(w)?;
                let step_s = match st {
                    Some(x) => parse_time(x)?,
                    None => window_s,
                };
                if window_s <= 0.0 || step_s <= 0.0 {
                    return Err(JobError(
                        "activity_sweep window and step must be positive durations".into(),
                    ));
                }
                if let Some(to) = to_s {
                    if to <= from_s {
                        return Err(JobError(
                            "activity_sweep 'to' must be greater than 'from'".into(),
                        ));
                    }
                }
                Some(ActivitySweep {
                    from_s,
                    to_s,
                    window_s,
                    step_s,
                })
            }
        };
        // One measurement or many, not both: a job asking for both is ambiguous about which
        // number `--fail-on-budget` gates on, and guessing an answer to that is worse than
        // asking.
        if activity_sweep.is_some() && activity_window.is_some() {
            return Err(JobError(
                "give either activity_window (one measurement) or activity_sweep (many), not both"
                    .into(),
            ));
        }
        // The sweep needs a per-transition timeline AND per-window counts. The VCD reader has
        // both; FST's reader windows but does not yet bucket, so accepting it here would
        // silently measure something else.
        if activity_sweep.is_some() && vcd.is_none() {
            return Err(JobError(
                "activity_sweep requires a 'vcd:' source (FST sweep is not implemented yet; SAIF is cumulative)".into(),
            ));
        }

        // The power curve is written as a copy of the dump it was measured from, so it needs
        // both a sweep to have a curve at all and the VCD to copy.
        let emit_power_vcd = kv.get("emit_power_vcd").filter(|s| !s.is_empty()).cloned();
        if emit_power_vcd.is_some() && activity_sweep.is_none() {
            return Err(JobError(
                "emit_power_vcd requires an 'activity_sweep:' — a single measurement is one number, not a curve".into(),
            ));
        }

        Ok(PwrJob {
            design: get("design")?,
            netlist: get("netlist")?,
            libs,
            clock_port,
            period_ns,
            vdd: kv.get("vdd").and_then(|s| s.parse().ok()),
            vcd,
            saif,
            fst,
            activity_window,
            activity_sweep,
            scope: kv.get("scope").filter(|s| !s.is_empty()).cloned(),
            spef: kv.get("spef").filter(|s| !s.is_empty()).cloned(),
            activity_factor: num("activity", 0.2)?,
            default_wire_cap_pf: num("default_wire_cap", 0.0)?,
            power_budget_mw: kv.get("power_budget_mw").and_then(|s| s.parse().ok()),
            emit_activity: kv.get("emit_activity").filter(|s| !s.is_empty()).cloned(),
            emit_power_vcd,
            base_dir: base_dir.to_string(),
        })
    }

    pub fn load(path: &str) -> Result<PwrJob, JobError> {
        let text = std::fs::read_to_string(path).map_err(|e| JobError(format!("{path}: {e}")))?;
        let base_dir = Path::new(path)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        PwrJob::parse(&text, &base_dir)
    }
}

#[derive(Debug)]
pub struct JobError(pub String);
impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "job error: {}", self.0)
    }
}
impl std::error::Error for JobError {}

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(i) => &line[..i],
        None => line,
    }
}

/// Parse a time token `<number><unit>` (unit required: fs/ps/ns/us/ms/s) into seconds.
/// A unit is mandatory so the value is unambiguous vs. the VCD's per-file `$timescale`.
fn parse_time(tok: &str) -> Result<f64, JobError> {
    let s = tok.trim().to_lowercase();
    // Longer suffixes first so "ms"/"ns"/etc. win before the bare "s".
    let units = [
        ("fs", 1e-15),
        ("ps", 1e-12),
        ("ns", 1e-9),
        ("us", 1e-6),
        ("ms", 1e-3),
        ("s", 1.0),
    ];
    for (suf, scale) in units {
        if let Some(num) = s.strip_suffix(suf) {
            let n: f64 = num
                .trim()
                .parse()
                .map_err(|_| JobError(format!("bad time value: {tok:?}")))?;
            return Ok(n * scale);
        }
    }
    Err(JobError(format!(
        "time needs a unit (fs/ps/ns/us/ms/s): {tok:?}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_job() {
        let j = PwrJob::parse(
            "design: b\nnetlist: b.v\nlib: a.lib, c.lib\nclock: clk 10.0\nactivity: 0.15\n",
            "/tmp",
        )
        .unwrap();
        assert_eq!(j.design, "b");
        assert_eq!(j.libs.len(), 2);
        assert_eq!(j.clock_port, "clk");
        assert!((j.freq_hz() - 1.0e8).abs() < 1.0); // 10 ns -> 100 MHz
        assert!((j.activity_factor - 0.15).abs() < 1e-9);
        assert_eq!(j.resolve("b.v"), "/tmp/b.v");
    }

    #[test]
    fn missing_clock_errors() {
        let r = PwrJob::parse("design: b\nnetlist: b.v\nlib: a.lib\n", "");
        assert!(r.is_err());
    }

    const WIN_BASE: &str = "design: b\nnetlist: b.v\nlib: a.lib\nclock: clk 10\n";

    #[test]
    fn parses_activity_window() {
        let j = PwrJob::parse(
            &format!("{WIN_BASE}vcd: b.vcd\nactivity_window: 200ns 1200ns\n"),
            "",
        )
        .unwrap();
        let (f, t) = j.activity_window.unwrap();
        assert!((f - 200e-9).abs() < 1e-18);
        assert!((t.unwrap() - 1200e-9).abs() < 1e-18);
        assert!(j.activity_desc().contains("200.000ns"));
    }

    #[test]
    fn open_ended_window() {
        let j =
            PwrJob::parse(&format!("{WIN_BASE}vcd: b.vcd\nactivity_window: 1us\n"), "").unwrap();
        let (f, t) = j.activity_window.unwrap();
        assert!((f - 1e-6).abs() < 1e-18);
        assert!(t.is_none());
    }

    #[test]
    fn parses_fst_and_exclusivity() {
        let j = PwrJob::parse(&format!("{WIN_BASE}fst: b.fst\n"), "").unwrap();
        assert_eq!(j.fst.as_deref(), Some("b.fst"));
        assert!(j.activity_desc().starts_with("fst:"));
        // only one vectored source
        assert!(PwrJob::parse(&format!("{WIN_BASE}vcd: b.vcd\nfst: b.fst\n"), "").is_err());
        // activity_window is allowed with fst (FST has a per-transition timeline)
        assert!(PwrJob::parse(
            &format!("{WIN_BASE}fst: b.fst\nactivity_window: 200ns\n"),
            ""
        )
        .is_ok());
    }

    #[test]
    fn window_requires_vcd() {
        // vectorless + window -> error
        assert!(PwrJob::parse(&format!("{WIN_BASE}activity_window: 200ns\n"), "").is_err());
        // saif + window -> error (SAIF is cumulative, cannot be windowed)
        assert!(PwrJob::parse(
            &format!("{WIN_BASE}saif: b.saif\nactivity_window: 200ns\n"),
            ""
        )
        .is_err());
    }

    #[test]
    fn parses_activity_sweep() {
        let j = PwrJob::parse(
            &format!("{WIN_BASE}vcd: b.vcd\nactivity_sweep: 0ns 1us 50ns\n"),
            "",
        )
        .unwrap();
        let s = j.activity_sweep.unwrap();
        assert!((s.from_s).abs() < 1e-18);
        assert!((s.to_s.unwrap() - 1e-6).abs() < 1e-18);
        assert!((s.window_s - 50e-9).abs() < 1e-18);
        assert!(
            (s.step_s - s.window_s).abs() < 1e-18,
            "step defaults to window"
        );
        assert_eq!(s.window_count(1e-6), 20.0);
        assert!(j.activity_desc().contains("sweep"));
    }

    #[test]
    fn sweep_to_end_and_explicit_step() {
        let j = PwrJob::parse(
            &format!("{WIN_BASE}vcd: b.vcd\nactivity_sweep: 100ns end 50ns 25ns\n"),
            "",
        )
        .unwrap();
        let s = j.activity_sweep.unwrap();
        assert!(s.to_s.is_none(), "`end` sweeps to the end of the dump");
        assert!((s.step_s - 25e-9).abs() < 1e-18, "overlapping windows");
        assert!(j.activity_desc().contains("step"));
    }

    #[test]
    fn sweep_rejects_what_it_cannot_measure() {
        let bad =
            |line: &str| PwrJob::parse(&format!("{WIN_BASE}vcd: b.vcd\n{line}\n"), "").is_err();
        assert!(bad("activity_sweep: 0ns 1us"), "needs from/to/window");
        assert!(bad("activity_sweep: 0 1us 50ns"), "units are mandatory");
        assert!(
            bad("activity_sweep: 0ns 1us 0ns"),
            "a zero window is not a measurement"
        );
        assert!(
            bad("activity_sweep: 1us 100ns 50ns"),
            "'to' must follow 'from'"
        );
        // One measurement or many, never both — otherwise --fail-on-budget is ambiguous.
        assert!(bad(
            "activity_sweep: 0ns 1us 50ns\nactivity_window: 0ns 1us"
        ));
        // The sweep needs per-window counts, which only the VCD reader produces today.
        assert!(PwrJob::parse(
            &format!("{WIN_BASE}saif: b.saif\nactivity_sweep: 0ns 1us 50ns\n"),
            ""
        )
        .is_err());
        assert!(PwrJob::parse(
            &format!("{WIN_BASE}fst: b.fst\nactivity_sweep: 0ns 1us 50ns\n"),
            ""
        )
        .is_err());
    }

    #[test]
    fn window_needs_unit_and_ordering() {
        // bare number, no unit -> error
        assert!(
            PwrJob::parse(&format!("{WIN_BASE}vcd: b.vcd\nactivity_window: 200\n"), "").is_err()
        );
        // to <= from -> error
        assert!(PwrJob::parse(
            &format!("{WIN_BASE}vcd: b.vcd\nactivity_window: 200ns 100ns\n"),
            ""
        )
        .is_err());
    }
}
