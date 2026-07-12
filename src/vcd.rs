//! Minimal VCD reader for **vectored** activity: per-signal transition counts over
//! the dump window, so power can use measured toggle rates instead of an estimate.
//!
//! v0 scope: scalar signals (1-bit); a vector change counts as one transition.
//! Signals are keyed by their **full hierarchical path** (`$scope`/`$upscope`), and a
//! netlist net resolves to one of them by leaf + optional `scope:` — see [`crate::names`].
//! Depth reserved: bit-level vector toggling, FST.

use std::collections::HashMap;

use crate::names::NetIndex;

#[derive(Debug, Clone, Default)]
pub struct Vcd {
    pub idx: NetIndex,   // full-path toggle counts + leaf index + optional design scope
    pub sim_time_s: f64, // total dumped time in seconds
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
    /// Transitions / second for a netlist net (0 if unresolved, ambiguous, or
    /// zero-duration sim). Resolution is scope-aware — see [`crate::names::NetIndex`].
    pub fn toggle_rate(&self, net: &str) -> f64 {
        match self.idx.resolve(net) {
            Some(n) if self.sim_time_s > 0.0 => n as f64 / self.sim_time_s,
            _ => 0.0,
        }
    }

    /// Set the design scope (job `scope:`) used to disambiguate leaf names.
    pub fn with_scope(mut self, scope: Option<String>) -> Self {
        self.idx.scope = scope;
        self
    }

    /// Number of leaf names declared under more than one scope (collision risk when
    /// no `scope:` is set).
    pub fn collisions(&self) -> usize {
        self.idx.collisions()
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

    /// Load with both a time window and a design scope.
    pub fn load_scoped(
        path: &str,
        window: Option<(f64, Option<f64>)>,
        scope: Option<String>,
    ) -> Result<Vcd, VcdError> {
        Ok(Vcd::load_windowed(path, window)?.with_scope(scope))
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
        let mut sym2name: HashMap<String, String> = HashMap::new(); // sym -> full hierarchical path
        let mut last: HashMap<String, char> = HashMap::new();
        let mut idx = NetIndex::default();
        let mut scope_stack: Vec<String> = Vec::new();
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
                "$scope" => {
                    // $scope <type> <name> $end
                    let _ty = toks.next();
                    let name = toks.next().unwrap_or("").to_string();
                    for t in toks.by_ref() {
                        if t == "$end" {
                            break;
                        }
                    }
                    if !name.is_empty() {
                        scope_stack.push(name);
                    }
                }
                "$upscope" => {
                    for t in toks.by_ref() {
                        if t == "$end" {
                            break;
                        }
                    }
                    scope_stack.pop();
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
                        let full = if scope_stack.is_empty() {
                            name
                        } else {
                            format!("{}.{}", scope_stack.join("."), name)
                        };
                        idx.declare(&full);
                        sym2name.insert(sym, full);
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
                                        idx.add_toggles(name, 1);
                                    }
                                }
                            }
                            'b' | 'B' | 'r' | 'R' => {
                                // vector/real change: <value> <sym> (sym is the next token)
                                if let Some(sym) = toks.next() {
                                    if count_now {
                                        if let Some(name) = sym2name.get(sym) {
                                            idx.add_toggles(name, 1);
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
        Ok(Vcd { idx, sim_time_s })
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

    // Two scopes with a colliding leaf `clk`: a top-level one and a nested `dut` one.
    const VCD_HIER: &str = r#"
$timescale 1ns $end
$scope module tb $end
$var wire 1 ! clk $end
$scope module dut $end
$var wire 1 @ clk $end
$upscope $end
$upscope $end
$enddefinitions $end
#0
0!
0@
#5
1!
#10
0!
#15
1@
#20
0@
"#;

    #[test]
    fn counts_transitions_and_time() {
        let v = Vcd::parse(VCD).unwrap();
        assert!((v.sim_time_s - 20.0e-9).abs() < 1e-18);
        // clk: 0->1->0->1->0 = 4 transitions over 20 ns -> 200 MHz toggle rate
        assert_eq!(*v.idx.toggles.get("top.clk").unwrap(), 4);
        assert!((v.toggle_rate("clk") - 2.0e8).abs() < 1.0); // leaf resolves (single scope)
        assert_eq!(*v.idx.toggles.get("top.a").unwrap(), 1);
    }

    #[test]
    fn window_restricts_and_rescales() {
        // [5ns,15ns): clk transitions at t=5 (0->1) and t=10 (1->0); t=15 excluded.
        let v = Vcd::parse_windowed(VCD, Some((5.0e-9, Some(15.0e-9)))).unwrap();
        assert_eq!(*v.idx.toggles.get("top.clk").unwrap(), 2);
        assert!((v.sim_time_s - 10.0e-9).abs() < 1e-18);
        assert!((v.toggle_rate("clk") - 2.0e8).abs() < 1.0);
        // 'a' toggles once, at t=5 -> inside the window
        assert_eq!(*v.idx.toggles.get("top.a").unwrap(), 1);
    }

    #[test]
    fn window_open_ended_runs_to_end() {
        // [10ns, end]: clk transitions at 10, 15, 20 all counted (no upper bound).
        let v = Vcd::parse_windowed(VCD, Some((10.0e-9, None))).unwrap();
        assert_eq!(*v.idx.toggles.get("top.clk").unwrap(), 3);
        assert!((v.sim_time_s - 10.0e-9).abs() < 1e-18); // 20ns dump end - 10ns from
    }

    #[test]
    fn window_outside_dump_is_zero_duration() {
        // Beyond the dump -> zero duration, zero rates, no crash.
        let v = Vcd::parse_windowed(VCD, Some((100.0e-9, Some(200.0e-9)))).unwrap();
        assert!(v.sim_time_s.abs() < 1e-18);
        assert_eq!(v.toggle_rate("clk"), 0.0);
    }

    #[test]
    fn no_window_matches_full_dump() {
        // parse() == parse_windowed(None): unchanged behaviour.
        let full = Vcd::parse(VCD).unwrap();
        let none = Vcd::parse_windowed(VCD, None).unwrap();
        assert_eq!(full.idx.toggles.get("top.clk"), none.idx.toggles.get("top.clk"));
        assert!((full.sim_time_s - none.sim_time_s).abs() < 1e-18);
    }

    #[test]
    fn scope_aware_resolution() {
        let v = Vcd::parse(VCD_HIER).unwrap();
        // tb.clk: 0->1->0 = 2 toggles; dut.clk: 0->1->0 = 2 toggles.
        assert_eq!(*v.idx.toggles.get("tb.clk").unwrap(), 2);
        assert_eq!(*v.idx.toggles.get("tb.dut.clk").unwrap(), 2);
        assert_eq!(v.collisions(), 1);
        // Bare `clk` collides tb vs dut -> unresolved (0), no silent pick.
        assert_eq!(v.toggle_rate("clk"), 0.0);
        // scope: dut -> resolves to tb.dut.clk.
        let scoped = Vcd::parse(VCD_HIER).unwrap().with_scope(Some("dut".to_string()));
        assert!((scoped.toggle_rate("clk") - 1.0e8).abs() < 1.0); // 2 / 20ns
    }
}
