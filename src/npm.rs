//! `package-lock.json` (npm lockfileVersion 2 and 3) ecosystem adapter.
//!
//! Uses whichdep's built-in zero-dependency JSON parser to diff `"packages"`
//! entries, model semver dependency/peer closures, render partial lockfiles
//! (keeping v2 legacy `"dependencies"` in sync when present), and verify that
//! `npm ci` kept the exact requested package graph.

use crate::adapter::Adapter;
use crate::dd::{DepClosure, Set};
use crate::json::Json;
use crate::model::{Change, Ecosystem};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
struct NpmPkgInfo {
    path: String,
    name: String,
    version: String,
    resolved: Option<String>,
    integrity: Option<String>,
    deps: Vec<(String, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnitKind {
    Updated,
    Added,
    Removed,
}

#[derive(Clone, Debug)]
struct NpmUnit {
    path: String,
    kind: UnitKind,
}

pub struct NpmPlan {
    lockfile: String,
    good_root: Json,
    bad_root: Json,
    good_pkgs: BTreeMap<String, NpmPkgInfo>,
    bad_pkgs: BTreeMap<String, NpmPkgInfo>,
    units: Vec<NpmUnit>,
    changes: Vec<Change>,
    closure: DepClosure,
    warnings: Vec<String>,
}

fn is_local_entry(path: &str, value: &Json) -> bool {
    path.is_empty()
        || !path.contains("node_modules/")
        || value.get("link").and_then(Json::as_bool) == Some(true)
}

fn package_name_from_path(path: &str) -> String {
    if let Some((_, tail)) = path.rsplit_once("node_modules/") {
        tail.to_string()
    } else {
        path.to_string()
    }
}

fn display_name_for_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("node_modules/") {
        if let Some((parent, child)) = rest.rsplit_once("/node_modules/") {
            return format!("{child} (under {parent})");
        }
        return rest.to_string();
    }
    path.to_string()
}

fn parse_npm_lock(text: &str) -> Result<(Json, u64, BTreeMap<String, NpmPkgInfo>), String> {
    let root = Json::parse(text)?;
    let lock_version = root
        .get("lockfileVersion")
        .and_then(Json::as_i64)
        .ok_or_else(|| "missing `lockfileVersion`: not a valid package-lock.json".to_string())?;
    if !(2..=3).contains(&lock_version) {
        return Err(format!(
            "package-lock.json lockfileVersion {lock_version} is not supported (only versions 2 and 3)"
        ));
    }
    let packages = root
        .get("packages")
        .and_then(Json::as_obj)
        .ok_or_else(|| "package-lock.json is missing the `packages` map".to_string())?;

    let mut map = BTreeMap::new();
    for (path, val) in packages {
        if is_local_entry(path, val) {
            continue;
        }
        let version = val
            .get("version")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string();
        let resolved = val
            .get("resolved")
            .and_then(Json::as_str)
            .map(str::to_string);
        let integrity = val
            .get("integrity")
            .and_then(Json::as_str)
            .map(str::to_string);
        let mut deps = Vec::new();
        for section in ["dependencies", "peerDependencies"] {
            if let Some(obj) = val.get(section).and_then(Json::as_obj) {
                for (k, v) in obj {
                    if let Some(range) = v.as_str() {
                        deps.push((k.clone(), range.to_string()));
                    }
                }
            }
        }
        map.insert(
            path.clone(),
            NpmPkgInfo {
                path: path.clone(),
                name: package_name_from_path(path),
                version,
                resolved,
                integrity,
                deps,
            },
        );
    }
    Ok((root, lock_version as u64, map))
}

fn parse_semver_triple(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let patch = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

/// Evaluates common npm semver ranges (`^`, `~`, `>=`, `>`, `<=`, `<`, exact, `*`, `||`).
fn npm_range_matches(range: &str, version: &str) -> bool {
    let range = range.trim();
    if range.is_empty() || range == "*" || range == "latest" || range.eq_ignore_ascii_case("x") {
        return true;
    }
    let Some(ver) = parse_semver_triple(version) else {
        return range == version;
    };
    for branch in range.split("||") {
        let mut all_ok = true;
        let tokens: Vec<&str> = branch.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }
        for tok in tokens {
            if !single_comparator_matches(tok, ver) {
                all_ok = false;
                break;
            }
        }
        if all_ok {
            return true;
        }
    }
    false
}

fn single_comparator_matches(tok: &str, ver: (u64, u64, u64)) -> bool {
    let tok = tok.trim();
    if tok == "*" || tok.eq_ignore_ascii_case("x") {
        return true;
    }
    if let Some(rest) = tok.strip_prefix('^') {
        let Some(base) = parse_semver_triple(rest) else {
            return true;
        };
        if ver < base {
            return false;
        }
        return if base.0 > 0 {
            ver.0 == base.0
        } else if base.1 > 0 {
            ver.0 == 0 && ver.1 == base.1
        } else {
            ver == base
        };
    }
    if let Some(rest) = tok.strip_prefix('~') {
        let Some(base) = parse_semver_triple(rest) else {
            return true;
        };
        return ver >= base && ver.0 == base.0 && ver.1 == base.1;
    }
    if let Some(rest) = tok.strip_prefix(">=") {
        return parse_semver_triple(rest).is_none_or(|b| ver >= b);
    }
    if let Some(rest) = tok.strip_prefix("<=") {
        return parse_semver_triple(rest).is_none_or(|b| ver <= b);
    }
    if let Some(rest) = tok.strip_prefix('>') {
        return parse_semver_triple(rest).is_none_or(|b| ver > b);
    }
    if let Some(rest) = tok.strip_prefix('<') {
        return parse_semver_triple(rest).is_none_or(|b| ver < b);
    }
    let exact = tok.strip_prefix('=').unwrap_or(tok);
    parse_semver_triple(exact).is_none_or(|b| ver == b)
}

/// Resolve a dependency name from a `node_modules/...` package path using
/// Node's ancestor-directory lookup rules.
fn resolve_dep_path<'a>(
    from_path: &str,
    dep_name: &str,
    pkgs: &'a BTreeMap<String, NpmPkgInfo>,
) -> Option<&'a str> {
    let mut prefix = from_path;
    loop {
        let candidate = if prefix.is_empty() {
            format!("node_modules/{dep_name}")
        } else {
            format!("{prefix}/node_modules/{dep_name}")
        };
        if let Some((k, _)) = pkgs.get_key_value(&candidate) {
            return Some(k.as_str());
        }
        if prefix.is_empty() {
            return None;
        }
        prefix = match prefix.rsplit_once("/node_modules/") {
            Some((parent, _)) => parent,
            None => "",
        };
    }
}

fn root_dep_names(root: &Json) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Some(root_pkg) = root.get("packages").and_then(|p| p.get("")) else {
        return out;
    };
    for sec in ["dependencies", "devDependencies", "optionalDependencies"] {
        if let Some(obj) = root_pkg.get(sec).and_then(Json::as_obj) {
            for (name, _) in obj {
                out.insert(name.clone());
            }
        }
    }
    out
}

impl NpmPlan {
    pub fn new(lockfile: &str, good_text: &str, bad_text: &str) -> Result<NpmPlan, String> {
        let (good_root, good_ver, good_pkgs) =
            parse_npm_lock(good_text).map_err(|e| format!("good: {e}"))?;
        let (bad_root, bad_ver, bad_pkgs) =
            parse_npm_lock(bad_text).map_err(|e| format!("bad: {e}"))?;

        let mut warnings = Vec::new();
        if good_ver != bad_ver {
            warnings.push(format!(
                "lockfileVersion differs between good ({good_ver}) and bad ({bad_ver})"
            ));
        }

        let (gd, bd) = (root_dep_names(&good_root), root_dep_names(&bad_root));
        if gd != bd {
            let added: Vec<&String> = bd.difference(&gd).collect();
            let removed: Vec<&String> = gd.difference(&bd).collect();
            warnings.push(format!(
                "root package.json dependencies differ between good and bad (added {added:?}, removed {removed:?}): whichdep only bisects lockfile changes"
            ));
        }

        let mut units = Vec::new();
        let mut changes = Vec::new();
        for (path, g) in &good_pkgs {
            match bad_pkgs.get(path) {
                Some(b)
                    if b.version == g.version
                        && b.resolved == g.resolved
                        && b.integrity == g.integrity => {}
                Some(b) => {
                    units.push(NpmUnit {
                        path: path.clone(),
                        kind: UnitKind::Updated,
                    });
                    changes.push(Change {
                        name: display_name_for_path(path),
                        from: vec![g.version.clone()],
                        to: vec![b.version.clone()],
                    });
                }
                None => {
                    units.push(NpmUnit {
                        path: path.clone(),
                        kind: UnitKind::Removed,
                    });
                    changes.push(Change {
                        name: display_name_for_path(path),
                        from: vec![g.version.clone()],
                        to: Vec::new(),
                    });
                }
            }
        }
        for (path, b) in &bad_pkgs {
            if !good_pkgs.contains_key(path) {
                units.push(NpmUnit {
                    path: path.clone(),
                    kind: UnitKind::Added,
                });
                changes.push(Change {
                    name: display_name_for_path(path),
                    from: Vec::new(),
                    to: vec![b.version.clone()],
                });
            }
        }

        let unit_ix: HashMap<&str, usize> = units
            .iter()
            .enumerate()
            .map(|(i, u)| (u.path.as_str(), i))
            .collect();

        let mut edges: BTreeSet<(usize, usize)> = BTreeSet::new();
        // (a) Updated/Added package in bad requiring an Added package or an
        // incompatible Updated package version.
        for (ui, u) in units.iter().enumerate() {
            if u.kind == UnitKind::Removed {
                continue;
            }
            let Some(bpkg) = bad_pkgs.get(&u.path) else {
                continue;
            };
            for (dep_name, range) in &bpkg.deps {
                let Some(dep_path) = resolve_dep_path(&u.path, dep_name, &bad_pkgs) else {
                    continue;
                };
                let Some(&vi) = unit_ix.get(dep_path) else {
                    continue;
                };
                if vi == ui {
                    continue;
                }
                match units[vi].kind {
                    UnitKind::Added => {
                        edges.insert((ui, vi));
                    }
                    UnitKind::Updated => {
                        if let Some(gdep) = good_pkgs.get(dep_path) {
                            if !npm_range_matches(range, &gdep.version) {
                                edges.insert((ui, vi));
                            }
                        }
                    }
                    UnitKind::Removed => {}
                }
            }
        }

        // (b) Reverse check: if an Updated/Removed dependency breaks an
        // existing package's good range, pull that dependent along.
        for (ui, u) in units.iter().enumerate() {
            if u.kind == UnitKind::Added {
                continue;
            }
            let Some(gpkg) = good_pkgs.get(&u.path) else {
                continue;
            };
            for (dep_name, range) in &gpkg.deps {
                let Some(dep_path) = resolve_dep_path(&u.path, dep_name, &good_pkgs) else {
                    continue;
                };
                let Some(&vi) = unit_ix.get(dep_path) else {
                    continue;
                };
                if vi == ui {
                    continue;
                }
                match units[vi].kind {
                    UnitKind::Removed => {
                        edges.insert((vi, ui));
                    }
                    UnitKind::Updated => {
                        if let Some(bdep) = bad_pkgs.get(dep_path) {
                            if !npm_range_matches(range, &bdep.version) {
                                edges.insert((vi, ui));
                            }
                        }
                    }
                    UnitKind::Added => {}
                }
            }
        }

        let closure = DepClosure::new(units.len(), edges, []);
        Ok(NpmPlan {
            lockfile: lockfile.to_string(),
            good_root,
            bad_root,
            good_pkgs,
            bad_pkgs,
            units,
            changes,
            closure,
            warnings,
        })
    }

    fn expected_versions(&self, applied: &Set) -> BTreeMap<String, (String, Option<String>)> {
        let mut by_path: HashMap<&str, usize> = HashMap::new();
        for (i, u) in self.units.iter().enumerate() {
            by_path.insert(u.path.as_str(), i);
        }
        let mut out = BTreeMap::new();
        for (path, g) in &self.good_pkgs {
            match by_path.get(path.as_str()) {
                Some(&ui) if applied.contains(&ui) => {
                    if let Some(b) = self.bad_pkgs.get(path) {
                        out.insert(path.clone(), (b.version.clone(), b.integrity.clone()));
                    }
                }
                _ => {
                    out.insert(path.clone(), (g.version.clone(), g.integrity.clone()));
                }
            }
        }
        for (ui, u) in self.units.iter().enumerate() {
            if u.kind == UnitKind::Added && applied.contains(&ui) {
                if let Some(b) = self.bad_pkgs.get(&u.path) {
                    out.insert(u.path.clone(), (b.version.clone(), b.integrity.clone()));
                }
            }
        }
        out
    }
}

impl Ecosystem for NpmPlan {
    fn kind(&self) -> &'static str {
        "npm"
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
        let mut rendered = self.good_root.clone();
        let bad_packages = self
            .bad_root
            .get("packages")
            .and_then(Json::as_obj)
            .unwrap_or(&[]);
        let bad_pkg_map: HashMap<&str, &Json> =
            bad_packages.iter().map(|(k, v)| (k.as_str(), v)).collect();

        let unit_by_path: HashMap<&str, usize> = self
            .units
            .iter()
            .enumerate()
            .map(|(i, u)| (u.path.as_str(), i))
            .collect();

        let mut new_entries: BTreeMap<String, Json> = BTreeMap::new();
        if let Some(good_packages) = self.good_root.get("packages").and_then(Json::as_obj) {
            for (path, val) in good_packages {
                if is_local_entry(path, val) {
                    if let Some(&bad_local) = bad_pkg_map.get(path.as_str()) {
                        new_entries.insert(path.clone(), bad_local.clone());
                    } else {
                        new_entries.insert(path.clone(), val.clone());
                    }
                    continue;
                }
                match unit_by_path.get(path.as_str()) {
                    Some(&ui) if applied.contains(&ui) => {
                        if let Some(&bad_val) = bad_pkg_map.get(path.as_str()) {
                            new_entries.insert(path.clone(), bad_val.clone());
                        }
                    }
                    _ => {
                        new_entries.insert(path.clone(), val.clone());
                    }
                }
            }
        }
        // Include any newly added local workspaces from bad, plus applied Added units.
        for (path, val) in bad_packages {
            if is_local_entry(path, val) && !new_entries.contains_key(path) {
                new_entries.insert(path.clone(), val.clone());
            }
        }
        for (ui, u) in self.units.iter().enumerate() {
            if u.kind == UnitKind::Added && applied.contains(&ui) {
                if let Some(&bad_val) = bad_pkg_map.get(u.path.as_str()) {
                    new_entries.insert(u.path.clone(), bad_val.clone());
                }
            }
        }

        let ordered_packages: Vec<(String, Json)> = new_entries.into_iter().collect();
        rendered.set("packages", Json::Obj(ordered_packages));

        // For lockfileVersion 2, keep top-level legacy `dependencies` in sync.
        if let Some(deps_obj) = rendered.get_mut("dependencies").and_then(Json::as_obj_mut) {
            let bad_deps: HashMap<&str, &Json> = self
                .bad_root
                .get("dependencies")
                .and_then(Json::as_obj)
                .map(|entries| entries.iter().map(|(k, v)| (k.as_str(), v)).collect())
                .unwrap_or_default();
            for (ui, u) in self.units.iter().enumerate() {
                if !applied.contains(&ui) {
                    continue;
                }
                let Some(top_name) = u.path.strip_prefix("node_modules/") else {
                    continue;
                };
                if top_name.contains("node_modules/") {
                    continue;
                }
                match u.kind {
                    UnitKind::Updated => {
                        if let Some(&bv) = bad_deps.get(top_name) {
                            if let Some((_, existing)) =
                                deps_obj.iter_mut().find(|(k, _)| k == top_name)
                            {
                                *existing = bv.clone();
                            }
                        }
                    }
                    UnitKind::Removed => {
                        deps_obj.retain(|(k, _)| k != top_name);
                    }
                    UnitKind::Added => {
                        if let Some(&bv) = bad_deps.get(top_name) {
                            if !deps_obj.iter().any(|(k, _)| k == top_name) {
                                deps_obj.push((top_name.to_string(), bv.clone()));
                            }
                        }
                    }
                }
            }
        }

        rendered.pretty()
    }

    fn verify(&self, applied: &Set, lock_after_prepare: &str) -> Result<(), String> {
        let (_, _, after_pkgs) = parse_npm_lock(lock_after_prepare)
            .map_err(|e| format!("npm left an unreadable package-lock.json: {e}"))?;
        let have: BTreeMap<String, (String, Option<String>)> = after_pkgs
            .into_iter()
            .map(|(k, v)| (k, (v.version, v.integrity)))
            .collect();
        let want = self.expected_versions(applied);
        if have == want {
            return Ok(());
        }
        let mut diffs = Vec::new();
        for (k, (v, _)) in &have {
            if want.get(k).map(|(wv, _)| wv) != Some(v) {
                diffs.push(format!("{k}@{v}"));
            }
        }
        Err(format!(
            "npm changed package-lock.json during prepare: [{}]",
            diffs.into_iter().take(4).collect::<Vec<_>>().join(", ")
        ))
    }

    fn default_prepare(&self) -> &'static str {
        "npm ci --no-audit --no-fund --prefer-offline"
    }

    fn env(&self, trial_dir: &Path, shared_dir: &Path) -> Vec<(String, String)> {
        let bin = trial_dir.join("node_modules").join(".bin");
        let npm_cache = shared_dir.join("npm-cache");
        let path = std::env::var("PATH").unwrap_or_default();
        vec![
            ("npm_config_cache".into(), npm_cache.display().to_string()),
            ("PATH".into(), format!("{}:{}", bin.display(), path)),
        ]
    }

    fn warnings(&self) -> &[String] {
        &self.warnings
    }

    fn pin_hint(&self, unit: usize) -> Option<String> {
        let u = self.units.get(unit)?;
        if u.kind != UnitKind::Updated {
            return None;
        }
        let top_name = u.path.strip_prefix("node_modules/")?;
        if top_name.contains("node_modules/") {
            return None;
        }
        let g = self.good_pkgs.get(&u.path)?;
        if g.version.is_empty() {
            return None;
        }
        Some(format!("npm install --save-exact {top_name}@{}", g.version))
    }
}

/// Built-in adapter for Node.js `package-lock.json` (v2 and v3).
pub struct NpmAdapter;

impl Adapter for NpmAdapter {
    fn id(&self) -> &'static str {
        "npm"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["package-lock"]
    }

    fn description(&self) -> &'static str {
        "Node.js package-lock.json (v2, v3)"
    }

    fn lockfiles(&self) -> &'static [&'static str] {
        &["package-lock.json"]
    }

    fn plan(
        &self,
        lockfile: &str,
        good_text: &str,
        bad_text: &str,
    ) -> Result<Box<dyn Ecosystem>, String> {
        Ok(Box::new(NpmPlan::new(lockfile, good_text, bad_text)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dd::Closure;

    const GOOD_NPM: &str = r#"{
  "name": "demo",
  "version": "1.0.0",
  "lockfileVersion": 3,
  "requires": true,
  "packages": {
    "": {
      "name": "demo",
      "version": "1.0.0",
      "dependencies": {
        "express": "^4.18.0",
        "@scope/util": "^1.0.0"
      }
    },
    "node_modules/@scope/util": {
      "version": "1.0.0",
      "resolved": "https://registry.npmjs.org/@scope/util/-/util-1.0.0.tgz",
      "integrity": "sha512-aaa"
    },
    "node_modules/express": {
      "version": "4.18.2",
      "resolved": "https://registry.npmjs.org/express/-/express-4.18.2.tgz",
      "integrity": "sha512-bbb",
      "dependencies": {
        "qs": "^6.11.0"
      }
    },
    "node_modules/qs": {
      "version": "6.11.0",
      "resolved": "https://registry.npmjs.org/qs/-/qs-6.11.0.tgz",
      "integrity": "sha512-ccc"
    }
  }
}
"#;

    const BAD_NPM: &str = r#"{
  "name": "demo",
  "version": "1.0.0",
  "lockfileVersion": 3,
  "requires": true,
  "packages": {
    "": {
      "name": "demo",
      "version": "1.0.0",
      "dependencies": {
        "express": "^4.18.0",
        "@scope/util": "^1.0.0"
      }
    },
    "node_modules/@scope/util": {
      "version": "1.2.0",
      "resolved": "https://registry.npmjs.org/@scope/util/-/util-1.2.0.tgz",
      "integrity": "sha512-ddd"
    },
    "node_modules/express": {
      "version": "4.19.2",
      "resolved": "https://registry.npmjs.org/express/-/express-4.19.2.tgz",
      "integrity": "sha512-eee",
      "dependencies": {
        "qs": "^6.12.0",
        "fresh-helper": "^1.0.0"
      }
    },
    "node_modules/fresh-helper": {
      "version": "1.0.0",
      "resolved": "https://registry.npmjs.org/fresh-helper/-/fresh-helper-1.0.0.tgz",
      "integrity": "sha512-fff"
    },
    "node_modules/qs": {
      "version": "6.12.1",
      "resolved": "https://registry.npmjs.org/qs/-/qs-6.12.1.tgz",
      "integrity": "sha512-ggg"
    }
  }
}
"#;

    fn unit(p: &NpmPlan, name: &str) -> usize {
        p.changes().iter().position(|c| c.name == name).unwrap()
    }

    #[test]
    fn diffs_scoped_updated_and_added_packages() {
        let p = NpmPlan::new("package-lock.json", GOOD_NPM, BAD_NPM).unwrap();
        let names: Vec<&str> = p.changes().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["@scope/util", "express", "qs", "fresh-helper"]);
        assert!(p.warnings().is_empty());
        assert_eq!(
            p.pin_hint(unit(&p, "@scope/util")).as_deref(),
            Some("npm install --save-exact @scope/util@1.0.0")
        );
    }

    #[test]
    fn semver_range_and_new_dependency_build_closure() {
        let p = NpmPlan::new("package-lock.json", GOOD_NPM, BAD_NPM).unwrap();
        let express = unit(&p, "express");
        let qs = unit(&p, "qs");
        let fresh = unit(&p, "fresh-helper");
        // express 4.19.2 needs qs ^6.12.0 (incompatible with 6.11.0) and new fresh-helper.
        let closed = p.closure().up(&[express].into_iter().collect());
        assert!(closed.contains(&express));
        assert!(closed.contains(&qs));
        assert!(closed.contains(&fresh));
    }

    #[test]
    fn renders_valid_partial_lockfile_and_verifies() {
        let p = NpmPlan::new("package-lock.json", GOOD_NPM, BAD_NPM).unwrap();
        let util = unit(&p, "@scope/util");
        let set: Set = [util].into_iter().collect();
        let rendered = p.render(&set);
        let parsed = Json::parse(&rendered).unwrap();
        let pkgs = parsed.get("packages").unwrap();
        assert_eq!(
            pkgs.get("node_modules/@scope/util")
                .unwrap()
                .get("version")
                .and_then(Json::as_str),
            Some("1.2.0")
        );
        assert_eq!(
            pkgs.get("node_modules/express")
                .unwrap()
                .get("version")
                .and_then(Json::as_str),
            Some("4.18.2")
        );
        assert!(pkgs.get("node_modules/fresh-helper").is_none());
        assert!(p.verify(&set, &rendered).is_ok());
        assert!(p.verify(&set, GOOD_NPM).is_err());
    }

    #[test]
    fn rejects_lockfile_v1() {
        let v1 = r#"{"name": "x", "lockfileVersion": 1, "dependencies": {}}"#;
        assert!(NpmPlan::new("package-lock.json", v1, v1)
            .err()
            .unwrap()
            .contains("not supported"));
    }
}
