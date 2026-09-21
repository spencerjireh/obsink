//! Continuous sync as a driver *above* the engine: it decides when to call
//! `prepare_sync` / `complete_sync` and never changes what they do. One
//! daemon per vault, one cycle at a time.
//!
//! Local edits arrive from the filesystem watcher and go through a
//! debouncer (a per-path quiet period plus a global batch window, with a
//! stat gate so a file still being written waits); remote edits are found
//! by polling the manifest ETag, quickly while there is activity and slowly
//! when idle. Fatal errors back off exponentially. Conflicts are never
//! resolved here: every one is `Defer`red, so everything else keeps syncing
//! while the conflicted paths wait for the user and are reported each cycle.
//! A single command channel carries `Sync now`, the user's resolutions and
//! `Stop`, so nothing else ever runs a cycle on a daemon-managed vault.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};

use crate::{
    api_client::ApiClient,
    crypto::{derive_keys, KeyBytes},
    hash_cache::Stat,
    pacing::{Backoff, PollPacing},
    progress::ProgressSink,
    sync_engine::{
        complete_sync, is_fatal_sync_error, prepare_sync, remote_changed, SyncEngineError,
    },
    types::{ConflictResolution, ConflictResolutionChoice, SyncPlan, SyncResult, VaultConfig},
    watcher::{spawn_watcher, WatcherError},
};

#[derive(Debug, Clone)]
pub struct DaemonOptions {
    /// A path must be untouched this long before it counts as quiet.
    pub quiet: Duration,
    /// The longest a batch of quiet paths waits for stragglers.
    pub batch: Duration,
    /// Remote poll interval while there was activity within `active_window`.
    pub poll_active: Duration,
    /// Remote poll interval when idle.
    pub poll_idle: Duration,
    pub active_window: Duration,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        DaemonOptions {
            quiet: Duration::from_millis(750),
            batch: Duration::from_secs(2),
            poll_active: Duration::from_secs(5),
            poll_idle: Duration::from_secs(60),
            active_window: Duration::from_secs(60),
            backoff_base: Duration::from_secs(5),
            backoff_max: Duration::from_secs(5 * 60),
        }
    }
}

/// What the daemon tells its host. `SyncFinished` carries the cycle's
/// result; `ConflictsPending` the plan the user has to resolve (re-sent
/// after every cycle while any conflict remains).
#[derive(Debug, Clone)]
pub enum DaemonEvent {
    Started,
    SyncStarted,
    SyncFinished(SyncResult),
    ConflictsPending {
        plan: SyncPlan,
    },
    Failed {
        message: String,
        fatal: bool,
        /// Set when a fatal error suppresses triggers for a while.
        retry_in: Option<Duration>,
    },
    Stopped,
}

type Reply = oneshot::Sender<Result<SyncResult, SyncEngineError>>;

/// Why a command did not yield a result.
#[derive(Debug, Error)]
pub enum DaemonCallError {
    #[error("daemon stopped")]
    Stopped,
    #[error(transparent)]
    Sync(#[from] SyncEngineError),
}

pub enum DaemonCommand {
    /// Run a cycle now; the reply (if any) gets its result.
    SyncNow {
        reply: Option<Reply>,
    },
    /// Apply the user's choices to the pending conflicts; anything not
    /// listed stays deferred.
    Resolve {
        resolutions: Vec<ConflictResolution>,
        reply: Reply,
    },
    Stop,
}

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error(transparent)]
    Watcher(#[from] WatcherError),
}

#[derive(Clone)]
pub struct DaemonHandle {
    tx: mpsc::Sender<DaemonCommand>,
}

impl DaemonHandle {
    /// Queue a cycle without waiting for it.
    pub fn sync_now(&self) {
        let _ = self.tx.try_send(DaemonCommand::SyncNow { reply: None });
    }

    /// Run a cycle and wait for its result.
    pub async fn sync_and_wait(&self) -> Result<SyncResult, DaemonCallError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DaemonCommand::SyncNow { reply: Some(reply) })
            .await
            .map_err(|_| DaemonCallError::Stopped)?;
        Ok(rx.await.map_err(|_| DaemonCallError::Stopped)??)
    }

    pub async fn resolve(
        &self,
        resolutions: Vec<ConflictResolution>,
    ) -> Result<SyncResult, DaemonCallError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(DaemonCommand::Resolve { resolutions, reply })
            .await
            .map_err(|_| DaemonCallError::Stopped)?;
        Ok(rx.await.map_err(|_| DaemonCallError::Stopped)??)
    }

    pub fn stop(&self) {
        let _ = self.tx.try_send(DaemonCommand::Stop);
    }
}

pub fn daemon_channel() -> (DaemonHandle, mpsc::Receiver<DaemonCommand>) {
    let (tx, rx) = mpsc::channel(16);
    (DaemonHandle { tx }, rx)
}

/// Per-path quiet timers plus a global batch window, with a stat gate: a
/// path fires only once it has been quiet for `quiet` *and* its stat pair
/// still matches the one seen at its last event (a file mid-write keeps
/// changing size); the batch window caps how long a busy path can hold the
/// others back.
#[derive(Debug)]
pub struct Debouncer {
    quiet: Duration,
    batch: Duration,
    pending: BTreeMap<String, (Instant, Option<Stat>)>,
    first_event: Option<Instant>,
}

impl Debouncer {
    pub fn new(quiet: Duration, batch: Duration) -> Self {
        Debouncer {
            quiet,
            batch,
            pending: BTreeMap::new(),
            first_event: None,
        }
    }

    pub fn observe(&mut self, path: String, stat: Option<Stat>, now: Instant) {
        self.first_event.get_or_insert(now);
        self.pending.insert(path, (now, stat));
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// When to look again.
    pub fn next_deadline(&self) -> Option<Instant> {
        let first = self.first_event?;
        let last = self.pending.values().map(|(at, _)| *at).max()?;
        Some((first + self.batch).min(last + self.quiet))
    }

    /// The paths ready to sync, if any. `stat_now` reads a path's current
    /// stat; a path whose stat moved is re-armed instead of released.
    pub fn due(
        &mut self,
        now: Instant,
        stat_now: impl Fn(&str) -> Option<Stat>,
    ) -> Option<Vec<String>> {
        let first = self.first_event?;
        if now.duration_since(first) >= self.batch {
            return Some(self.take_all());
        }
        let mut all_quiet = true;
        for (path, (at, stat)) in self.pending.iter_mut() {
            if now.duration_since(*at) < self.quiet {
                all_quiet = false;
                continue;
            }
            let current = stat_now(path);
            if current != *stat {
                *stat = current;
                *at = now;
                all_quiet = false;
            }
        }
        all_quiet.then(|| self.take_all())
    }

    fn take_all(&mut self) -> Vec<String> {
        self.first_event = None;
        std::mem::take(&mut self.pending).into_keys().collect()
    }
}

/// The remote poll interval: quick after activity, slow when idle.
pub fn poll_interval(options: &DaemonOptions, now: Instant, last_activity: Instant) -> Duration {
    PollPacing {
        active: options.poll_active,
        idle: options.poll_idle,
        active_window: options.active_window,
    }
    .interval(now.duration_since(last_activity))
}

fn stat_of(root: &Path, path: &str) -> Option<Stat> {
    fs::metadata(root.join(path))
        .ok()
        .and_then(|metadata| Stat::try_from(&metadata).ok())
}

/// Run the daemon for one vault until `Stop` (or the command channel
/// closes). Returns only after `Stopped` was sent.
pub async fn run_daemon(
    config: VaultConfig,
    key: KeyBytes,
    options: DaemonOptions,
    mut commands: mpsc::Receiver<DaemonCommand>,
    events: mpsc::Sender<DaemonEvent>,
    progress: Arc<dyn ProgressSink>,
) -> Result<(), DaemonError> {
    let root = PathBuf::from(&config.local_path);
    let ignore = config.ignore_rules();
    let keys = derive_keys(&key);
    let client = ApiClient::new(config.clone());

    let (watch_tx, mut watch_rx) = mpsc::unbounded_channel();
    let _watcher = spawn_watcher(root.clone(), ignore, watch_tx)?;
    let _ = events.send(DaemonEvent::Started).await;

    let mut debouncer = Debouncer::new(options.quiet, options.batch);
    let mut state = State {
        config,
        key,
        progress,
        events,
        backoff: Backoff::new(options.backoff_base, options.backoff_max),
        pending_plan: None,
        suppressed_until: None,
        self_writes: BTreeMap::new(),
    };
    let mut last_activity = Instant::now();
    let mut last_poll = Instant::now();

    loop {
        let now = Instant::now();
        let poll_due = last_poll + poll_interval(&options, now, last_activity);
        let mut wake = poll_due;
        if let Some(deadline) = debouncer.next_deadline() {
            wake = wake.min(deadline);
        }
        if let Some(until) = state.suppressed_until {
            wake = wake.max(until);
        }

        tokio::select! {
            command = commands.recv() => match command {
                None | Some(DaemonCommand::Stop) => break,
                Some(DaemonCommand::SyncNow { reply }) => {
                    let outcome = state.cycle(Trigger::Manual).await;
                    if outcome.is_ok() {
                        last_activity = Instant::now();
                    }
                    state.drop_self_writes(&mut watch_rx, &mut debouncer, &root);
                    if let Some(reply) = reply {
                        let _ = reply.send(outcome);
                    }
                }
                Some(DaemonCommand::Resolve { resolutions, reply }) => {
                    let outcome = state.cycle(Trigger::Resolve(resolutions)).await;
                    last_activity = Instant::now();
                    state.drop_self_writes(&mut watch_rx, &mut debouncer, &root);
                    let _ = reply.send(outcome);
                }
            },
            paths = watch_rx.recv() => {
                let Some(paths) = paths else { break };
                let now = Instant::now();
                for path in paths {
                    if state.is_self_write(&path, now) {
                        continue;
                    }
                    debouncer.observe(path.clone(), stat_of(&root, &path), now);
                }
                last_activity = now;
            }
            _ = tokio::time::sleep_until(wake) => {
                let now = Instant::now();
                if state.suppressed_until.is_some_and(|until| now < until) {
                    continue;
                }
                state.suppressed_until = None;
                if debouncer.due(now, |path| stat_of(&root, path)).is_some() {
                    if state.cycle(Trigger::Local).await.is_ok() {
                        last_activity = Instant::now();
                    }
                    state.drop_self_writes(&mut watch_rx, &mut debouncer, &root);
                    continue;
                }
                if now >= poll_due {
                    last_poll = now;
                    match remote_changed(&client, &root, &keys).await {
                        Ok(true) => {
                            if state.cycle(Trigger::Remote).await.is_ok() {
                                last_activity = Instant::now();
                            }
                            state.drop_self_writes(&mut watch_rx, &mut debouncer, &root);
                        }
                        Ok(false) => {}
                        Err(error) => {
                            let fatal = is_fatal_sync_error(&error);
                            state.report_failure(error.to_string(), fatal).await;
                        }
                    }
                }
            }
        }
    }

    let _ = state.events.send(DaemonEvent::Stopped).await;
    Ok(())
}

enum Trigger {
    Local,
    Remote,
    Manual,
    Resolve(Vec<ConflictResolution>),
}

/// Everything a cycle reads and updates.
struct State {
    config: VaultConfig,
    key: KeyBytes,
    progress: Arc<dyn ProgressSink>,
    events: mpsc::Sender<DaemonEvent>,
    backoff: Backoff,
    /// Conflicts the last cycle left for the user.
    pending_plan: Option<SyncPlan>,
    /// No trigger runs a cycle before this instant (fatal-error backoff).
    suppressed_until: Option<Instant>,
    /// Paths the last cycle wrote locally and when, so their watcher events
    /// (which FSEvents may deliver a second later) are not mistaken for
    /// edits.
    self_writes: BTreeMap<String, Instant>,
}

/// How long after a cycle its own writes are still recognised as such.
const SELF_WRITE_GRACE: Duration = Duration::from_secs(3);

impl State {
    async fn cycle(&mut self, trigger: Trigger) -> Result<SyncResult, SyncEngineError> {
        let _ = self.events.send(DaemonEvent::SyncStarted).await;
        match self.execute(trigger).await {
            Ok(result) => {
                let fatal = result
                    .failures
                    .iter()
                    .find(|failure| failure.fatal)
                    .map(|failure| failure.error.clone())
                    .or_else(|| {
                        result
                            .checkpoint_error
                            .as_ref()
                            .map(|error| format!("checkpoint: {error}"))
                    });
                match fatal {
                    Some(message) => self.report_failure(message, true).await,
                    None => self.backoff.reset(),
                }
                let written = Instant::now();
                self.self_writes = result
                    .download
                    .iter()
                    .map(|action| (action.path.clone(), written))
                    .collect();
                self.pending_plan = SyncPlan::from_late_conflicts(&result);
                if let Some(plan) = self.pending_plan.clone() {
                    let _ = self
                        .events
                        .send(DaemonEvent::ConflictsPending { plan })
                        .await;
                }
                let _ = self
                    .events
                    .send(DaemonEvent::SyncFinished(result.clone()))
                    .await;
                Ok(result)
            }
            Err(error) => {
                self.report_failure(error.to_string(), is_fatal_sync_error(&error))
                    .await;
                Err(error)
            }
        }
    }

    async fn execute(&self, trigger: Trigger) -> Result<SyncResult, SyncEngineError> {
        let progress = self.progress.as_ref();
        let (plan, resolutions) = match trigger {
            Trigger::Resolve(resolutions) => {
                // The user's choices apply to the plan the last cycle left
                // pending; conflicts they did not answer stay deferred.
                let plan = match self.pending_plan.as_ref() {
                    Some(plan) => plan.clone(),
                    None => prepare_sync(&self.config, &self.key, progress).await?,
                };
                (plan, resolutions)
            }
            _ => (
                prepare_sync(&self.config, &self.key, progress).await?,
                Vec::new(),
            ),
        };
        let resolutions = defer_the_rest(&plan, resolutions);
        complete_sync(&self.config, &self.key, &plan, &resolutions, progress).await
    }

    /// A fatal failure suppresses every trigger for the backoff's wait.
    async fn report_failure(&mut self, message: String, fatal: bool) {
        let retry_in = fatal.then(|| self.backoff.next_wait());
        if let Some(wait) = retry_in {
            self.suppressed_until = Some(Instant::now() + wait);
        }
        let _ = self
            .events
            .send(DaemonEvent::Failed {
                message,
                fatal,
                retry_in,
            })
            .await;
    }

    fn is_self_write(&self, path: &str, now: Instant) -> bool {
        self.self_writes
            .get(path)
            .is_some_and(|written| now.duration_since(*written) < SELF_WRITE_GRACE)
    }

    /// The sync's own writes (downloads, local deletes) come back through
    /// the watcher; events already queued for the paths of the cycle that
    /// just ran are dropped, and `is_self_write` keeps dropping them for a
    /// grace period. Anything else that arrived during the cycle stays
    /// pending (the "dirty" flag of the state machine).
    fn drop_self_writes(
        &mut self,
        watch_rx: &mut mpsc::UnboundedReceiver<Vec<String>>,
        debouncer: &mut Debouncer,
        root: &Path,
    ) {
        let now = Instant::now();
        while let Ok(paths) = watch_rx.try_recv() {
            for path in paths {
                if self.is_self_write(&path, now) {
                    continue;
                }
                debouncer.observe(path.clone(), stat_of(root, &path), now);
            }
        }
    }
}

/// Every conflict of `plan` without an explicit choice is deferred.
fn defer_the_rest(
    plan: &SyncPlan,
    mut resolutions: Vec<ConflictResolution>,
) -> Vec<ConflictResolution> {
    let answered: BTreeSet<&str> = resolutions
        .iter()
        .map(|resolution| resolution.path.as_str())
        .collect();
    let missing: Vec<String> = plan
        .conflicts
        .iter()
        .filter(|conflict| !answered.contains(conflict.path.as_str()))
        .map(|conflict| conflict.path.clone())
        .collect();
    resolutions.extend(missing.into_iter().map(|path| ConflictResolution {
        path,
        choice: ConflictResolutionChoice::Defer,
    }));
    resolutions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(size: u64) -> Stat {
        Stat {
            mtime_secs: 1,
            mtime_nanos: 0,
            size,
        }
    }

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn a_path_fires_after_its_quiet_period() {
        let start = Instant::now();
        let mut debouncer = Debouncer::new(ms(750), ms(2000));
        debouncer.observe("a.md".into(), Some(stat(10)), start);
        assert_eq!(debouncer.due(start + ms(500), |_| Some(stat(10))), None);
        assert_eq!(
            debouncer.due(start + ms(800), |_| Some(stat(10))),
            Some(vec!["a.md".to_string()])
        );
        assert!(debouncer.is_empty());
    }

    #[test]
    fn a_growing_file_re_arms_instead_of_firing() {
        let start = Instant::now();
        let mut debouncer = Debouncer::new(ms(750), ms(10_000));
        debouncer.observe("a.md".into(), Some(stat(10)), start);
        // Quiet long enough, but the bytes moved: still being written.
        assert_eq!(debouncer.due(start + ms(800), |_| Some(stat(20))), None);
        assert_eq!(debouncer.due(start + ms(1200), |_| Some(stat(20))), None);
        assert_eq!(
            debouncer.due(start + ms(1600), |_| Some(stat(20))),
            Some(vec!["a.md".to_string()])
        );
    }

    #[test]
    fn the_batch_window_caps_the_wait() {
        let start = Instant::now();
        let mut debouncer = Debouncer::new(ms(750), ms(2000));
        debouncer.observe("a.md".into(), Some(stat(1)), start);
        for tick in [300, 600, 900, 1200, 1500, 1800] {
            debouncer.observe("b.md".into(), Some(stat(tick)), start + ms(tick));
            assert_eq!(debouncer.due(start + ms(tick), |_| Some(stat(1))), None);
        }
        assert_eq!(
            debouncer.due(start + ms(2000), |_| Some(stat(1))),
            Some(vec!["a.md".to_string(), "b.md".to_string()])
        );
    }

    #[test]
    fn events_during_a_cycle_land_in_the_next_batch() {
        let start = Instant::now();
        let mut debouncer = Debouncer::new(ms(750), ms(2000));
        debouncer.observe("a.md".into(), None, start);
        assert!(debouncer.due(start + ms(800), |_| None).is_some());
        assert!(debouncer.is_empty());
        assert_eq!(debouncer.next_deadline(), None);
        debouncer.observe("b.md".into(), None, start + ms(900));
        assert_eq!(debouncer.next_deadline(), Some(start + ms(1650)));
        assert_eq!(
            debouncer.due(start + ms(1700), |_| None),
            Some(vec!["b.md".to_string()])
        );
    }

    #[test]
    fn the_poll_interval_slows_down_when_idle() {
        let options = DaemonOptions::default();
        let now = Instant::now();
        assert_eq!(
            poll_interval(&options, now, now - Duration::from_secs(30)),
            options.poll_active
        );
        assert_eq!(
            poll_interval(&options, now, now - Duration::from_secs(61)),
            options.poll_idle
        );
    }

    #[test]
    fn unanswered_conflicts_are_deferred() {
        use crate::types::{Conflict, FileEntry};
        let plan = SyncPlan {
            upload: vec![],
            download: vec![],
            conflicts: vec![
                Conflict {
                    path: "a.md".into(),
                    local: FileEntry::default(),
                    remote: FileEntry::default(),
                },
                Conflict {
                    path: "b.md".into(),
                    local: FileEntry::default(),
                    remote: FileEntry::default(),
                },
            ],
            failures: vec![],
        };
        let resolutions = defer_the_rest(
            &plan,
            vec![ConflictResolution {
                path: "a.md".into(),
                choice: ConflictResolutionChoice::KeepLocal,
            }],
        );
        assert_eq!(resolutions.len(), 2);
        assert_eq!(resolutions[1].path, "b.md");
        assert_eq!(resolutions[1].choice, ConflictResolutionChoice::Defer);
    }
}

#[cfg(test)]
mod integration {
    //! `cargo test -p obsink-core daemon_end_to_end -- --ignored --nocapture`:
    //! a real watcher and FSEvents latency, so it runs on request.
    use std::{sync::Arc, time::Duration};

    use httpmock::{Method::GET, Method::POST, MockServer};

    use super::*;
    use crate::{
        crypto::{content_hmac, derive_keys, encrypt_path, path_token},
        progress::NoProgress,
    };

    async fn next_event(rx: &mut mpsc::Receiver<DaemonEvent>, timeout: Duration) -> DaemonEvent {
        tokio::time::timeout(timeout, rx.recv())
            .await
            .expect("daemon event before the deadline")
            .expect("daemon events open")
    }

    #[tokio::test]
    #[ignore]
    async fn daemon_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let server = MockServer::start_async().await;
        let key = [30_u8; 32];
        let keys = derive_keys(&key);
        let token = path_token(&keys.path_token, "note.md");
        let enc_path = encrypt_path(&keys.path_enc, "note.md").unwrap();

        // httpmock answers with the first matching mock, so the conditional
        // ones come first: a poll with the v1 ETag is a 304, the checkpoint
        // re-fetch after the upload (ETag v0) sees the note, and the very
        // first fetch sees an empty vault.
        let hash = content_hmac(&keys.content_mac, b"hello");
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/vaults/vault_123/manifest")
                    .header("if-none-match", "\"v1\"");
                then.status(304);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/vaults/vault_123/manifest")
                    .header("if-none-match", "\"v0\"");
                then.status(200).header("etag", "\"v1\"").json_body_obj(&serde_json::json!({
                    token.clone(): { "hash": hash, "modified": 1, "size": 5, "deleted": false, "encPath": enc_path }
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/vaults/vault_123/manifest");
                then.status(200)
                    .header("etag", "\"v0\"")
                    .json_body_obj(&serde_json::json!({}));
            })
            .await;
        let batch = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/vaults/vault_123/batch")
                    .body_contains(format!("\"path\":\"{token}\""));
                then.status(200).json_body_obj(&serde_json::json!({
                    "results": [{ "path": token, "status": 200, "conflict": null }]
                }));
            })
            .await;

        let config = VaultConfig {
            server_url: server.base_url(),
            api_key: "token".into(),
            vault_id: "vault_123".into(),
            local_path: root.display().to_string(),
            ignore: Vec::new(),
        };
        let options = DaemonOptions {
            quiet: Duration::from_millis(200),
            batch: Duration::from_millis(600),
            poll_active: Duration::from_secs(60),
            poll_idle: Duration::from_secs(60),
            ..DaemonOptions::default()
        };
        let (handle, commands) = daemon_channel();
        let (events_tx, mut events) = mpsc::channel(32);
        let daemon = tokio::spawn(run_daemon(
            config,
            key,
            options,
            commands,
            events_tx,
            Arc::new(NoProgress),
        ));
        assert!(matches!(
            next_event(&mut events, Duration::from_secs(5)).await,
            DaemonEvent::Started
        ));
        tokio::time::sleep(Duration::from_millis(500)).await;

        // An edit: the watcher sees it, the debouncer releases it, one cycle
        // uploads it.
        std::fs::write(root.join("note.md"), "hello").unwrap();
        assert!(matches!(
            next_event(&mut events, Duration::from_secs(10)).await,
            DaemonEvent::SyncStarted
        ));
        match next_event(&mut events, Duration::from_secs(10)).await {
            DaemonEvent::SyncFinished(result) => {
                assert_eq!(result.upload.len(), 1);
                assert!(result.failures.is_empty(), "{:?}", result.failures);
            }
            other => panic!("expected SyncFinished, got {other:?}"),
        }
        batch.assert_hits_async(1).await;

        // The checkpoint write under .obsink/ must not trigger another cycle.
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), events.recv())
                .await
                .is_err(),
            "a self-write triggered a cycle"
        );

        // Sync now runs a cycle on demand and answers.
        let result = handle.sync_and_wait().await.unwrap();
        assert!(result.upload.is_empty());
        assert!(matches!(
            next_event(&mut events, Duration::from_secs(5)).await,
            DaemonEvent::SyncStarted
        ));
        assert!(matches!(
            next_event(&mut events, Duration::from_secs(5)).await,
            DaemonEvent::SyncFinished(_)
        ));

        handle.stop();
        assert!(matches!(
            next_event(&mut events, Duration::from_secs(5)).await,
            DaemonEvent::Stopped
        ));
        daemon.await.unwrap().unwrap();
    }
}
