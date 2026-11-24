//! Dependency-free persistence for experiment history and reusable trial results.
//!
//! Each cached trial is a separate atomically-written file. The complete
//! composite key is stored beside its filename hash and compared on load.
//! The inputs used to create that key include caller-provided fingerprints,
//! which are probabilistic rather than cryptographic. `Unresolved` is not
//! cached because environment/setup failures are often transient.

use crate::dd::Outcome;
use crate::diagnosis::Diagnosis;
use crate::fingerprint;
use crate::json::Json;
use crate::model::Experiment;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct ExperimentStore {
    root: PathBuf,
}

impl ExperimentStore {
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self { root })
    }

    pub fn default_root() -> PathBuf {
        if let Some(path) = std::env::var_os("WHICHDEP_HOME") {
            return PathBuf::from(path);
        }
        #[cfg(windows)]
        if let Some(path) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(path).join("whichdep");
        }
        if let Some(path) = std::env::var_os("XDG_CACHE_HOME") {
            return PathBuf::from(path).join("whichdep");
        }
        if let Some(path) = std::env::var_os("HOME") {
            return PathBuf::from(path).join(".cache").join("whichdep");
        }
        std::env::temp_dir().join("whichdep-cache")
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn experiment_dir(&self, id: &str) -> io::Result<PathBuf> {
        if id.is_empty()
            || id.len() > 120
            || id == "."
            || id == ".."
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "experiment id must contain only ASCII letters, digits, '.', '_' or '-'",
            ));
        }
        let experiments = self.root.join("experiments");
        create_private_dir(&experiments)?;
        let dir = experiments.join(id);
        create_private_dir(&dir)?;
        Ok(dir)
    }

    fn trials_dir(&self, id: &str) -> io::Result<PathBuf> {
        let dir = self.experiment_dir(id)?.join("trials");
        create_private_dir(&dir)?;
        Ok(dir)
    }

    /// Return cached Pass/Fail outcomes for the exact experiment namespace.
    pub fn load_cache(
        &self,
        experiment_id: &str,
        namespace: &str,
    ) -> io::Result<HashMap<Vec<usize>, Outcome>> {
        let dir = self.trials_dir(experiment_id)?;
        let mut cache = HashMap::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if entry.path().extension().and_then(|s| s.to_str()) != Some("trial") {
                continue;
            }
            let mut contents = String::new();
            if fs::File::open(entry.path())?
                .read_to_string(&mut contents)
                .is_err()
            {
                continue;
            }
            let mut lines = contents.lines();
            if lines.next() != Some("whichdep-trial-v1") {
                continue;
            }
            let Some(encoded_key) = lines.next() else {
                continue;
            };
            let Some(outcome_text) = lines.next() else {
                continue;
            };
            let Some(key) = hex_decode(encoded_key) else {
                continue;
            };
            let prefix = format!("{namespace}\n");
            let Some(config_text) = key.strip_prefix(prefix.as_str()) else {
                continue;
            };
            let config = if config_text.is_empty() {
                Vec::new()
            } else {
                let parsed: Result<Vec<usize>, _> =
                    config_text.split(',').map(str::parse).collect();
                match parsed {
                    Ok(v) => v,
                    Err(_) => continue,
                }
            };
            let outcome = match outcome_text {
                "pass" => Outcome::Pass,
                "fail" => Outcome::Fail,
                // Unresolved results are intentionally never cacheable.
                _ => continue,
            };
            cache.insert(config, outcome);
        }
        Ok(cache)
    }

    /// Persist one conclusive result as soon as a trial finishes. This is the
    /// path used by the engine's trial callback, so a process interruption does
    /// not discard earlier completed work.
    pub fn save_trial(
        &self,
        experiment_id: &str,
        namespace: &str,
        config: &[usize],
        outcome: Outcome,
    ) -> io::Result<()> {
        let outcome_text = match outcome {
            Outcome::Pass => "pass",
            Outcome::Fail => "fail",
            Outcome::Unresolved => "unresolved",
        };
        let dir = self.trials_dir(experiment_id)?;
        let config_text = config
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let key = format!("{namespace}\n{config_text}");
        let file_name = format!("{}.trial", fingerprint::hash(key.as_bytes()));
        let path = dir.join(file_name);

        if outcome == Outcome::Unresolved {
            // A forced fresh run that becomes unresolved must not leave a
            // previous conclusive result available for the next resume.
            if path.exists() {
                let existing = fs::read_to_string(&path).unwrap_or_default();
                let mut lines = existing.lines();
                let same_key = lines.next() == Some("whichdep-trial-v1")
                    && lines.next().and_then(hex_decode).as_deref() == Some(key.as_str());
                if same_key {
                    let _ = fs::remove_file(&path);
                }
            }
            return Ok(());
        }

        if path.exists() {
            let existing = fs::read_to_string(&path).unwrap_or_default();
            let mut lines = existing.lines();
            let valid_header = lines.next() == Some("whichdep-trial-v1");
            let stored_key = lines.next().and_then(hex_decode);
            let stored_outcome = lines.next();
            if valid_header && stored_key.as_deref() == Some(key.as_str()) {
                if stored_outcome != Some(outcome_text) {
                    // Conflicting conclusive outcomes indicate a flaky or
                    // incompletely fingerprinted oracle; invalidate rather than
                    // choosing either result.
                    let _ = fs::remove_file(&path);
                }
                return Ok(());
            }
            // Corrupt or malformed file at this path: replace it cleanly below.
            if !valid_header || stored_key.is_none() || stored_outcome.is_none() {
                let contents = format!(
                    "whichdep-trial-v1\n{}\n{outcome_text}\n",
                    hex_encode(key.as_bytes())
                );
                return write_atomic_replace(&path, contents.as_bytes());
            }
            return Ok(());
        }
        let contents = format!(
            "whichdep-trial-v1\n{}\n{outcome_text}\n",
            hex_encode(key.as_bytes())
        );
        write_atomic(&path, contents.as_bytes())
    }

    /// Persist all conclusive results in a Runner cache.
    pub fn save_cache(
        &self,
        experiment_id: &str,
        namespace: &str,
        cache: &HashMap<Vec<usize>, Outcome>,
    ) -> io::Result<()> {
        for (config, outcome) in cache {
            self.save_trial(experiment_id, namespace, config, *outcome)?;
        }
        Ok(())
    }

    /// Persist the experiment definition before any trials are executed. If
    /// the process is interrupted, the experiment and its already completed
    /// trials are still discoverable and can be resumed.
    pub fn save_experiment_definition(
        &self,
        experiment: &Experiment,
        closure_fingerprint: Option<&str>,
    ) -> io::Result<()> {
        let dir = self.experiment_dir(&experiment.id)?;
        let metadata = experiment_metadata(experiment, closure_fingerprint);
        write_atomic_replace(&dir.join("experiment.json"), metadata.as_bytes())
    }

    /// Persist the experiment definition and one immutable diagnosis snapshot.
    pub fn save_run(
        &self,
        experiment: &Experiment,
        diagnosis: &Diagnosis,
        closure_fingerprint: Option<&str>,
    ) -> io::Result<PathBuf> {
        let dir = self.experiment_dir(&experiment.id)?;
        let runs = dir.join("runs");
        create_private_dir(&runs)?;

        let metadata = experiment_metadata(experiment, closure_fingerprint);
        // Keep the latest definition for quick inspection, and an immutable
        // per-run definition below so changes to budgets/descriptions remain
        // auditable when the same experiment is resumed later.
        write_atomic_replace(&dir.join("experiment.json"), metadata.as_bytes())?;

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let run_stem = format!("run-{stamp}-{}-{sequence}", std::process::id());
        let path = runs.join(format!("{run_stem}.json"));
        write_atomic(&path, diagnosis.to_json().as_bytes())?;
        write_atomic(
            &runs.join(format!("{run_stem}.experiment.json")),
            metadata.as_bytes(),
        )?;

        // A compact append-only history index. The immutable JSON snapshots
        // remain the source of truth if an interrupted append loses this line.
        let mut index = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("history.log"))?;
        writeln!(
            index,
            "{}",
            path.file_name().unwrap_or_default().to_string_lossy()
        )?;
        index.sync_data()?;
        Ok(path)
    }

    /// Create a persistent directory for logs produced by one CLI run.
    pub fn create_run_artifacts_dir(&self, experiment_id: &str) -> io::Result<PathBuf> {
        let dir = self.experiment_dir(experiment_id)?.join("artifacts");
        create_private_dir(&dir)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let run_dir = dir.join(format!("run-{stamp}-{}-{sequence}", std::process::id()));
        create_private_dir(&run_dir)?;
        Ok(run_dir)
    }

    /// List persistent artifact directories (currently trial command logs).
    pub fn artifacts(&self, experiment_id: &str) -> io::Result<Vec<PathBuf>> {
        let artifacts = self.experiment_dir(experiment_id)?.join("artifacts");
        if !artifacts.exists() {
            return Ok(Vec::new());
        }
        let mut paths = fs::read_dir(artifacts)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect::<Vec<_>>();
        paths.sort();
        Ok(paths)
    }

    /// List immutable diagnosis snapshots for an experiment, oldest first.
    pub fn history(&self, experiment_id: &str) -> io::Result<Vec<PathBuf>> {
        let runs = self.experiment_dir(experiment_id)?.join("runs");
        if !runs.exists() {
            return Ok(Vec::new());
        }
        let mut paths = fs::read_dir(runs)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name().and_then(|s| s.to_str()).is_some_and(|name| {
                    name.starts_with("run-")
                        && name.ends_with(".json")
                        && !name.ends_with(".experiment.json")
                })
            })
            .collect::<Vec<_>>();
        paths.sort();
        Ok(paths)
    }

    /// A build-cache location outside the user's source tree. The caller
    /// should include a source/runtime fingerprint in `project_key`.
    pub fn build_cache_dir(&self, project_key: &str) -> io::Result<PathBuf> {
        let build = self.root.join("build");
        create_private_dir(&build)?;
        let dir = build.join(fingerprint::hash(project_key.as_bytes()));
        create_private_dir(&dir)?;
        Ok(dir)
    }

    /// Summarize all experiments persisted in this store.
    pub fn list_experiments(&self) -> io::Result<Vec<ExperimentSummary>> {
        let experiments_dir = self.root.join("experiments");
        if !experiments_dir.exists() {
            return Ok(Vec::new());
        }
        let mut summaries = Vec::new();
        for entry in fs::read_dir(&experiments_dir)? {
            let entry = entry?;
            if !entry.path().is_dir() {
                continue;
            }
            let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let meta_path = entry.path().join("experiment.json");
            let meta_text = fs::read_to_string(&meta_path).unwrap_or_default();
            let parsed = Json::parse(&meta_text).ok();
            let ecosystem = parsed
                .as_ref()
                .and_then(|j| j.get("ecosystem"))
                .and_then(Json::as_str)
                .unwrap_or("unknown")
                .to_string();
            let description = parsed
                .as_ref()
                .and_then(|j| j.get("description"))
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_string();
            let baseline_label = parsed
                .as_ref()
                .and_then(|j| j.get("baseline_label"))
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_string();
            let target_label = parsed
                .as_ref()
                .and_then(|j| j.get("target_label"))
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_string();
            let unit_count = parsed
                .as_ref()
                .and_then(|j| j.get("units"))
                .and_then(Json::as_arr)
                .map_or(0, <[Json]>::len);

            let run_paths = self.history(&id).unwrap_or_default();
            let latest = run_paths
                .last()
                .and_then(|p| fs::read_to_string(p).ok())
                .and_then(|s| Diagnosis::from_json(&s).ok());
            let latest_verification = latest
                .as_ref()
                .map(|d| d.verification.status.as_str().to_string());
            let latest_strategy = latest.as_ref().map(|d| d.strategy.clone());

            summaries.push(ExperimentSummary {
                id,
                ecosystem,
                description,
                baseline_label,
                target_label,
                unit_count,
                runs: run_paths.len(),
                latest_verification,
                latest_strategy,
            });
        }
        summaries.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(summaries)
    }

    /// Load all recorded diagnosis snapshots for one experiment, oldest first.
    pub fn load_diagnoses(&self, experiment_id: &str) -> io::Result<Vec<Diagnosis>> {
        let mut out = Vec::new();
        for path in self.history(experiment_id)? {
            let text = fs::read_to_string(&path)?;
            if let Ok(diag) = Diagnosis::from_json(&text) {
                out.push(diag);
            }
        }
        Ok(out)
    }

    /// Remove reusable build caches under `build/` and return reclaimed bytes.
    pub fn clean_build_cache(&self) -> io::Result<u64> {
        let build = self.root.join("build");
        if !build.exists() {
            return Ok(0);
        }
        let bytes = dir_size(&build);
        fs::remove_dir_all(&build)?;
        Ok(bytes)
    }

    /// Remove both build caches and persisted experiments, returning reclaimed bytes.
    pub fn clean_all(&self) -> io::Result<u64> {
        let mut bytes = self.clean_build_cache()?;
        let experiments = self.root.join("experiments");
        if experiments.exists() {
            bytes += dir_size(&experiments);
            fs::remove_dir_all(&experiments)?;
        }
        Ok(bytes)
    }
}

/// Summary metadata for one persisted experiment in [`ExperimentStore`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExperimentSummary {
    pub id: String,
    pub ecosystem: String,
    pub description: String,
    pub baseline_label: String,
    pub target_label: String,
    pub unit_count: usize,
    pub runs: usize,
    pub latest_verification: Option<String>,
    pub latest_strategy: Option<String>,
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            total += dir_size(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}

/// Create a store-owned directory that may contain project metadata or logs.
/// On Unix, keep these directories private even when the process umask is
/// permissive. This is filesystem access control, not command sandboxing.
fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn experiment_metadata(experiment: &Experiment, closure_fingerprint: Option<&str>) -> String {
    let units = experiment
        .units
        .iter()
        .map(|unit| {
            Json::obj([
                ("name", Json::str(unit.name.clone())),
                ("from", Json::strs(unit.from.iter().cloned())),
                ("to", Json::strs(unit.to.iter().cloned())),
            ])
        })
        .collect();
    Json::obj([
        ("schema", Json::str("whichdep-experiment-v1")),
        ("id", Json::str(experiment.id.clone())),
        ("description", Json::str(experiment.description.clone())),
        ("ecosystem", Json::str(experiment.ecosystem.clone())),
        (
            "baseline_label",
            Json::str(experiment.baseline.label.clone()),
        ),
        (
            "baseline_fingerprint",
            Json::str(experiment.baseline.fingerprint.clone()),
        ),
        ("target_label", Json::str(experiment.target.label.clone())),
        (
            "target_fingerprint",
            Json::str(experiment.target.fingerprint.clone()),
        ),
        (
            "oracle_fingerprint",
            Json::str(experiment.oracle_fingerprint.clone()),
        ),
        (
            "closure_fingerprint",
            Json::opt_str(closure_fingerprint.map(str::to_string)),
        ),
        (
            "max_trials",
            experiment.max_trials.map_or(Json::Null, Json::int),
        ),
        ("max_culprits", Json::int(experiment.max_culprits)),
        ("units", Json::Arr(units)),
    ])
    .pretty()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic-write path has no parent",
        )
    })?;
    fs::create_dir_all(parent)?;
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".tmp-{}-{id}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    if let Err(e) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
    drop(file);
    match fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(_e) if path.exists() => {
            let _ = fs::remove_file(&temp);
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&temp);
            Err(e)
        }
    }
}

fn write_atomic_replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic-write path has no parent",
        )
    })?;
    fs::create_dir_all(parent)?;
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".tmp-{}-{id}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    if let Err(e) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
    drop(file);
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path)?;
    }
    match fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&temp);
            Err(e)
        }
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode(text: &str) -> Option<String> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().as_chunks::<2>().0 {
        let digit = |b: u8| -> Option<u8> {
            match b {
                b'0'..=b'9' => Some(b - b'0'),
                b'a'..=b'f' => Some(b - b'a' + 10),
                b'A'..=b'F' => Some(b - b'A' + 10),
                _ => None,
            }
        };
        out.push((digit(pair[0])? << 4) | digit(pair[1])?);
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "whichdep-store-test-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn cache_is_namespaced_and_round_trips_conclusive_outcomes() {
        let root = temp_root();
        let store = ExperimentStore::open(&root).unwrap();
        let mut cache = HashMap::new();
        cache.insert(vec![1, 4], Outcome::Fail);
        cache.insert(vec![], Outcome::Pass);
        cache.insert(vec![9], Outcome::Unresolved);
        store.save_cache("test-exp", "namespace-A", &cache).unwrap();
        let loaded = store.load_cache("test-exp", "namespace-A").unwrap();
        assert_eq!(loaded.get(&vec![1, 4]), Some(&Outcome::Fail));
        assert_eq!(loaded.get(&vec![]), Some(&Outcome::Pass));
        assert!(!loaded.contains_key(&vec![9]));
        assert!(store
            .load_cache("test-exp", "namespace-B")
            .unwrap()
            .is_empty());
        // A fresh unresolved trial invalidates an older conclusive answer.
        store
            .save_trial("test-exp", "namespace-A", &[1, 4], Outcome::Unresolved)
            .unwrap();
        assert!(!store
            .load_cache("test-exp", "namespace-A")
            .unwrap()
            .contains_key(&vec![1, 4]));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn conflicting_conclusive_results_invalidate_the_cached_entry() {
        let root = temp_root();
        let store = ExperimentStore::open(&root).unwrap();
        store
            .save_trial("conflict", "ns", &[2, 5], Outcome::Pass)
            .unwrap();
        assert_eq!(
            store.load_cache("conflict", "ns").unwrap().get(&vec![2, 5]),
            Some(&Outcome::Pass)
        );
        store
            .save_trial("conflict", "ns", &[2, 5], Outcome::Fail)
            .unwrap();
        assert!(!store
            .load_cache("conflict", "ns")
            .unwrap()
            .contains_key(&vec![2, 5]));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_path_like_experiment_ids() {
        let root = temp_root();
        let store = ExperimentStore::open(&root).unwrap();
        assert!(store.load_cache("../escape", "x").is_err());
        let _ = fs::remove_dir_all(root);
    }
}
