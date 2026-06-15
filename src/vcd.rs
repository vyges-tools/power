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
        let text = std::fs::read_to_string(path).map_err(|e| VcdError(format!("{path}: {e}")))?;
        Vcd::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Vcd, VcdError> {
        let mut tick_s = 1.0e-9; // default 1ns
        let mut sym2name: HashMap<String, String> = HashMap::new();
        let mut last: HashMap<String, char> = HashMap::new();
        let mut toggles: HashMap<String, u64> = HashMap::new();
        let mut time_ticks: u64 = 0;

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
                        }
                    } else if let Some(first) = tok.chars().next() {
                        match first {
                            '0' | '1' | 'x' | 'X' | 'z' | 'Z' => {
                                // scalar change: <value><sym>
                                let sym = &tok[1..];
                                if let Some(name) = sym2name.get(sym) {
                                    let v = first.to_ascii_lowercase();
                                    let prev = last.insert(name.clone(), v);
                                    if prev.map(|p| p != v).unwrap_or(false) {
                                        *toggles.entry(name.clone()).or_insert(0) += 1;
                                    }
                                }
                            }
                            'b' | 'B' | 'r' | 'R' => {
                                // vector/real change: <value> <sym> (sym is the next token)
                                if let Some(sym) = toks.next() {
                                    if let Some(name) = sym2name.get(sym) {
                                        *toggles.entry(name.clone()).or_insert(0) += 1;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        Ok(Vcd { toggles, sim_time_s: time_ticks as f64 * tick_s })
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
}
