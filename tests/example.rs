//! End-to-end: load the example job, analyze, and check the report + em-ir seam.

use vyges_power::engine;
use vyges_power::job::PwrJob;
use vyges_power::vcd::Vcd;

#[test]
fn counter_sweep_measures_the_workload_over_time() {
    let job = PwrJob::load("examples/counter/counter-sweep.pwr").expect("load job");
    let rep = engine::analyze_sweep_job(&job).expect("sweep");

    // 50 ns dump, 10 ns windows.
    assert_eq!(rep.windows.len(), 5);
    assert!((rep.dump_end_s - 50.0e-9).abs() < 1e-18);
    for (i, w) in rep.windows.iter().enumerate() {
        assert_eq!(w.index, i);
        assert!(w.total_w() > 0.0, "window {i} has no power at all");
        assert!(
            w.total_w().is_finite(),
            "window {i} produced a non-finite power"
        );
        // The three groups account for the whole window — the split is a partition, not a
        // sample of it.
        let grouped = w.sequential_w + w.combinational_w + w.clock_w;
        assert!(
            (grouped - w.total_w()).abs() < 1e-15,
            "window {i}: groups sum to {grouped}, total is {}",
            w.total_w()
        );
    }

    // The peak is a real window, and it is the largest one.
    let peak = rep.peak_window();
    assert!(rep.windows.iter().all(|w| w.total_w() <= peak.total_w()));
    assert!(
        rep.peak_to_mean() > 1.0,
        "a workload with any variation has a peak above its mean; got {}",
        rep.peak_to_mean()
    );
}

#[test]
fn a_swept_window_agrees_with_the_same_window_measured_alone() {
    // THE EQUIVALENCE THAT MAKES THE SWEEP TRUSTWORTHY. Sweeping is an optimisation — one
    // parse instead of N — and an optimisation that changes the answer is a different tool.
    // Job files differing only in how the window is asked for must produce the same power.
    let sweep = engine::analyze_sweep_job(
        &PwrJob::load("examples/counter/counter-sweep.pwr").expect("load sweep job"),
    )
    .expect("sweep");

    let base = std::fs::read_to_string("examples/counter/counter.pwr").expect("base job");
    let measure_alone = |from_s: f64, to_s: f64| {
        let single = format!(
            "{base}\nactivity_window: {:.6}ns {:.6}ns\n",
            from_s * 1e9,
            to_s * 1e9
        );
        let job = PwrJob::parse(&single, "examples/counter").expect("windowed job");
        engine::analyze_job(&job).expect("analyze").total_w()
    };

    let last = sweep.windows.len() - 1;
    for w in &sweep.windows[..last] {
        let one = measure_alone(w.from_s, w.to_s);
        let rel = (one - w.total_w()).abs() / w.total_w().max(1e-30);
        assert!(
            rel < 1e-9,
            "window {} [{:.3} ns, {:.3} ns): swept {:.6e} W vs measured alone {:.6e} W",
            w.index,
            w.from_s * 1e9,
            w.to_s * 1e9,
            w.total_w(),
            one
        );
    }

    // THE ONE DELIBERATE DIFFERENCE, pinned so it stays deliberate. A half-open window
    // `[40ns, 50ns)` excludes a transition at exactly 50 ns, and for a single measurement that
    // is the right rule. But 50 ns is the *end of the dump*: there is no later window for those
    // transitions to land in, so a sweep that dropped them would lose activity the whole-dump
    // read counts. The sweep therefore folds the final timestamp into the last window, and that
    // window reads higher than the same bounds measured alone.
    let w = &sweep.windows[last];
    let alone = measure_alone(w.from_s, w.to_s);
    assert!(
        w.total_w() > alone,
        "the last window should carry the dump-end transitions the half-open window drops"
    );
}

#[test]
fn sweeping_does_not_invent_or_lose_transitions() {
    // The sweep's windows must partition the dump: every transition the whole-dump read sees
    // is counted in exactly one window. A sweep that drops the tail of a dump would quietly
    // report a design as idler than it is.
    let whole = Vcd::load("examples/counter/counter.vcd").expect("vcd");
    let sweep = Vcd::load_sweep(
        "examples/counter/counter.vcd",
        0.0,
        None,
        10.0e-9,
        None,
        None,
    )
    .expect("sweep");
    for (path, n) in &whole.idx.toggles {
        let summed: u64 = sweep
            .windows
            .iter()
            .map(|w| w.toggles.get(path).copied().unwrap_or(0))
            .sum();
        assert_eq!(summed, *n, "{path}: sweep {summed} vs whole dump {n}");
    }
}

#[test]
fn example_block_runs_end_to_end() {
    let job = PwrJob::load("examples/block/block.pwr").expect("load job");
    let rep = engine::analyze_job(&job).expect("analyze");

    assert_eq!(rep.insts.len(), 3, "u_nand, u_inv, u_ff");
    assert!(rep.unmatched.is_empty(), "all cells in tiny.lib");
    assert_eq!(rep.mode, "vectored (VCD)");

    // power split is positive and ordered: total > leakage
    assert!(rep.leakage_w > 0.0);
    assert!(rep.total_w() > rep.leakage_w);

    // toggle rates come straight from block.vcd (50 ns window):
    //   n1 (u_nand): 4 transitions -> 8e7 ; n2 (u_inv): 3 -> 6e7 ; q (u_ff): 2 -> 4e7
    let rate = |inst: &str| {
        rep.insts
            .iter()
            .find(|i| i.inst == inst)
            .unwrap()
            .toggle_rate
    };
    assert!((rate("u_nand") - 8.0e7).abs() < 1.0);
    assert!((rate("u_inv") - 6.0e7).abs() < 1.0);
    assert!((rate("u_ff") - 4.0e7).abs() < 1.0);

    // the em-ir activity map carries every instance + a positive current
    let map = rep.activity_map();
    for inst in ["u_nand", "u_inv", "u_ff"] {
        assert!(map.contains(inst), "activity map missing {inst}");
    }
    let i_total: f64 = rep.insts.iter().map(|i| i.avg_current_a).sum();
    assert!(i_total > 0.0);
}

#[test]
fn counter_uses_real_extracted_spef() {
    let job = PwrJob::load("examples/counter/counter.pwr").expect("load counter job");
    let rep = engine::analyze_job(&job).expect("analyze counter");
    assert!(rep.spef, "SPEF should be in use");
    // clk (clkbuf) and n0 (u0) are the two nets vyges-extract produced
    assert_eq!(rep.spef_nets, 2, "clk + n0 carry extracted caps");
    assert_eq!(rep.insts.len(), 5);
    assert!(rep.unmatched.is_empty());
    // clkbuf drives the high-toggle clk net -> it should be the top consumer
    let top = rep
        .insts
        .iter()
        .max_by(|a, b| a.total_w().partial_cmp(&b.total_w()).unwrap())
        .unwrap();
    assert_eq!(top.inst, "clkbuf");
}

#[test]
fn saif_matches_vcd() {
    // swap the vcd line for the equivalent SAIF -> identical vectored rates.
    let text = std::fs::read_to_string("examples/block/block.pwr").unwrap();
    let swapped: String = text
        .lines()
        .map(|l| {
            if l.trim_start().starts_with("vcd:") {
                "saif: block.saif"
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let job = PwrJob::parse(&swapped, "examples/block").unwrap();
    let rep = engine::analyze_job(&job).expect("analyze saif");

    assert_eq!(rep.mode, "vectored (SAIF)");
    // same toggle rates as block.vcd (TC over the 50 ns DURATION):
    let rate = |inst: &str| {
        rep.insts
            .iter()
            .find(|i| i.inst == inst)
            .unwrap()
            .toggle_rate
    };
    assert!((rate("u_nand") - 8.0e7).abs() < 1.0);
    assert!((rate("u_inv") - 6.0e7).abs() < 1.0);
    assert!((rate("u_ff") - 4.0e7).abs() < 1.0);
}

#[test]
fn vcd_and_saif_are_mutually_exclusive() {
    let both = "design: b\nnetlist: b.v\nlib: a.lib\nclock: clk 10.0\nvcd: b.vcd\nsaif: b.saif\n";
    assert!(PwrJob::parse(both, "").is_err());
}

#[test]
fn vectorless_when_no_vcd() {
    // strip the vcd line -> vectorless mode, still runs
    let text = std::fs::read_to_string("examples/block/block.pwr").unwrap();
    let stripped: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("vcd:"))
        .collect::<Vec<_>>()
        .join("\n");
    let job = PwrJob::parse(&stripped, "examples/block").unwrap();
    let rep = engine::analyze_job(&job).expect("analyze vectorless");
    assert_eq!(rep.mode, "vectorless");
    assert_eq!(rep.insts.len(), 3);
}
