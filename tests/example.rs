//! End-to-end: load the example job, analyze, and check the report + em-ir seam.

use vyges_power::engine;
use vyges_power::job::PwrJob;

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
