//! `ttskit watch`: push scripts and UI to the running game whenever the
//! unpacked tree changes.
//!
//! The loop knows nothing about the file system or the game. Both sit behind
//! traits ([`Events`], [`Game`]) so tests drive it with fakes.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ttskit_editor::{ConsoleEvent, Desired, EditorError, Listener, send};

use crate::editor::{CliError, Endpoints, change_name, editor_err, write_event};

/// Default quiet time after the last file event before a push starts.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(300);

/// A source of changed paths (the file system watcher).
pub trait Events {
    /// The next changed path, or `None` when the source has ended.
    /// Must be cancel safe: dropping the future loses no event.
    fn next(&mut self) -> impl Future<Output = Option<PathBuf>>;
}

/// What one push did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PushSummary {
    /// One entry per object sent: `Name (guid): script modify, ui add`.
    pub sent: Vec<String>,
    /// Objects of the game that the source does not contain.
    pub untouched: usize,
}

/// The running game.
pub trait Game {
    /// Compare `desired` with the live scripts and send Save & Play for what
    /// changed. Nothing is sent when nothing changed.
    fn push(&mut self, desired: &[Desired]) -> impl Future<Output = Result<PushSummary, CliError>>;

    /// The next console line (print or Lua error) from the game. Messages of
    /// other kinds are skipped. Must be cancel safe between messages.
    fn console(&mut self) -> impl Future<Output = Result<ConsoleEvent, CliError>>;
}

/// What a changed path means for the push.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A script, UI or module file: pushed.
    Source,
    /// Object data, script state or anything else in the tree: not pushed.
    Data,
    /// Editor temp files, hidden directories, directories.
    Ignore,
}

fn has_ext(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Classify a changed `path` of the tree at `root` (or of an include dir).
pub fn classify(root: &Path, includes: &[PathBuf], path: &Path) -> Kind {
    if path.is_dir() {
        return Kind::Ignore;
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.ends_with('~')
        || name.starts_with(".#")
        || has_ext(path, &["tmp", "swp", "swx", "bak", "ttskit-tmp"])
    {
        return Kind::Ignore;
    }
    if includes.iter().any(|d| path.starts_with(d)) {
        return if has_ext(path, &["lua", "ttslua", "xml"]) {
            Kind::Source
        } else {
            Kind::Ignore
        };
    }
    let rel = path.strip_prefix(root).unwrap_or(path);
    let hidden = rel
        .components()
        .any(|c| c.as_os_str().to_str().is_some_and(|s| s.starts_with('.')));
    if hidden {
        return Kind::Ignore;
    }
    match name {
        "script.lua" | "ui.xml" => Kind::Source,
        _ => Kind::Data,
    }
}

/// What to build from the tree when something changed.
pub struct Built {
    pub desired: Vec<Desired>,
    /// Lines to print (for example that the packed save was written).
    pub notes: Vec<String>,
}

pub struct Settings {
    pub root: PathBuf,
    pub includes: Vec<PathBuf>,
    pub debounce: Duration,
}

fn print_push(summary: &PushSummary, out: &mut (dyn Write + Send)) -> std::io::Result<()> {
    if summary.sent.is_empty() {
        return writeln!(
            out,
            "No script or UI changes for the game; it was not reloaded."
        );
    }
    writeln!(
        out,
        "Sent Save & Play for {} objects; the game reloads.",
        summary.sent.len()
    )?;
    for line in &summary.sent {
        writeln!(out, "  {line}")?;
    }
    Ok(())
}

/// Build from the tree and push once. Errors are returned, not printed.
pub async fn push_once<G: Game>(
    game: &mut G,
    build: &mut impl FnMut() -> Result<Built, String>,
    out: &mut (dyn Write + Send),
) -> Result<(), CliError> {
    let built = build().map_err(CliError::from)?;
    let io = |e: std::io::Error| CliError::from(e.to_string());
    for note in &built.notes {
        writeln!(out, "{note}").map_err(io)?;
    }
    let summary = game.push(&built.desired).await?;
    print_push(&summary, out).map_err(io)
}

fn describe(kind_data: &[String], out: &mut (dyn Write + Send)) -> std::io::Result<()> {
    for path in kind_data {
        writeln!(
            out,
            "note: {path} changed; only scripts and UI are pushed. Object data needs a full \
             `ttskit save pack` and loading the save in the game"
        )?;
    }
    Ok(())
}

/// Watch until `stop` completes. File errors, pack errors and push errors are
/// printed to `err` and the loop keeps running; only a broken file watcher or
/// a broken listener ends it with an error.
pub async fn watch<E: Events, G: Game>(
    events: &mut E,
    game: &mut G,
    mut build: impl FnMut() -> Result<Built, String>,
    settings: &Settings,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
    stop: impl Future<Output = ()>,
) -> Result<(), CliError> {
    let io = |e: std::io::Error| CliError::from(e.to_string());
    tokio::pin!(stop);
    let mut pending: BTreeSet<PathBuf> = BTreeSet::new();
    let mut data: BTreeSet<String> = BTreeSet::new();
    let mut deadline: Option<tokio::time::Instant> = None;
    loop {
        let wait = async {
            match deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = &mut stop => return Ok(()),
            event = events.next() => {
                let Some(path) = event else {
                    return Err("the file watcher stopped unexpectedly".into());
                };
                match classify(&settings.root, &settings.includes, &path) {
                    Kind::Ignore => {}
                    kind => {
                        if kind == Kind::Data {
                            let shown = path.strip_prefix(&settings.root).unwrap_or(&path);
                            data.insert(shown.display().to_string());
                        }
                        pending.insert(path);
                        deadline = Some(tokio::time::Instant::now() + settings.debounce);
                    }
                }
            }
            () = wait => {
                deadline = None;
                pending.clear();
                let data_paths: Vec<String> = std::mem::take(&mut data).into_iter().collect();
                describe(&data_paths, out).map_err(io)?;
                if let Err(e) = push_once(game, &mut build, out).await {
                    writeln!(err, "error: {e}").map_err(io)?;
                }
                out.flush().map_err(io)?;
            }
            event = game.console() => {
                write_event(jiff::Timestamp::now(), &event?, false, out).map_err(io)?;
            }
        }
    }
}

// ---- the real game --------------------------------------------------------

/// The game behind the External Editor API.
pub struct RealGame<'a> {
    pub listener: &'a Listener,
    pub ep: Endpoints,
}

impl Game for RealGame<'_> {
    async fn push(&mut self, desired: &[Desired]) -> Result<PushSummary, CliError> {
        let live = ttskit_editor::fetch_scripts(self.listener, self.ep.game, self.ep.timeout)
            .await
            .map_err(editor_err)?;
        let plan = ttskit_editor::plan(&live, desired).map_err(|e| e.to_string())?;
        let summary = PushSummary {
            sent: plan
                .changes
                .iter()
                .map(|c| {
                    let name = if c.name.is_empty() {
                        "(not listed)"
                    } else {
                        &c.name
                    };
                    format!(
                        "{name} ({}): script {}, ui {}",
                        c.guid,
                        change_name(&c.script),
                        change_name(&c.ui)
                    )
                })
                .collect(),
            untouched: plan.untouched,
        };
        if !plan.is_empty() {
            send(self.ep.game, &plan.message(), self.ep.timeout)
                .await
                .map_err(editor_err)?;
        }
        Ok(summary)
    }

    async fn console(&mut self) -> Result<ConsoleEvent, CliError> {
        loop {
            match self.listener.next_message(self.ep.timeout).await {
                Ok(message) => {
                    if let Some(event) = ConsoleEvent::from_incoming(message) {
                        return Ok(event);
                    }
                }
                // One bad message must not end the watch.
                Err(
                    e @ (EditorError::Codec(_)
                    | EditorError::Timeout(_)
                    | EditorError::TooLarge
                    | EditorError::NotUtf8),
                ) => eprintln!("warning: skipped a message: {e}"),
                Err(e) => return Err(editor_err(e)),
            }
        }
    }
}

// ---- the real file system watcher ------------------------------------------

/// File system events from the `notify` crate.
pub struct NotifyEvents {
    rx: tokio::sync::mpsc::UnboundedReceiver<PathBuf>,
    _watcher: notify::RecommendedWatcher,
}

impl NotifyEvents {
    /// Watch every directory in `dirs` recursively.
    pub fn start(dirs: &[PathBuf]) -> Result<Self, String> {
        use notify::{EventKind, RecursiveMode, Watcher};
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut watcher =
            notify::recommended_watcher(move |event: Result<notify::Event, notify::Error>| {
                match event {
                    Ok(event) => {
                        if !matches!(event.kind, EventKind::Access(_)) {
                            for path in event.paths {
                                let _ = tx.send(path);
                            }
                        }
                    }
                    Err(e) => eprintln!("warning: file watcher: {e}"),
                }
            })
            .map_err(|e| format!("cannot start the file watcher: {e}"))?;
        for dir in dirs {
            watcher
                .watch(dir, RecursiveMode::Recursive)
                .map_err(|e| format!("cannot watch {}: {e}", dir.display()))?;
        }
        Ok(Self {
            rx,
            _watcher: watcher,
        })
    }
}

impl Events for NotifyEvents {
    async fn next(&mut self) -> Option<PathBuf> {
        self.rx.recv().await
    }
}

/// The command: `ttskit watch`.
pub struct WatchOptions {
    pub dir: PathBuf,
    pub includes: Vec<PathBuf>,
    pub once: bool,
    /// With `once`: how long to show the game's console output after the push.
    pub quiet: Duration,
    pub debounce: Duration,
}

/// Absolute path, so file events (which carry absolute paths) can be compared.
fn absolute(path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path).map_err(|e| format!("cannot resolve {}: {e}", path.display()))
}

pub fn run(
    options: WatchOptions,
    ep: Endpoints,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> Result<(), CliError> {
    let root = absolute(&options.dir)?;
    let includes = options
        .includes
        .iter()
        .map(|p| absolute(p))
        .collect::<Result<Vec<_>, _>>()?;
    let mut build = {
        let (root, includes) = (root.clone(), includes.clone());
        move || {
            crate::editor::desired_from_tree(&root, includes.clone()).map(|desired| Built {
                desired,
                notes: vec![],
            })
        }
    };
    let rt = crate::editor::runtime()?;
    rt.block_on(async {
        let io = |e: std::io::Error| CliError::from(e.to_string());
        let listener = Listener::bind(ep.editor).await.map_err(editor_err)?;
        let mut game = RealGame {
            listener: &listener,
            ep,
        };
        if options.once {
            // Fail early on a bad tree, then show the game's reaction briefly.
            push_once(&mut game, &mut build, out).await?;
            let stop = tokio::time::sleep(options.quiet);
            tokio::pin!(stop);
            loop {
                tokio::select! {
                    () = &mut stop => return Ok(()),
                    event = game.console() => {
                        write_event(jiff::Timestamp::now(), &event?, false, out).map_err(io)?;
                    }
                }
            }
        }
        let mut dirs = vec![root.clone()];
        dirs.extend(includes.iter().cloned());
        let mut events = NotifyEvents::start(&dirs)?;
        writeln!(
            out,
            "Watching {} and {} include directories. Every change to a script or UI file \
             reloads the game (Save & Play). Press Ctrl+C to stop.",
            root.display(),
            includes.len()
        )
        .map_err(io)?;
        out.flush().map_err(io)?;
        let settings = Settings {
            root,
            includes,
            debounce: options.debounce,
        };
        let stop = async {
            let _ = tokio::signal::ctrl_c().await;
        };
        watch(&mut events, &mut game, build, &settings, out, err, stop).await
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    struct FakeEvents(mpsc::UnboundedReceiver<PathBuf>);

    impl Events for FakeEvents {
        async fn next(&mut self) -> Option<PathBuf> {
            self.0.recv().await
        }
    }

    #[derive(Default)]
    struct FakeGame {
        pushes: Arc<Mutex<Vec<Vec<Desired>>>>,
        /// Results for successive pushes; the last one repeats.
        results: Vec<Result<PushSummary, String>>,
        console: Option<mpsc::UnboundedReceiver<ConsoleEvent>>,
    }

    impl Game for FakeGame {
        async fn push(&mut self, desired: &[Desired]) -> Result<PushSummary, CliError> {
            let mut pushes = self.pushes.lock().unwrap();
            let n = pushes.len();
            pushes.push(desired.to_vec());
            match self.results.get(n).or(self.results.last()) {
                Some(Ok(s)) => Ok(s.clone()),
                Some(Err(m)) => Err(m.as_str().into()),
                None => Ok(PushSummary::default()),
            }
        }

        async fn console(&mut self) -> Result<ConsoleEvent, CliError> {
            match &mut self.console {
                Some(rx) => match rx.recv().await {
                    Some(e) => Ok(e),
                    None => std::future::pending().await,
                },
                None => std::future::pending().await,
            }
        }
    }

    fn sent(name: &str) -> PushSummary {
        PushSummary {
            sent: vec![format!("{name} (a1): script modify, ui unchanged")],
            untouched: 0,
        }
    }

    fn settings() -> Settings {
        Settings {
            root: PathBuf::from("tree"),
            includes: vec![],
            debounce: Duration::from_millis(300),
        }
    }

    fn desired(script: &str) -> Vec<Desired> {
        vec![Desired {
            guid: "a1".into(),
            script: Some(script.into()),
            ui: None,
        }]
    }

    struct Harness {
        tx: mpsc::UnboundedSender<PathBuf>,
        pushes: Arc<Mutex<Vec<Vec<Desired>>>>,
        builds: Arc<Mutex<usize>>,
        out: Arc<Mutex<Vec<u8>>>,
        err: Arc<Mutex<Vec<u8>>>,
        stop: tokio::sync::oneshot::Sender<()>,
        task: tokio::task::JoinHandle<Result<(), CliError>>,
        console: Option<mpsc::UnboundedSender<ConsoleEvent>>,
    }

    struct Shared(Arc<Mutex<Vec<u8>>>);
    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// `build_results`: what each build returns, in order (the last repeats).
    fn start(
        results: Vec<Result<PushSummary, String>>,
        build_results: Vec<Result<&'static str, &'static str>>,
        with_console: bool,
    ) -> Harness {
        let (tx, rx) = mpsc::unbounded_channel();
        let (ctx, crx) = mpsc::unbounded_channel();
        let mut game = FakeGame {
            results,
            console: with_console.then_some(crx),
            ..Default::default()
        };
        let pushes = game.pushes.clone();
        let builds = Arc::new(Mutex::new(0usize));
        let out = Arc::new(Mutex::new(Vec::new()));
        let err = Arc::new(Mutex::new(Vec::new()));
        let (stop, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let (b, o, e) = (builds.clone(), out.clone(), err.clone());
        let task = tokio::spawn(async move {
            let mut events = FakeEvents(rx);
            let build = move || {
                let mut n = b.lock().unwrap();
                let r = build_results.get(*n).or(build_results.last()).unwrap();
                *n += 1;
                match r {
                    Ok(s) => Ok(Built {
                        desired: desired(s),
                        notes: vec![],
                    }),
                    Err(m) => Err((*m).to_owned()),
                }
            };
            watch(
                &mut events,
                &mut game,
                build,
                &settings(),
                &mut Shared(o),
                &mut Shared(e),
                async {
                    let _ = stop_rx.await;
                },
            )
            .await
        });
        Harness {
            tx,
            pushes,
            builds,
            out,
            err,
            stop,
            task,
            console: with_console.then_some(ctx),
        }
    }

    impl Harness {
        fn touch(&self, path: &str) {
            self.tx.send(PathBuf::from(path)).unwrap();
        }
        async fn settle(&self, ms: u64) {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
        fn pushes(&self) -> usize {
            self.pushes.lock().unwrap().len()
        }
        fn out(&self) -> String {
            String::from_utf8(self.out.lock().unwrap().clone()).unwrap()
        }
        fn err(&self) -> String {
            String::from_utf8(self.err.lock().unwrap().clone()).unwrap()
        }
        async fn finish(self) -> Result<(), CliError> {
            self.stop.send(()).unwrap();
            self.task.await.unwrap()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_of_writes_triggers_one_push() {
        let h = start(vec![Ok(sent("Board"))], vec![Ok("x")], false);
        for _ in 0..5 {
            h.touch("tree/script.lua");
            h.settle(100).await; // each write restarts the quiet period
        }
        assert_eq!(h.pushes(), 0, "still inside the debounce window");
        h.settle(400).await;
        assert_eq!(h.pushes(), 1);
        assert!(h.out().contains("Sent Save & Play for 1 objects"));
        h.finish().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn separate_changes_push_separately() {
        let h = start(vec![Ok(sent("Board"))], vec![Ok("x")], false);
        h.touch("tree/script.lua");
        h.settle(500).await;
        h.touch("tree/objects/Card.f00d/ui.xml");
        h.settle(500).await;
        assert_eq!(h.pushes(), 2);
        h.finish().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_build_error_is_printed_and_the_loop_keeps_running() {
        let h = start(
            vec![Ok(sent("Board"))],
            vec![Err("script.lua: module `util` not found"), Ok("fixed")],
            false,
        );
        h.touch("tree/script.lua");
        h.settle(500).await;
        assert_eq!(h.pushes(), 0, "nothing is sent when bundling fails");
        assert!(h.err().contains("module `util` not found"), "{}", h.err());
        h.touch("tree/script.lua");
        h.settle(500).await;
        assert_eq!(h.pushes(), 1);
        assert_eq!(
            h.pushes.lock().unwrap()[0][0].script.as_deref(),
            Some("fixed")
        );
        h.finish().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn an_unreachable_game_is_printed_and_the_loop_keeps_running() {
        let h = start(
            vec![Err("cannot connect to the game".into()), Ok(sent("Board"))],
            vec![Ok("x")],
            false,
        );
        h.touch("tree/script.lua");
        h.settle(500).await;
        assert!(h.err().contains("cannot connect to the game"));
        h.touch("tree/script.lua");
        h.settle(500).await;
        assert_eq!(h.pushes(), 2);
        assert!(h.out().contains("Sent Save & Play"));
        h.finish().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn data_changes_are_reported_as_not_pushed() {
        let h = start(vec![Ok(PushSummary::default())], vec![Ok("x")], false);
        h.touch("tree/objects/Board.f00d/object.json");
        h.settle(500).await;
        let out = h.out();
        assert!(out.contains("object.json"), "{out}");
        assert!(out.contains("full `ttskit save pack`"), "{out}");
        h.finish().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn ignored_paths_do_not_trigger_a_push() {
        let h = start(vec![Ok(sent("Board"))], vec![Ok("x")], false);
        h.touch("tree/script.lua~");
        h.touch("tree/.ttskit/unpack.json");
        h.touch("tree/.git/index");
        h.touch("tree/script.lua.swp");
        h.settle(1000).await;
        assert_eq!(h.pushes(), 0);
        assert_eq!(*h.builds.lock().unwrap(), 0);
        h.finish().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn console_output_is_shown_while_watching() {
        let h = start(vec![], vec![Ok("x")], true);
        h.console
            .as_ref()
            .unwrap()
            .send(ConsoleEvent::Print {
                message: "hello".into(),
            })
            .unwrap();
        h.settle(10).await;
        assert!(h.out().contains("print  hello"), "{}", h.out());
        h.finish().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn console_output_does_not_lose_a_pending_debounce() {
        let h = start(vec![Ok(sent("Board"))], vec![Ok("x")], true);
        h.touch("tree/script.lua");
        h.settle(100).await;
        h.console
            .as_ref()
            .unwrap()
            .send(ConsoleEvent::Print {
                message: "x".into(),
            })
            .unwrap();
        h.settle(400).await;
        assert_eq!(h.pushes(), 1);
        h.finish().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_stopped_file_watcher_is_an_error() {
        let h = start(vec![], vec![Ok("x")], false);
        drop(h.tx.clone()); // the sender is still alive: no end yet
        let Harness { tx, task, stop, .. } = h;
        drop(tx);
        let r = task.await.unwrap();
        assert!(r.unwrap_err().message.contains("file watcher"));
        drop(stop);
    }

    #[test]
    fn classification() {
        let root = Path::new("tree");
        let inc = vec![PathBuf::from("lib")];
        let k = |p: &str| classify(root, &inc, Path::new(p));
        assert_eq!(k("tree/script.lua"), Kind::Source);
        assert_eq!(k("tree/objects/A.1/ui.xml"), Kind::Source);
        assert_eq!(k("lib/util/math.lua"), Kind::Source);
        assert_eq!(k("lib/Panel.xml"), Kind::Source);
        assert_eq!(k("lib/readme.md"), Kind::Ignore);
        assert_eq!(k("tree/save.json"), Kind::Data);
        assert_eq!(k("tree/objects/A.1/state.json"), Kind::Data);
        assert_eq!(k("tree/.ttskit/unpack.json"), Kind::Ignore);
        assert_eq!(k("tree/script.lua~"), Kind::Ignore);
        assert_eq!(k("tree/4913.tmp"), Kind::Ignore);
    }

    // ---- --once and the real game / file watcher, still without the game ----

    use ttskit_editor::ScriptState;
    use ttskit_editor::fake::FakeGame as ServerFake;

    async fn real_setup(live: Vec<ScriptState>) -> (Listener, ServerFake, Endpoints) {
        let listener = Listener::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let editor = listener.local_addr().unwrap();
        let game = ServerFake::start(editor, live).await.unwrap();
        let ep = Endpoints {
            game: game.addr(),
            editor,
            timeout: Duration::from_secs(5),
        };
        (listener, game, ep)
    }

    fn live(script: &str) -> ScriptState {
        ScriptState {
            name: "Board".into(),
            guid: "a1".into(),
            script: Some(script.into()),
            ui: None,
        }
    }

    #[tokio::test]
    async fn once_pushes_a_changed_script_and_reports_it() {
        let (listener, game, ep) = real_setup(vec![live("old")]).await;
        let mut real = RealGame {
            listener: &listener,
            ep,
        };
        let mut build = || {
            Ok(Built {
                desired: desired("new"),
                notes: vec![],
            })
        };
        let mut out = Vec::new();
        push_once(&mut real, &mut build, &mut out).await.unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("Sent Save & Play for 1 objects"), "{out}");
        assert!(out.contains("Board (a1): script modify"), "{out}");
        for _ in 0..40 {
            if game.received().iter().any(|v| v["messageID"] == 1) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let sent: Vec<_> = game
            .received()
            .into_iter()
            .filter(|v| v["messageID"] == 1)
            .collect();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["scriptStates"][0]["script"], "new");
    }

    #[tokio::test]
    async fn once_with_nothing_changed_does_not_reload() {
        let (listener, game, ep) = real_setup(vec![live("same")]).await;
        let mut real = RealGame {
            listener: &listener,
            ep,
        };
        let mut build = || {
            Ok(Built {
                desired: desired("same"),
                notes: vec![],
            })
        };
        let mut out = Vec::new();
        push_once(&mut real, &mut build, &mut out).await.unwrap();
        assert!(String::from_utf8(out).unwrap().contains("not reloaded"));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(game.received().iter().all(|v| v["messageID"] != 1));
    }

    #[tokio::test]
    async fn once_fails_on_a_bad_tree_without_touching_the_game() {
        let (listener, game, ep) = real_setup(vec![live("x")]).await;
        let mut real = RealGame {
            listener: &listener,
            ep,
        };
        let mut build = || Err("script.lua: module `u` not found".to_owned());
        let e = push_once(&mut real, &mut build, &mut Vec::new())
            .await
            .unwrap_err();
        assert!(e.message.contains("module `u`"));
        assert!(!e.game_unreachable);
        assert!(game.received().is_empty());
    }

    #[tokio::test]
    async fn once_reports_an_unreachable_game_as_such() {
        let listener = Listener::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        // A port nothing listens on: bind and drop.
        let dead = {
            let l = Listener::bind("127.0.0.1:0".parse().unwrap())
                .await
                .unwrap();
            l.local_addr().unwrap()
        };
        let ep = Endpoints {
            game: dead,
            editor: listener.local_addr().unwrap(),
            timeout: Duration::from_secs(10),
        };
        let mut real = RealGame {
            listener: &listener,
            ep,
        };
        let mut build = || {
            Ok(Built {
                desired: desired("x"),
                notes: vec![],
            })
        };
        let e = push_once(&mut real, &mut build, &mut Vec::new())
            .await
            .unwrap_err();
        assert!(e.game_unreachable, "{e}");
    }

    #[tokio::test]
    async fn the_real_file_watcher_reports_a_written_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let mut events = NotifyEvents::start(std::slice::from_ref(&root)).unwrap();
        let file = root.join("script.lua");
        std::fs::write(&file, "x").unwrap();
        let got = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(p) = events.next().await {
                if p == file {
                    return true;
                }
            }
            false
        })
        .await;
        assert_eq!(got, Ok(true));
    }
}
