use clap::{Parser, Subcommand, ValueEnum};
use controlfreak_e2e::{
    Tier, github,
    host::{self, Binaries},
    report,
    runner::{self, Environment, Plan},
    suites,
};
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

#[derive(Parser)]
#[command(
    about = "Live end-to-end suites for the ControlFreak server on a disposable Windows desktop"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the suites and tasks a tier would run. Touches nothing.
    List {
        #[arg(long, value_enum, default_value_t = TierArg::Nightly)]
        tier: TierArg,
    },
    /// Run the suites. Needs `CONTROLFREAK_LIVE_DESKTOP=1` on a disposable desktop.
    Run {
        #[arg(long, value_enum, default_value_t = TierArg::Pr)]
        tier: TierArg,
        /// Run only these suites (repeatable).
        #[arg(long = "suite")]
        suites: Vec<String>,
        /// Treat BLOCKED tasks (missing capabilities) as failures.
        #[arg(long)]
        strict: bool,
        /// Where reports and logs go.
        #[arg(long, default_value = "target/e2e")]
        out: PathBuf,
        /// Directory holding controlfreak.exe and controlfreak-fixture.exe. Defaults to the
        /// directory of this binary.
        #[arg(long)]
        bin_dir: Option<PathBuf>,
    },
    /// Render the test report for a CI run and post it as the pull request's sticky comment.
    PrReport {
        /// Repository as owner/name.
        #[arg(long)]
        repo: String,
        /// The CI workflow run to report on.
        #[arg(long)]
        run_id: u64,
        /// The commit the run tested.
        #[arg(long)]
        head_sha: String,
        /// The `workflow_run` action: `requested` posts a running note, `completed` the report.
        #[arg(long)]
        event: String,
        /// Where downloads and the rendered comment go.
        #[arg(long, default_value = "target/pr-report")]
        work_dir: PathBuf,
        /// Render only; do not post the comment.
        #[arg(long)]
        no_post: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum TierArg {
    Pr,
    Nightly,
}

impl From<TierArg> for Tier {
    fn from(tier: TierArg) -> Self {
        match tier {
            TierArg::Pr => Self::Pr,
            TierArg::Nightly => Self::Nightly,
        }
    }
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::PrReport {
            repo,
            run_id,
            head_sha,
            event,
            work_dir,
            no_post,
        } => pr_report(&github::Request {
            repo: &repo,
            run_id,
            head_sha: &head_sha,
            finished: event == "completed",
            work_dir: &work_dir,
            post: !no_post,
        }),
        Command::List { tier } => {
            list(tier.into());
            ExitCode::SUCCESS
        }
        Command::Run {
            tier,
            suites,
            strict,
            out,
            bin_dir,
        } => {
            let plan = Plan {
                tier: tier.into(),
                strict,
                suites,
                out_dir: out,
            };
            match run(&plan, bin_dir.as_deref()) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(error) => {
                    eprintln!("controlfreak-e2e: {error}");
                    ExitCode::from(2)
                }
            }
        }
    }
}

fn pr_report(request: &github::Request<'_>) -> ExitCode {
    let rendered = std::fs::create_dir_all(request.work_dir)
        .map_err(|error| error.to_string())
        .and_then(|()| github::report(request))
        .and_then(|body| {
            std::fs::write(request.work_dir.join("comment.md"), body).map_err(|e| e.to_string())
        });
    match rendered {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("controlfreak-e2e: {error}");
            ExitCode::from(2)
        }
    }
}

fn list(tier: Tier) {
    let all = suites::all();
    let plan = Plan {
        tier,
        strict: false,
        suites: Vec::new(),
        out_dir: PathBuf::new(),
    };
    let Ok(selected) = runner::select(&all, &plan) else {
        return;
    };
    for (suite, tasks) in selected {
        println!("{} ({} tasks): {}", suite.name, tasks.len(), suite.about);
        for task in tasks {
            let needs: Vec<&str> = task.requires.iter().map(|c| c.name()).collect();
            println!(
                "  {:<36} {:<8} {:<14} {}",
                task.name,
                task.tier.name(),
                task.scene.unwrap_or("-"),
                if needs.is_empty() {
                    String::new()
                } else {
                    format!("needs {}", needs.join(", "))
                }
            );
        }
    }
}

fn run(plan: &Plan, bin_dir: Option<&Path>) -> Result<bool, String> {
    let all = suites::all();
    runner::select(&all, plan)?;
    host::require_live_desktop()?;
    let mut environment = Environment::windows(Binaries::locate(bin_dir)?);
    println!(
        "controlfreak-e2e {} run, reports in {}",
        plan.tier.name(),
        plan.out_dir.display()
    );
    let report = runner::run(&all, plan, &mut environment, &mut |record| {
        println!("{}", report::task_line(record));
    })?;
    report::write(&report, &plan.out_dir)?;
    println!("{}", report::summary(&report));
    Ok(report.succeeded())
}
