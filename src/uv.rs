//! `uv.lock` (lockfile version 1) ecosystem adapter.
//!
//! Preserves every `[[package]]` block (including wheels, sdist hashes,
//! markers, extras, dev/optional dependency tables and metadata) verbatim from
//! the good or bad lockfile so `uv sync --locked` can verify and install exact
//! subsets without re-resolving.

use crate::adapter::Adapter;
use crate::cargo::cmp_version;
use crate::dd::{DepClosure, Set};
use crate::model::{Change, Ecosystem};
use crate::pip::normalize;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UvNodeId {
    pub name: String,
    pub version: String,
    pub source: String,
}

impl UvNodeId {
    fn norm_name(&self) -> String {
        normalize(&self.name)
    }

    fn is_local(&self) -> bool {
        self.source.is_empty()
            || self.source.contains("virtual")
            || self.source.contains("editable")
            || self.source.contains("path")
            || self.source.contains("directory")
    }

    fn show(&self) -> String {
        if self.source.contains("git") {
            let rev = self
                .source
                .split('#')
                .nth(1)
                .and_then(|s| s.split('"').next())
                .unwrap_or("");
            if !rev.is_empty() {
                return format!("{} (git {})", self.version, &rev[..rev.len().min(8)]);
            }
        }
        self.version.clone()
    }
}

impl Ord for UvNodeId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.norm_name()
            .cmp(&other.norm_name())
            .then_with(|| cmp_version(&self.version, &other.version))
            .then_with(|| self.source.cmp(&other.source))
    }
}

impl PartialOrd for UvNodeId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug)]
struct UvDepRef {
    name: String,
    version: Option<String>,
}

#[derive(Clone, Debug)]
struct UvPkg {
    id: UvNodeId,
    deps: Vec<UvDepRef>,
    raw: String,
}

#[derive(Debug)]
struct UvLock {
    preamble: String,
    version: u32,
    pkgs: Vec<UvPkg>,
}

fn parse_quoted_value(val: &str) -> Option<String> {
    let v = val.trim();
    if let Some(inner) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return Some(inner.replace("\\\"", "\"").replace("\\\\", "\\"));
    }
    if let Some(inner) = v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        return Some(inner.to_string());
    }
    None
}

fn extract_inline_field(table: &str, field: &str) -> Option<String> {
    let mut i = 0;
    let bytes = table.as_bytes();
    while i < bytes.len() {
        // Skip whitespace and punctuation
        while i < bytes.len()
            && (bytes[i].is_ascii_whitespace() || matches!(bytes[i], b'{' | b'}' | b','))
        {
            i += 1;
        }
        let key_start = i;
        while i < bytes.len()
            && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'-' | b'_'))
        {
            i += 1;
        }
        let key = &table[key_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            while i < bytes.len() && bytes[i] != b',' {
                i += 1;
            }
            continue;
        }
        i += 1; // '='
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let val_start = i;
        let val_end = if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            i += 1;
            let mut esc = false;
            while i < bytes.len() {
                if esc {
                    esc = false;
                } else if quote == b'"' && bytes[i] == b'\\' {
                    esc = true;
                } else if bytes[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            i
        } else if bytes[i] == b'{' {
            let mut depth = 0usize;
            while i < bytes.len() {
                if bytes[i] == b'{' {
                    depth += 1;
                } else if bytes[i] == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                i += 1;
            }
            i
        } else {
            while i < bytes.len() && !matches!(bytes[i], b',' | b'}') {
                i += 1;
            }
            i
        };
        if key == field {
            let raw_val = table[val_start..val_end].trim();
            return parse_quoted_value(raw_val).or_else(|| Some(raw_val.to_string()));
        }
    }
    None
}

/// Extract `{ name = "...", version = "..." }` entries from a TOML array slice.
fn extract_dep_tables(array_text: &str) -> Vec<UvDepRef> {
    let mut deps = Vec::new();
    let bytes = array_text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            let start = i;
            let mut depth = 0usize;
            let mut in_str: Option<u8> = None;
            let mut esc = false;
            while i < bytes.len() {
                let b = bytes[i];
                if let Some(q) = in_str {
                    if esc {
                        esc = false;
                    } else if q == b'"' && b == b'\\' {
                        esc = true;
                    } else if b == q {
                        in_str = None;
                    }
                } else if b == b'"' || b == b'\'' {
                    in_str = Some(b);
                } else if b == b'{' {
                    depth += 1;
                } else if b == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        let table = &array_text[start..=i];
                        if let Some(name) = extract_inline_field(table, "name") {
                            let version = extract_inline_field(table, "version");
                            deps.push(UvDepRef { name, version });
                        }
                        i += 1;
                        break;
                    }
                }
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    deps
}

fn parse_uv_pkg(raw: String) -> Result<UvPkg, String> {
    let lines: Vec<&str> = raw.lines().collect();
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut source = String::new();
    let mut deps: Vec<UvDepRef> = Vec::new();

    let mut section = "package";
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        i += 1;
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed == "[[package]]" {
            section = "package";
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let inner = trimmed.trim_matches(|c| c == '[' || c == ']').trim();
            section = if inner == "package.dev-dependencies" {
                "dev-dependencies"
            } else if inner == "package.optional-dependencies" {
                "optional-dependencies"
            } else if inner == "package.metadata" {
                "metadata"
            } else {
                "other"
            };
            continue;
        }

        let Some((k, v)) = trimmed.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());

        if section == "package" {
            match k {
                "name" => name = parse_quoted_value(v),
                "version" => version = parse_quoted_value(v),
                "source" => source = v.to_string(),
                "dependencies" => {
                    let mut buf = v.to_string();
                    while !bracket_balanced(&buf) && i < lines.len() {
                        buf.push('\n');
                        buf.push_str(lines[i]);
                        i += 1;
                    }
                    deps.extend(extract_dep_tables(&buf));
                }
                _ => {}
            }
        } else if matches!(section, "dev-dependencies" | "optional-dependencies") {
            let mut buf = v.to_string();
            while !bracket_balanced(&buf) && i < lines.len() {
                buf.push('\n');
                buf.push_str(lines[i]);
                i += 1;
            }
            deps.extend(extract_dep_tables(&buf));
        }
    }

    let name =
        name.ok_or_else(|| "a [[package]] table in uv.lock is missing `name`".to_string())?;
    let version = version.unwrap_or_default();
    Ok(UvPkg {
        id: UvNodeId {
            name,
            version,
            source,
        },
        deps,
        raw,
    })
}

fn bracket_balanced(text: &str) -> bool {
    let mut depth: isize = 0;
    let mut in_str: Option<u8> = None;
    let mut esc = false;
    for &b in text.as_bytes() {
        if let Some(q) = in_str {
            if esc {
                esc = false;
            } else if q == b'"' && b == b'\\' {
                esc = true;
            } else if b == q {
                in_str = None;
            }
        } else if b == b'"' || b == b'\'' {
            in_str = Some(b);
        } else if b == b'[' {
            depth += 1;
        } else if b == b']' {
            depth -= 1;
        }
    }
    depth <= 0
}

fn parse_uv_lock(text: &str) -> Result<UvLock, String> {
    let mut preamble = String::new();
    let mut blocks: Vec<String> = Vec::new();
    let mut current: Option<String> = None;

    for line in text.lines() {
        if line.trim() == "[[package]]" {
            if let Some(prev) = current.take() {
                blocks.push(prev);
            }
            let mut blk = String::from(line);
            blk.push('\n');
            current = Some(blk);
        } else if let Some(cur) = current.as_mut() {
            cur.push_str(line);
            cur.push('\n');
        } else {
            preamble.push_str(line);
            preamble.push('\n');
        }
    }
    if let Some(last) = current {
        blocks.push(last);
    }

    let mut lock_version: Option<u32> = None;
    for line in preamble.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("version") {
            if let Some(val) = rest.trim_start().strip_prefix('=') {
                lock_version = val.trim().parse::<u32>().ok();
                break;
            }
        }
    }
    let version = lock_version
        .ok_or_else(|| "missing `version = 1` header: not a valid uv.lock file".to_string())?;
    if version != 1 {
        return Err(format!(
            "uv.lock version {version} is not supported (only version 1)"
        ));
    }

    let mut pkgs = Vec::with_capacity(blocks.len());
    for block in blocks {
        pkgs.push(parse_uv_pkg(block)?);
    }
    Ok(UvLock {
        preamble,
        version,
        pkgs,
    })
}

fn major_compatible(a: &str, b: &str) -> bool {
    let major = |v: &str| v.split('.').next().and_then(|n| n.parse::<u64>().ok());
    let minor = |v: &str| v.split('.').nth(1).and_then(|n| n.parse::<u64>().ok());
    match (major(a), major(b)) {
        (Some(0), Some(0)) => minor(a) == minor(b),
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

impl UvLock {
    fn by_norm_name(&self) -> HashMap<String, Vec<usize>> {
        let mut map: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, p) in self.pkgs.iter().enumerate() {
            map.entry(p.id.norm_name()).or_default().push(i);
        }
        map
    }

    fn resolve(&self, ix: &HashMap<String, Vec<usize>>, dep: &UvDepRef) -> Option<usize> {
        let list = ix.get(&normalize(&dep.name))?;
        list.iter().copied().find(|&i| {
            dep.version
                .as_ref()
                .is_none_or(|v| *v == self.pkgs[i].id.version)
        })
    }
}

pub struct UvPlan {
    lockfile: String,
    good: UvLock,
    bad: UvLock,
    has_local_roots: bool,
    unit_ix: HashMap<String, usize>,
    changes: Vec<Change>,
    closure: DepClosure,
    warnings: Vec<String>,
}

impl UvPlan {
    pub fn new(lockfile: &str, good_text: &str, bad_text: &str) -> Result<UvPlan, String> {
        let good = parse_uv_lock(good_text).map_err(|e| format!("good: {e}"))?;
        let bad = parse_uv_lock(bad_text).map_err(|e| format!("bad: {e}"))?;
        let mut warnings = Vec::new();
        if good.version != bad.version {
            warnings.push("the uv.lock format version differs between good and bad".to_string());
        }

        let has_local_roots =
            bad.pkgs.iter().any(|p| p.id.is_local()) || good.pkgs.iter().any(|p| p.id.is_local());

        let ids = |l: &UvLock| -> BTreeMap<String, (String, BTreeSet<UvNodeId>)> {
            let mut m: BTreeMap<String, (String, BTreeSet<UvNodeId>)> = BTreeMap::new();
            for p in l.pkgs.iter().filter(|p| !p.id.is_local()) {
                let norm = p.id.norm_name();
                let entry = m
                    .entry(norm)
                    .or_insert_with(|| (p.id.name.clone(), BTreeSet::new()));
                entry.1.insert(p.id.clone());
            }
            m
        };
        let (gm, bm) = (ids(&good), ids(&bad));
        let mut units = Vec::new();
        let mut changes = Vec::new();
        for (norm, (display_name, gset)) in &gm {
            if let Some((_, bset)) = bm.get(norm) {
                if gset != bset {
                    units.push(norm.clone());
                    changes.push(Change {
                        name: display_name.clone(),
                        from: gset.iter().map(UvNodeId::show).collect(),
                        to: bset.iter().map(UvNodeId::show).collect(),
                    });
                }
            } else if !has_local_roots {
                units.push(norm.clone());
                changes.push(Change {
                    name: display_name.clone(),
                    from: gset.iter().map(UvNodeId::show).collect(),
                    to: Vec::new(),
                });
            }
        }
        if !has_local_roots {
            for (norm, (display_name, bset)) in &bm {
                if !gm.contains_key(norm) {
                    units.push(norm.clone());
                    changes.push(Change {
                        name: display_name.clone(),
                        from: Vec::new(),
                        to: bset.iter().map(UvNodeId::show).collect(),
                    });
                }
            }
        }
        let unit_ix: HashMap<String, usize> = units
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, n)| (n, i))
            .collect();

        // Warn when local workspace package direct dependencies changed.
        let local_deps = |l: &UvLock| -> BTreeMap<String, BTreeSet<String>> {
            l.pkgs
                .iter()
                .filter(|p| p.id.is_local())
                .map(|p| {
                    (
                        p.id.name.clone(),
                        p.deps.iter().map(|d| normalize(&d.name)).collect(),
                    )
                })
                .collect()
        };
        let (gd, bd) = (local_deps(&good), local_deps(&bad));
        for (name, gs) in &gd {
            if let Some(bs) = bd.get(name) {
                if gs != bs {
                    let added: Vec<&String> = bs.difference(gs).collect();
                    let removed: Vec<&String> = gs.difference(bs).collect();
                    warnings.push(format!(
                        "dependencies of workspace package `{name}` differ between good and bad (added {added:?}, removed {removed:?}): pyproject.toml changed too, whichdep only bisects lockfile changes"
                    ));
                }
            }
        }

        let closure = Self::closure_for(&good, &bad, &units, &unit_ix);
        Ok(UvPlan {
            lockfile: lockfile.to_string(),
            good,
            bad,
            has_local_roots,
            unit_ix,
            changes,
            closure,
            warnings,
        })
    }

    fn closure_for(
        good: &UvLock,
        bad: &UvLock,
        units: &[String],
        unit_ix: &HashMap<String, usize>,
    ) -> DepClosure {
        let (gx, bx) = (good.by_norm_name(), bad.by_norm_name());
        let good_ids: HashSet<&UvNodeId> = good.pkgs.iter().map(|p| &p.id).collect();
        let good_names: HashSet<String> = good.pkgs.iter().map(|p| p.id.norm_name()).collect();
        let mut edges: BTreeSet<(usize, usize)> = BTreeSet::new();

        for (pi, p) in bad.pkgs.iter().enumerate() {
            let Some(&u) = unit_ix.get(&p.id.norm_name()) else {
                continue;
            };
            let mut seen: HashSet<usize> = HashSet::new();
            let mut stack = vec![pi];
            while let Some(i) = stack.pop() {
                for r in &bad.pkgs[i].deps {
                    let Some(d) = bad.resolve(&bx, r) else {
                        continue;
                    };
                    if !seen.insert(d) {
                        continue;
                    }
                    let did = &bad.pkgs[d].id;
                    if good_ids.contains(did) {
                        continue;
                    }
                    let dnorm = did.norm_name();
                    match unit_ix.get(&dnorm) {
                        Some(&v) => {
                            let stand_in = gx.get(&dnorm).is_some_and(|ix| {
                                ix.iter().any(|&g| {
                                    major_compatible(&good.pkgs[g].id.version, &did.version)
                                })
                            });
                            if v != u && !stand_in {
                                edges.insert((u, v));
                            }
                        }
                        None if !good_names.contains(&dnorm) => stack.push(d),
                        None => {}
                    }
                }
            }
        }

        for p in &good.pkgs {
            let Some(&dep_unit) = unit_ix.get(&p.id.norm_name()) else {
                continue;
            };
            for r in &p.deps {
                let Some(q) = good.resolve(&gx, r) else {
                    continue;
                };
                let qid = &good.pkgs[q].id;
                let qnorm = qid.norm_name();
                let Some(&q_unit) = unit_ix.get(&qnorm) else {
                    continue;
                };
                if q_unit == dep_unit {
                    continue;
                }
                let survives = bx.get(&qnorm).is_some_and(|ix| {
                    ix.iter()
                        .any(|&i| major_compatible(&qid.version, &bad.pkgs[i].id.version))
                });
                if !survives {
                    edges.insert((q_unit, dep_unit));
                }
            }
        }
        DepClosure::new(units.len(), edges, [])
    }

    fn selected_packages(&self, applied: &Set) -> Vec<&UvPkg> {
        let applied_name = |norm: &str| self.unit_ix.get(norm).is_some_and(|u| applied.contains(u));
        let good_names: HashSet<String> = self.good.pkgs.iter().map(|p| p.id.norm_name()).collect();

        let mut cand: Vec<(&UvPkg, bool)> = Vec::new();
        for p in &self.good.pkgs {
            if !p.id.is_local() && !applied_name(&p.id.norm_name()) {
                cand.push((p, false));
            }
        }
        for p in &self.bad.pkgs {
            let norm = p.id.norm_name();
            let take = p.id.is_local()
                || applied_name(&norm)
                || (self.has_local_roots && !good_names.contains(&norm));
            if take {
                cand.push((p, true));
            }
        }

        if !self.has_local_roots {
            cand.sort_by(|(a, _), (b, _)| a.id.cmp(&b.id));
            return cand.into_iter().map(|(p, _)| p).collect();
        }

        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, (p, _)) in cand.iter().enumerate() {
            by_name.entry(p.id.norm_name()).or_default().push(i);
        }

        let mut reach = vec![false; cand.len()];
        let mut queue: VecDeque<usize> = VecDeque::new();
        for (i, (p, _)) in cand.iter().enumerate() {
            if p.id.is_local() {
                reach[i] = true;
                queue.push_back(i);
            }
        }

        while let Some(i) = queue.pop_front() {
            for dep in &cand[i].0.deps {
                let norm = normalize(&dep.name);
                let Some(targets) = by_name.get(&norm) else {
                    continue;
                };
                let chosen: Vec<usize> = if let Some(ver) = &dep.version {
                    let exact: Vec<usize> = targets
                        .iter()
                        .copied()
                        .filter(|&t| cand[t].0.id.version == *ver)
                        .collect();
                    if exact.is_empty() {
                        targets.clone()
                    } else {
                        exact
                    }
                } else {
                    targets.clone()
                };
                for t in chosen {
                    if !reach[t] {
                        reach[t] = true;
                        queue.push_back(t);
                    }
                }
            }
        }

        let mut out: Vec<&UvPkg> = cand
            .into_iter()
            .enumerate()
            .filter_map(|(i, (p, _))| reach[i].then_some(p))
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }
}

impl Ecosystem for UvPlan {
    fn kind(&self) -> &'static str {
        "uv"
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
        let selected = self.selected_packages(applied);
        let mut out = self.good.preamble.trim_end().to_string();
        out.push('\n');
        for pkg in selected {
            out.push('\n');
            out.push_str(pkg.raw.trim_end());
            out.push('\n');
        }
        out
    }

    fn verify(&self, applied: &Set, lock_after_prepare: &str) -> Result<(), String> {
        let after = parse_uv_lock(lock_after_prepare)
            .map_err(|e| format!("uv left an unreadable uv.lock: {e}"))?;
        let have: BTreeSet<UvNodeId> = after.pkgs.into_iter().map(|p| p.id).collect();
        let want: BTreeSet<UvNodeId> = self
            .selected_packages(applied)
            .into_iter()
            .map(|p| p.id.clone())
            .collect();
        if have == want {
            return Ok(());
        }
        let fmt = |s: &BTreeSet<UvNodeId>| -> String {
            s.iter()
                .take(4)
                .map(|i| format!("{} {}", i.name, i.version))
                .collect::<Vec<_>>()
                .join(", ")
        };
        Err(format!(
            "uv changed the lockfile: it added [{}] and dropped [{}]",
            fmt(&have.difference(&want).cloned().collect()),
            fmt(&want.difference(&have).cloned().collect())
        ))
    }

    fn default_prepare(&self) -> &'static str {
        r#"set -e
uv sync --locked --quiet
uv pip check --python "$VIRTUAL_ENV/bin/python"
"#
    }

    fn env(&self, trial_dir: &Path, shared_dir: &Path) -> Vec<(String, String)> {
        let venv = trial_dir.join(".whichdep-venv");
        let uv_cache = shared_dir.join("uv-cache");
        let path = std::env::var("PATH").unwrap_or_default();
        vec![
            ("UV_PROJECT_ENVIRONMENT".into(), venv.display().to_string()),
            ("VIRTUAL_ENV".into(), venv.display().to_string()),
            ("UV_CACHE_DIR".into(), uv_cache.display().to_string()),
            (
                "PATH".into(),
                format!("{}:{}", venv.join("bin").display(), path),
            ),
        ]
    }

    fn warnings(&self) -> &[String] {
        &self.warnings
    }

    fn pin_hint(&self, unit: usize) -> Option<String> {
        let c = self.changes.get(unit)?;
        let changed_from: Vec<&String> = c.from.iter().filter(|v| !c.to.contains(v)).collect();
        let changed_to: Vec<&String> = c.to.iter().filter(|v| !c.from.contains(v)).collect();
        match (changed_from.as_slice(), changed_to.as_slice()) {
            ([from], [_to]) if !from.contains(' ') && !from.is_empty() => {
                Some(format!("uv lock --upgrade-package {}=={}", c.name, from))
            }
            _ => None,
        }
    }
}

/// Built-in adapter for Python `uv.lock` (v1).
pub struct UvAdapter;

impl Adapter for UvAdapter {
    fn id(&self) -> &'static str {
        "uv"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["uv-lock"]
    }

    fn description(&self) -> &'static str {
        "Python uv.lock (v1)"
    }

    fn lockfiles(&self) -> &'static [&'static str] {
        &["uv.lock"]
    }

    fn plan(
        &self,
        lockfile: &str,
        good_text: &str,
        bad_text: &str,
    ) -> Result<Box<dyn Ecosystem>, String> {
        Ok(Box::new(UvPlan::new(lockfile, good_text, bad_text)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dd::Closure;

    const GOOD_UV: &str = r#"version = 1
requires-python = ">=3.11"

[[package]]
name = "anyio"
version = "4.4.0"
source = { registry = "https://pypi.org/simple" }
wheels = [
    { url = "https://example.test/anyio-4.4.0.whl", hash = "sha256:aaa" },
]

[[package]]
name = "app"
version = "0.1.0"
source = { virtual = "." }
dependencies = [
    { name = "httpx" },
    { name = "pydantic" },
]

[[package]]
name = "httpx"
version = "0.27.0"
source = { registry = "https://pypi.org/simple" }
dependencies = [
    { name = "anyio" },
    { name = "legacy-helper" },
]

[[package]]
name = "legacy-helper"
version = "1.0.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "pydantic"
version = "2.8.0"
source = { registry = "https://pypi.org/simple" }
"#;

    const BAD_UV: &str = r#"version = 1
requires-python = ">=3.11"

[[package]]
name = "anyio"
version = "4.5.0"
source = { registry = "https://pypi.org/simple" }
wheels = [
    { url = "https://example.test/anyio-4.5.0.whl", hash = "sha256:bbb" },
]

[[package]]
name = "app"
version = "0.1.0"
source = { virtual = "." }
dependencies = [
    { name = "httpx" },
    { name = "pydantic" },
]

[[package]]
name = "fast-helper"
version = "0.2.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "httpx"
version = "0.27.2"
source = { registry = "https://pypi.org/simple" }
dependencies = [
    { name = "anyio" },
    { name = "fast-helper" },
]

[[package]]
name = "pydantic"
version = "2.9.2"
source = { registry = "https://pypi.org/simple" }
"#;

    fn unit(plan: &UvPlan, name: &str) -> usize {
        plan.changes()
            .iter()
            .position(|c| normalize(&c.name) == normalize(name))
            .unwrap()
    }

    #[test]
    fn identifies_changed_packages_and_prunes_transitive_swaps() {
        let plan = UvPlan::new("uv.lock", GOOD_UV, BAD_UV).unwrap();
        let names: Vec<&str> = plan.changes().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["anyio", "httpx", "pydantic"]);
        assert!(plan.warnings().is_empty());
        assert_eq!(
            plan.pin_hint(unit(&plan, "httpx")).as_deref(),
            Some("uv lock --upgrade-package httpx==0.27.0")
        );
    }

    #[test]
    fn rendering_swaps_transitive_dependencies_and_preserves_wheels() {
        let plan = UvPlan::new("uv.lock", GOOD_UV, BAD_UV).unwrap();
        let httpx = unit(&plan, "httpx");
        let rendered = plan.render(&[httpx].into_iter().collect());
        assert!(rendered.contains("name = \"httpx\"\nversion = \"0.27.2\""));
        assert!(rendered.contains("name = \"fast-helper\""));
        assert!(!rendered.contains("name = \"legacy-helper\""));
        assert!(rendered.contains("anyio-4.4.0.whl"));
        assert!(rendered.contains("name = \"pydantic\"\nversion = \"2.8.0\""));
        assert!(plan
            .verify(&[httpx].into_iter().collect(), &rendered)
            .is_ok());
        assert!(plan
            .verify(&[httpx].into_iter().collect(), GOOD_UV)
            .is_err());
    }

    #[test]
    fn major_bump_couples_dependent_and_dependency_in_closure() {
        let good = r#"version = 1

[[package]]
name = "app"
version = "0.1.0"
source = { virtual = "." }
dependencies = [{ name = "web" }]

[[package]]
name = "core"
version = "1.0.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "web"
version = "1.0.0"
source = { registry = "https://pypi.org/simple" }
dependencies = [{ name = "core" }]
"#;
        let bad = r#"version = 1

[[package]]
name = "app"
version = "0.1.0"
source = { virtual = "." }
dependencies = [{ name = "web" }]

[[package]]
name = "core"
version = "2.0.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "web"
version = "1.1.0"
source = { registry = "https://pypi.org/simple" }
dependencies = [{ name = "core" }]
"#;
        let plan = UvPlan::new("uv.lock", good, bad).unwrap();
        let (core, web) = (unit(&plan, "core"), unit(&plan, "web"));
        assert_eq!(
            plan.closure().up(&[web].into_iter().collect()),
            [core, web].into_iter().collect()
        );
        assert_eq!(
            plan.closure().up(&[core].into_iter().collect()),
            [core, web].into_iter().collect()
        );
    }

    #[test]
    fn rejects_unsupported_uv_lock_versions() {
        let bad = "version = 99\n";
        assert!(UvPlan::new("uv.lock", bad, bad)
            .err()
            .unwrap()
            .contains("not supported"));
    }
}
