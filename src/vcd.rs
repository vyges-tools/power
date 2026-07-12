//! Minimal VCD reader for **vectored** activity: per-signal transition counts over
//! the dump window, so power can use measured toggle rates instead of an estimate.
//!
//! v0 scope: scalar signals (1-bit) mapped by leaf name; a vector change counts as
//! one transition. Hierarchical scopes are flattened to the leaf name (the netlist
//! nets are top-level). Depth reserved: bit-level vector toggling, SAIF, scope-aware
//! name resolution.

use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Vcd {
    pub toggles: HashMap<String, u64>, // signal name -> transition count
    pub sim_time_s: f64,               // total dumped time in seconds
}

#[derive(Debug)]
pub struct VcdError(pub String);
impl std::fmt::Display for VcdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "vcd error: {}", self.0)
    }
}
impl std::error::Error for VcdError {}

impl Vcd {
    /// Transitions / second for a net (0 if absent or zero-duration sim).
    pub fn toggle_rate(&self, name: &str) -> f64 {
        match self.toggles.get(name) {
            Some(&n) if self.sim_time_s > 0.0 => n as f64 / self.sim_time_s,
            _ => 0.0,
        }
    }

    pub fn load(path: &str) -> Result<Vcd, VcdError> {
        Vcd::load_windowed(path, None)
    }

    /// Like [`load`](Vcd::load), but restrict activity to a `[from, to)` time window
    /// (seconds; `to = None` runs to end-of-dump). See [`parse_windowed`](Vcd::parse_windowed).
    pub fn load_windowed(path: &str, window: Option<(f64, Option<f64>)>) -> Result<Vcd, VcdError> {
        let text = std::fs::read_to_string(path).map_err(|e| VcdError(format!("{path}: {e}")))?;
        Vcd::parse_windowed(&text, window)
    }

    pub fn parse(text: &str) -> Result<Vcd, VcdError> {
        Vcd::parse_windowed(text, None)
    }

    /// Parse a VCD into per-net transition counts. When `window = Some((from, to))`,
    /// only transitions with `from <= t < to` (seconds) are counted and `sim_time_s`
    /// is the window duration (clamped to the dumped span); `to = None` runs to
    /// end-of-dump. All value changes still update signal state, so the first
    /// in-window change is measured against the correct pre-window value. Windowing
    /// excludes reset/boot from the measurement (VCD only — SAIF is already cumulative).
    pub fn parse_windowed(
        text: &str,
        window: Option<(f64, Option<f64>)>,
    ) -> Result<Vcd, VcdError> {
        let from_s = window.map(|(f, _)| f).unwrap_or(0.0);
        let to_opt = window.and_then(|(_, t)| t);
        // Does the *current* sim time fall in the counting window? Re-evaluated at each `#t`.
        let in_window = |t: f64| t >= from_s && match to_opt {
            Some(to) => t < to,
            None => true,
        };

        let mut tick_s = 1.0e-9; // default 1ns
        let mut sym2name: HashMap<String, String> = HashMap::new();
        let mut last: HashMap<String, char> = HashMap::new();
        let mut toggles: HashMap<String, u64> = HashMap::new();
        let mut time_ticks: u64 = 0;
        let mut count_now = in_window(0.0);

        let mut toks = text.split_whitespace().peekable();
        while let Some(tok) = toks.next() {
            match tok {
                "$timescale" => {
                    // e.g. "1ns" or "1" then "ns"
                    let mut unit = String::new();
                    for t in toks.by_ref() {
                        if t == "$end" {
                            break;
                        }
                        unit.push_str(t);
                    }
                    tick_s = parse_timescale(&unit);
                }
                "$var" => {
                    // $var <type> <width> <sym> <name> [range] $end
                    let _ty = toks.next();
                    let _w = toks.next();
                    let sym = toks.next().unwrap_or("").to_string();
                    let name = toks.next().unwrap_or("").to_string();
                    // consume up to $end (drops any [msb:lsb])
                    for t in toks.by_ref() {
                        if t == "$end" {
                            break;
                        }
                    }
                    if !sym.is_empty() && !name.is_empty() {
                        sym2name.insert(sym, name);
                    }
                }
                _ => {
                    if let Some(rest) = tok.strip_prefix('#') {
                        if let Ok(t) = rest.parse::<u64>() {
                            time_ticks = t;
                            count_now = in_window(time_ticks as f64 * tick_s);
                        }
                    } else if let Some(first) = tok.chars().next() {
                        match first {
                            '0' | '1' | 'x' | 'X' | 'z' | 'Z' => {
                                // scalar change: <value><sym>
                                let sym = &tok[1..];
                                if let Some(name) = sym2name.get(sym) {
                                    let v = first.to_ascii_lowercase();
                                    let prev = last.insert(name.clone(), v);
                                    if count_now && prev.map(|p| p != v).unwrap_or(false) {
                                        *toggles.entry(name.clone()).or_insert(0) += 1;
                                    }
                                }
                            }
                            'b' | 'B' | 'r' | 'R' => {
                                // vector/real change: <value> <sym> (sym is the next token)
                                if let Some(sym) = toks.next() {
                                    if count_now {
                                        if let Some(name) = sym2name.get(sym) {
                                            *toggles.entry(name.clone()).or_insert(0) += 1;
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        let last_time_s = time_ticks as f64 * tick_s;
        let sim_time_s = match window {
            None => last_time_s,
            Some((f, t)) => {
                let eff_from = f.clamp(0.0, last_time_s);
                let eff_to = t.unwrap_or(last_time_s).clamp(eff_from, last_time_s);
                eff_to - eff_from
            }
        };
        Ok(Vcd { toggles, sim_time_s })
    }
}

fn parse_timescale(s: &str) -> f64 {
    let s = s.trim().to_lowercase();
    let units = [("fs", 1e-15), ("ps", 1e-12), ("ns", 1e-9), ("us", 1e-6), ("ms", 1e-3), ("s", 1.0)];
    for (suf, scale) in units {
        if let Some(num) = s.strip_suffix(suf) {
            let n: f64 = num.trim().parse().unwrap_or(1.0);
            return n * scale;
        }
    }
    1.0e-9
}

#[cfg(test)]
mod tests {
    use super::*;

    const VCD: &str = r#"
$timescale 1ns $end
$scope module top $end
$var wire 1 ! clk $end
$var wire 1 " a $end
$upscope $end
$enddefinitions $end
#0
0!
0"
#5
1!
1"
#10
0!
#15
1!
#20
0!
"#;

    #[test]
    fn counts_transitions_and_time() {
        let v = Vcd::parse(VCD).unwrap();
        assert!((v.sim_time_s - 20.0e-9).abs() < 1e-18);
        // clk: 0->1->0->1->0 = 4 transitions over 20 ns -> 200 MHz toggle rate
        assert_eq!(*v.toggles.get("clk").unwrap(), 4);
        assert!((v.toggle_rate("clk") - 2.0e8).abs() < 1.0);
        assert_eq!(*v.toggles.get("a").unwrap(), 1);
    }

    #[test]
    fn window_restricts_and_rescales() {
        // [5ns,15ns): clk transitions at t=5 (0->1) and t=10 (1->0); t=15 excluded.
        let v = Vcd::parse_windowed(VCD, Some((5.0e-9, Some(15.0e-9)))).unwrap();
        assert_eq!(*v.toggles.get("clk").unwrap(), 2);
        assert!((v.sim_time_s - 10.0e-9).abs() < 1e-18);
        assert!((v.toggle_rate("clk") - 2.0e8).abs() < 1.0);
        // 'a' toggles once, at t=5 -> inside the window
        assert_eq!(*v.toggles.get("a").unwrap(), 1);
    }

    #[test]
    fn window_open_ended_runs_to_end() {
        // [10ns, end]: clk transitions at 10, 15, 20 all counted (no upper bound).
        let v = Vcd::parse_windowed(VCD, Some((10.0e-9, None))).unwrap();
        assert_eq!(*v.toggles.get("clk").unwrap(), 3);
        assert!((v.sim_time_s - 10.0e-9).abs() < 1e-18); // 20ns dump end - 10ns from
    }

    #[test]
    fn window_outside_dump_is_zero_duration() {
        // Beyond the dump -> zero duration, zero rates, no crash.
        let v = Vcd::parse_windowed(VCD, Some((100.0e-9, Some(200.0e-9)))).unwrap();
        assert!(v.sim_time_s.abs() < 1e-18);
        assert_eq!(v.toggle_rate("clk"), 0.0);
        assert_eq!(v.toggles.get("clk").copied().unwrap_or(0), 0);
    }

    #[test]
    fn no_window_matches_full_dump() {
        // parse() == parse_windowed(None): unchanged behaviour.
        let full = Vcd::parse(VCD).unwrap();
        let none = Vcd::parse_windowed(VCD, None).unwrap();
        assert_eq!(full.toggles.get("clk"), none.toggles.get("clk"));
        assert!((full.sim_time_s - none.sim_time_s).abs() < 1e-18);
    }
}
