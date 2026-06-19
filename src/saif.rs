//! Minimal SAIF (Switching Activity Interchange Format) reader for **vectored**
//! activity — the cumulative-statistics counterpart to a VCD.
//!
//! Where a VCD logs every transition (size grows with sim length), SAIF stores
//! per net the time spent in 0/1/X (`T0`/`T1`/`TX`) plus a **toggle count** `TC`
//! — so its size scales with the *design*, not the run. Verilator emits it via
//! `--trace-saif` (`VerilatedSaifC`); OpenSTA reads it with `read_saif`. This
//! reader turns `TC` over the run `DURATION` into a per-net toggle rate — the
//! exact quantity [`crate::vcd::Vcd`] provides, so it feeds the same `Activity`.
//!
//! v0 scope: "backward" SAIF (the power flavour). Nets are keyed by the leaf name
//! within their (possibly nested) `INSTANCE`/`NET` groups — the netlist nets are
//! top-level, matching the VCD reader's flatten-to-leaf behaviour. If `DURATION`
//! is absent the per-net `T0+T1+TX` span is used. Depth reserved: bit-level vector
//! nets, glitch (`IG`) power, divider-aware hierarchical path resolution.

use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Saif {
    pub toggles: HashMap<String, u64>, // net leaf name -> toggle count (TC)
    pub sim_time_s: f64,               // DURATION in seconds
}

#[derive(Debug)]
pub struct SaifError(pub String);
impl std::fmt::Display for SaifError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "saif error: {}", self.0)
    }
}
impl std::error::Error for SaifError {}

impl Saif {
    /// Transitions / second for a net (0 if absent or zero-duration run).
    pub fn toggle_rate(&self, name: &str) -> f64 {
        match self.toggles.get(name) {
            Some(&n) if self.sim_time_s > 0.0 => n as f64 / self.sim_time_s,
            _ => 0.0,
        }
    }

    pub fn load(path: &str) -> Result<Saif, SaifError> {
        let text = std::fs::read_to_string(path).map_err(|e| SaifError(format!("{path}: {e}")))?;
        Saif::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Saif, SaifError> {
        let root = Node::parse(text)?;

        let timescale_s = root
            .find_kv("TIMESCALE")
            .map(|v| parse_timescale(&v))
            .unwrap_or(1.0e-9);
        let duration_units = root
            .find_kv("DURATION")
            .and_then(|v| v.first().and_then(|s| s.parse::<f64>().ok()));

        let mut toggles: HashMap<String, u64> = HashMap::new();
        let mut max_span_units = 0.0_f64;
        root.walk_nets(&mut |name, tc, span| {
            // last write wins for a repeated leaf name (top-level nets are unique)
            toggles.insert(name, tc);
            if span > max_span_units {
                max_span_units = span;
            }
        });

        let dur_units = duration_units.filter(|d| *d > 0.0).unwrap_or(max_span_units);
        Ok(Saif { toggles, sim_time_s: dur_units * timescale_s })
    }
}

fn parse_timescale(v: &[String]) -> f64 {
    // "(TIMESCALE 1 ns)" -> ["1","ns"]; "(TIMESCALE 1ns)" -> ["1ns"]
    let s = v.join("").to_lowercase();
    let units = [("fs", 1e-15), ("ps", 1e-12), ("ns", 1e-9), ("us", 1e-6), ("ms", 1e-3), ("s", 1.0)];
    for (suf, scale) in units {
        if let Some(num) = s.strip_suffix(suf) {
            let n: f64 = num.trim().parse().unwrap_or(1.0);
            return n * scale;
        }
    }
    1.0e-9
}

// ---- tiny s-expression tree (SAIF is parenthesised) ----------------------------

enum Node {
    Atom(String),
    List(Vec<Node>),
}

impl Node {
    /// Parse the whole text into one synthetic root list of top-level nodes.
    fn parse(text: &str) -> Result<Node, SaifError> {
        let toks = tokenize(text);
        let mut pos = 0;
        let mut roots = Vec::new();
        while pos < toks.len() {
            roots.push(parse_node(&toks, &mut pos)?);
        }
        Ok(Node::List(roots))
    }

    fn head(&self) -> Option<&str> {
        match self {
            Node::List(items) => match items.first() {
                Some(Node::Atom(a)) => Some(a.as_str()),
                _ => None,
            },
            _ => None,
        }
    }

    /// First (pre-order) list whose head matches `key`; returns its trailing atoms.
    fn find_kv(&self, key: &str) -> Option<Vec<String>> {
        if let Node::List(items) = self {
            if self.head().map(|h| h.eq_ignore_ascii_case(key)).unwrap_or(false) {
                let tail = items[1..]
                    .iter()
                    .filter_map(|n| match n {
                        Node::Atom(a) => Some(a.clone()),
                        _ => None,
                    })
                    .collect();
                return Some(tail);
            }
            for it in items {
                if let Some(v) = it.find_kv(key) {
                    return Some(v);
                }
            }
        }
        None
    }

    /// Visit every net entry under any `NET` group: `f(leaf_name, toggle_count, span_units)`.
    fn walk_nets<F: FnMut(String, u64, f64)>(&self, f: &mut F) {
        if let Node::List(items) = self {
            if self.head().map(|h| h.eq_ignore_ascii_case("NET")).unwrap_or(false) {
                for child in &items[1..] {
                    if let (Node::List(_), Some(name)) = (child, child.head()) {
                        let tc = child
                            .find_kv("TC")
                            .and_then(|v| v.first().and_then(|s| s.parse::<f64>().ok()))
                            .unwrap_or(0.0) as u64;
                        let span: f64 = ["T0", "T1", "TX"]
                            .iter()
                            .map(|k| {
                                child
                                    .find_kv(k)
                                    .and_then(|v| v.first().and_then(|s| s.parse::<f64>().ok()))
                                    .unwrap_or(0.0)
                            })
                            .sum();
                        f(name.to_string(), tc, span);
                    }
                }
            }
            for it in items {
                it.walk_nets(f);
            }
        }
    }
}

enum Tok {
    Open,
    Close,
    Atom(String),
}

fn tokenize(text: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            '(' => {
                toks.push(Tok::Open);
                chars.next();
            }
            ')' => {
                toks.push(Tok::Close);
                chars.next();
            }
            '"' => {
                chars.next(); // opening quote
                let mut s = String::new();
                for ch in chars.by_ref() {
                    if ch == '"' {
                        break;
                    }
                    s.push(ch);
                }
                toks.push(Tok::Atom(s));
            }
            ws if ws.is_whitespace() => {
                chars.next();
            }
            _ => {
                let mut s = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch == '(' || ch == ')' || ch == '"' || ch.is_whitespace() {
                        break;
                    }
                    s.push(ch);
                    chars.next();
                }
                toks.push(Tok::Atom(s));
            }
        }
    }
    toks
}

fn parse_node(toks: &[Tok], pos: &mut usize) -> Result<Node, SaifError> {
    match toks.get(*pos) {
        Some(Tok::Open) => {
            *pos += 1;
            let mut items = Vec::new();
            loop {
                match toks.get(*pos) {
                    Some(Tok::Close) => {
                        *pos += 1;
                        return Ok(Node::List(items));
                    }
                    None => return Err(SaifError("unbalanced '(' — missing ')'".into())),
                    _ => items.push(parse_node(toks, pos)?),
                }
            }
        }
        Some(Tok::Atom(a)) => {
            let n = Node::Atom(a.clone());
            *pos += 1;
            Ok(n)
        }
        Some(Tok::Close) => Err(SaifError("unexpected ')'".into())),
        None => Err(SaifError("unexpected end of input".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAIF: &str = r#"
(SAIFILE
  (SAIFVERSION "2.0")
  (DIRECTION "backward")
  (DIVIDER / )
  (TIMESCALE 1 ns)
  (DURATION 50.0)
  (INSTANCE top
    (NET
      (clk (T0 25.0) (T1 25.0) (TX 0.0) (TC 10) (IG 0))
      (n1  (T0 30.0) (T1 20.0) (TX 0.0) (TC 4)  (IG 0))
      (q   (T0 35.0) (T1 15.0) (TX 0.0) (TC 2)  (IG 0))
    )
    (INSTANCE sub
      (NET
        (deep (T0 40.0) (T1 10.0) (TX 0.0) (TC 6) (IG 0))
      )
    )
  )
)
"#;

    #[test]
    fn parses_counts_and_rates() {
        let s = Saif::parse(SAIF).unwrap();
        assert!((s.sim_time_s - 50.0e-9).abs() < 1e-18);
        assert_eq!(*s.toggles.get("clk").unwrap(), 10);
        assert_eq!(*s.toggles.get("n1").unwrap(), 4);
        assert_eq!(*s.toggles.get("q").unwrap(), 2);
        // nested INSTANCE/NET nets are reached too
        assert_eq!(*s.toggles.get("deep").unwrap(), 6);
        // TC / DURATION: n1 = 4 / 50ns = 8e7
        assert!((s.toggle_rate("n1") - 8.0e7).abs() < 1.0);
        assert!((s.toggle_rate("q") - 4.0e7).abs() < 1.0);
        assert_eq!(s.toggle_rate("absent"), 0.0);
    }

    #[test]
    fn falls_back_to_span_without_duration() {
        let no_dur = "(SAIFILE (TIMESCALE 1 ns) (NET (a (T0 60.0) (T1 40.0) (TX 0.0) (TC 5))))";
        let s = Saif::parse(no_dur).unwrap();
        // span = 100 ns -> rate = 5 / 100ns = 5e7
        assert!((s.sim_time_s - 100.0e-9).abs() < 1e-18);
        assert!((s.toggle_rate("a") - 5.0e7).abs() < 1.0);
    }
}
