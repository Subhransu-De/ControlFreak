use crate::pr_report::{self, CargoTests, E2eRun, Job, MARKER, Report, Results};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

const MAX_FILE: u64 = 16 * 1024 * 1024;
const WORKFLOW: &str = "ci.yml";

pub struct Request<'a> {
    pub repo: &'a str,
    pub run_id: u64,
    pub head_sha: &'a str,
    pub finished: bool,
    pub work_dir: &'a Path,
    pub post: bool,
}

pub fn report(request: &Request<'_>) -> Result<String, String> {
    let run_url = format!(
        "https://github.com/{}/actions/runs/{}",
        request.repo, request.run_id
    );
    let body = if request.finished {
        let current = collect(
            request.repo,
            request.run_id,
            request.head_sha,
            &request.work_dir.join("current"),
        )?;
        let baseline = baseline_run(request.repo)?
            .filter(|(id, _)| *id != request.run_id)
            .map(|(id, sha)| collect(request.repo, id, &sha, &request.work_dir.join("baseline")))
            .transpose()?;
        pr_report::render(&Report {
            current: &current,
            baseline: baseline.as_ref(),
            run_url: &run_url,
        })
    } else {
        pr_report::render_running(request.head_sha, &run_url)
    };
    if let Ok(summary) = std::env::var("GITHUB_STEP_SUMMARY") {
        let _ = fs::write(summary, &body);
    }
    if request.post {
        match pull_request(request.repo, request.head_sha)? {
            Some(number) => upsert(request.repo, number, &body, request.work_dir)?,
            None => eprintln!(
                "no open pull request has head {}; nothing posted",
                request.head_sha
            ),
        }
    }
    Ok(body)
}

fn gh(args: &[&str]) -> Result<String, String> {
    let output = Command::new("gh")
        .args(args)
        .output()
        .map_err(|error| format!("cannot run gh: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "gh {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn collect(repo: &str, run_id: u64, sha: &str, dir: &Path) -> Result<Results, String> {
    let run = run_id.to_string();
    fs::create_dir_all(dir).map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let dir_text = dir.to_string_lossy().into_owned();
    if let Err(error) = gh(&[
        "run",
        "download",
        &run,
        "-R",
        repo,
        "-D",
        &dir_text,
        "-p",
        "test-results-*",
    ]) {
        eprintln!("run {run}: no test results downloaded ({error})");
    }
    let jobs: Vec<Job> = pr_report::parse_jobs(&gh(&[
        "api",
        &format!("repos/{repo}/actions/runs/{run}/jobs?per_page=100"),
    ])?);
    let workspace: Option<CargoTests> =
        find(dir, "cargo-test.log").and_then(|path| pr_report::parse_cargo_test(&read(&path)?));
    let e2e: Option<E2eRun> = find(dir, "timeline.jsonl").map(|path| {
        let timeline = read(&path).unwrap_or_default();
        let run_json = path
            .parent()
            .map(|p| p.join("run.json"))
            .and_then(|p| read(&p));
        pr_report::parse_e2e(&timeline, run_json.as_deref())
    });
    Ok(Results {
        commit: sha.to_owned(),
        workspace,
        e2e,
        jobs,
    })
}

fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in fs::read_dir(&next).ok()?.flatten() {
            let path = entry.path();
            let kind = entry.file_type().ok()?;
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() && path.file_name().is_some_and(|n| n == name) {
                return Some(path);
            }
        }
    }
    None
}

fn read(path: &Path) -> Option<String> {
    let mut text = String::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_FILE)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

fn baseline_run(repo: &str) -> Result<Option<(u64, String)>, String> {
    let listed = gh(&[
        "run",
        "list",
        "-R",
        repo,
        "--workflow",
        WORKFLOW,
        "--branch",
        "main",
        "--event",
        "push",
        "--status",
        "success",
        "-L",
        "1",
        "--json",
        "databaseId,headSha",
    ])?;
    let value: Value = serde_json::from_str(&listed).map_err(|error| error.to_string())?;
    Ok(value
        .as_array()
        .and_then(|runs| runs.first())
        .and_then(|run| {
            Some((
                run["databaseId"].as_u64()?,
                run["headSha"].as_str()?.to_owned(),
            ))
        }))
}

fn pull_request(repo: &str, sha: &str) -> Result<Option<u64>, String> {
    let listed = gh(&[
        "pr",
        "list",
        "-R",
        repo,
        "--state",
        "open",
        "--search",
        sha,
        "--json",
        "number,headRefOid",
    ])?;
    let value: Value = serde_json::from_str(&listed).map_err(|error| error.to_string())?;
    Ok(value.as_array().and_then(|prs| {
        prs.iter()
            .find(|pr| pr["headRefOid"] == sha)
            .and_then(|pr| pr["number"].as_u64())
    }))
}

fn upsert(repo: &str, number: u64, body: &str, work_dir: &Path) -> Result<(), String> {
    let filter = format!(
        ".[] | select(.user.login == \"github-actions[bot]\" and (.body | startswith(\"{MARKER}\"))) | .id"
    );
    let ids = gh(&[
        "api",
        "--paginate",
        &format!("repos/{repo}/issues/{number}/comments"),
        "--jq",
        &filter,
    ])?;
    let payload = work_dir.join("comment.json");
    fs::write(&payload, json!({ "body": body }).to_string())
        .map_err(|error| format!("cannot write {}: {error}", payload.display()))?;
    let payload = payload.to_string_lossy().into_owned();
    match ids
        .lines()
        .next()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(id) => gh(&[
            "api",
            "-X",
            "PATCH",
            &format!("repos/{repo}/issues/comments/{id}"),
            "--input",
            &payload,
        ]),
        None => gh(&[
            "api",
            "-X",
            "POST",
            &format!("repos/{repo}/issues/{number}/comments"),
            "--input",
            &payload,
        ]),
    }
    .map(drop)
}
