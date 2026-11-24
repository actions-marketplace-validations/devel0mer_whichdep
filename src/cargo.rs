//! `Cargo.lock` (lockfile versions 3 and 4).
//!
//! The units are the crate *names* whose set of locked versions differs
//! between the good and the bad lockfile. Crates that only exist on one side
//! are not units: they come and go with the crates that need them, which is
//! worked out by reachability from the workspace members, the same way Cargo
//! prunes its lockfile. Whatever cannot be modelled is caught afterwards:
//! `cargo fetch` normalises the lockfile and [`Ecosystem::verify`] rejects the
//! configuration as unresolved if Cargo had to change a version.

use crate::dd::{DepClosure, Set};
use crate::model::{Change, Ecosystem};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NodeId {
    pub name: String,
    pub version: String,
    pub source: Option<String>,
}

fn parse_version(v: &str) -> Option<([u64; 3], Option<&str>)> {
    let v = v.split('+').next()?;
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let mut it = core.split('.');
    let nums = [
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    ];
    if it.next().is_some() {
        return None;
    }
    Some((nums, pre))
}

fn cmp_pre(a: Option<&str>, b: Option<&str>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => {
            let (mut xi, mut yi) = (x.split('.'), y.split('.'));
            loop {
                match (xi.next(), yi.next()) {
                    (None, None) => return Ordering::Equal,
                    (None, Some(_)) => return Ordering::Less,
                    (Some(_), None) => return Ordering::Greater,
                    (Some(p), Some(q)) => {
                        let o = match (p.parse::<u64>(), q.parse::<u64>()) {
                            (Ok(m), Ok(n)) => m.cmp(&n),
                            (Ok(_), Err(_)) => Ordering::Less,
                            (Err(_), Ok(_)) => Ordering::Greater,
                            (Err(_), Err(_)) => p.cmp(q),
                        };
                        if o != Ordering::Equal {
                            return o;
                        }
                    }
                }
            }
        }
    }
}

/// Semver precedence, with the plain string order as the last tie-break so
/// that it agrees with `Eq`.
pub fn cmp_version(a: &str, b: &str) -> Ordering {
    match (parse_version(a), parse_version(b)) {
        (Some((x, xp)), Some((y, yp))) => x
            .cmp(&y)
            .then_with(|| cmp_pre(xp, yp))
            .then_with(|| a.cmp(b)),
        _ => a.cmp(b),
    }
}

impl Ord for NodeId {
    /// The order Cargo writes packages in: name, version, source.
    fn cmp(&self, other: &Self) -> Ordering {
        self.name
            .cmp(&other.name)
            .then_with(|| cmp_version(&self.version, &other.version))
            .then_with(|| self.source.cmp(&other.source))
    }
}

impl PartialOrd for NodeId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl NodeId {
    fn is_local(&self) -> bool {
        self.source.is_none()
    }

    fn show(&self) -> String {
        match &self.source {
            Some(s) if s.starts_with("git+") => {
                let rev = s.rsplit('#').next().unwrap_or("");
                format!("{} (git {})", self.version, &rev[..rev.len().min(8)])
            }
            _ => self.version.clone(),
        }
    }
}

#[derive(Clone, Debug)]
struct DepRef {
    name: String,
    version: Option<String>,
    source: Option<String>,
}

#[derive(Clone, Debug)]
struct Pkg {
    id: NodeId,
    checksum: Option<String>,
    deps: Vec<DepRef>,
}

#[derive(Debug)]
struct Lock {
    preamble: String,
    lock_version: Option<u32>,
    pkgs: Vec<Pkg>,
    tail: String,
}

fn unquote(s: &str) -> Option<String> {
    let s = s.trim().strip_prefix('"')?.strip_suffix('"')?;
    Some(s.replace("\\\"", "\"").replace("\\\\", "\\"))
}

fn quoted_items(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut inside = false;
    let mut esc = false;
    for c in s.chars() {
        if inside {
            if esc {
                cur.push(c);
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                out.push(std::mem::take(&mut cur));
                inside = false;
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            inside = true;
        }
    }
    out
}

fn parse_dep(s: &str) -> DepRef {
    let mut it = s.splitn(2, ' ');
    let name = it.next().unwrap_or("").to_string();
    let rest = it.next().unwrap_or("").trim();
    let strip = |x: &str| {
        x.trim()
            .trim_start_matches('(')
            .trim_end_matches(')')
            .to_string()
    };
    let (version, source) = if rest.is_empty() {
        (None, None)
    } else if rest.starts_with('(') {
        (None, Some(strip(rest)))
    } else {
        match rest.split_once(' ') {
            Some((v, src)) => (Some(v.to_string()), Some(strip(src))),
            None => (Some(rest.to_string()), None),
        }
    };
    DepRef {
        name,
        version,
        source,
    }
}

fn parse_pkg(body: &[&str]) -> Result<Pkg, String> {
    let (mut name, mut version, mut source, mut checksum) = (None, None, None, None);
    let mut deps = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let line = body[i].trim();
        i += 1;
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "name" => name = unquote(v),
            "version" => version = unquote(v),
            "source" => source = unquote(v),
            "checksum" => checksum = unquote(v),
            "dependencies" => {
                let mut buf = v.to_string();
                while !buf.contains(']') && i < body.len() {
                    buf.push(' ');
                    buf.push_str(body[i].trim());
                    i += 1;
                }
                deps = quoted_items(&buf).iter().map(|d| parse_dep(d)).collect();
            }
            _ => {}
        }
    }
    match (name, version) {
        (Some(name), Some(version)) => Ok(Pkg {
            id: NodeId {
                name,
                version,
                source,
            },
            checksum,
            deps,
        }),
        _ => Err("a [[package]] table without name or version".into()),
    }
}

fn parse_lock(text: &str) -> Result<Lock, String> {
    let mut sections: Vec<(String, Vec<&str>)> = vec![(String::new(), Vec::new())];
    for line in text.lines() {
        if line.starts_with('[') {
            sections.push((line.trim().to_string(), Vec::new()));
        } else {
            sections.last_mut().unwrap().1.push(line);
        }
    }
    let mut preamble = String::new();
    let mut lock_version = None;
    for l in &sections[0].1 {
        preamble.push_str(l);
        preamble.push('\n');
        if let Some(v) = l.trim().strip_prefix("version") {
            lock_version = v.trim().trim_start_matches('=').trim().parse::<u32>().ok();
        }
    }
    if let Some(v) = lock_version {
        if !(3..=4).contains(&v) {
            return Err(format!(
                "Cargo.lock version {v} is not supported (only 3 and 4)"
            ));
        }
    } else {
        return Err(
            "no `version = N` line: Cargo.lock formats older than version 3 are not supported"
                .into(),
        );
    }
    let mut pkgs = Vec::new();
    let mut tail = String::new();
    for (head, body) in &sections[1..] {
        if head == "[[package]]" {
            pkgs.push(parse_pkg(body)?);
        } else {
            tail.push_str(head);
            tail.push('\n');
            for l in body {
                tail.push_str(l);
                tail.push('\n');
            }
        }
    }
    Ok(Lock {
        preamble,
        lock_version,
        pkgs,
        tail,
    })
}

fn semver(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.split(['-', '+']).next()?;
    let mut it = core.split('.');
    Some((
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    ))
}

/// Cargo's caret compatibility: `1.2.3` and `1.9.0` are, `0.2.1` and `0.3.0`
/// are not.
fn compatible(a: &str, b: &str) -> bool {
    match (semver(a), semver(b)) {
        (Some(x), Some(y)) => {
            if x.0 > 0 || y.0 > 0 {
                x.0 == y.0
            } else if x.1 > 0 || y.1 > 0 {
                x.1 == y.1
            } else {
                x == y
            }
        }
        _ => a == b,
    }
}

impl Lock {
    fn by_name(&self) -> HashMap<&str, Vec<usize>> {
        let mut m: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, p) in self.pkgs.iter().enumerate() {
            m.entry(p.id.name.as_str()).or_default().push(i);
        }
        m
    }

    /// The package a dependency reference points at, inside this lockfile.
    fn resolve(&self, ix: &HashMap<&str, Vec<usize>>, r: &DepRef) -> Option<usize> {
        ix.get(r.name.as_str())?.iter().copied().find(|&i| {
            let id = &self.pkgs[i].id;
            r.version.as_ref().is_none_or(|v| *v == id.version)
                && r.source
                    .as_ref()
                    .is_none_or(|s| Some(s) == id.source.as_ref())
        })
    }
}

/// One concrete lockfile: which packages, and who depends on whom.
struct Config {
    nodes: Vec<(NodeId, Option<String>)>,
    edges: Vec<Vec<usize>>,
}

pub struct CargoPlan {
    lockfile: String,
    good: Lock,
    bad: Lock,
    unit_ix: HashMap<String, usize>,
    changes: Vec<Change>,
    closure: DepClosure,
    warnings: Vec<String>,
}

impl CargoPlan {
    pub fn new(lockfile: &str, good_text: &str, bad_text: &str) -> Result<CargoPlan, String> {
        let good = parse_lock(good_text).map_err(|e| format!("good: {e}"))?;
        let bad = parse_lock(bad_text).map_err(|e| format!("bad: {e}"))?;
        let mut warnings = Vec::new();
        if good.lock_version != bad.lock_version {
            warnings.push("the lockfile format version differs between good and bad".to_string());
        }

        let ids = |l: &Lock| -> BTreeMap<String, BTreeSet<NodeId>> {
            let mut m: BTreeMap<String, BTreeSet<NodeId>> = BTreeMap::new();
            for p in l.pkgs.iter().filter(|p| !p.id.is_local()) {
                m.entry(p.id.name.clone()).or_default().insert(p.id.clone());
            }
            m
        };
        let (gm, bm) = (ids(&good), ids(&bad));
        let mut units = Vec::new();
        let mut changes = Vec::new();
        for (name, gset) in &gm {
            if let Some(bset) = bm.get(name) {
                if gset != bset {
                    units.push(name.clone());
                    changes.push(Change {
                        name: name.clone(),
                        from: gset.iter().map(NodeId::show).collect(),
                        to: bset.iter().map(NodeId::show).collect(),
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

        // The manifests are not versioned by us: they stay as they are in the
        // working tree, so a root package whose dependency names differ
        // between good and bad is a hint that more than the lock changed.
        let dep_names = |l: &Lock| -> BTreeMap<String, BTreeSet<String>> {
            l.pkgs
                .iter()
                .filter(|p| p.id.is_local())
                .map(|p| {
                    (
                        p.id.name.clone(),
                        p.deps.iter().map(|d| d.name.clone()).collect(),
                    )
                })
                .collect()
        };
        let (gd, bd) = (dep_names(&good), dep_names(&bad));
        for (name, gs) in &gd {
            if let Some(bs) = bd.get(name) {
                if gs != bs {
                    let added: Vec<&String> = bs.difference(gs).collect();
                    let removed: Vec<&String> = gs.difference(bs).collect();
                    warnings.push(format!(
                        "dependencies of workspace package `{name}` differ between good and bad (added {added:?}, removed {removed:?}): the manifest changed too, whichdep only bisects lockfile changes"
                    ));
                }
            }
        }

        let closure = Self::closure_for(&good, &bad, &units, &unit_ix);
        Ok(CargoPlan {
            lockfile: lockfile.to_string(),
            good,
            bad,
            unit_ix,
            changes,
            closure,
            warnings,
        })
    }

    fn closure_for(
        good: &Lock,
        bad: &Lock,
        units: &[String],
        unit_ix: &HashMap<String, usize>,
    ) -> DepClosure {
        let (gx, bx) = (good.by_name(), bad.by_name());
        let good_ids: HashSet<&NodeId> = good.pkgs.iter().map(|p| &p.id).collect();
        let mut edges: BTreeSet<(usize, usize)> = BTreeSet::new();

        // (a) a changed crate whose new version needs a version of another
        // changed crate that no good version can stand in for (a new major, or
        // a new 0.x minor) needs that crate, also when the path leads through
        // crates that only exist in the bad lockfile and come along with it.
        // Every other pairing is left to Cargo: it keeps the old version when
        // that still satisfies the requirement and the trial is classified as
        // unresolved when it does not.
        let good_names: HashSet<&str> = good.pkgs.iter().map(|p| p.id.name.as_str()).collect();
        for (pi, p) in bad.pkgs.iter().enumerate() {
            let Some(&u) = unit_ix.get(&p.id.name) else {
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
                    match unit_ix.get(&did.name) {
                        Some(&v) => {
                            let stand_in = gx.get(did.name.as_str()).is_some_and(|ix| {
                                ix.iter()
                                    .any(|&g| compatible(&good.pkgs[g].id.version, &did.version))
                            });
                            if v != u && !stand_in {
                                edges.insert((u, v));
                            }
                        }
                        None if !good_names.contains(did.name.as_str()) => stack.push(d),
                        None => {}
                    }
                }
            }
        }
        // (b) a breaking bump of a crate cannot sit next to the old versions
        // of the changed crates that depend on it.
        for p in &good.pkgs {
            let Some(&dep_unit) = unit_ix.get(&p.id.name) else {
                continue;
            };
            for r in &p.deps {
                let Some(q) = good.resolve(&gx, r) else {
                    continue;
                };
                let qid = &good.pkgs[q].id;
                let Some(&q_unit) = unit_ix.get(&qid.name) else {
                    continue;
                };
                if q_unit == dep_unit {
                    continue;
                }
                let survives = bx.get(qid.name.as_str()).is_some_and(|ix| {
                    ix.iter()
                        .any(|&i| compatible(&qid.version, &bad.pkgs[i].id.version))
                });
                if !survives {
                    edges.insert((q_unit, dep_unit));
                }
            }
        }
        DepClosure::new(units.len(), edges, [])
    }

    /// Which packages the configuration "good + applied" consists of.
    fn config(&self, applied: &Set) -> Config {
        let applied_name = |n: &str| self.unit_ix.get(n).is_some_and(|u| applied.contains(u));
        // (package, comes from the bad lockfile?)
        let mut cand: Vec<(&Pkg, bool)> = Vec::new();
        let good_names: HashSet<&str> = self.good.pkgs.iter().map(|p| p.id.name.as_str()).collect();
        for p in &self.good.pkgs {
            if !p.id.is_local() && !applied_name(&p.id.name) {
                cand.push((p, false));
            }
        }
        for p in &self.bad.pkgs {
            let take = p.id.is_local()
                || applied_name(&p.id.name)
                || !good_names.contains(p.id.name.as_str());
            if take {
                cand.push((p, true));
            }
        }
        let by_name: HashMap<&str, Vec<usize>> = {
            let mut m: HashMap<&str, Vec<usize>> = HashMap::new();
            for (i, (p, _)) in cand.iter().enumerate() {
                m.entry(p.id.name.as_str()).or_default().push(i);
            }
            m
        };
        let (gx, bx) = (self.good.by_name(), self.bad.by_name());

        // Where does a dependency of candidate `ci` land in this configuration?
        let target = |ci: usize, r: &DepRef| -> Option<usize> {
            let (p, from_bad) = cand[ci];
            let lock = if from_bad { &self.bad } else { &self.good };
            let ix = if from_bad { &bx } else { &gx };
            let _ = p;
            let want = &lock.pkgs[lock.resolve(ix, r)?].id;
            let same = by_name.get(want.name.as_str())?;
            if let Some(&i) = same.iter().find(|&&i| &cand[i].0.id == want) {
                return Some(i);
            }
            // The exact version is not part of this configuration: take the
            // one that is, preferably on the same compatibility line. If that
            // is a lie (the dependent really needs the other version) Cargo
            // changes the lockfile and the trial is classified as unresolved.
            let newest =
                |a: &usize, b: &usize| cmp_version(&cand[*a].0.id.version, &cand[*b].0.id.version);
            same.iter()
                .copied()
                .filter(|&i| compatible(&want.version, &cand[i].0.id.version))
                .max_by(newest)
                .or_else(|| same.iter().copied().max_by(newest))
        };

        let mut reach = vec![false; cand.len()];
        let mut queue: VecDeque<usize> = VecDeque::new();
        for (i, (p, _)) in cand.iter().enumerate() {
            if p.id.is_local() {
                reach[i] = true;
                queue.push_back(i);
            }
        }
        let mut raw_edges: HashMap<usize, Vec<usize>> = HashMap::new();
        while let Some(i) = queue.pop_front() {
            let mut out = Vec::new();
            for r in &cand[i].0.deps {
                if let Some(t) = target(i, r) {
                    out.push(t);
                    if !reach[t] {
                        reach[t] = true;
                        queue.push_back(t);
                    }
                }
            }
            raw_edges.insert(i, out);
        }

        let mut order: Vec<usize> = (0..cand.len()).filter(|&i| reach[i]).collect();
        order.sort_by(|&a, &b| cand[a].0.id.cmp(&cand[b].0.id));
        let pos: HashMap<usize, usize> = order.iter().enumerate().map(|(n, &i)| (i, n)).collect();
        let nodes = order
            .iter()
            .map(|&i| (cand[i].0.id.clone(), cand[i].0.checksum.clone()))
            .collect();
        let edges = order
            .iter()
            .map(|i| {
                let mut e: Vec<usize> = raw_edges[i].iter().map(|t| pos[t]).collect();
                e.sort_unstable();
                e.dedup();
                e
            })
            .collect();
        Config { nodes, edges }
    }
}

fn dep_string(nodes: &[(NodeId, Option<String>)], t: usize) -> String {
    let id = &nodes[t].0;
    let same_name = nodes.iter().filter(|(n, _)| n.name == id.name).count();
    if same_name == 1 {
        return id.name.clone();
    }
    let same_version = nodes
        .iter()
        .filter(|(n, _)| n.name == id.name && n.version == id.version)
        .count();
    match (&id.source, same_version > 1) {
        (Some(s), true) => format!("{} {} ({})", id.name, id.version, s),
        _ => format!("{} {}", id.name, id.version),
    }
}

impl Ecosystem for CargoPlan {
    fn kind(&self) -> &'static str {
        "cargo"
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
        let cfg = self.config(applied);
        let mut out = self.good.preamble.trim_end().to_string();
        out.push('\n');
        for (i, (id, checksum)) in cfg.nodes.iter().enumerate() {
            out.push_str("\n[[package]]\n");
            out.push_str(&format!(
                "name = \"{}\"\nversion = \"{}\"\n",
                id.name, id.version
            ));
            if let Some(s) = &id.source {
                out.push_str(&format!("source = \"{s}\"\n"));
            }
            if let Some(c) = checksum {
                out.push_str(&format!("checksum = \"{c}\"\n"));
            }
            if !cfg.edges[i].is_empty() {
                out.push_str("dependencies = [\n");
                let deps: Vec<String> = cfg.edges[i]
                    .iter()
                    .map(|&t| dep_string(&cfg.nodes, t))
                    .collect();
                for d in deps {
                    out.push_str(&format!(" \"{d}\",\n"));
                }
                out.push_str("]\n");
            }
        }
        if !self.good.tail.trim().is_empty() {
            out.push('\n');
            out.push_str(self.good.tail.trim_end());
            out.push('\n');
        }
        out
    }

    fn verify(&self, applied: &Set, lock_after_prepare: &str) -> Result<(), String> {
        let after = parse_lock(lock_after_prepare)
            .map_err(|e| format!("cargo left an unreadable lockfile: {e}"))?;
        let have: BTreeSet<NodeId> = after.pkgs.iter().map(|p| p.id.clone()).collect();
        let want: BTreeSet<NodeId> = self
            .config(applied)
            .nodes
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        if have == want {
            return Ok(());
        }
        let fmt = |s: &BTreeSet<NodeId>| -> String {
            s.iter()
                .take(4)
                .map(|i| format!("{} {}", i.name, i.version))
                .collect::<Vec<_>>()
                .join(", ")
        };
        Err(format!(
            "cargo changed the lockfile: it added [{}] and dropped [{}]",
            fmt(&have.difference(&want).cloned().collect()),
            fmt(&want.difference(&have).cloned().collect())
        ))
    }

    fn default_prepare(&self) -> &'static str {
        "cargo fetch -q"
    }

    fn env(&self, _trial_dir: &Path, shared_dir: &Path) -> Vec<(String, String)> {
        // The caller supplies a per-rendered-lockfile cache beneath a
        // fingerprinted project/experiment root. This avoids both source-tree
        // pollution and cross-trial target-directory contamination.
        vec![("CARGO_TARGET_DIR".into(), shared_dir.display().to_string())]
    }

    fn pin_hint(&self, unit: usize) -> Option<String> {
        let c = self.changes.get(unit)?;
        // Filter out versions that stayed identical across good and bad (common
        // when a crate has two major versions in the same lockfile, e.g. syn 1 & 2).
        let changed_from: Vec<&String> = c.from.iter().filter(|v| !c.to.contains(v)).collect();
        let changed_to: Vec<&String> = c.to.iter().filter(|v| !c.from.contains(v)).collect();
        match (changed_from.as_slice(), changed_to.as_slice()) {
            ([from], [to]) if !from.contains(' ') && !to.contains(' ') => {
                Some(format!("cargo update {}@{} --precise {}", c.name, to, from))
            }
            _ => None,
        }
    }

    fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dd::Closure;

    const REG: &str = "registry+https://github.com/rust-lang/crates.io-index";

    fn checksum(name: &str, version: &str) -> String {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in name.bytes().chain(version.bytes()) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!(
            "{:016x}{:016x}{:016x}{:016x}",
            h,
            h.rotate_left(16),
            h.rotate_left(32),
            h.rotate_left(48)
        )
    }

    /// (name, version, registry crate?, dependencies as written in the lockfile)
    type P<'a> = (&'a str, &'a str, bool, &'a [&'a str]);

    /// Writes a lockfile the way Cargo does; callers list the packages in
    /// Cargo's order.
    fn lock(pkgs: &[P]) -> String {
        let mut out = String::from(
            "# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n",
        );
        for (name, version, registry, deps) in pkgs {
            out.push_str(&format!(
                "\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n"
            ));
            if *registry {
                out.push_str(&format!(
                    "source = \"{REG}\"\nchecksum = \"{}\"\n",
                    checksum(name, version)
                ));
            }
            if !deps.is_empty() {
                out.push_str("dependencies = [\n");
                for d in deps.iter() {
                    out.push_str(&format!(" \"{d}\",\n"));
                }
                out.push_str("]\n");
            }
        }
        out
    }

    fn good() -> String {
        lock(&[
            ("app", "0.1.0", false, &["serde", "serde_json"]),
            ("itoa", "1.0.10", true, &[]),
            ("proc-macro2", "1.0.78", true, &["unicode-ident"]),
            ("quote", "1.0.35", true, &["proc-macro2"]),
            ("ryu", "1.0.16", true, &[]),
            ("serde", "1.0.197", true, &["serde_derive"]),
            (
                "serde_derive",
                "1.0.197",
                true,
                &["proc-macro2", "quote", "syn"],
            ),
            ("serde_json", "1.0.114", true, &["itoa", "ryu", "serde"]),
            (
                "syn",
                "2.0.50",
                true,
                &["proc-macro2", "quote", "unicode-ident"],
            ),
            ("unicode-ident", "1.0.12", true, &[]),
        ])
    }

    /// serde, serde_derive, serde_json, proc-macro2 and syn moved; serde_json
    /// swapped `ryu` for the new `memchr`.
    fn bad() -> String {
        lock(&[
            ("app", "0.1.0", false, &["serde", "serde_json"]),
            ("itoa", "1.0.10", true, &[]),
            ("memchr", "2.7.1", true, &[]),
            ("proc-macro2", "1.0.79", true, &["unicode-ident"]),
            ("quote", "1.0.35", true, &["proc-macro2"]),
            ("serde", "1.0.200", true, &["serde_derive"]),
            (
                "serde_derive",
                "1.0.200",
                true,
                &["proc-macro2", "quote", "syn"],
            ),
            ("serde_json", "1.0.116", true, &["itoa", "memchr", "serde"]),
            (
                "syn",
                "2.0.52",
                true,
                &["proc-macro2", "quote", "unicode-ident"],
            ),
            ("unicode-ident", "1.0.12", true, &[]),
        ])
    }

    fn plan() -> CargoPlan {
        CargoPlan::new("Cargo.lock", &good(), &bad()).unwrap()
    }

    fn unit(p: &CargoPlan, name: &str) -> usize {
        p.changes()
            .iter()
            .position(|c| c.name == name)
            .unwrap_or_else(|| panic!("no unit {name}"))
    }

    fn set(v: &[usize]) -> Set {
        v.iter().copied().collect()
    }

    #[test]
    fn units_are_the_crates_whose_locked_versions_differ() {
        let p = plan();
        let names: Vec<&str> = p.changes().iter().map(|c| c.name.as_str()).collect();
        // `memchr` is new and `ryu` is gone: they come and go with serde_json.
        assert_eq!(
            names,
            vec!["proc-macro2", "serde", "serde_derive", "serde_json", "syn"]
        );
        assert_eq!(
            p.changes()[unit(&p, "serde_json")].describe(),
            "serde_json 1.0.114 -> 1.0.116"
        );
        assert!(p.warnings().is_empty(), "{:?}", p.warnings());
    }

    #[test]
    fn nothing_applied_is_the_good_lockfile_and_everything_applied_the_bad_one() {
        let p = plan();
        assert_eq!(p.render(&Set::new()), good());
        let all: Set = (0..p.changes().len()).collect();
        assert_eq!(p.render(&all), bad());
    }

    #[test]
    fn a_new_dependency_comes_with_the_crate_that_needs_it() {
        let p = plan();
        let out = p.render(&set(&[unit(&p, "serde_json")]));
        assert!(out.contains("name = \"serde_json\"\nversion = \"1.0.116\""));
        assert!(
            out.contains("name = \"memchr\""),
            "memchr is serde_json's new dependency"
        );
        assert!(
            !out.contains("name = \"ryu\""),
            "nothing needs ryu any more, Cargo would prune it"
        );
        // everything else stays at the good versions
        assert!(out.contains("name = \"serde\"\nversion = \"1.0.197\""));
        assert!(out.contains("name = \"syn\"\nversion = \"2.0.50\""));
        assert!(out.contains("name = \"proc-macro2\"\nversion = \"1.0.78\""));
    }

    #[test]
    fn compatible_updates_are_independent_of_each_other() {
        // serde_json 1.0.116 builds with serde 1.0.197, so it is its own unit.
        let p = plan();
        let sj = unit(&p, "serde_json");
        assert_eq!(p.closure().up(&set(&[sj])), set(&[sj]));
        let s = unit(&p, "serde");
        assert_eq!(p.closure().up(&set(&[s])), set(&[s]));
    }

    #[test]
    fn a_breaking_bump_and_its_dependent_cannot_be_separated() {
        let good = lock(&[
            ("a", "1.0.0", true, &["b"]),
            ("app", "0.1.0", false, &["a", "b"]),
            ("b", "1.0.0", true, &[]),
        ]);
        let bad = lock(&[
            ("a", "1.1.0", true, &["b"]),
            ("app", "0.1.0", false, &["a", "b"]),
            ("b", "2.0.0", true, &[]),
        ]);
        let p = CargoPlan::new("Cargo.lock", &good, &bad).unwrap();
        let (a, b) = (unit(&p, "a"), unit(&p, "b"));
        assert_eq!(p.closure().up(&set(&[a])), set(&[a, b]));
        assert_eq!(p.closure().up(&set(&[b])), set(&[a, b]));
        let all = set(&[a, b]);
        assert_eq!(p.render(&all), bad);
        assert_eq!(p.render(&Set::new()), good);
    }

    const GOOD_MULTI: &[P] = &[
        ("app", "0.1.0", false, &["serde_json", "syn"]),
        ("serde_json", "1.0.114", true, &[]),
        ("syn", "2.0.50", true, &[]),
    ];
    const BAD_MULTI: &[P] = &[
        ("app", "0.1.0", false, &["serde_json", "syn 2.0.52"]),
        ("legacy", "0.3.1", true, &["syn 1.0.109"]),
        ("serde_json", "1.0.116", true, &["legacy"]),
        ("syn", "1.0.109", true, &[]),
        ("syn", "2.0.52", true, &[]),
    ];

    #[test]
    fn a_crate_that_comes_along_drags_its_own_requirements_in() {
        let p = CargoPlan::new("Cargo.lock", &lock(GOOD_MULTI), &lock(BAD_MULTI)).unwrap();
        let (sj, syn) = (unit(&p, "serde_json"), unit(&p, "syn"));
        // `legacy` is new, comes with serde_json and needs the new syn 1.x.
        assert_eq!(p.closure().up(&set(&[sj])), set(&[sj, syn]));
        assert_eq!(p.closure().up(&set(&[syn])), set(&[syn]));
    }

    #[test]
    fn two_versions_of_a_crate_are_told_apart_in_dependency_lists() {
        let p = CargoPlan::new("Cargo.lock", &lock(GOOD_MULTI), &lock(BAD_MULTI)).unwrap();
        let (sj, syn) = (unit(&p, "serde_json"), unit(&p, "syn"));
        assert_eq!(p.render(&set(&[sj, syn])), lock(BAD_MULTI));
        assert_eq!(p.render(&Set::new()), lock(GOOD_MULTI));
        // syn 1.0.109 only exists for `legacy`; without legacy it is pruned and
        // the remaining dependency on syn loses its version again.
        let only_syn = p.render(&set(&[syn]));
        assert!(only_syn.contains("name = \"syn\"\nversion = \"2.0.52\""));
        assert!(!only_syn.contains("1.0.109"));
        assert!(only_syn.contains(" \"syn\",\n"));
    }

    #[test]
    fn versions_are_ordered_like_cargo_orders_them() {
        let lockfile = lock(&[
            ("app", "0.1.0", false, &["x 0.9.0", "x 0.10.0"]),
            ("x", "0.9.0", true, &[]),
            ("x", "0.10.0", true, &[]),
        ]);
        let p = CargoPlan::new("Cargo.lock", &lockfile, &lockfile).unwrap();
        assert!(p.changes().is_empty());
        assert_eq!(p.render(&Set::new()), lockfile);
        assert_eq!(cmp_version("1.0.0-alpha", "1.0.0"), Ordering::Less);
        assert_eq!(
            cmp_version("1.0.0-alpha.2", "1.0.0-alpha.10"),
            Ordering::Less
        );
        assert_eq!(cmp_version("1.2.3", "1.10.0"), Ordering::Less);
    }

    #[test]
    fn verify_notices_when_cargo_had_to_change_the_lockfile() {
        let p = plan();
        let sj = set(&[unit(&p, "serde_json")]);
        let written = p.render(&sj);
        assert!(p.verify(&sj, &written).is_ok());
        // Cargo found the mix unsatisfiable and fell back to the good versions.
        let err = p.verify(&sj, &good()).unwrap_err();
        assert!(err.contains("memchr"), "{err}");
        assert!(err.contains("serde_json 1.0.114"), "{err}");
    }

    #[test]
    fn manifest_changes_are_called_out() {
        let good = lock(&[("a", "1.0.0", true, &[]), ("app", "0.1.0", false, &["a"])]);
        let bad = lock(&[
            ("a", "1.0.0", true, &[]),
            ("app", "0.1.0", false, &["a", "b"]),
            ("b", "1.0.0", true, &[]),
        ]);
        let p = CargoPlan::new("Cargo.lock", &good, &bad).unwrap();
        assert_eq!(p.warnings().len(), 1);
        assert!(p.warnings()[0].contains("manifest"), "{:?}", p.warnings());
    }

    #[test]
    fn unsupported_lockfile_formats_are_rejected() {
        let v1 = "[[package]]\nname = \"a\"\nversion = \"1.0.0\"\n";
        assert!(CargoPlan::new("Cargo.lock", v1, v1)
            .err()
            .unwrap()
            .contains("older than version 3"));
        let v9 = "version = 9\n";
        assert!(CargoPlan::new("Cargo.lock", v9, v9)
            .err()
            .unwrap()
            .contains("not supported"));
    }

    #[test]
    fn git_sources_show_the_commit() {
        let id = NodeId {
            name: "x".into(),
            version: "0.1.0".into(),
            source: Some("git+https://github.com/a/x?rev=abc#0123456789abcdef".into()),
        };
        assert_eq!(id.show(), "0.1.0 (git 01234567)");
    }

    #[test]
    fn tables_after_the_packages_survive() {
        let mut g = good();
        g.push_str("\n[[patch.unused]]\nname = \"zzz\"\nversion = \"0.1.0\"\n");
        let b = bad() + "\n[[patch.unused]]\nname = \"zzz\"\nversion = \"0.1.0\"\n";
        let p = CargoPlan::new("Cargo.lock", &g, &b).unwrap();
        assert_eq!(p.render(&Set::new()), g);
    }
}
