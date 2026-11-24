//! Delta debugging over a set of atomic changes ("units").
//!
//! [`ddmin`] is the minimising algorithm from Zeller & Hildebrandt,
//! "Simplifying and Isolating Failure-Inducing Input" (IEEE TSE, 2002).
//! Given a baseline that passes and a configuration with every change applied
//! that fails, it returns a 1-minimal set of changes that still fails.
//!
//! Two things make it practical for lockfiles:
//!
//! * **Three-valued outcomes.** A configuration can be `Unresolved` (the
//!   environment cannot be built, the test is flaky, ...). Unresolved results
//!   never steer the search, they only force finer granularity.
//! * **Closure awareness.** Changes can require each other (a new crate that
//!   another changed crate depends on). The engine only ever proposes
//!   *closed* configurations, so it does not waste trials on lockfiles that
//!   cannot exist.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::sync::RwLock;

pub use crate::oracle::{Oracle, Outcome, Set};

/// Dependencies between changes.
///
/// `up(s)` is the smallest closed superset of `s`, `down(s)` the largest
/// closed subset. Both must be monotone and idempotent.
pub trait Closure {
    fn up(&self, s: &Set) -> Set;
    fn down(&self, s: &Set) -> Set;

    /// Top-level units in `s` that no other unit in `s` requires.
    ///
    /// Used to distinguish a genuine multi-package interaction (multiple roots)
    /// from a single root change that pulled in required companion updates
    /// through the closure.
    fn roots(&self, s: &Set) -> Vec<usize> {
        s.iter().copied().collect()
    }

    /// Stable identity for persistent trial-cache reuse. Custom closures
    /// should return `Some` only when the value fully captures their rules.
    /// The default deliberately disables persistent reuse for unknown rules.
    fn cache_fingerprint(&self) -> Option<String> {
        None
    }
}

/// Changes are independent.
pub struct NoClosure;

impl Closure for NoClosure {
    fn up(&self, s: &Set) -> Set {
        s.clone()
    }
    fn down(&self, s: &Set) -> Set {
        s.clone()
    }
    fn cache_fingerprint(&self) -> Option<String> {
        Some("no-closure-v1".to_string())
    }
}

/// "a requires b" edges plus a set of units that are part of every
/// configuration (including the baseline).
pub struct DepClosure {
    requires: RwLock<Vec<Vec<usize>>>,
    forced: Set,
}

impl DepClosure {
    pub fn new<E, F>(n: usize, requires: E, forced: F) -> Self
    where
        E: IntoIterator<Item = (usize, usize)>,
        F: IntoIterator<Item = usize>,
    {
        let mut req: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (a, b) in requires {
            assert!(a < n && b < n, "edge ({a}, {b}) out of range for {n} units");
            if a != b && !req[a].contains(&b) {
                req[a].push(b);
            }
        }
        let mut forced: Set = forced.into_iter().collect();
        assert!(forced.iter().all(|&f| f < n), "forced unit out of range");
        // Transitively close `forced` so `down()` always preserves closedness.
        let mut stack: Vec<usize> = forced.iter().copied().collect();
        while let Some(x) = stack.pop() {
            for &r in &req[x] {
                if forced.insert(r) {
                    stack.push(r);
                }
            }
        }
        DepClosure {
            requires: RwLock::new(req),
            forced,
        }
    }

    /// Dynamically add a learned dependency edge `a -> b` ("applying `a` requires `b`").
    /// Returns `true` if the edge was newly added.
    pub fn add_edge(&self, a: usize, b: usize) -> bool {
        let mut req = self.requires.write().unwrap();
        if a >= req.len() || b >= req.len() || a == b || req[a].contains(&b) {
            return false;
        }
        req[a].push(b);
        true
    }

    pub fn requires_of(&self, i: usize) -> Vec<usize> {
        self.requires.read().unwrap()[i].clone()
    }

    pub fn forced(&self) -> &Set {
        &self.forced
    }
}

impl Closure for DepClosure {
    fn cache_fingerprint(&self) -> Option<String> {
        let req = self.requires.read().unwrap();
        let mut edges = Vec::new();
        for (from, dependencies) in req.iter().enumerate() {
            for &to in dependencies {
                edges.push((from, to));
            }
        }
        edges.sort_unstable();
        let mut identity = String::from("dep-closure-v1;");
        for (from, to) in edges {
            identity.push_str(&format!("{from}>{to};"));
        }
        identity.push('|');
        for unit in &self.forced {
            identity.push_str(&format!("{unit};"));
        }
        Some(identity)
    }

    fn up(&self, s: &Set) -> Set {
        let req = self.requires.read().unwrap();
        let mut out = s.clone();
        out.extend(self.forced.iter().copied());
        let mut stack: Vec<usize> = out.iter().copied().collect();
        while let Some(x) = stack.pop() {
            for &r in &req[x] {
                if out.insert(r) {
                    stack.push(r);
                }
            }
        }
        out
    }

    fn down(&self, s: &Set) -> Set {
        let req = self.requires.read().unwrap();
        let mut out = s.clone();
        loop {
            let broken: Vec<usize> = out
                .iter()
                .copied()
                .filter(|x| !self.forced.contains(x) && req[*x].iter().any(|r| !out.contains(r)))
                .collect();
            if broken.is_empty() {
                return out;
            }
            for b in broken {
                out.remove(&b);
            }
        }
    }

    fn roots(&self, s: &Set) -> Vec<usize> {
        if s.is_empty() {
            return Vec::new();
        }
        let req = self.requires.read().unwrap();
        let required: Set = s
            .iter()
            .flat_map(|&u| req[u].iter().copied())
            .filter(|r| s.contains(r))
            .collect();
        let mut roots: Set = s
            .iter()
            .copied()
            .filter(|u| !required.contains(u))
            .collect();
        // Guard against disconnected cycles within `s` (e.g. A <-> B) that
        // have no in-degree-0 node.
        let mut reachable = roots.clone();
        let mut stack: Vec<usize> = roots.iter().copied().collect();
        while let Some(x) = stack.pop() {
            for &r in &req[x] {
                if s.contains(&r) && reachable.insert(r) {
                    stack.push(r);
                }
            }
        }
        for &u in s {
            if !reachable.contains(&u) {
                roots.insert(u);
            }
        }
        roots.into_iter().collect()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub executed: usize,
    pub cache_hits: usize,
    pub pass: usize,
    pub fail: usize,
    pub unresolved: usize,
}

#[derive(Clone, Debug)]
pub struct Trial {
    pub config: Vec<usize>,
    pub outcome: Outcome,
}

#[derive(Debug)]
pub struct Exhausted;

type TrialCallback<'a> = Box<dyn FnMut(&[usize], Outcome) -> io::Result<()> + 'a>;

/// Memoises the oracle, counts trials and enforces a trial budget.
pub struct Runner<'a, O: Oracle> {
    oracle: &'a mut O,
    cache: HashMap<Vec<usize>, Outcome>,
    budget: Option<usize>,
    pub stats: Stats,
    /// Every trial that really ran (cache hits are not logged).
    pub log: Vec<Trial>,
    on_trial: Option<TrialCallback<'a>>,
    callback_error: Option<io::Error>,
    fatal_error: Option<String>,
}

impl<'a, O: Oracle> Runner<'a, O> {
    pub fn new(oracle: &'a mut O) -> Self {
        Runner {
            oracle,
            cache: HashMap::new(),
            budget: None,
            stats: Stats::default(),
            log: Vec::new(),
            on_trial: None,
            callback_error: None,
            fatal_error: None,
        }
    }

    pub fn with_budget(mut self, max_trials: Option<usize>) -> Self {
        self.budget = max_trials;
        self
    }

    /// Pre-seed results (for example from a cache file of an earlier run).
    pub fn with_cache(mut self, cache: HashMap<Vec<usize>, Outcome>) -> Self {
        self.cache = cache;
        self
    }

    pub fn cache(&self) -> &HashMap<Vec<usize>, Outcome> {
        &self.cache
    }

    pub fn aborted(&self) -> bool {
        self.fatal_error.is_some()
    }

    pub fn fatal_error(&self) -> Option<&str> {
        self.fatal_error.as_deref()
    }

    /// Persist each newly executed trial immediately. The callback is not
    /// invoked for cache hits. Any callback error is surfaced by the owner
    /// after the search returns, without changing ddmin's search semantics.
    pub fn with_trial_callback<F>(mut self, callback: F) -> Self
    where
        F: FnMut(&[usize], Outcome) -> io::Result<()> + 'a,
    {
        self.on_trial = Some(Box::new(callback));
        self
    }

    pub fn take_callback_error(&mut self) -> Option<io::Error> {
        self.callback_error.take()
    }

    pub fn run(&mut self, s: &Set) -> Result<Outcome, Exhausted> {
        if self.fatal_error.is_some() {
            return Err(Exhausted);
        }
        if let Some(msg) = self.oracle.fatal_error().map(str::to_string) {
            self.fatal_error = Some(msg);
            return Err(Exhausted);
        }
        let key: Vec<usize> = s.iter().copied().collect();
        if let Some(&o) = self.cache.get(&key) {
            self.stats.cache_hits += 1;
            return Ok(o);
        }
        if let Some(b) = self.budget {
            if self.stats.executed >= b {
                return Err(Exhausted);
            }
        }
        let o = self.oracle.test(s);
        if let Some(msg) = self.oracle.fatal_error().map(str::to_string) {
            self.fatal_error = Some(msg);
            return Err(Exhausted);
        }
        self.stats.executed += 1;
        match o {
            Outcome::Pass => self.stats.pass += 1,
            Outcome::Fail => self.stats.fail += 1,
            Outcome::Unresolved => self.stats.unresolved += 1,
        }
        self.cache.insert(key.clone(), o);
        self.log.push(Trial {
            config: key.clone(),
            outcome: o,
        });
        if let Some(callback) = self.on_trial.as_mut() {
            if let Err(error) = callback(&key, o) {
                if self.callback_error.is_none() {
                    self.callback_error = Some(error);
                }
            }
        }
        Ok(o)
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DdError {
    /// Good and bad are identical (or everything is forced).
    NothingToBisect,
    /// The baseline fails: the failure is not caused by the changes.
    BaselineFails,
    /// The baseline could not be evaluated (environment did not build, flaky).
    BaselineUnresolved,
    /// Everything applied passes: the failure does not reproduce.
    BadPasses,
    /// Everything applied could not be evaluated.
    BadUnresolved,
    /// The trial budget ran out before the preconditions were checked.
    Exhausted,
    /// The oracle reported an unrecoverable error and aborted the search.
    Aborted,
}

impl fmt::Display for DdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            DdError::NothingToBisect => "there is nothing to bisect: good and bad do not differ",
            DdError::BaselineFails => {
                "the baseline (good state) already fails: the failure is not caused by the lockfile changes"
            }
            DdError::BaselineUnresolved => {
                "the baseline (good state) could not be evaluated: its environment did not build or its result is flaky"
            }
            DdError::BadPasses => "the bad state passes: the failure does not reproduce",
            DdError::BadUnresolved => {
                "the bad state could not be evaluated: its environment did not build or its result is flaky"
            }
            DdError::Exhausted => "trial budget exhausted before the endpoints were checked",
            DdError::Aborted => "the search aborted due to a fatal oracle error",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for DdError {}

/// Outcome of [`ddmin`].
#[derive(Debug, Clone)]
pub struct MinResult {
    /// The 1-minimal set of changes that still makes the test fail, sorted.
    /// Units that are part of the baseline (forced) are not included, units
    /// that the causes merely require are.
    pub causes: Vec<usize>,
    /// The closed configuration that was tested to fail.
    pub config: Set,
    /// The trial budget ran out, so `causes` may not be minimal yet.
    pub exhausted: bool,
}

fn split(items: &[usize], parts: usize) -> Vec<Vec<usize>> {
    let len = items.len();
    let base = len / parts;
    let rem = len % parts;
    let mut out = Vec::with_capacity(parts);
    let mut i = 0;
    for k in 0..parts {
        let size = base + usize::from(k < rem);
        out.push(items[i..i + size].to_vec());
        i += size;
    }
    out
}

/// Checks the two endpoints with one trial each: the baseline must pass and
/// everything applied must fail. Returns the closed set of all changes.
pub(crate) fn check_endpoints<O: Oracle, C: Closure>(
    n: usize,
    closure: &C,
    runner: &mut Runner<'_, O>,
) -> Result<Set, DdError> {
    let base = closure.up(&Set::new());
    let full = closure.up(&(0..n).collect());
    if full == base {
        return Err(DdError::NothingToBisect);
    }
    let map_stop = |aborted: bool| {
        if aborted {
            DdError::Aborted
        } else {
            DdError::Exhausted
        }
    };
    match runner.run(&base) {
        Ok(Outcome::Pass) => {}
        Ok(Outcome::Fail) => return Err(DdError::BaselineFails),
        Ok(Outcome::Unresolved) => return Err(DdError::BaselineUnresolved),
        Err(Exhausted) => return Err(map_stop(runner.aborted())),
    }
    match runner.run(&full) {
        Ok(Outcome::Fail) => {}
        Ok(Outcome::Pass) => return Err(DdError::BadPasses),
        Ok(Outcome::Unresolved) => return Err(DdError::BadUnresolved),
        Err(Exhausted) => return Err(map_stop(runner.aborted())),
    }
    Ok(full)
}

/// Minimising delta debugging over `n` units.
///
/// Finds a 1-minimal closed set of changes that still fails when applied to the
/// baseline: removing any single change (together with whatever then loses
/// its requirements) makes the failure disappear or the configuration
/// unresolvable. Because it minimises the *set* instead of looking for one
/// flipping change, two independent packages that only break together come back
/// as a pair, while a change with closure dependencies comes back together with
/// its required companions (distinguished via [`Cause::roots`]).
///
/// Preconditions are checked with two trials, the baseline (must pass) and
/// everything applied (must fail).
pub fn ddmin<O: Oracle, C: Closure>(
    n: usize,
    closure: &C,
    runner: &mut Runner<'_, O>,
) -> Result<MinResult, DdError> {
    let full = check_endpoints(n, closure, runner)?;
    let res = minimize(full, closure, runner);
    if runner.aborted() {
        return Err(DdError::Aborted);
    }
    Ok(res)
}

/// The reduction loop of [`ddmin`], starting from a closed configuration
/// `start` that is known to fail.
pub fn minimize<O: Oracle, C: Closure>(
    start: Set,
    closure: &C,
    runner: &mut Runner<'_, O>,
) -> MinResult {
    let base = closure.up(&Set::new());
    let mut cur = start;
    let mut granularity = 2usize;
    let mut exhausted = false;

    'outer: loop {
        let delta: Vec<usize> = cur.difference(&base).copied().collect();
        if delta.len() <= 1 {
            break;
        }
        let parts = split(&delta, granularity.min(delta.len()));

        // Reduce to subset: a single part (plus what it requires) already fails.
        for p in &parts {
            let s = closure.up(&p.iter().copied().collect());
            if s == cur {
                continue;
            }
            match runner.run(&s) {
                Err(Exhausted) => {
                    exhausted = true;
                    break 'outer;
                }
                Ok(Outcome::Fail) => {
                    cur = s;
                    granularity = 2;
                    continue 'outer;
                }
                Ok(_) => {}
            }
        }
        // Reduce to complement: dropping one part still fails.
        for p in &parts {
            let mut s = cur.clone();
            for x in p {
                s.remove(x);
            }
            let s = closure.down(&s);
            if s == base {
                continue;
            }
            match runner.run(&s) {
                Err(Exhausted) => {
                    exhausted = true;
                    break 'outer;
                }
                Ok(Outcome::Fail) => {
                    cur = s;
                    granularity = (granularity - 1).max(2);
                    continue 'outer;
                }
                Ok(_) => {}
            }
        }
        // Increase granularity, or stop when parts are already single units.
        if granularity < delta.len() {
            granularity = (granularity * 2).min(delta.len());
            continue;
        }
        break;
    }

    let causes: Vec<usize> = cur.difference(&base).copied().collect();
    MinResult {
        causes,
        config: cur,
        exhausted,
    }
}

/// How far the explanation of the failure goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Reverting the causes in the bad state makes the test pass again, so
    /// together they explain the whole failure.
    Verified,
    /// The state without the causes could not be evaluated.
    Unverified,
    /// The state without the causes still fails but the search stopped:
    /// the trial budget or the cause limit was reached.
    Incomplete,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Verified => "verified",
            Verdict::Unverified => "unverified",
            Verdict::Incomplete => "incomplete",
        }
    }
}

/// One independent explanation of the failure.
#[derive(Debug, Clone)]
pub struct Cause {
    /// A 1-minimal set of changes that fails when applied to the baseline.
    pub units: Vec<usize>,
    /// Top-level units in `units` that no other unit in `units` requires.
    ///
    /// When `roots.len() == 1` and `units.len() > 1`, the remaining units are
    /// required companions pulled in by the closure rather than an independent
    /// multi-package interaction.
    pub roots: Vec<usize>,
    /// The budget ran out while minimising, so the set may not be minimal.
    pub exhausted: bool,
}

impl Cause {
    pub fn from_units<C: Closure>(closure: &C, units: Vec<usize>, exhausted: bool) -> Self {
        let set: Set = units.iter().copied().collect();
        let roots = closure.roots(&set);
        Self {
            units,
            roots,
            exhausted,
        }
    }

    /// Units in `units` that are pulled in as required companions of `roots`.
    pub fn companions(&self) -> Vec<usize> {
        self.units
            .iter()
            .copied()
            .filter(|u| !self.roots.contains(u))
            .collect()
    }

    /// True when multiple independent root changes are needed together.
    pub fn is_interaction(&self) -> bool {
        self.roots.len() > 1
    }
}

#[derive(Debug, Clone)]
pub struct Findings {
    pub causes: Vec<Cause>,
    pub verdict: Verdict,
}

/// Finds every independent reason for the failure.
///
/// Runs [`ddmin`], then reverts what it found in the bad configuration and
/// tests that. If it passes the causes explain the failure completely;
/// if it still fails there is another, independent cause and the search
/// continues on what is left. At most `max_causes` sets are collected.
pub fn find_causes<O: Oracle, C: Closure>(
    n: usize,
    closure: &C,
    runner: &mut Runner<'_, O>,
    max_causes: usize,
) -> Result<Findings, DdError> {
    let base = closure.up(&Set::new());
    let mut remaining = check_endpoints(n, closure, runner)?;
    let mut causes: Vec<Cause> = Vec::new();
    let verdict = loop {
        let m = minimize(remaining.clone(), closure, runner);
        if runner.aborted() {
            return Err(DdError::Aborted);
        }
        let exhausted = m.exhausted;
        let mut next = remaining.clone();
        for u in &m.causes {
            next.remove(u);
        }
        causes.push(Cause::from_units(closure, m.causes, exhausted));
        if exhausted {
            break Verdict::Incomplete;
        }
        let next = closure.down(&next);
        if next == base {
            break Verdict::Verified;
        }
        match runner.run(&next) {
            Err(Exhausted) if runner.aborted() => return Err(DdError::Aborted),
            Err(Exhausted) => break Verdict::Incomplete,
            Ok(Outcome::Pass) => break Verdict::Verified,
            Ok(Outcome::Unresolved) => break Verdict::Unverified,
            Ok(Outcome::Fail) => {
                if causes.len() >= max_causes {
                    break Verdict::Incomplete;
                }
                remaining = next;
            }
        }
    };
    Ok(Findings { causes, verdict })
}
