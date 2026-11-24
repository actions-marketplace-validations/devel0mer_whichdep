//! Developer platform outputs: Markdown reports, sticky GitHub PR comments,
//! CI step summaries/annotations, and self-contained HTML dashboards.

use crate::dd::{Outcome, Verdict};
use crate::json::Json;
use crate::report::{self, Report, SearchResult};
use crate::store::ExperimentSummary;
use crate::ui;
use std::fmt::Write as FmtWrite;
use std::fs::OpenOptions;
use std::io::{self, Write as IoWrite};

/// Hidden HTML comment marker allowing CI workflows to update an existing
/// whichdep comment on a pull request rather than posting duplicates.
pub const STICKY_COMMENT_MARKER: &str = "<!-- whichdep-report -->";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
    Markdown,
    GithubComment,
    Html,
}

impl OutputFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
            Self::Markdown => "markdown",
            Self::GithubComment => "github-comment",
            Self::Html => "html",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "text" | "plain" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            "markdown" | "md" => Ok(Self::Markdown),
            "github-comment" | "pr-comment" | "github" => Ok(Self::GithubComment),
            "html" | "dashboard" => Ok(Self::Html),
            other => Err(format!(
                "unknown output format `{other}`: use text, json, markdown, github-comment or html"
            )),
        }
    }
}

/// Render a report in GitHub-Flavored Markdown suitable for issues, PRs, and
/// `$GITHUB_STEP_SUMMARY`.
pub fn markdown(rep: &Report) -> String {
    let mut o = String::new();
    let stats = rep.stats();

    match &rep.result {
        SearchResult::ListOnly => {
            let _ = writeln!(o, "### whichdep — {} changes", rep.changes.len());
            let _ = writeln!(
                o,
                "\nEcosystem: `{}` · Lockfile: `{}` (`{}` → `{}`)\n",
                rep.kind, rep.lockfile, rep.good, rep.bad
            );
            let _ = writeln!(o, "| Package | Good | Bad |");
            let _ = writeln!(o, "| --- | --- | --- |");
            for c in &rep.changes {
                let from = if c.from.is_empty() {
                    "*(new)*".to_string()
                } else {
                    format!("`{}`", c.from.join(", "))
                };
                let to = if c.to.is_empty() {
                    "*(removed)*".to_string()
                } else {
                    format!("`{}`", c.to.join(", "))
                };
                let _ = writeln!(o, "| `{}` | {from} | {to} |", c.name);
            }
            return o;
        }
        SearchResult::Fatal(msg) => {
            let _ = writeln!(o, "### whichdep — Fatal Error");
            let _ = writeln!(o, "\n> **Error:** `{msg}`\n");
            return o;
        }
        SearchResult::NoResult(err) => {
            let _ = writeln!(o, "### whichdep — No Culprit Isolated");
            let _ = writeln!(
                o,
                "\nEcosystem: `{}` · Lockfile: `{}` · Strategy: `{}`\n",
                rep.kind, rep.lockfile, rep.strategy
            );
            let _ = writeln!(o, "> **Reason:** {err}\n");
        }
        SearchResult::Found(f) => {
            let badge = match f.verdict {
                Verdict::Verified => "Verified",
                Verdict::Unverified => "Not Verified",
                Verdict::Incomplete => "Incomplete",
            };
            let _ = writeln!(o, "### whichdep — Dependency Regression Found ({badge})");
            let _ = writeln!(
                o,
                "\nEcosystem: `{}` · Lockfile: `{}` · Strategy: `{}` · `{}` → `{}`\n",
                rep.kind, rep.lockfile, rep.strategy, rep.good, rep.bad
            );

            for (i, c) in f.culprits.iter().enumerate() {
                if f.culprits.len() > 1 {
                    let _ = writeln!(o, "#### Culprit {} of {}", i + 1, f.culprits.len());
                }
                if c.units.len() > 1 {
                    let _ = writeln!(
                        o,
                        "*These updates reproduce the failure together (1-minimal set):*\n"
                    );
                }
                let _ = writeln!(o, "| Package | Good | Bad |");
                let _ = writeln!(o, "| --- | --- | --- |");
                for &u in &c.units {
                    let ch = &rep.changes[u];
                    let from = if ch.from.is_empty() {
                        "*(new)*".to_string()
                    } else {
                        format!("`{}`", ch.from.join(", "))
                    };
                    let to = if ch.to.is_empty() {
                        "*(removed)*".to_string()
                    } else {
                        format!("`{}`", ch.to.join(", "))
                    };
                    let _ = writeln!(o, "| `{}` | {from} | {to} |", ch.name);
                }
                if c.exhausted {
                    let _ = writeln!(
                        o,
                        "\n> Stopped early due to trial budget or search limit; this set may not be minimal yet."
                    );
                }
                let _ = writeln!(o);
            }

            let verdict_line = match f.verdict {
                Verdict::Verified => {
                    "**Verified:** Reverting the culprit(s) in the bad lockfile makes the test command pass again."
                }
                Verdict::Unverified => {
                    "**Not verified:** The bad lockfile without the culprit(s) could not be built or tested."
                }
                Verdict::Incomplete => {
                    "**Incomplete:** The bad lockfile without the culprit(s) still fails (stopped by budget or culprit limit)."
                }
            };
            let _ = writeln!(o, "{verdict_line}\n");

            let pins = report::revert_commands(rep);
            if !pins.is_empty() {
                let _ = writeln!(o, "#### Suggested Revert / Pin Commands\n");
                let _ = writeln!(o, "```sh");
                for p in pins {
                    let _ = writeln!(o, "{p}");
                }
                let _ = writeln!(o, "```\n");
            }
        }
    }

    if !rep.warnings.is_empty() {
        let _ = writeln!(o, "#### Warnings\n");
        for w in &rep.warnings {
            let _ = writeln!(o, "- {w}");
        }
        let _ = writeln!(o);
    }

    let _ = writeln!(
        o,
        "**Trials:** {} run (+{} reused) · {} pass · {} fail · {} unresolved · {}",
        stats.run,
        stats.reused,
        stats.pass,
        stats.fail,
        stats.unresolved,
        ui::duration(rep.secs)
    );

    if !rep.records.is_empty() {
        let _ = writeln!(
            o,
            "\n<details>\n<summary>Trial log ({} entries)</summary>\n",
            rep.records.len()
        );
        let _ = writeln!(o, "| # | Outcome | Applied | Reason | Duration |");
        let _ = writeln!(o, "| ---: | --- | --- | --- | ---: |");
        for r in &rep.records {
            let applied = if r.config.is_empty() {
                "*good lockfile*".to_string()
            } else if r.config.len() == rep.changes.len() {
                "*bad lockfile*".to_string()
            } else if r.config.len() <= 3 {
                r.config
                    .iter()
                    .filter_map(|&i| rep.changes.get(i))
                    .map(|c| format!("`{}`", c.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                format!("{} of {} changes", r.config.len(), rep.changes.len())
            };
            let dur = if r.reused_from.is_some() {
                "reused".to_string()
            } else {
                ui::duration(r.secs)
            };
            let _ = writeln!(
                o,
                "| {} | `{}` | {} | {} | {} |",
                r.no,
                r.outcome.as_str(),
                applied,
                r.reason.replace('|', "\\|"),
                dur
            );
        }
        let _ = writeln!(o, "\n</details>");
    }

    if let Some(id) = &rep.experiment_id {
        let _ = writeln!(
            o,
            "\n<sub>Experiment ID: `{id}` · Generated by `whichdep {}`</sub>",
            crate::cli::VERSION
        );
    }
    o
}

/// Sticky PR comment with hidden HTML marker so GitHub Actions can upsert the
/// same comment cleanly on repeated CI runs.
pub fn github_comment(rep: &Report) -> String {
    format!("{STICKY_COMMENT_MARKER}\n{}", markdown(rep))
}

/// Render a sticky PR comment or Markdown summary directly from a saved
/// `whichdep --json` machine-readable report file.
pub fn format_from_report_json(json_text: &str, format: OutputFormat) -> Result<String, String> {
    let root = Json::parse(json_text)?;
    if format == OutputFormat::Json {
        return Ok(root.pretty());
    }
    let status = root
        .get("status")
        .and_then(Json::as_str)
        .unwrap_or("unknown");
    let verdict = root.get("verdict").and_then(Json::as_str).unwrap_or("");
    let ecosystem = root
        .get("ecosystem")
        .and_then(Json::as_str)
        .unwrap_or("unknown");
    let lockfile = root.get("lockfile").and_then(Json::as_str).unwrap_or("");
    let strategy = root
        .get("strategy")
        .and_then(Json::as_str)
        .unwrap_or("ddmin");
    let good = root.get("good").and_then(Json::as_str).unwrap_or("");
    let bad = root.get("bad").and_then(Json::as_str).unwrap_or("");
    let error = root.get("error").and_then(Json::as_str);
    let exp_id = root.get("experiment_id").and_then(Json::as_str);

    let mut md = String::new();
    if format == OutputFormat::GithubComment {
        let _ = writeln!(md, "{STICKY_COMMENT_MARKER}");
    }
    let title = match status {
        "found" => format!("### whichdep — Dependency Regression Found ({verdict})"),
        "no-result" => "### whichdep — No Culprit Isolated".to_string(),
        "error" => "### whichdep — Fatal Error".to_string(),
        _ => "### whichdep — Report".to_string(),
    };
    let _ = writeln!(md, "{title}");
    let _ = writeln!(
        md,
        "\nEcosystem: `{ecosystem}` · Lockfile: `{lockfile}` · Strategy: `{strategy}` · `{good}` → `{bad}`\n"
    );
    if let Some(err) = error {
        let _ = writeln!(md, "> **Reason:** {err}\n");
    }
    if let Some(culprits) = root.get("culprits").and_then(Json::as_arr) {
        for (i, c) in culprits.iter().enumerate() {
            if culprits.len() > 1 {
                let _ = writeln!(md, "#### Culprit {} of {}", i + 1, culprits.len());
            }
            if let Some(changes) = c.get("changes").and_then(Json::as_arr) {
                let _ = writeln!(md, "| Package | Good | Bad |");
                let _ = writeln!(md, "| --- | --- | --- |");
                for ch in changes {
                    let pkg = ch.get("package").and_then(Json::as_str).unwrap_or("");
                    let join_arr = |k: &str, empty: &str| -> String {
                        let items: Vec<&str> = ch
                            .get(k)
                            .and_then(Json::as_arr)
                            .map(|a| a.iter().filter_map(Json::as_str).collect())
                            .unwrap_or_default();
                        if items.is_empty() {
                            empty.to_string()
                        } else {
                            format!("`{}`", items.join(", "))
                        }
                    };
                    let _ = writeln!(
                        md,
                        "| `{pkg}` | {} | {} |",
                        join_arr("from", "*(new)*"),
                        join_arr("to", "*(removed)*")
                    );
                }
                let _ = writeln!(md);
            }
        }
    }
    if let Some(cmds) = root.get("revert_commands").and_then(Json::as_arr) {
        let pins: Vec<&str> = cmds.iter().filter_map(Json::as_str).collect();
        if !pins.is_empty() {
            let _ = writeln!(md, "#### Suggested Revert / Pin Commands\n\n```sh");
            for p in pins {
                let _ = writeln!(md, "{p}");
            }
            let _ = writeln!(md, "```\n");
        }
    }
    if let Some(id) = exp_id {
        let _ = writeln!(md, "<sub>Experiment ID: `{id}`</sub>");
    }

    match format {
        OutputFormat::Markdown | OutputFormat::GithubComment | OutputFormat::Text => Ok(md),
        OutputFormat::Html => Ok(wrap_html_page(
            "whichdep report",
            &format!("<pre class=\"md\">{}</pre>", html_escape(&md)),
            &root.pretty(),
        )),
        OutputFormat::Json => unreachable!(),
    }
}

/// Append Markdown report to `$GITHUB_STEP_SUMMARY` when running inside GitHub Actions.
pub fn write_github_summary(rep: &Report) -> io::Result<bool> {
    let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY") else {
        return Ok(false);
    };
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "\n{}", markdown(rep))?;
    Ok(true)
}

/// Emit GitHub Actions workflow annotations (`::error`, `::warning`, `::notice`).
pub fn emit_github_annotations(rep: &Report) {
    for w in &rep.warnings {
        eprintln!("::warning file={}::{w}", rep.lockfile);
    }
    match &rep.result {
        SearchResult::Found(f) => {
            let names: Vec<String> = f
                .culprits
                .iter()
                .map(|c| {
                    c.units
                        .iter()
                        .map(|&u| rep.changes[u].describe())
                        .collect::<Vec<_>>()
                        .join(" + ")
                })
                .collect();
            eprintln!(
                "::error file={},title=Dependency Regression Culprit ({})::{}",
                rep.lockfile,
                f.verdict.as_str(),
                names.join("; ")
            );
        }
        SearchResult::NoResult(err) => {
            eprintln!("::notice file={},title=whichdep::{err}", rep.lockfile);
        }
        SearchResult::Fatal(msg) => {
            eprintln!("::error title=whichdep fatal error::{msg}");
        }
        SearchResult::ListOnly => {}
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Self-contained single-file HTML report for one run, with embedded
/// machine-readable JSON payload for future web dashboard tooling.
pub fn html_report(rep: &Report) -> String {
    let stats = rep.stats();
    let json_payload = report::json(rep);

    let mut body = String::new();
    let (badge_cls, status_text) = match &rep.result {
        SearchResult::Found(f) => match f.verdict {
            Verdict::Verified => ("ok", "Culprit Isolated · Verified"),
            Verdict::Unverified => ("warn", "Culprit Isolated · Unverified"),
            Verdict::Incomplete => ("warn", "Partial Result · Incomplete"),
        },
        SearchResult::NoResult(_) => ("muted", "No Culprit Isolated"),
        SearchResult::Fatal(_) => ("err", "Fatal Error"),
        SearchResult::ListOnly => ("muted", "Change List"),
    };

    let _ = writeln!(
        body,
        "<header class=\"hero\"><div><span class=\"pill {badge_cls}\">{status_text}</span><h1>whichdep · {}</h1><p class=\"sub\">{} · strategy <code>{}</code> · <code>{}</code> → <code>{}</code></p></div></header>",
        html_escape(&rep.kind),
        html_escape(&rep.lockfile),
        html_escape(&rep.strategy),
        html_escape(&rep.good),
        html_escape(&rep.bad),
    );

    let _ = writeln!(
        body,
        "<section class=\"metrics\"><div class=\"card\"><div class=\"k\">Changes</div><div class=\"v\">{}</div></div><div class=\"card\"><div class=\"k\">Trials Run</div><div class=\"v\">{} <small>(+{} cached)</small></div></div><div class=\"card\"><div class=\"k\">Pass / Fail / Unresolved</div><div class=\"v\">{} / {} / {}</div></div><div class=\"card\"><div class=\"k\">Duration</div><div class=\"v\">{}</div></div></section>",
        rep.changes.len(),
        stats.run,
        stats.reused,
        stats.pass,
        stats.fail,
        stats.unresolved,
        html_escape(&ui::duration(rep.secs)),
    );

    if let SearchResult::Found(f) = &rep.result {
        let _ = writeln!(body, "<section class=\"panel\"><h2>Culprits</h2>");
        for (i, c) in f.culprits.iter().enumerate() {
            if f.culprits.len() > 1 {
                let _ = writeln!(body, "<h3>Culprit {} of {}</h3>", i + 1, f.culprits.len());
            }
            let _ = writeln!(
                body,
                "<table><thead><tr><th>Package</th><th>Good</th><th>Bad</th></tr></thead><tbody>"
            );
            for &u in &c.units {
                let ch = &rep.changes[u];
                let _ = writeln!(
                    body,
                    "<tr><td><code>{}</code></td><td><code>{}</code></td><td><code>{}</code></td></tr>",
                    html_escape(&ch.name),
                    html_escape(&ch.from.join(", ")),
                    html_escape(&ch.to.join(", "))
                );
            }
            let _ = writeln!(body, "</tbody></table>");
        }
        let pins = report::revert_commands(rep);
        if !pins.is_empty() {
            let _ = writeln!(
                body,
                "<h3>Revert / Pin Commands</h3><pre><code>{}</code></pre>",
                html_escape(&pins.join("\n"))
            );
        }
        let _ = writeln!(body, "</section>");
    } else if let SearchResult::NoResult(err) = &rep.result {
        let _ = writeln!(
            body,
            "<section class=\"panel\"><h2>No Result</h2><p>{}</p></section>",
            html_escape(&err.to_string())
        );
    } else if let SearchResult::Fatal(msg) = &rep.result {
        let _ = writeln!(
            body,
            "<section class=\"panel\"><h2>Error</h2><p>{}</p></section>",
            html_escape(msg)
        );
    }

    if !rep.records.is_empty() {
        let _ = writeln!(
            body,
            "<section class=\"panel\"><h2>Trial Timeline</h2><table><thead><tr><th>#</th><th>Outcome</th><th>Config</th><th>Reason</th><th>Time</th></tr></thead><tbody>"
        );
        for r in &rep.records {
            let cls = match r.outcome {
                Outcome::Pass => "ok",
                Outcome::Fail => "err",
                Outcome::Unresolved => "warn",
            };
            let cfg = if r.config.is_empty() {
                "good lockfile".to_string()
            } else if r.config.len() == rep.changes.len() {
                "bad lockfile".to_string()
            } else {
                r.config
                    .iter()
                    .filter_map(|&i| rep.changes.get(i))
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let _ = writeln!(
                body,
                "<tr><td>#{}</td><td><span class=\"pill {cls}\">{}</span></td><td><code>{}</code></td><td>{}</td><td>{}</td></tr>",
                r.no,
                r.outcome.as_str(),
                html_escape(&cfg),
                html_escape(&r.reason),
                html_escape(&ui::duration(r.secs))
            );
        }
        let _ = writeln!(body, "</tbody></table></section>");
    }

    wrap_html_page("whichdep — regression report", &body, &json_payload)
}

/// Self-contained multi-experiment HTML dashboard generated from `ExperimentStore`.
pub fn dashboard_html(summaries: &[ExperimentSummary]) -> String {
    let items: Vec<Json> = summaries
        .iter()
        .map(|s| {
            Json::obj([
                ("id", Json::str(&s.id)),
                ("ecosystem", Json::str(&s.ecosystem)),
                ("description", Json::str(&s.description)),
                ("baseline_label", Json::str(&s.baseline_label)),
                ("target_label", Json::str(&s.target_label)),
                ("unit_count", Json::int(s.unit_count)),
                ("runs", Json::int(s.runs)),
                (
                    "latest_verification",
                    Json::opt_str(s.latest_verification.clone()),
                ),
                ("latest_strategy", Json::opt_str(s.latest_strategy.clone())),
            ])
        })
        .collect();
    let payload = Json::obj([
        ("schema", Json::str("whichdep-dashboard-v1")),
        ("experiments", Json::Arr(items)),
    ])
    .pretty();

    let mut body = String::new();
    let _ = writeln!(
        body,
        "<header class=\"hero\"><div><span class=\"pill ok\">whichdep dashboard</span><h1>Experiment History</h1><p class=\"sub\">{} persisted experiments</p></div></header>",
        summaries.len()
    );
    let _ = writeln!(
        body,
        "<section class=\"panel\"><table><thead><tr><th>Experiment ID</th><th>Ecosystem</th><th>Description</th><th>Units</th><th>Runs</th><th>Strategy</th><th>Latest Verdict</th></tr></thead><tbody>"
    );
    for s in summaries {
        let verdict = s.latest_verification.as_deref().unwrap_or("none");
        let cls = match verdict {
            "verified" => "ok",
            "incomplete" | "unverified" => "warn",
            _ => "muted",
        };
        let _ = writeln!(
            body,
            "<tr><td><code>{}</code></td><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td><code>{}</code></td><td><span class=\"pill {cls}\">{}</span></td></tr>",
            html_escape(&s.id),
            html_escape(&s.ecosystem),
            html_escape(&s.description),
            s.unit_count,
            s.runs,
            html_escape(s.latest_strategy.as_deref().unwrap_or("-")),
            html_escape(verdict)
        );
    }
    let _ = writeln!(body, "</tbody></table></section>");
    wrap_html_page("whichdep — experiment dashboard", &body, &payload)
}

fn wrap_html_page(title: &str, body: &str, embedded_json: &str) -> String {
    let safe_json = embedded_json.replace("</script>", "<\\/script>");
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title}</title>
<style>
:root {{ color-scheme: light dark; --bg: #0f1117; --card: #171b24; --border: #2a3142; --fg: #e6edf3; --muted: #8b949e; --ok: #2ea043; --warn: #d29922; --err: #f85149; }}
@media (prefers-color-scheme: light) {{ :root {{ --bg: #f6f8fa; --card: #ffffff; --border: #d0d7de; --fg: #1f2328; --muted: #656d76; }} }}
body {{ font-family: ui-sans-serif, system-ui, -apple-system, sans-serif; background: var(--bg); color: var(--fg); margin: 0; padding: 2rem; line-height: 1.5; }}
main {{ max-width: 980px; margin: 0 auto; }}
.hero h1 {{ margin: .4rem 0 .2rem; font-size: 1.7rem; }}
.sub {{ color: var(--muted); margin: 0 0 1.4rem; }}
.metrics {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: .8rem; margin-bottom: 1.2rem; }}
.card, .panel {{ background: var(--card); border: 1px solid var(--border); border-radius: 10px; padding: 1rem 1.2rem; margin-bottom: 1rem; }}
.card .k {{ font-size: .8rem; color: var(--muted); text-transform: uppercase; letter-spacing: .04em; }}
.card .v {{ font-size: 1.25rem; font-weight: 600; margin-top: .2rem; }}
table {{ width: 100%; border-collapse: collapse; font-size: .92rem; }}
th, td {{ text-align: left; padding: .5rem .6rem; border-bottom: 1px solid var(--border); }}
code, pre {{ font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: .86rem; }}
pre {{ background: rgba(127,127,127,.1); padding: .8rem 1rem; border-radius: 8px; overflow-x: auto; }}
.pill {{ display: inline-block; padding: .15rem .55rem; border-radius: 999px; font-size: .78rem; font-weight: 600; }}
.pill.ok {{ background: rgba(46,160,67,.2); color: var(--ok); }}
.pill.warn {{ background: rgba(210,153,34,.2); color: var(--warn); }}
.pill.err {{ background: rgba(248,81,73,.2); color: var(--err); }}
.pill.muted {{ background: rgba(139,148,158,.2); color: var(--muted); }}
</style>
</head>
<body>
<main>
{body}
</main>
<script type="application/json" id="whichdep-data">
{safe_json}
</script>
</body>
</html>
"#,
        title = html_escape(title)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dd::{Culprit, Findings};
    use crate::model::Change;

    fn sample_report() -> Report {
        Report {
            kind: "npm".to_string(),
            lockfile: "package-lock.json".to_string(),
            strategy: "adaptive".to_string(),
            good: "origin/main".to_string(),
            bad: "HEAD".to_string(),
            changes: vec![Change {
                name: "zod".to_string(),
                from: vec!["3.22.4".to_string()],
                to: vec!["3.23.8".to_string()],
            }],
            hints: vec![Some("npm install --save-exact zod@3.22.4".to_string())],
            warnings: vec![],
            result: SearchResult::Found(Findings {
                culprits: vec![Culprit {
                    units: vec![0],
                    exhausted: false,
                }],
                verdict: Verdict::Verified,
            }),
            records: vec![],
            cached_trials: vec![],
            cache_hits: 0,
            secs: 1.5,
            kept: None,
            experiment_id: Some("exp-test".to_string()),
        }
    }

    #[test]
    fn github_comment_includes_sticky_marker_table_and_pin_command() {
        let comment = github_comment(&sample_report());
        assert!(comment.starts_with(STICKY_COMMENT_MARKER));
        assert!(comment.contains("| `zod` | `3.22.4` | `3.23.8` |"));
        assert!(comment.contains("npm install --save-exact zod@3.22.4"));
        assert!(comment.contains("exp-test"));
    }

    #[test]
    fn html_report_embeds_valid_json_payload() {
        let html = html_report(&sample_report());
        assert!(html.contains("<script type=\"application/json\" id=\"whichdep-data\">"));
        assert!(html.contains("\"schema\": \"whichdep-report-v2\""));
        assert!(html.contains("zod"));
    }

    #[test]
    fn format_from_report_json_renders_sticky_pr_comment() {
        let json = report::json(&sample_report());
        let comment = format_from_report_json(&json, OutputFormat::GithubComment).unwrap();
        assert!(comment.starts_with(STICKY_COMMENT_MARKER));
        assert!(comment.contains("zod"));
        assert!(comment.contains("npm install --save-exact zod@3.22.4"));
    }
}
