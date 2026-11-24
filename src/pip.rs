//! Fully pinned requirements files: `pip freeze`, `pip-compile`,
//! `uv pip compile` and `uv export --format requirements-txt` output.
//!
//! Every requirement block (the line, its `--hash` continuation lines and the
//! `# via` comments below it) is one unit. Applying a unit swaps the block of
//! the good file for the one of the bad file, so hashes always travel with
//! their version.

use crate::dd::{DepClosure, Set};
use crate::model::{Change, Ecosystem};
use std::collections::HashMap;
use std::path::Path;

/// PEP 503 name normalisation.
pub fn normalize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_sep = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            if !last_sep {
                out.push('-');
            }
            last_sep = true;
        } else {
            out.push(c.to_ascii_lowercase());
            last_sep = false;
        }
    }
    out
}

#[derive(Clone, Debug)]
struct Req {
    key: String,
    name: String,
    marker: String,
    spec: String,
    text: String,
    comments: Vec<String>,
    via: Vec<String>,
}

#[derive(Debug)]
enum Item {
    Other(String),
    Req(usize),
}

#[derive(Debug)]
struct Parsed {
    items: Vec<Item>,
    reqs: Vec<Req>,
}

fn strip_inline_comment(line: &str) -> &str {
    // In requirements.txt, an inline comment begins with whitespace followed by `#`.
    // Preserving `#` without leading whitespace avoids truncating URL fragments like `#egg=...`.
    let bytes = line.as_bytes();
    for i in 1..bytes.len() {
        if bytes[i] == b'#' && bytes[i - 1].is_ascii_whitespace() {
            return line[..i].trim_end();
        }
    }
    line
}

fn parse_req(line: &str, text: String) -> Result<Req, String> {
    let line = strip_inline_comment(line);
    let main = match line.find(" --") {
        Some(p) => &line[..p],
        None => line,
    };
    let (spec_part, marker) = match main.split_once(';') {
        Some((a, b)) => (a.trim(), b.trim()),
        None => (main.trim(), ""),
    };
    let name_end = spec_part
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        .unwrap_or(spec_part.len());
    let name = &spec_part[..name_end];
    if name.is_empty() {
        return Err(format!("cannot parse requirement: {line}"));
    }
    let mut rest = spec_part[name_end..].trim_start();
    if rest.starts_with('[') {
        match rest.find(']') {
            Some(p) => rest = rest[p + 1..].trim_start(),
            None => return Err(format!("unterminated extras in: {line}")),
        }
    }
    let spec = match rest.strip_prefix("==") {
        Some(v) => v.trim().to_string(),
        None => rest.trim().to_string(),
    };
    let marker: String = marker.split_whitespace().collect::<Vec<_>>().join(" ");
    Ok(Req {
        key: format!("{}|{}", normalize(name), marker),
        name: name.to_string(),
        marker,
        spec,
        text,
        comments: Vec::new(),
        via: Vec::new(),
    })
}

/// `# via a, b`, `# via` followed by `#   a` lines, `# via -r requirements.in`.
fn parse_via(comments: &[String]) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_via = false;
    for c in comments {
        let body = c.trim_start_matches('#').trim();
        let rest = if let Some(r) = body.strip_prefix("via") {
            in_via = true;
            r
        } else if in_via {
            body
        } else {
            continue;
        };
        let mut toks = rest
            .split(|ch: char| ch == ',' || ch.is_whitespace())
            .filter(|t| !t.is_empty());
        while let Some(t) = toks.next() {
            if t == "-r" || t == "-c" {
                toks.next();
                continue;
            }
            if t.starts_with('-')
                || t.ends_with(".in")
                || t.ends_with(".txt")
                || t.ends_with(".toml")
            {
                continue;
            }
            names.push(normalize(t));
        }
    }
    names
}

impl Parsed {
    fn parse(text: &str) -> Result<Parsed, String> {
        let lines: Vec<&str> = text.lines().collect();
        let mut items = Vec::new();
        let mut reqs: Vec<Req> = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let start = i;
            let mut logical = String::new();
            while i < lines.len() {
                let t = lines[i].trim_end();
                i += 1;
                if let Some(head) = t.strip_suffix('\\') {
                    logical.push_str(head);
                    logical.push(' ');
                } else {
                    logical.push_str(t);
                    break;
                }
            }
            let raw = lines[start..i].join("\n") + "\n";
            let trimmed = logical.trim();
            if trimmed.is_empty() || trimmed.starts_with('-') {
                items.push(Item::Other(raw));
            } else if trimmed.starts_with('#') {
                let indented = lines[start].starts_with(char::is_whitespace);
                match items.last() {
                    Some(Item::Req(r)) if indented => {
                        let r = *r;
                        reqs[r].text.push_str(&raw);
                        reqs[r].comments.push(trimmed.to_string());
                    }
                    _ => items.push(Item::Other(raw)),
                }
            } else {
                reqs.push(parse_req(trimmed, raw)?);
                items.push(Item::Req(reqs.len() - 1));
            }
        }
        for r in &mut reqs {
            r.via = parse_via(&r.comments);
        }
        Ok(Parsed { items, reqs })
    }

    fn by_key(&self) -> HashMap<&str, usize> {
        self.reqs
            .iter()
            .enumerate()
            .map(|(i, r)| (r.key.as_str(), i))
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Updated,
    Added,
    Removed,
}

#[derive(Debug)]
struct Unit {
    kind: Kind,
    good: Option<usize>,
    bad: Option<usize>,
}

pub struct PipPlan {
    lockfile: String,
    good: Parsed,
    bad: Parsed,
    units: Vec<Unit>,
    changes: Vec<Change>,
    closure: DepClosure,
    warnings: Vec<String>,
}

fn label(r: &Req) -> String {
    if r.marker.is_empty() {
        r.name.clone()
    } else {
        format!("{} ; {}", r.name, r.marker)
    }
}

impl PipPlan {
    pub fn new(lockfile: &str, good_text: &str, bad_text: &str) -> Result<PipPlan, String> {
        let good = Parsed::parse(good_text).map_err(|e| format!("good: {e}"))?;
        let bad = Parsed::parse(bad_text).map_err(|e| format!("bad: {e}"))?;
        let mut warnings = Vec::new();
        for (side, p) in [("good", &good), ("bad", &bad)] {
            let loose = p
                .reqs
                .iter()
                .filter(|r| !r.spec.is_empty() && !is_pinned(r))
                .count();
            if loose > 0 || p.reqs.iter().any(|r| r.spec.is_empty()) {
                warnings.push(format!(
                    "{side} file has requirements that are not pinned with ==; compile it first (uv pip compile / pip-compile) for reliable results"
                ));
            }
        }
        let good_ix = good.by_key();
        let bad_ix = bad.by_key();

        let mut units = Vec::new();
        let mut changes = Vec::new();
        for (gi, g) in good.reqs.iter().enumerate() {
            match bad_ix.get(g.key.as_str()) {
                Some(&bi) if bad.reqs[bi].spec == g.spec => {}
                Some(&bi) => {
                    units.push(Unit {
                        kind: Kind::Updated,
                        good: Some(gi),
                        bad: Some(bi),
                    });
                    changes.push(Change {
                        name: label(g),
                        from: vec![g.spec.clone()],
                        to: vec![bad.reqs[bi].spec.clone()],
                    });
                }
                None => {
                    units.push(Unit {
                        kind: Kind::Removed,
                        good: Some(gi),
                        bad: None,
                    });
                    changes.push(Change {
                        name: label(g),
                        from: vec![g.spec.clone()],
                        to: vec![],
                    });
                }
            }
        }
        for (bi, b) in bad.reqs.iter().enumerate() {
            if !good_ix.contains_key(b.key.as_str()) {
                units.push(Unit {
                    kind: Kind::Added,
                    good: None,
                    bad: Some(bi),
                });
                changes.push(Change {
                    name: label(b),
                    from: vec![],
                    to: vec![b.spec.clone()],
                });
            }
        }

        // Closure edges derived from `# via` comments (pip-compile / uv pip compile):
        // 1. A changed parent named by `# via` for a newly added package requires
        //    that added package to come along with it (`parent -> added_child`).
        // 2. A removed package whose `good` `# via` names a changed/removed parent
        //    cannot be removed unless that parent change is also applied
        //    (`removed_child -> parent`). That way, reverting the parent during
        //    verification automatically restores the removed transitive dependency
        //    (e.g. reverting `httpx 0.28.1 -> 0.27.2` restores `sniffio`).
        let mut edges = Vec::new();
        for (ci, c) in units.iter().enumerate() {
            match c.kind {
                Kind::Added => {
                    let child = &bad.reqs[c.bad.unwrap()];
                    for parent in &child.via {
                        for (pi, p) in units.iter().enumerate() {
                            if pi == ci || p.kind == Kind::Removed {
                                continue;
                            }
                            let pr = &bad.reqs[p.bad.unwrap()];
                            if normalize(&pr.name) == *parent {
                                edges.push((pi, ci));
                            }
                        }
                    }
                }
                Kind::Removed => {
                    let child = &good.reqs[c.good.unwrap()];
                    for parent in &child.via {
                        for (pi, p) in units.iter().enumerate() {
                            if pi == ci || p.kind == Kind::Added {
                                continue;
                            }
                            let pr = &good.reqs[p.good.unwrap()];
                            if normalize(&pr.name) == *parent {
                                edges.push((ci, pi));
                            }
                        }
                    }
                }
                Kind::Updated => {}
            }
        }
        let closure = DepClosure::new(units.len(), edges, []);
        Ok(PipPlan {
            lockfile: lockfile.to_string(),
            good,
            bad,
            units,
            changes,
            closure,
            warnings,
        })
    }
}

fn is_pinned(r: &Req) -> bool {
    // "==" was stripped while parsing, so a pinned spec starts with a digit
    // (or is a direct reference).
    r.spec.starts_with(|c: char| c.is_ascii_digit()) || r.spec.starts_with('@')
}

impl Ecosystem for PipPlan {
    fn kind(&self) -> &'static str {
        "pip"
    }
    fn lockfile(&self) -> &str {
        &self.lockfile
    }
    fn changes(&self) -> &[Change] {
        &self.changes
    }
    fn closure(&self) -> &DepClosure {
        &self.closure
    }

    fn render(&self, applied: &Set) -> String {
        let mut by_good: HashMap<usize, usize> = HashMap::new();
        for (ui, u) in self.units.iter().enumerate() {
            if let Some(g) = u.good {
                by_good.insert(g, ui);
            }
        }
        let mut out = String::new();
        for item in &self.good.items {
            match item {
                Item::Other(s) => out.push_str(s),
                Item::Req(gi) => match by_good.get(gi) {
                    Some(&ui) if applied.contains(&ui) => {
                        if let Some(bi) = self.units[ui].bad {
                            out.push_str(&self.bad.reqs[bi].text);
                        }
                    }
                    _ => out.push_str(&self.good.reqs[*gi].text),
                },
            }
        }
        for (ui, u) in self.units.iter().enumerate() {
            if u.kind == Kind::Added && applied.contains(&ui) {
                out.push_str(&self.bad.reqs[u.bad.unwrap()].text);
            }
        }
        out
    }

    fn verify(&self, _applied: &Set, _lock_after_prepare: &str) -> Result<(), String> {
        // The prepare script installs exactly the pins (no resolution) and
        // runs a consistency check, so there is nothing left to compare.
        Ok(())
    }

    fn default_prepare(&self) -> &'static str {
        r#"set -e
VENV=.whichdep-venv
if command -v uv >/dev/null 2>&1; then
  [ -d "$VENV" ] || uv venv --quiet "$VENV"
  uv pip sync --quiet --python "$VENV/bin/python" "$WHICHDEP_LOCKFILE"
  uv pip check --python "$VENV/bin/python"
else
  rm -rf "$VENV"
  python3 -m venv "$VENV"
  "$VENV/bin/pip" install --quiet --no-deps -r "$WHICHDEP_LOCKFILE"
  "$VENV/bin/pip" check
fi
"#
    }

    fn env(&self, trial_dir: &Path, _shared_dir: &Path) -> Vec<(String, String)> {
        let venv = trial_dir.join(".whichdep-venv");
        let path = std::env::var("PATH").unwrap_or_default();
        vec![
            ("VIRTUAL_ENV".into(), venv.display().to_string()),
            (
                "PATH".into(),
                format!("{}:{}", venv.join("bin").display(), path),
            ),
            ("WHICHDEP_LOCKFILE".into(), self.lockfile.clone()),
        ]
    }

    fn warnings(&self) -> &[String] {
        &self.warnings
    }

    fn pin_hint(&self, unit: usize) -> Option<String> {
        let g = self.good.reqs.get(self.units.get(unit)?.good?)?;
        if is_pinned(g) && !g.spec.starts_with('@') {
            Some(format!("{}=={}", g.name, g.spec))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dd::Closure;

    const GOOD: &str = "\
# compiled
--index-url https://pypi.org/simple
alpha==1.0.0 \\
    --hash=sha256:aa \\
    --hash=sha256:ab
    # via -r requirements.in
beta==2.0.0
    # via alpha
gamma==3.0.0
";
    const BAD: &str = "\
# compiled
--index-url https://pypi.org/simple
alpha==1.1.0 \\
    --hash=sha256:ba
    # via -r requirements.in
beta==2.0.0
    # via alpha
delta==0.5.0
    # via alpha
";

    fn plan() -> PipPlan {
        PipPlan::new("requirements.txt", GOOD, BAD).unwrap()
    }

    fn idx(p: &PipPlan, name: &str) -> usize {
        p.changes.iter().position(|c| c.name == name).unwrap()
    }

    #[test]
    fn finds_updated_removed_and_added_requirements() {
        let p = plan();
        let described: Vec<String> = p.changes().iter().map(|c| c.describe()).collect();
        assert_eq!(
            described,
            vec![
                "alpha 1.0.0 -> 1.1.0",
                "gamma 3.0.0 (removed)",
                "delta 0.5.0 (new)"
            ]
        );
    }

    #[test]
    fn nothing_applied_reproduces_the_good_file() {
        assert_eq!(plan().render(&Set::new()), GOOD);
    }

    #[test]
    fn everything_applied_matches_the_bad_file_up_to_order() {
        let p = plan();
        let all: Set = (0..p.changes().len()).collect();
        let out = p.render(&all);
        assert!(
            out.contains("alpha==1.1.0 \\\n    --hash=sha256:ba\n    # via -r requirements.in\n")
        );
        assert!(
            !out.contains("--hash=sha256:aa"),
            "hashes of the old version must go with it"
        );
        assert!(!out.contains("gamma"));
        assert!(out.ends_with("delta==0.5.0\n    # via alpha\n"));
        assert!(out.contains("--index-url https://pypi.org/simple\n"));
    }

    #[test]
    fn updated_parent_pulls_in_its_new_child_dependency() {
        let p = plan();
        let (alpha, delta) = (idx(&p, "alpha"), idx(&p, "delta"));
        assert_eq!(
            p.closure().up(&[alpha].into_iter().collect()),
            [alpha, delta].into_iter().collect()
        );
        assert_eq!(
            p.closure().up(&[delta].into_iter().collect()),
            [delta].into_iter().collect()
        );
    }

    #[test]
    fn inline_comments_do_not_corrupt_versions_or_trigger_false_changes() {
        let good = "alpha==1.0.0 # old note\nbeta==2.0.0 # pinned\n";
        let bad = "alpha==1.0.0 # new note\nbeta==2.1.0 # bumped\n";
        let p = PipPlan::new("r.txt", good, bad).unwrap();
        assert_eq!(p.changes().len(), 1);
        assert_eq!(p.changes()[0].describe(), "beta 2.0.0 -> 2.1.0");
        assert_eq!(p.pin_hint(0).as_deref(), Some("beta==2.0.0"));
    }

    #[test]
    fn reverting_a_parent_restores_its_removed_transitive_dependency() {
        // Real-world pattern observed in httpx 0.27.2 -> 0.28.1, which dropped sniffio.
        let good = "httpx==0.27.2\n    # via -r requirements.in\nsniffio==1.3.1\n    # via httpx\nrich==13.8.0\n";
        let bad = "httpx==0.28.1\n    # via -r requirements.in\nrich==13.9.4\n";
        let p = PipPlan::new("requirements.txt", good, bad).unwrap();
        let httpx = idx(&p, "httpx");
        let sniffio = idx(&p, "sniffio");
        let rich = idx(&p, "rich");

        // Removing sniffio requires applying httpx's upgrade.
        assert_eq!(
            p.closure().up(&[sniffio].into_iter().collect()),
            [httpx, sniffio].into_iter().collect()
        );
        // Reverting httpx from the full bad state also drops the sniffio-removal
        // unit via `down()`, restoring sniffio==1.3.1 in the rendered lockfile.
        let without_httpx = p.closure().down(&[sniffio, rich].into_iter().collect());
        assert_eq!(without_httpx, [rich].into_iter().collect());
        let rendered = p.render(&without_httpx);
        assert!(rendered.contains("httpx==0.27.2"));
        assert!(rendered.contains("sniffio==1.3.1"));
        assert!(rendered.contains("rich==13.9.4"));
    }

    #[test]
    fn names_are_compared_the_way_pip_does() {
        let good = "Foo_Bar==1.0\n";
        let bad = "foo.bar==1.1\n";
        let p = PipPlan::new("r.txt", good, bad).unwrap();
        assert_eq!(p.changes().len(), 1);
        assert_eq!(p.changes()[0].describe(), "Foo_Bar 1.0 -> 1.1");
    }

    #[test]
    fn markers_make_separate_units() {
        let good = "numpy==1.26.4 ; python_version < \"3.12\"\nnumpy==2.0.0 ; python_version >= \"3.12\"\n";
        let bad = "numpy==1.26.4 ; python_version < \"3.12\"\nnumpy==2.1.0 ; python_version >= \"3.12\"\n";
        let p = PipPlan::new("r.txt", good, bad).unwrap();
        assert_eq!(p.changes().len(), 1);
        assert!(p.changes()[0].name.contains("python_version >= \"3.12\""));
    }

    #[test]
    fn unpinned_requirements_are_reported() {
        let p = PipPlan::new("r.txt", "foo>=1\n", "foo>=2\n").unwrap();
        assert!(!p.warnings().is_empty());
    }
}
