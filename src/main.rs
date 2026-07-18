//! vyges-power CLI.
//!
//!   vyges-power run   JOB [-o OUT] [--json] [--fail-on-budget]   analyze -> report
//!   vyges-power check JOB                                        validate the job
//!   vyges-power demo  [-o OUT] [--json]                          built-in design
//!
//! Common flags: -h/--help, -V/--version, -q/--quiet, -v/--verbose.
//! Exit codes: 0 ok · 1 runtime/analysis error · 2 usage/validation · 3 power
//! over budget (only with --fail-on-budget).

use std::process::exit;

use vyges_power::engine;
use vyges_power::job::PwrJob;
use vyges_power::power::PowerReport;

const USAGE: &str = "\
vyges-power — gate-level power analysis (leakage + dynamic) with a CI gate

usage:
  vyges-power run   JOB [-o OUT] [--json] [--fail-on-budget]
  vyges-power check JOB
  vyges-power demo       [-o OUT] [--json]

A JOB is a small declarative `.pwr` file (netlist + lib(s) + clock + activity).
With `vcd:` or `saif:` it uses measured per-net toggle rates; otherwise a
vectorless `activity:` factor × clock. With `emit_activity:` it writes the
per-instance map that vyges-em-ir consumes (closing char -> power -> em-ir).

flags:
  -o FILE             write the report to FILE (default: stdout)
  --json              machine-readable JSON instead of the text report
  --fail-on-budget    exit 3 if total power exceeds the job's power_budget_mw
  -q, --quiet         suppress non-essential output
  -v, --verbose       extra detail on stderr
  --describe          print a machine-readable JSON description of the command
  -h, --help          show this help
  -V, --version       show version
  --bug-report        file a bug (central: vyges/community)
  --feature-request   request a feature (central)
  --sponsor           sponsor Vyges (github.com/sponsors/vyges-ip)
  --star              star this tool on GitHub ⭐
";

const BUG_URL: &str =
    "https://github.com/vyges/community/issues/new?template=bug_report_template.yaml";
const FEATURE_URL: &str = "https://github.com/vyges/community/issues/new?labels=enhancement";
const SPONSOR_URL: &str = "https://github.com/sponsors/vyges-ip";
const STAR_URL: &str = "https://github.com/vyges-tools/power";

fn link(label: &str, url: &str) {
    use std::io::IsTerminal;
    println!("{label}:\n  {url}");
    if std::io::stdout().is_terminal() {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let _ = std::process::Command::new(opener).arg(url).status();
    }
}

#[derive(Default)]
struct Cli {
    positionals: Vec<String>,
    out: Option<String>,
    json: bool,
    quiet: bool,
    verbose: bool,
    fail_on_budget: bool,
    help: bool,
    version: bool,
    bug_report: bool,
    feature_request: bool,
    sponsor: bool,
    star: bool,
}

fn parse_cli(args: &[String]) -> Cli {
    let mut c = Cli::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                c.out = args.get(i + 1).cloned();
                i += 1;
            }
            "--json" => c.json = true,
            "--fail-on-budget" => c.fail_on_budget = true,
            "-q" | "--quiet" => c.quiet = true,
            "-v" | "--verbose" => c.verbose = true,
            "-h" | "--help" => c.help = true,
            "-V" | "--version" => c.version = true,
            "--bug-report" => c.bug_report = true,
            "--feature-request" => c.feature_request = true,
            "--sponsor" => c.sponsor = true,
            "--star" => c.star = true,
            other => c.positionals.push(other.to_string()),
        }
        i += 1;
    }
    c
}

fn write_out(text: &str, cli: &Cli) {
    match &cli.out {
        Some(path) => match std::fs::write(path, text) {
            Ok(_) => {
                if !cli.quiet {
                    println!("wrote {path}");
                }
            }
            Err(e) => {
                eprintln!("error: {path}: {e}");
                exit(1);
            }
        },
        None => print!("{text}"),
    }
}

/// Emit the vyges-events causal trail for the power analysis — to stderr (the report
/// goes to stdout / -o). code=POWER-* is the clustering key; objects are net refs used
/// for cross-stage co-reference. Always emits the POWER-DONE summary; additionally emits
/// one POWER-HOT warn per hotspot net (an instance dominating the design's total power).
fn emit_power_events(rep: &PowerReport) {
    use vyges_events::{Event, Severity};
    let e = |sev, code: &str, msg: String, objs: Vec<String>| {
        vyges_events::emit(
            &Event::new("vyges-power", sev, msg)
                .with_code(code)
                .with_objects(objs),
        );
    };

    let total_w = rep.total_w();
    // Hotspot nets: instances whose total power is a large share of the design total.
    // Flag the top few so downstream (em-ir) can co-reference the offending nets.
    if total_w > 0.0 {
        let mut hot: Vec<&vyges_power::power::InstPower> = rep
            .insts
            .iter()
            .filter(|i| !i.out_net.is_empty() && i.total_w() > 0.25 * total_w)
            .collect();
        hot.sort_by(|a, b| {
            b.total_w()
                .partial_cmp(&a.total_w())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for i in hot {
            e(
                Severity::Warn,
                "POWER-HOT",
                format!(
                    "hotspot net {} ({} / {}): {:.3} mW ({:.1}% of total), toggle {:.3e} Hz",
                    i.out_net,
                    i.inst,
                    i.cell,
                    i.total_w() * 1e3,
                    100.0 * i.total_w() / total_w,
                    i.toggle_rate
                ),
                vec![format!("net:{}", i.out_net)],
            );
        }
    }

    e(
        Severity::Info,
        "POWER-DONE",
        format!(
            "power analysis complete for {} ({} mode): total {:.3} mW = dynamic {:.3} mW + leakage {:.3} mW over {} instance(s)",
            rep.design,
            rep.mode,
            total_w * 1e3,
            rep.dynamic_w() * 1e3,
            rep.leakage_w * 1e3,
            rep.insts.len()
        ),
        vec![],
    );
}

/// Emit the report; optionally write the em-ir activity map; honour the budget gate.
fn emit(job: Option<&PwrJob>, rep: &PowerReport, cli: &Cli) -> ! {
    emit_power_events(rep);
    let text = if cli.json {
        engine::report_json(rep)
    } else {
        engine::render_report(rep)
    };
    write_out(&text, cli);

    if let Some(job) = job {
        if let Some(path) = &job.emit_activity {
            let resolved = job.resolve(path);
            match std::fs::write(&resolved, rep.activity_map()) {
                Ok(_) => {
                    if !cli.quiet {
                        eprintln!("wrote em-ir activity map: {resolved}");
                    }
                }
                Err(e) => eprintln!("warning: could not write activity map {resolved}: {e}"),
            }
        }
        if cli.fail_on_budget {
            if let Some(budget_mw) = job.power_budget_mw {
                let total_mw = rep.total_w() * 1e3;
                if total_mw > budget_mw {
                    if !cli.quiet {
                        eprintln!("power OVER BUDGET: {total_mw:.3} mW > {budget_mw:.3} mW");
                    }
                    exit(3);
                }
            } else if !cli.quiet {
                eprintln!("note: --fail-on-budget set but the job has no power_budget_mw");
            }
        }
    }
    exit(0);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--describe") {
        // Machine-readable description of `run` for tooling that drives it.
        const DESCRIBE: &str = r#"{
  "name": "power",
  "summary": "gate-level power analysis (leakage + dynamic) with a CI gate",
  "maturity": "workflow-validated",
  "provenance_limitations": [
      "The job names the netlist, Liberty and any VCD/SAIF activity; input_hash covers the job path and arguments, not their contents."
  ],
  "invocation": {
    "args_template": ["run", "{job}"],
    "optional": [ { "arg": "out", "flag": "-o" } ],
    "emits_json": true
  },
  "inputs": {
    "type": "object",
    "required": ["job"],
    "properties": {
      "job": { "type": "string", "description": "path to the .pwr job file (netlist + lib(s) + clock + activity)" },
      "out": { "type": "string", "description": "path to write the report to (default: stdout)" }
    }
  },
  "artifacts": [ { "role": "power_report", "from_arg": "out" } ],
  "assertion": {
    "id": "power-analysis",
    "not_applicable": true
  },
  "consumes": ["netlist", "liberty", "vcd"]
}
"#;
        print!("{DESCRIBE}");
        return;
    }
    let cli = parse_cli(&args);

    if cli.bug_report {
        return link("Report a bug (central — vyges/community)", BUG_URL);
    }
    if cli.feature_request {
        return link("Request a feature (central — vyges/community)", FEATURE_URL);
    }
    if cli.sponsor {
        return link("Sponsor Vyges", SPONSOR_URL);
    }
    if cli.star {
        return link("Star vyges-power on GitHub ⭐", STAR_URL);
    }
    if cli.version {
        println!(
            "vyges-power {} ({})",
            vyges_power::VERSION,
            env!("VYGES_GIT_SHA")
        );
        println!("{}", vyges_power::COPYRIGHT);
        return;
    }
    let cmd = cli.positionals.first().cloned().unwrap_or_default();
    if cli.help || cmd.is_empty() {
        print!("{USAGE}");
        exit(if cmd.is_empty() && !cli.help { 2 } else { 0 });
    }

    match cmd.as_str() {
        "demo" => {
            let rep = engine::demo();
            emit(None, &rep, &cli);
        }
        "check" => {
            let Some(path) = cli.positionals.get(1) else {
                eprintln!("usage: vyges-power check JOB");
                exit(2);
            };
            match PwrJob::load(path) {
                Ok(j) => println!(
                    "OK  design={} netlist={} libs={} clock={}@{}ns activity={}",
                    j.design,
                    j.netlist,
                    j.libs.len(),
                    j.clock_port,
                    j.period_ns,
                    j.activity_desc()
                ),
                Err(e) => {
                    eprintln!("error: {e}");
                    exit(2);
                }
            }
        }
        "run" => {
            let Some(path) = cli.positionals.get(1) else {
                eprintln!("usage: vyges-power run JOB [-o OUT]");
                exit(2);
            };
            let job = match PwrJob::load(path) {
                Ok(j) => j,
                Err(e) => {
                    eprintln!("error: {e}");
                    exit(2);
                }
            };
            if cli.verbose {
                eprintln!(
                    "loaded {} ({} lib(s)); activity = {}",
                    job.netlist,
                    job.libs.len(),
                    job.activity_desc()
                );
            }
            match engine::analyze_job(&job) {
                Ok(rep) => emit(Some(&job), &rep, &cli),
                Err(e) => {
                    eprintln!("error: {e}");
                    exit(1);
                }
            }
        }
        other => {
            eprintln!("vyges-power: unknown command {other:?}\n");
            print!("{USAGE}");
            exit(2);
        }
    }
}
