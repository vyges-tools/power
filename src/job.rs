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
//! activity:      0.2                # vectorless default toggle factor (used when no vcd/saif)
//! default_wire_cap: 0.0             # pF added to every net's switched cap (optional)
//! power_budget_mw:  5.0             # optional CI gate (--fail-on-budget)
//! emit_activity: block.activity     # optional: write the per-instance map em-ir consumes
//! ```
//!
//! Activity has two vectored sources — a **VCD** or a **SAIF** (`saif`, e.g.
//! Verilator `--trace-saif`); give at most one. Both yield measured per-net toggle
//! rates; without either, the engine falls back to the vectorless `activity` factor
//! × clock for every net. The `emit_activity` output is the seam into `vyges-em-ir`
//! — see `docs/engines-integration.md`.

use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct PwrJob {
    pub design: String,
    pub netlist: String,
    pub libs: Vec<String>,
    pub clock_port: String,
    pub period_ns: f64,
    pub vdd: Option<f64>,             // supply (V); None -> take the lib's nominal
    pub vcd: Option<String>,          // vectored activity source (VCD)
    pub saif: Option<String>,         // vectored activity source (SAIF); exclusive with vcd
    pub fst: Option<String>,          // vectored activity source (FST binary); exclusive with vcd/saif
    pub activity_window: Option<(f64, Option<f64>)>, // VCD-only: count [from, to) seconds; None=full dump
    pub scope: Option<String>,        // design instance path in the VCD/SAIF (disambiguates leaf names)
    pub spef: Option<String>,         // extracted wire parasitics (from vyges-extract)
    pub activity_factor: f64,         // vectorless default toggle factor (0..1), default 0.2
    pub default_wire_cap_pf: f64,     // pF added per net (crude wire-cap stand-in)
    pub power_budget_mw: Option<f64>, // CI gate with --fail-on-budget
    pub emit_activity: Option<String>,
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
            Path::new(&self.base_dir).join(rel).to_string_lossy().into_owned()
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
                let period: f64 =
                    per.parse().map_err(|_| JobError(format!("bad clock period: {per:?}")))?;
                clock = Some((port.to_string(), period));
                continue;
            }
            kv.insert(key, val);
        }
        let get = |k: &str| kv.get(k).cloned().ok_or_else(|| JobError(format!("missing key: {k}")));
        let num = |k: &str, dflt: f64| -> Result<f64, JobError> {
            match kv.get(k) {
                Some(s) => s.parse().map_err(|_| JobError(format!("bad number for {k}: {s:?}"))),
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
                            "activity_window needs 'from [to]' with units, e.g. '200ns 1200ns'".into(),
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
            scope: kv.get("scope").filter(|s| !s.is_empty()).cloned(),
            spef: kv.get("spef").filter(|s| !s.is_empty()).cloned(),
            activity_factor: num("activity", 0.2)?,
            default_wire_cap_pf: num("default_wire_cap", 0.0)?,
            power_budget_mw: kv.get("power_budget_mw").and_then(|s| s.parse().ok()),
            emit_activity: kv.get("emit_activity").filter(|s| !s.is_empty()).cloned(),
            base_dir: base_dir.to_string(),
        })
    }

    pub fn load(path: &str) -> Result<PwrJob, JobError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| JobError(format!("{path}: {e}")))?;
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
    let units = [("fs", 1e-15), ("ps", 1e-12), ("ns", 1e-9), ("us", 1e-6), ("ms", 1e-3), ("s", 1.0)];
    for (suf, scale) in units {
        if let Some(num) = s.strip_suffix(suf) {
            let n: f64 = num
                .trim()
                .parse()
                .map_err(|_| JobError(format!("bad time value: {tok:?}")))?;
            return Ok(n * scale);
        }
    }
    Err(JobError(format!("time needs a unit (fs/ps/ns/us/ms/s): {tok:?}")))
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
        assert!(PwrJob::parse(&format!("{WIN_BASE}fst: b.fst\nactivity_window: 200ns\n"), "").is_ok());
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
    fn window_needs_unit_and_ordering() {
        // bare number, no unit -> error
        assert!(PwrJob::parse(&format!("{WIN_BASE}vcd: b.vcd\nactivity_window: 200\n"), "").is_err());
        // to <= from -> error
        assert!(PwrJob::parse(
            &format!("{WIN_BASE}vcd: b.vcd\nactivity_window: 200ns 100ns\n"),
            ""
        )
        .is_err());
    }
}
