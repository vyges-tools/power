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
        if let Some(s) = &self.saif {
            format!("saif:{s}")
        } else if let Some(v) = &self.vcd {
            v.clone()
        } else {
            "vectorless".to_string()
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
        if vcd.is_some() && saif.is_some() {
            return Err(JobError("specify only one vectored source: 'vcd' or 'saif'".into()));
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
}
