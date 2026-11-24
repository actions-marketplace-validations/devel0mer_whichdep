//! `pnpm-lock.yaml` (lockfile versions 6 and 9) ecosystem adapter.
//!
//! Parses top-level YAML sections (`importers`, `packages`, `snapshots`, and
//! single-package v6 `dependencies`/`devDependencies`) while preserving each
//! package block verbatim so integrity hashes, peer suffixes, and resolutions
//! remain intact for `pnpm install --frozen-lockfile`.

use crate::adapter::Adapter;
use crate::cargo::cmp_version;
use crate::dd::{DepClosure, Set};
use crate::model::{Change, Ecosystem};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

#[derive(Clone, Debug)]
struct YamlBlock {
    key: String,
    raw: String,
}

#[derive(Clone, Debug)]
struct PnpmLock {
    lockfile_version: String,
    header: String,
    sections: Vec<(String, Vec<YamlBlock>, String)>,
    packages: Vec<PnpmPkgEntry>,
    snapshots: Vec<PnpmSnapEntry>,
}

type DepPair = (String, String);

#[derive(Clone, Debug)]
struct PnpmPkgEntry {
    raw_key: String,
    name: String,
    version: String,
    raw: String,
    deps: Vec<DepPair>,
}

#[derive(Clone, Debug)]
struct PnpmSnapEntry {
    raw_key: String,
    name: String,
    peers: Vec<DepPair>,
    raw: String,
    deps: Vec<DepPair>,
}

struct ParsedPnpmKey {
    name: String,
    version: String,
    peers: Vec<DepPair>,
}

fn unquote_yaml(s: &str) -> String {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')) {
        return inner.replace("''", "'");
    }
    if let Some(inner) = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        return inner.replace("\\\"", "\"").replace("\\\\", "\\");
    }
    s.to_string()
}

/// Parse a pnpm key like `'@scope/pkg@1.2.3(react@18.2.0)'` or `/lodash@4.17.21`
/// into `(name, base_version, peers)`.
fn parse_pnpm_key(raw_key: &str) -> Option<ParsedPnpmKey> {
    let key = unquote_yaml(raw_key);
    let key = key.strip_prefix('/').unwrap_or(&key);
    let (base, peer_part) = match key.find('(') {
        Some(idx) => (&key[..idx], Some(&key[idx..])),
        None => (key, None),
    };
    let at = base.rfind('@')?;
    if at == 0 {
        return None;
    }
    let name = base[..at].to_string();
    let version = base[at + 1..].to_string();
    let mut peers = Vec::new();
    if let Some(pp) = peer_part {
        let mut cur = pp;
        while let Some(open) = cur.find('(') {
            let after = &cur[open + 1..];
            let Some(close) = after.find(')') else {
                break;
            };
            let inside = &after[..close];
            if let Some(pat) = inside.rfind('@') {
                if pat > 0 {
                    peers.push((inside[..pat].to_string(), inside[pat + 1..].to_string()));
                }
            }
            cur = &after[close + 1..];
        }
    }
    Some(ParsedPnpmKey {
        name,
        version,
        peers,
    })
}

fn extract_yaml_dep_map(block: &str) -> Vec<(String, String)> {
    let mut deps = Vec::new();
    let mut in_deps = false;
    let mut deps_indent = 0usize;
    for line in block.lines().skip(1) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start_matches(' ').len();
        if in_deps {
            if indent <= deps_indent {
                in_deps = false;
            } else if let Some((k, v)) = trimmed.split_once(':') {
                let name = unquote_yaml(k);
                let val = unquote_yaml(v);
                if !name.is_empty() && !val.is_empty() {
                    deps.push((name, val));
                }
                continue;
            }
        }
        if matches!(
            trimmed,
            "dependencies:" | "optionalDependencies:" | "peerDependencies:"
        ) {
            in_deps = true;
            deps_indent = indent;
        }
    }
    deps
}

fn split_indented_blocks(section_body: &str) -> Vec<YamlBlock> {
    let mut blocks = Vec::new();
    let mut cur_key: Option<String> = None;
    let mut cur_raw = String::new();

    for line in section_body.lines() {
        let indent = line.len() - line.trim_start_matches(' ').len();
        let trimmed = line.trim();
        if indent == 2 && !trimmed.is_empty() && !trimmed.starts_with('#') && trimmed.ends_with(':')
        {
            if let Some(k) = cur_key.take() {
                blocks.push(YamlBlock {
                    key: k,
                    raw: std::mem::take(&mut cur_raw),
                });
            }
            let key = trimmed.trim_end_matches(':').to_string();
            cur_key = Some(key);
            cur_raw.push_str(line);
            cur_raw.push('\n');
        } else if cur_key.is_some() {
            cur_raw.push_str(line);
            cur_raw.push('\n');
        }
    }
    if let Some(k) = cur_key {
        blocks.push(YamlBlock {
            key: k,
            raw: cur_raw,
        });
    }
    blocks
}

fn parse_pnpm_lock(text: &str) -> Result<PnpmLock, String> {
    let mut lockfile_version: Option<String> = None;
    let mut header = String::new();
    let mut raw_sections: Vec<(String, String)> = Vec::new();
    let mut cur_sec: Option<(String, String)> = None;

    for line in text.lines() {
        let is_top = !line.is_empty()
            && !line.starts_with(' ')
            && !line.starts_with('\t')
            && !line.starts_with('#');
        if is_top {
            if let Some(rest) = line.strip_prefix("lockfileVersion:") {
                lockfile_version = Some(unquote_yaml(rest));
            }
            if line.ends_with(':') {
                if let Some(prev) = cur_sec.take() {
                    raw_sections.push(prev);
                }
                let name = line.trim_end_matches(':').trim().to_string();
                cur_sec = Some((name, String::new()));
                continue;
            }
        }
        if let Some((_, body)) = cur_sec.as_mut() {
            body.push_str(line);
            body.push('\n');
        } else {
            header.push_str(line);
            header.push('\n');
        }
    }
    if let Some(last) = cur_sec {
        raw_sections.push(last);
    }

    let ver = lockfile_version
        .ok_or_else(|| "missing `lockfileVersion`: not a valid pnpm-lock.yaml".to_string())?;
    let major = ver
        .split('.')
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    if !matches!(major, 6 | 9) {
        return Err(format!(
            "pnpm-lock.yaml lockfileVersion `{ver}` is not supported (only v6 and v9)"
        ));
    }

    let mut sections = Vec::new();
    let mut packages = Vec::new();
    let mut snapshots = Vec::new();

    for (sec_name, body) in raw_sections {
        if sec_name == "packages" {
            let blocks = split_indented_blocks(&body);
            for b in &blocks {
                if let Some(parsed) = parse_pnpm_key(&b.key) {
                    packages.push(PnpmPkgEntry {
                        raw_key: b.key.clone(),
                        name: parsed.name,
                        version: parsed.version,
                        raw: b.raw.clone(),
                        deps: extract_yaml_dep_map(&b.raw),
                    });
                }
            }
            sections.push((sec_name, blocks, body));
        } else if sec_name == "snapshots" {
            let blocks = split_indented_blocks(&body);
            for b in &blocks {
                if let Some(parsed) = parse_pnpm_key(&b.key) {
                    snapshots.push(PnpmSnapEntry {
                        raw_key: b.key.clone(),
                        name: parsed.name,
                        peers: parsed.peers,
                        raw: b.raw.clone(),
                        deps: extract_yaml_dep_map(&b.raw),
                    });
                }
            }
            sections.push((sec_name, blocks, body));
        } else {
            sections.push((sec_name, Vec::new(), body));
        }
    }

    Ok(PnpmLock {
        lockfile_version: ver,
        header,
        sections,
        packages,
        snapshots,
    })
}

fn major_compatible(a: &str, b: &str) -> bool {
    let nums = |v: &str| -> Option<(u64, u64)> {
        let core = v.split(['-', '+']).next()?;
        let mut it = core.split('.');
        Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
    };
    match (nums(a), nums(b)) {
        (Some((0, m1)), Some((0, m2))) => m1 == m2,
        (Some((maj1, _)), Some((maj2, _))) => maj1 == maj2,
        _ => a == b,
    }
}

/// Rewrite version references for changed packages inside `importers:` or
/// top-level `dependencies:` / `devDependencies:` blocks.
fn rewrite_importer_versions(
    body: &str,
    replacements: &HashMap<String, (String, String)>,
) -> String {
    let mut out = String::new();
    let mut current_pkg: Option<String> = None;
    let mut pkg_indent = 0usize;

    for line in body.lines() {
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start_matches(' ').len();

        if let Some(pkg) = current_pkg.as_deref() {
            if indent <= pkg_indent && !trimmed.is_empty() {
                current_pkg = None;
            } else if let Some(rest) = trimmed.strip_prefix("version:") {
                if let Some((from_ver, to_ver)) = replacements.get(pkg) {
                    let cur_val = unquote_yaml(rest);
                    let base_val = cur_val.split('(').next().unwrap_or(&cur_val);
                    if base_val == from_ver {
                        let new_val = cur_val.replacen(from_ver, to_ver, 1);
                        let pad = &line[..indent];
                        let quote = if rest.trim().starts_with('\'') {
                            "'"
                        } else {
                            ""
                        };
                        out.push_str(&format!("{pad}version: {quote}{new_val}{quote}\n"));
                        continue;
                    }
                }
            }
        }

        if trimmed.ends_with(':')
            && !matches!(
                trimmed,
                "dependencies:" | "devDependencies:" | "optionalDependencies:" | ".:"
            )
            && indent >= 2
        {
            let name = unquote_yaml(trimmed.trim_end_matches(':'));
            if replacements.contains_key(&name) {
                current_pkg = Some(name);
                pkg_indent = indent;
            }
        }

        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Rewrite inner dependency versions inside a snapshot/package YAML block when
/// a referenced single-version package changed.
fn rewrite_dep_lines(raw: &str, replacements: &HashMap<String, (String, String)>) -> String {
    if replacements.is_empty() {
        return raw.to_string();
    }
    let mut out = String::new();
    let mut in_deps = false;
    let mut deps_indent = 0usize;
    for (idx, line) in raw.lines().enumerate() {
        if idx == 0 {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start_matches(' ').len();
        if in_deps {
            if indent <= deps_indent && !trimmed.is_empty() {
                in_deps = false;
            } else if let Some((k, v)) = trimmed.split_once(':') {
                let name = unquote_yaml(k);
                let val = unquote_yaml(v);
                if let Some((from_ver, to_ver)) = replacements.get(&name) {
                    let base_val = val.split('(').next().unwrap_or(&val);
                    if base_val == from_ver {
                        let new_val = val.replacen(from_ver, to_ver, 1);
                        let pad = &line[..indent];
                        let quote = if v.trim().starts_with('\'') { "'" } else { "" };
                        out.push_str(&format!("{pad}{k}: {quote}{new_val}{quote}\n"));
                        continue;
                    }
                }
            }
        }
        if matches!(trimmed, "dependencies:" | "optionalDependencies:") {
            in_deps = true;
            deps_indent = indent;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PnpmUnitKind {
    Updated,
    Added,
    Removed,
}

#[derive(Clone, Debug)]
struct PnpmUnit {
    name: String,
    kind: PnpmUnitKind,
    from: Vec<String>,
    to: Vec<String>,
}

pub struct PnpmPlan {
    lockfile: String,
    good: PnpmLock,
    bad: PnpmLock,
    units: Vec<PnpmUnit>,
    changes: Vec<Change>,
    closure: DepClosure,
    warnings: Vec<String>,
}

impl PnpmPlan {
    pub fn new(lockfile: &str, good_text: &str, bad_text: &str) -> Result<PnpmPlan, String> {
        let good = parse_pnpm_lock(good_text).map_err(|e| format!("good: {e}"))?;
        let bad = parse_pnpm_lock(bad_text).map_err(|e| format!("bad: {e}"))?;

        let mut warnings = Vec::new();
        if good.lockfile_version != bad.lockfile_version {
            warnings.push(format!(
                "pnpm lockfileVersion differs between good ({}) and bad ({})",
                good.lockfile_version, bad.lockfile_version
            ));
        }

        let versions_by_name = |l: &PnpmLock| -> BTreeMap<String, BTreeSet<String>> {
            let mut m: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            for p in &l.packages {
                m.entry(p.name.clone())
                    .or_default()
                    .insert(p.version.clone());
            }
            m
        };
        let (gm, bm) = (versions_by_name(&good), versions_by_name(&bad));

        let mut units = Vec::new();
        let mut changes = Vec::new();

        for (name, gvers) in &gm {
            let mut gv: Vec<String> = gvers.iter().cloned().collect();
            gv.sort_by(|a, b| cmp_version(a, b));
            match bm.get(name) {
                Some(bvers) if bvers == gvers => {}
                Some(bvers) => {
                    let mut bv: Vec<String> = bvers.iter().cloned().collect();
                    bv.sort_by(|a, b| cmp_version(a, b));
                    units.push(PnpmUnit {
                        name: name.clone(),
                        kind: PnpmUnitKind::Updated,
                        from: gv.clone(),
                        to: bv.clone(),
                    });
                    changes.push(Change {
                        name: name.clone(),
                        from: gv,
                        to: bv,
                    });
                }
                None => {
                    units.push(PnpmUnit {
                        name: name.clone(),
                        kind: PnpmUnitKind::Removed,
                        from: gv.clone(),
                        to: Vec::new(),
                    });
                    changes.push(Change {
                        name: name.clone(),
                        from: gv,
                        to: Vec::new(),
                    });
                }
            }
        }
        for (name, bvers) in &bm {
            if !gm.contains_key(name) {
                let mut bv: Vec<String> = bvers.iter().cloned().collect();
                bv.sort_by(|a, b| cmp_version(a, b));
                units.push(PnpmUnit {
                    name: name.clone(),
                    kind: PnpmUnitKind::Added,
                    from: Vec::new(),
                    to: bv.clone(),
                });
                changes.push(Change {
                    name: name.clone(),
                    from: Vec::new(),
                    to: bv,
                });
            }
        }

        let unit_ix: HashMap<&str, usize> = units
            .iter()
            .enumerate()
            .map(|(i, u)| (u.name.as_str(), i))
            .collect();

        let mut edges: BTreeSet<(usize, usize)> = BTreeSet::new();

        type BadEdgeSource<'a> = (&'a str, &'a [DepPair], &'a [DepPair]);
        // Inspect bad snapshots (v9) and packages (v6) for dependency/peer edges.
        let bad_edges: Vec<BadEdgeSource<'_>> = if !bad.snapshots.is_empty() {
            bad.snapshots
                .iter()
                .map(|s| (s.name.as_str(), s.deps.as_slice(), s.peers.as_slice()))
                .collect()
        } else {
            bad.packages
                .iter()
                .map(|p| (p.name.as_str(), p.deps.as_slice(), &[][..]))
                .collect()
        };

        for (pkg_name, deps, peers) in bad_edges {
            let Some(&ui) = unit_ix.get(pkg_name) else {
                continue;
            };
            for (dep_name, dep_ver) in deps.iter().chain(peers.iter()) {
                let Some(&vi) = unit_ix.get(dep_name.as_str()) else {
                    continue;
                };
                if vi == ui {
                    continue;
                }
                let dep_base = dep_ver.split('(').next().unwrap_or(dep_ver);
                match units[vi].kind {
                    PnpmUnitKind::Added => {
                        edges.insert((ui, vi));
                    }
                    PnpmUnitKind::Updated => {
                        let stand_in = units[vi]
                            .from
                            .iter()
                            .any(|gv| major_compatible(gv, dep_base));
                        if !stand_in {
                            edges.insert((ui, vi));
                        }
                    }
                    PnpmUnitKind::Removed => {}
                }
            }
        }

        // Reverse check on good snapshots/packages.
        let good_edges: Vec<(&str, &[(String, String)])> = if !good.snapshots.is_empty() {
            good.snapshots
                .iter()
                .map(|s| (s.name.as_str(), s.deps.as_slice()))
                .collect()
        } else {
            good.packages
                .iter()
                .map(|p| (p.name.as_str(), p.deps.as_slice()))
                .collect()
        };
        for (pkg_name, deps) in good_edges {
            let Some(&ui) = unit_ix.get(pkg_name) else {
                continue;
            };
            for (dep_name, dep_ver) in deps {
                let Some(&vi) = unit_ix.get(dep_name.as_str()) else {
                    continue;
                };
                if vi == ui {
                    continue;
                }
                let dep_base = dep_ver.split('(').next().unwrap_or(dep_ver);
                match units[vi].kind {
                    PnpmUnitKind::Removed => {
                        edges.insert((vi, ui));
                    }
                    PnpmUnitKind::Updated => {
                        let survives = units[vi].to.iter().any(|bv| major_compatible(dep_base, bv));
                        if !survives {
                            edges.insert((vi, ui));
                        }
                    }
                    PnpmUnitKind::Added => {}
                }
            }
        }

        let closure = DepClosure::new(units.len(), edges, []);
        Ok(PnpmPlan {
            lockfile: lockfile.to_string(),
            good,
            bad,
            units,
            changes,
            closure,
            warnings,
        })
    }

    fn applied_replacements(&self, applied: &Set) -> HashMap<String, (String, String)> {
        let mut map = HashMap::new();
        for (ui, u) in self.units.iter().enumerate() {
            if applied.contains(&ui) && u.kind == PnpmUnitKind::Updated {
                if let ([from], [to]) = (u.from.as_slice(), u.to.as_slice()) {
                    map.insert(u.name.clone(), (from.clone(), to.clone()));
                }
            }
        }
        map
    }

    fn expected_packages(&self, applied: &Set) -> BTreeSet<(String, String)> {
        let unit_by_name: HashMap<&str, usize> = self
            .units
            .iter()
            .enumerate()
            .map(|(i, u)| (u.name.as_str(), i))
            .collect();
        let mut out = BTreeSet::new();
        for p in &self.good.packages {
            match unit_by_name.get(p.name.as_str()) {
                Some(&ui) if applied.contains(&ui) => {}
                _ => {
                    out.insert((p.name.clone(), p.version.clone()));
                }
            }
        }
        for p in &self.bad.packages {
            if let Some(&ui) = unit_by_name.get(p.name.as_str()) {
                if applied.contains(&ui) {
                    out.insert((p.name.clone(), p.version.clone()));
                }
            }
        }
        out
    }
}

impl Ecosystem for PnpmPlan {
    fn kind(&self) -> &'static str {
        "pnpm"
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
        let unit_by_name: HashMap<&str, usize> = self
            .units
            .iter()
            .enumerate()
            .map(|(i, u)| (u.name.as_str(), i))
            .collect();
        let replacements = self.applied_replacements(applied);

        let mut out = String::new();
        out.push_str(self.good.header.trim_end());
        out.push('\n');

        let bad_pkg_by_name: HashMap<&str, Vec<&PnpmPkgEntry>> = {
            let mut m: HashMap<&str, Vec<&PnpmPkgEntry>> = HashMap::new();
            for p in &self.bad.packages {
                m.entry(p.name.as_str()).or_default().push(p);
            }
            m
        };
        let bad_snap_by_name: HashMap<&str, Vec<&PnpmSnapEntry>> = {
            let mut m: HashMap<&str, Vec<&PnpmSnapEntry>> = HashMap::new();
            for s in &self.bad.snapshots {
                m.entry(s.name.as_str()).or_default().push(s);
            }
            m
        };

        for (sec_name, _blocks, body) in &self.good.sections {
            out.push('\n');
            out.push_str(sec_name);
            out.push_str(":\n");

            if sec_name == "packages" {
                let mut entries: Vec<(String, String)> = Vec::new();
                let mut emitted_applied: BTreeSet<&str> = BTreeSet::new();
                for p in &self.good.packages {
                    match unit_by_name.get(p.name.as_str()) {
                        Some(&ui) if applied.contains(&ui) => {
                            if emitted_applied.insert(p.name.as_str()) {
                                if let Some(bp_list) = bad_pkg_by_name.get(p.name.as_str()) {
                                    for bp in bp_list {
                                        entries.push((
                                            bp.raw_key.clone(),
                                            rewrite_dep_lines(&bp.raw, &replacements),
                                        ));
                                    }
                                }
                            }
                        }
                        _ => {
                            entries.push((
                                p.raw_key.clone(),
                                rewrite_dep_lines(&p.raw, &replacements),
                            ));
                        }
                    }
                }
                for (ui, u) in self.units.iter().enumerate() {
                    if u.kind == PnpmUnitKind::Added && applied.contains(&ui) {
                        if let Some(bp_list) = bad_pkg_by_name.get(u.name.as_str()) {
                            for bp in bp_list {
                                entries.push((
                                    bp.raw_key.clone(),
                                    rewrite_dep_lines(&bp.raw, &replacements),
                                ));
                            }
                        }
                    }
                }
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                for (_, raw) in entries {
                    out.push_str(&raw);
                }
            } else if sec_name == "snapshots" {
                let mut entries: Vec<(String, String)> = Vec::new();
                let mut emitted_applied: BTreeSet<&str> = BTreeSet::new();
                for s in &self.good.snapshots {
                    match unit_by_name.get(s.name.as_str()) {
                        Some(&ui) if applied.contains(&ui) => {
                            if emitted_applied.insert(s.name.as_str()) {
                                if let Some(bs_list) = bad_snap_by_name.get(s.name.as_str()) {
                                    for bs in bs_list {
                                        entries.push((
                                            bs.raw_key.clone(),
                                            rewrite_dep_lines(&bs.raw, &replacements),
                                        ));
                                    }
                                }
                            }
                        }
                        _ => {
                            entries.push((
                                s.raw_key.clone(),
                                rewrite_dep_lines(&s.raw, &replacements),
                            ));
                        }
                    }
                }
                for (ui, u) in self.units.iter().enumerate() {
                    if u.kind == PnpmUnitKind::Added && applied.contains(&ui) {
                        if let Some(bs_list) = bad_snap_by_name.get(u.name.as_str()) {
                            for bs in bs_list {
                                entries.push((
                                    bs.raw_key.clone(),
                                    rewrite_dep_lines(&bs.raw, &replacements),
                                ));
                            }
                        }
                    }
                }
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                for (_, raw) in entries {
                    out.push_str(&raw);
                }
            } else if matches!(
                sec_name.as_str(),
                "importers" | "dependencies" | "devDependencies" | "optionalDependencies"
            ) {
                out.push_str(&rewrite_importer_versions(body, &replacements));
            } else {
                out.push_str(body);
            }
        }
        out
    }

    fn verify(&self, applied: &Set, lock_after_prepare: &str) -> Result<(), String> {
        let after = parse_pnpm_lock(lock_after_prepare)
            .map_err(|e| format!("pnpm left an unreadable pnpm-lock.yaml: {e}"))?;
        let have: BTreeSet<(String, String)> = after
            .packages
            .into_iter()
            .map(|p| (p.name, p.version))
            .collect();
        let want = self.expected_packages(applied);
        if have == want {
            return Ok(());
        }
        let fmt = |s: &BTreeSet<(String, String)>| -> String {
            s.iter()
                .take(4)
                .map(|(n, v)| format!("{n}@{v}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        Err(format!(
            "pnpm changed pnpm-lock.yaml: added [{}] and dropped [{}]",
            fmt(&have.difference(&want).cloned().collect()),
            fmt(&want.difference(&have).cloned().collect())
        ))
    }

    fn default_prepare(&self) -> &'static str {
        r#"set -e
pnpm install --frozen-lockfile --store-dir "$WHICHDEP_PNPM_STORE"
"#
    }

    fn env(&self, trial_dir: &Path, shared_dir: &Path) -> Vec<(String, String)> {
        let bin = trial_dir.join("node_modules").join(".bin");
        let store = shared_dir.join("pnpm-store");
        let path = std::env::var("PATH").unwrap_or_default();
        vec![
            ("WHICHDEP_PNPM_STORE".into(), store.display().to_string()),
            ("PATH".into(), format!("{}:{}", bin.display(), path)),
        ]
    }

    fn warnings(&self) -> &[String] {
        &self.warnings
    }

    fn pin_hint(&self, unit: usize) -> Option<String> {
        let u = self.units.get(unit)?;
        if u.kind != PnpmUnitKind::Updated {
            return None;
        }
        let changed_from: Vec<&String> = u.from.iter().filter(|v| !u.to.contains(v)).collect();
        match changed_from.as_slice() {
            [from] => Some(format!("pnpm add --save-exact {}@{from}", u.name)),
            _ => None,
        }
    }
}

/// Built-in adapter for Node.js `pnpm-lock.yaml` (v6 and v9).
pub struct PnpmAdapter;

impl Adapter for PnpmAdapter {
    fn id(&self) -> &'static str {
        "pnpm"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["pnpm-lock"]
    }

    fn description(&self) -> &'static str {
        "Node.js pnpm-lock.yaml (v6, v9)"
    }

    fn lockfiles(&self) -> &'static [&'static str] {
        &["pnpm-lock.yaml"]
    }

    fn plan(
        &self,
        lockfile: &str,
        good_text: &str,
        bad_text: &str,
    ) -> Result<Box<dyn Ecosystem>, String> {
        Ok(Box::new(PnpmPlan::new(lockfile, good_text, bad_text)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dd::Closure;

    const GOOD_PNPM_V9: &str = r#"lockfileVersion: '9.0'

settings:
  autoInstallPeers: true

importers:
  .:
    dependencies:
      '@scope/ui':
        specifier: ^1.0.0
        version: 1.0.0
      zod:
        specifier: ^3.22.0
        version: 3.22.4

packages:
  '@scope/ui@1.0.0':
    resolution: {integrity: sha512-aaa}
  zod@3.22.4:
    resolution: {integrity: sha512-bbb}

snapshots:
  '@scope/ui@1.0.0':
    dependencies:
      zod: 3.22.4
  zod@3.22.4: {}
"#;

    const BAD_PNPM_V9: &str = r#"lockfileVersion: '9.0'

settings:
  autoInstallPeers: true

importers:
  .:
    dependencies:
      '@scope/ui':
        specifier: ^1.0.0
        version: 1.1.0
      zod:
        specifier: ^3.22.0
        version: 3.23.8

packages:
  '@scope/ui@1.1.0':
    resolution: {integrity: sha512-ccc}
  helper@1.0.0:
    resolution: {integrity: sha512-ddd}
  zod@3.23.8:
    resolution: {integrity: sha512-eee}

snapshots:
  '@scope/ui@1.1.0':
    dependencies:
      helper: 1.0.0
      zod: 3.23.8
  helper@1.0.0: {}
  zod@3.23.8: {}
"#;

    fn unit(p: &PnpmPlan, name: &str) -> usize {
        p.changes().iter().position(|c| c.name == name).unwrap()
    }

    #[test]
    fn parses_pnpm_v9_diffs_and_closures() {
        let p = PnpmPlan::new("pnpm-lock.yaml", GOOD_PNPM_V9, BAD_PNPM_V9).unwrap();
        let names: Vec<&str> = p.changes().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["@scope/ui", "zod", "helper"]);
        let ui = unit(&p, "@scope/ui");
        let helper = unit(&p, "helper");
        assert_eq!(
            p.closure().up(&[ui].into_iter().collect()),
            [ui, helper].into_iter().collect()
        );
        assert_eq!(
            p.pin_hint(unit(&p, "zod")).as_deref(),
            Some("pnpm add --save-exact zod@3.22.4")
        );
    }

    #[test]
    fn renders_v9_subset_and_rewrites_importer_and_snapshot_refs() {
        let p = PnpmPlan::new("pnpm-lock.yaml", GOOD_PNPM_V9, BAD_PNPM_V9).unwrap();
        let zod = unit(&p, "zod");
        let set: Set = [zod].into_iter().collect();
        let rendered = p.render(&set);
        assert!(rendered.contains("version: 3.23.8"));
        assert!(rendered.contains("version: 1.0.0"));
        assert!(rendered.contains("zod@3.23.8:"));
        assert!(rendered.contains("'@scope/ui@1.0.0':"));
        assert!(!rendered.contains("helper@1.0.0:"));
        // Snapshot of @scope/ui@1.0.0 had its zod ref updated to 3.23.8.
        assert!(rendered.contains("      zod: 3.23.8"));
        assert!(p.verify(&set, &rendered).is_ok());
        assert!(p.verify(&set, GOOD_PNPM_V9).is_err());
    }

    #[test]
    fn supports_pnpm_v6_lockfiles() {
        let good = r#"lockfileVersion: '6.0'

dependencies:
  lodash:
    specifier: ^4.17.20
    version: 4.17.20

packages:
  /lodash@4.17.20:
    resolution: {integrity: sha512-old}
"#;
        let bad = r#"lockfileVersion: '6.0'

dependencies:
  lodash:
    specifier: ^4.17.20
    version: 4.17.21

packages:
  /lodash@4.17.21:
    resolution: {integrity: sha512-new}
"#;
        let p = PnpmPlan::new("pnpm-lock.yaml", good, bad).unwrap();
        assert_eq!(p.changes().len(), 1);
        assert_eq!(p.changes()[0].describe(), "lodash 4.17.20 -> 4.17.21");
        let rendered = p.render(&[0].into_iter().collect());
        assert!(rendered.contains("/lodash@4.17.21:"));
        assert!(rendered.contains("version: 4.17.21"));
        assert!(p.verify(&[0].into_iter().collect(), &rendered).is_ok());
    }

    #[test]
    fn rejects_unsupported_pnpm_versions() {
        let old = "lockfileVersion: 5.4\n";
        assert!(PnpmPlan::new("pnpm-lock.yaml", old, old)
            .err()
            .unwrap()
            .contains("not supported"));
    }
}
