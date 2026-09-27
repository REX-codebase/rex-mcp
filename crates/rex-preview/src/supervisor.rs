use crate::{
    browser::BrowserRuntime, detect_project, BrowserAction, Framework, IterationReceipt,
    LaunchPlan, PreviewError, PreviewRecipe, PreviewSession, ProductionReport, SessionState,
    PORT_MAX, PORT_MIN,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_LOG_BYTES: usize = 256 * 1024;
const READINESS_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleKind {
    Starting,
    Stdout,
    Stderr,
    Ready,
    Exited,
    Cancelled,
    TimedOut,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LifecycleEvent {
    pub sequence: u64,
    pub at_ms: u128,
    pub kind: LifecycleKind,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SupervisorSummary {
    pub id: String,
    pub state: SessionState,
    pub framework: Framework,
    pub url: String,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub events: Vec<LifecycleEvent>,
}

#[derive(Default)]
struct BoundedLog {
    lines: VecDeque<String>,
    bytes: usize,
}
impl BoundedLog {
    fn push(&mut self, mut line: String) {
        if line.len() > 16 * 1024 {
            line.truncate(16 * 1024);
        }
        self.bytes += line.len();
        self.lines.push_back(line);
        while self.bytes > MAX_LOG_BYTES {
            if let Some(v) = self.lines.pop_front() {
                self.bytes = self.bytes.saturating_sub(v.len())
            } else {
                break;
            }
        }
    }
    fn text(&self) -> String {
        self.lines.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

struct Runtime {
    id: String,
    framework: Framework,
    url: String,
    state: SessionState,
    child: Option<Child>,
    pid: Option<u32>,
    exit_code: Option<i32>,
    stdout: Arc<Mutex<BoundedLog>>,
    stderr: Arc<Mutex<BoundedLog>>,
    events: Arc<Mutex<VecDeque<LifecycleEvent>>>,
    stop: Arc<AtomicBool>,
    static_thread: Option<thread::JoinHandle<()>>,
    browser: Option<BrowserRuntime>,
    project_root: PathBuf,
    iteration: PreviewSession,
}

impl Runtime {
    fn event(&self, kind: LifecycleKind, detail: impl Into<String>) {
        let mut events = self.events.lock().unwrap();
        let sequence = events.back().map_or(1, |e| e.sequence + 1);
        events.push_back(LifecycleEvent {
            sequence,
            at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            kind,
            detail: detail.into(),
        });
        while events.len() > 512 {
            events.pop_front();
        }
    }
    fn summary(&mut self) -> SupervisorSummary {
        self.refresh_exit();
        SupervisorSummary {
            id: self.id.clone(),
            state: self.state.clone(),
            framework: self.framework,
            url: self.url.clone(),
            pid: self.pid,
            exit_code: self.exit_code,
            stdout: self.stdout.lock().unwrap().text(),
            stderr: self.stderr.lock().unwrap().text(),
            events: self.events.lock().unwrap().iter().cloned().collect(),
        }
    }
    fn refresh_exit(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if let Ok(Some(status)) = child.try_wait() {
                self.exit_code = status.code();
                self.state = SessionState::Failed;
                self.event(LifecycleKind::Exited, format!("preview exited: {status}"));
                self.child = None;
            }
        }
    }
    fn terminate(&mut self, kind: LifecycleKind) {
        self.stop.store(true, Ordering::SeqCst);
        self.browser.take();
        if let Some(mut child) = self.child.take() {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGTERM);
            }
            #[cfg(not(unix))]
            {
                let _ = child.kill();
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match child.try_wait() {
                    Ok(Some(s)) => {
                        self.exit_code = s.code();
                        break;
                    }
                    _ if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
                    _ => {
                        #[cfg(unix)]
                        unsafe {
                            libc::kill(-(child.id() as i32), libc::SIGKILL);
                        };
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }
        if let Some(handle) = self.static_thread.take() {
            let _ =
                TcpStream::connect(self.url.trim_start_matches("http://").trim_end_matches('/'));
            let _ = handle.join();
        }
        self.state = match kind {
            LifecycleKind::TimedOut => SessionState::TimedOut,
            LifecycleKind::Cancelled => SessionState::Cancelled,
            _ => SessionState::Failed,
        };
        self.event(kind, "preview process tree cleaned up");
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        if !self.stop.load(Ordering::SeqCst) {
            self.terminate(LifecycleKind::Cancelled)
        }
    }
}

#[derive(Clone)]
pub struct PreviewSupervisor {
    workspace: PathBuf,
    sessions: Arc<Mutex<HashMap<String, Runtime>>>,
}
impl PreviewSupervisor {
    pub fn new(workspace: impl AsRef<Path>) -> Result<Self, PreviewError> {
        let workspace = workspace.as_ref();
        fs::create_dir_all(workspace).map_err(|_| PreviewError::PathEscape)?;
        let workspace = workspace
            .canonicalize()
            .map_err(|_| PreviewError::PathEscape)?;
        Ok(Self {
            workspace,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        })
    }
    pub fn detect(&self, project: &Path) -> Result<PreviewRecipe, PreviewError> {
        let root = self.project_root(project)?;
        let mut files = BTreeMap::new();
        for name in [
            "package.json",
            "index.html",
            "next.config.js",
            "next.config.mjs",
            "next.config.ts",
            "vite.config.ts",
            "vite.config.js",
        ] {
            let p = root.join(name);
            if let Ok(m) = fs::metadata(&p) {
                if m.is_file() && m.len() <= MAX_MANIFEST_BYTES {
                    if let Ok(v) = fs::read_to_string(&p) {
                        files.insert(name.into(), v);
                    }
                }
            }
        }
        let mut recipe = detect_project(&self.workspace, project, &files)?;
        recipe.project_dir = root;
        Ok(recipe)
    }
    fn project_root(&self, project: &Path) -> Result<PathBuf, PreviewError> {
        crate::validate_project_path(&self.workspace, project)?;
        let raw = self.workspace.join(project);
        let canonical = raw.canonicalize().map_err(|_| PreviewError::PathEscape)?;
        if !canonical.starts_with(&self.workspace) {
            return Err(PreviewError::PathEscape);
        }
        // Canonical equality also rejects a symlinked project or ancestor escaping/aliasing the selected tree.
        if canonical != raw {
            return Err(PreviewError::SymlinkComponent);
        }
        Ok(canonical)
    }
    pub fn start(&self, project: &Path) -> Result<SupervisorSummary, PreviewError> {
        let recipe = self.detect(project)?;
        let (listener, port) = reserve_port()?;
        let id = format!(
            "preview-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        );
        let plan = LaunchPlan::from_recipe(&recipe, port)?;
        let url = plan.url(&recipe.readiness_path)?.to_string();
        let mut runtime = Runtime {
            id: id.clone(),
            framework: recipe.framework,
            url: url.clone(),
            state: SessionState::Starting,
            child: None,
            pid: None,
            exit_code: None,
            stdout: Default::default(),
            stderr: Default::default(),
            events: Default::default(),
            stop: Arc::new(AtomicBool::new(false)),
            static_thread: None,
            browser: None,
            project_root: plan.cwd.clone(),
            iteration: PreviewSession::new(id.clone(), plan.clone(), SystemTime::now()),
        };
        runtime.iteration.start()?;
        runtime.event(
            LifecycleKind::Starting,
            format!("{} {:?}", plan.bind, recipe.framework),
        );
        if recipe.framework == Framework::StaticHtml {
            runtime.static_thread = Some(spawn_static(
                listener,
                plan.cwd.clone(),
                runtime.stop.clone(),
                runtime.stderr.clone(),
            ));
        } else {
            drop(listener);
            let child = spawn_child(&plan, runtime.stdout.clone(), runtime.stderr.clone())?;
            runtime.pid = Some(child.id());
            runtime.child = Some(child);
        }
        let deadline = Instant::now() + Duration::from_millis(plan.startup_timeout_ms);
        loop {
            runtime.refresh_exit();
            if runtime.child.is_none() && recipe.framework != Framework::StaticHtml {
                runtime.terminate(LifecycleKind::Failed);
                return Err(PreviewError::NotRunning);
            }
            if ready(plan.bind, &recipe.readiness_path) {
                runtime.state = SessionState::Running;
                runtime.iteration.ready()?;
                runtime.event(LifecycleKind::Ready, url.clone());
                break;
            }
            if Instant::now() >= deadline {
                runtime.terminate(LifecycleKind::TimedOut);
                return Err(PreviewError::NotRunning);
            }
            thread::sleep(READINESS_INTERVAL);
        }
        let summary = runtime.summary();
        self.sessions.lock().unwrap().insert(id, runtime);
        Ok(summary)
    }
    pub fn summary(&self, id: &str) -> Result<SupervisorSummary, PreviewError> {
        let mut s = self.sessions.lock().unwrap();
        s.get_mut(id)
            .map(Runtime::summary)
            .ok_or(PreviewError::NotRunning)
    }
    pub fn action(&self, id: &str, action: &BrowserAction) -> Result<(), PreviewError> {
        action.validate()?;
        let mut s = self.sessions.lock().unwrap();
        let r = s.get_mut(id).ok_or(PreviewError::NotRunning)?;
        r.refresh_exit();
        if r.state != SessionState::Running {
            return Err(PreviewError::NotRunning);
        };
        if r.browser.is_none() {
            r.browser = Some(BrowserRuntime::launch(&r.url)?);
        }
        r.browser
            .as_mut()
            .ok_or(PreviewError::NotRunning)?
            .action(action, &r.url)
    }
    pub fn capture(&self, id: &str) -> Result<crate::BrowserEvidence, PreviewError> {
        let mut s = self.sessions.lock().unwrap();
        let r = s.get_mut(id).ok_or(PreviewError::NotRunning)?;
        if r.browser.is_none() {
            r.browser = Some(BrowserRuntime::launch(&r.url)?);
        }
        let before = source_tree_sha256(&r.project_root, r.framework == Framework::StaticHtml)?;
        let mut evidence = r
            .browser
            .as_mut()
            .ok_or(PreviewError::NotRunning)?
            .capture(&r.url)?;
        if before != source_tree_sha256(&r.project_root, r.framework == Framework::StaticHtml)? {
            return Err(PreviewError::SourceChanged);
        }
        evidence.source_sha256 = before;
        for item in evidence.items.iter().cloned() {
            r.iteration.push_evidence(item)?;
        }
        Ok(evidence)
    }
    pub fn begin_iteration(&self, id: &str) -> Result<u8, PreviewError> {
        let mut sessions = self.sessions.lock().unwrap();
        sessions
            .get_mut(id)
            .ok_or(PreviewError::NotRunning)?
            .iteration
            .begin_iteration()
    }
    pub fn record_iteration(
        &self,
        id: &str,
        receipt: IterationReceipt,
    ) -> Result<(), PreviewError> {
        let mut sessions = self.sessions.lock().unwrap();
        sessions
            .get_mut(id)
            .ok_or(PreviewError::NotRunning)?
            .iteration
            .record_iteration(receipt)
    }
    pub fn production_report(&self, id: &str) -> Result<ProductionReport, PreviewError> {
        let sessions = self.sessions.lock().unwrap();
        let runtime = sessions.get(id).ok_or(PreviewError::NotRunning)?;
        let mut gates = BTreeMap::new();
        for item in &runtime.iteration.evidence {
            if let crate::Evidence::Gate { name, passed, .. } = item {
                gates.insert(*name, *passed);
            }
        }
        Ok(ProductionReport {
            gates,
            receipts: runtime.iteration.receipts.clone(),
        })
    }
    pub fn finish(&self, id: &str) -> Result<(), PreviewError> {
        let mut sessions = self.sessions.lock().unwrap();
        let runtime = sessions.get_mut(id).ok_or(PreviewError::NotRunning)?;
        let mut gates = BTreeMap::new();
        for item in &runtime.iteration.evidence {
            if let crate::Evidence::Gate { name, passed, .. } = item {
                gates.insert(*name, *passed);
            }
        }
        runtime.iteration.finish(&ProductionReport {
            gates,
            receipts: runtime.iteration.receipts.clone(),
        })
    }
    pub fn cancel(&self, id: &str) -> Result<SupervisorSummary, PreviewError> {
        let mut s = self.sessions.lock().unwrap();
        let r = s.get_mut(id).ok_or(PreviewError::NotRunning)?;
        r.iteration.cancel();
        r.terminate(LifecycleKind::Cancelled);
        Ok(r.summary())
    }
    pub fn teardown(&self, id: &str) -> Result<(), PreviewError> {
        let mut s = self.sessions.lock().unwrap();
        let mut r = s.remove(id).ok_or(PreviewError::NotRunning)?;
        if !r.stop.load(Ordering::SeqCst) {
            r.terminate(LifecycleKind::Cancelled)
        };
        Ok(())
    }
}

/// Hash bounded, regular source files in canonical relative-path order.
/// This is a local snapshot check, not proof of an external host or remote build.
fn source_tree_sha256(root: &Path, static_html: bool) -> Result<String, PreviewError> {
    fn visit(
        root: &Path,
        dir: &Path,
        files: &mut Vec<PathBuf>,
        static_html: bool,
    ) -> Result<(), PreviewError> {
        for entry in fs::read_dir(dir).map_err(|_| PreviewError::SourceChanged)? {
            let entry = entry.map_err(|_| PreviewError::SourceChanged)?;
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .map_err(|_| PreviewError::PathEscape)?;
            // A static preview serves its project tree directly, including
            // dist/build. Those are excluded for framework dev servers to
            // avoid hashing volatile generated output and dependencies.
            if rel.components().any(|c| {
                matches!(
                    c.as_os_str().to_str(),
                    Some("node_modules" | ".git" | "target" | ".next")
                ) || (!static_html && matches!(c.as_os_str().to_str(), Some("dist" | "build")))
            }) {
                continue;
            }
            let meta = fs::symlink_metadata(&path).map_err(|_| PreviewError::SourceChanged)?;
            if meta.file_type().is_symlink() {
                return Err(PreviewError::SymlinkComponent);
            }
            if meta.is_dir() {
                visit(root, &path, files, static_html)?;
            } else if meta.is_file() {
                files.push(path);
                if files.len() > 4096 {
                    return Err(PreviewError::EvidenceLimit);
                }
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, root, &mut files, static_html)?;
    files.sort();
    let mut hash = Sha256::new();
    for file in files {
        let rel = file
            .strip_prefix(root)
            .map_err(|_| PreviewError::PathEscape)?;
        let content = fs::read(&file).map_err(|_| PreviewError::SourceChanged)?;
        if content.len() > 16 * 1024 * 1024 {
            return Err(PreviewError::EvidenceLimit);
        }
        hash.update((rel.as_os_str().as_encoded_bytes().len() as u64).to_le_bytes());
        hash.update(rel.as_os_str().as_encoded_bytes());
        hash.update((content.len() as u64).to_le_bytes());
        hash.update(&content);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn reserve_port() -> Result<(TcpListener, u16), PreviewError> {
    for port in PORT_MIN..=PORT_MAX {
        if let Ok(l) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
            l.set_nonblocking(true).ok();
            return Ok((l, port));
        }
    }
    Err(PreviewError::PortDenied)
}
fn spawn_child(
    plan: &LaunchPlan,
    out: Arc<Mutex<BoundedLog>>,
    err: Arc<Mutex<BoundedLog>>,
) -> Result<Child, PreviewError> {
    let mut cmd = Command::new(&plan.program);
    cmd.args(&plan.args)
        .current_dir(&plan.cwd)
        .env_clear()
        .env(
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
        )
        .envs(&plan.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let nofile = libc::rlimit {
                    rlim_cur: 256,
                    rlim_max: 256,
                };
                libc::setrlimit(libc::RLIMIT_NOFILE, &nofile);
                let cpu = libc::rlimit {
                    rlim_cur: 300,
                    rlim_max: 300,
                };
                libc::setrlimit(libc::RLIMIT_CPU, &cpu);
                Ok(())
            });
        }
    }
    let mut child = cmd.spawn().map_err(|_| PreviewError::CommandDenied)?;
    if let Some(v) = child.stdout.take() {
        pipe(v, out)
    }
    if let Some(v) = child.stderr.take() {
        pipe(v, err)
    }
    Ok(child)
}
fn pipe<R: Read + Send + 'static>(r: R, log: Arc<Mutex<BoundedLog>>) {
    thread::spawn(move || {
        for line in BufReader::new(r).lines().map_while(Result::ok) {
            log.lock().unwrap().push(line)
        }
    });
}
fn ready(addr: SocketAddr, path: &str) -> bool {
    if let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(250)) {
        let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = write!(s, "GET {} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n", path);
        let mut b = [0; 64];
        if let Ok(n) = s.read(&mut b) {
            return n > 12 && b.starts_with(b"HTTP/1.");
        }
    }
    false
}
fn spawn_static(
    listener: TcpListener,
    root: PathBuf,
    stop: Arc<AtomicBool>,
    errors: Arc<Mutex<BoundedLog>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut line = String::new();
                    if let Ok(clone) = stream.try_clone() {
                        let _ = BufReader::new(clone).read_line(&mut line);
                    }
                    let path = line
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("/")
                        .split('?')
                        .next()
                        .unwrap_or("/");
                    let rel = path.trim_start_matches('/');
                    let candidate = if rel.is_empty() {
                        root.join("index.html")
                    } else {
                        root.join(rel)
                    };
                    let canonical = candidate.canonicalize().ok();
                    // These paths are excluded from the capture source hash.
                    // A static preview must not serve untracked bytes as if
                    // they belonged to the attested project snapshot.
                    let excluded = Path::new(rel).components().any(|c| {
                        matches!(
                            c.as_os_str().to_str(),
                            Some("node_modules" | ".git" | "target" | ".next")
                        )
                    });
                    if let Some(file) =
                        canonical.filter(|p| !excluded && p.starts_with(&root) && p.is_file())
                    {
                        let data = fs::read(file).unwrap_or_default();
                        let _ = write!(
                            stream,
                            "HTTP/1.0 200 OK\r\nContent-Length: {}\r\nContent-Type: {}\r\nX-Content-Type-Options: nosniff\r\n\r\n",
                            data.len(),
                            mime(&candidate)
                        );
                        let _ = stream.write_all(&data);
                    } else {
                        let body = b"not found";
                        let _ = write!(
                            stream,
                            "HTTP/1.0 404 Not Found\r\nContent-Length: {}\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(body);
                    }
                    let _ = stream.shutdown(Shutdown::Both);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => {
                    errors.lock().unwrap().push(error.to_string());
                    break;
                }
            }
        }
    })
}
fn mime(p: &Path) -> &'static str {
    match p.extension().and_then(|v| v.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    // Several tests bind from the shared preview port range; serialize them
    // so a concurrently running test cannot occupy a port another test
    // expects to bind or skip.
    static PORT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn temp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("rex-preview-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }
    #[test]
    fn static_preview_refuses_unhashed_resource_paths() {
        let _port_guard = PORT_LOCK.lock().unwrap();
        let w = temp("unhashed-resource");
        let app = w.join("app");
        fs::create_dir(&app).unwrap();
        fs::write(app.join("index.html"), "<h1>visible</h1>").unwrap();
        for dirname in ["node_modules", ".next", "target", ".git"] {
            fs::create_dir(app.join(dirname)).unwrap();
            fs::write(app.join(dirname).join("asset.css"), "body{color:red}").unwrap();
        }
        let sup = PreviewSupervisor::new(&w).unwrap();
        let started = sup.start(Path::new("app")).unwrap();
        let addr = started
            .url
            .trim_start_matches("http://")
            .trim_end_matches('/');
        for dirname in ["node_modules", ".next", "target", ".git"] {
            let mut stream = TcpStream::connect(addr).unwrap();
            write!(stream, "GET /{dirname}/asset.css HTTP/1.0\r\n\r\n").unwrap();
            let mut body = String::new();
            stream.read_to_string(&mut body).unwrap();
            assert!(body.starts_with("HTTP/1.0 404"), "{dirname}: {body}");
        }
        sup.cancel(&started.id).unwrap();
        let _ = fs::remove_dir_all(w);
    }

    #[test]
    fn static_preview_hash_includes_served_generated_assets() {
        let w = temp("static-asset-hash");
        let app = w.join("app");
        fs::create_dir(&app).unwrap();
        fs::create_dir(app.join("dist")).unwrap();
        fs::write(
            app.join("index.html"),
            "<link rel='stylesheet' href='dist/theme.css'>",
        )
        .unwrap();
        fs::write(app.join("dist/theme.css"), "body{color:#123}").unwrap();
        let old = source_tree_sha256(&app, true).unwrap();
        let framework_old = source_tree_sha256(&app, false).unwrap();
        fs::write(app.join("dist/theme.css"), "body{color:#456}").unwrap();
        assert_ne!(old, source_tree_sha256(&app, true).unwrap());
        assert_eq!(framework_old, source_tree_sha256(&app, false).unwrap());
        let _ = fs::remove_dir_all(w);
    }

    #[test]
    fn static_preview_is_real_and_cancel_cleans_listener() {
        let _port_guard = PORT_LOCK.lock().unwrap();
        let w = temp("static");
        let app = w.join("app");
        fs::create_dir(&app).unwrap();
        fs::write(app.join("index.html"), "<h1>real</h1>").unwrap();
        let sup = PreviewSupervisor::new(&w).unwrap();
        let started = sup.start(Path::new("app")).unwrap();
        assert_eq!(started.state, SessionState::Running);
        let addr = started
            .url
            .trim_start_matches("http://")
            .trim_end_matches('/');
        let mut s = TcpStream::connect(addr).unwrap();
        write!(s, "GET / HTTP/1.0\r\n\r\n").unwrap();
        let mut body = String::new();
        s.read_to_string(&mut body).unwrap();
        assert!(body.contains("<h1>real</h1>"));
        let ended = sup.cancel(&started.id).unwrap();
        assert_eq!(ended.state, SessionState::Cancelled);
        assert!(ended
            .events
            .iter()
            .any(|e| matches!(e.kind, LifecycleKind::Cancelled)));
        let _ = fs::remove_dir_all(w);
    }
    #[test]
    fn symlinked_project_is_rejected() {
        let w = temp("link");
        let real = temp("outside");
        fs::write(real.join("index.html"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, w.join("app")).unwrap();
        let sup = PreviewSupervisor::new(&w).unwrap();
        #[cfg(unix)]
        assert_eq!(
            sup.detect(Path::new("app")).unwrap_err(),
            PreviewError::PathEscape
        );
        let _ = fs::remove_dir_all(w);
        let _ = fs::remove_dir_all(real);
    }
    #[test]
    fn local_external_script_runs_but_other_loopback_port_is_blocked() {
        let _port_guard = PORT_LOCK.lock().unwrap();
        let w = temp("local-script");
        let app = w.join("app");
        fs::create_dir(&app).unwrap();
        let other = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        other.set_nonblocking(true).unwrap();
        let other_port = other.local_addr().unwrap().port();
        fs::write(app.join("index.html"), format!(
            "<!doctype html><style>body{{background:linear-gradient(45deg,#123,#789)}}</style><main id='scene'>Waiting for scene</main><script src='/scene.js'></script><script src='http://127.0.0.1:{other_port}/foreign.js'></script>"
        )).unwrap();
        fs::write(
            app.join("scene.js"),
            "document.querySelector('#scene').textContent='Local scene executed';",
        )
        .unwrap();
        let sup = PreviewSupervisor::new(&w).unwrap();
        let started = sup.start(Path::new("app")).unwrap();
        sup.begin_iteration(&started.id).unwrap();
        let evidence = sup.capture(&started.id).unwrap();
        assert!(evidence.accessibility_text.contains("Local scene executed"));
        assert!(matches!(other.accept(), Err(ref e) if e.kind()==std::io::ErrorKind::WouldBlock));
        sup.cancel(&started.id).unwrap();
        let _ = fs::remove_dir_all(w);
    }
    #[test]
    fn javascript_off_is_fresh_and_probe_verified() {
        let _port_guard = PORT_LOCK.lock().unwrap();
        let w = temp("javascript-off");
        let app = w.join("app");
        fs::create_dir(&app).unwrap();
        fs::write(app.join("index.html"), r#"<!doctype html><style>body{background:linear-gradient(#123,#678)}</style><main id='scene'>Script-off fallback</main><script>document.querySelector('#scene').textContent='Script ran'</script>"#).unwrap();
        let sup = PreviewSupervisor::new(&w).unwrap();
        let started = sup.start(Path::new("app")).unwrap();
        sup.begin_iteration(&started.id).unwrap();
        assert!(sup
            .capture(&started.id)
            .unwrap()
            .dom_text
            .contains("Script ran"));
        sup.action(
            &started.id,
            &BrowserAction::SetJavaScript { enabled: false },
        )
        .unwrap();
        let off = sup.capture(&started.id).unwrap();
        assert!(off.dom_text.contains("Script-off fallback"));
        assert!(!off.accessibility_text.contains("Script ran"));
        assert_eq!(
            sup.action(&started.id, &BrowserAction::SetJavaScript { enabled: true })
                .unwrap_err(),
            PreviewError::ConditionNotApplied
        );
        sup.cancel(&started.id).unwrap();
        let _ = fs::remove_dir_all(w);
    }
    #[test]
    fn mobile_script_off_and_reduced_motion_are_real_capture_states() {
        let _port_guard = PORT_LOCK.lock().unwrap();
        let w = temp("mobile-conditions");
        let app = w.join("app");
        fs::create_dir(&app).unwrap();
        fs::write(app.join("index.html"), r#"<!doctype html><style>body{background:linear-gradient(#123,#678)}@media(prefers-reduced-motion:reduce){body{background:linear-gradient(#3475e3,#e8b875)}}</style><main id='scene'>Script-off fallback</main><script>document.querySelector('#scene').textContent='Script ran'</script>"#).unwrap();
        let sup = PreviewSupervisor::new(&w).unwrap();
        let started = sup.start(Path::new("app")).unwrap();
        sup.begin_iteration(&started.id).unwrap();
        sup.action(
            &started.id,
            &BrowserAction::SetViewport {
                width: 390,
                height: 650,
                scale: 1.0,
            },
        )
        .unwrap();
        let normal = sup.capture(&started.id).unwrap();
        assert!(normal.dom_text.contains("Script ran"));
        assert!(normal.items.iter().any(|item| matches!(
            item,
            crate::Evidence::Viewport {
                width: 390,
                height: 650,
                ..
            }
        )));
        sup.action(
            &started.id,
            &BrowserAction::SetReducedMotion { enabled: true },
        )
        .unwrap();
        let reduced = sup.capture(&started.id).unwrap();
        assert_ne!(normal.screenshot_data_url, reduced.screenshot_data_url);
        sup.action(
            &started.id,
            &BrowserAction::SetJavaScript { enabled: false },
        )
        .unwrap();
        let off = sup.capture(&started.id).unwrap();
        assert!(off.dom_text.contains("Script-off fallback"));
        assert!(!off.accessibility_text.contains("Script ran"));
        assert_eq!(off.source_sha256, normal.source_sha256);
        sup.cancel(&started.id).unwrap();
        let _ = fs::remove_dir_all(w);
    }

    #[test]
    fn missing_local_scene_asset_cannot_be_capture_evidence() {
        let _port_guard = PORT_LOCK.lock().unwrap();
        let w = temp("missing-scene");
        let app = w.join("app");
        fs::create_dir(&app).unwrap();
        fs::write(app.join("index.html"),"<!doctype html><style>body{background:linear-gradient(#123,#456)}</style><main>Scene not loaded</main><script src='/missing-scene.js'></script>").unwrap();
        let sup = PreviewSupervisor::new(&w).unwrap();
        let started = sup.start(Path::new("app")).unwrap();
        sup.begin_iteration(&started.id).unwrap();
        assert_eq!(
            sup.capture(&started.id).unwrap_err(),
            PreviewError::BrokenPage
        );
        sup.cancel(&started.id).unwrap();
        let _ = fs::remove_dir_all(w);
    }
    #[test]
    fn broken_script_and_empty_body_cannot_be_capture_evidence() {
        let _port_guard = PORT_LOCK.lock().unwrap();
        let w = temp("broken-evidence");
        let app = w.join("app");
        fs::create_dir(&app).unwrap();
        fs::write(app.join("index.html"),
            "<!doctype html><style>body{background:linear-gradient(#123,#456)}</style><main>Visible wrapper but missing scene</main><script>throw Error('scene failed')</script>").unwrap();
        let sup = PreviewSupervisor::new(&w).unwrap();
        let started = sup.start(Path::new("app")).unwrap();
        sup.begin_iteration(&started.id).unwrap();
        assert_eq!(
            sup.capture(&started.id).unwrap_err(),
            PreviewError::BrokenPage
        );
        sup.cancel(&started.id).unwrap();
        let _ = fs::remove_dir_all(w);
    }
    #[test]
    fn occupied_reserved_ports_are_skipped() {
        let _port_guard = PORT_LOCK.lock().unwrap();
        let guards: Vec<TcpListener> = (PORT_MIN..PORT_MIN + 3)
            .map(|p| TcpListener::bind((Ipv4Addr::LOCALHOST, p)).unwrap())
            .collect();
        let (_, p) = reserve_port().unwrap();
        assert!(p >= PORT_MIN + 3);
        drop(guards);
    }
    #[test]
    fn browser_capture_is_real_and_actions_reach_page() {
        let _port_guard = PORT_LOCK.lock().unwrap();
        let w = temp("browser");
        let app = w.join("app");
        fs::create_dir(&app).unwrap();
        fs::write(
            app.join("index.html"),
            r#"<!doctype html><style>body{background:linear-gradient(45deg,#194c5b,#f0bb74)}</style><button id='b' onclick="this.textContent='clicked'">press</button>"#,
        )
        .unwrap();
        let sup = PreviewSupervisor::new(&w).unwrap();
        let started = sup.start(Path::new("app")).unwrap();
        assert_eq!(sup.begin_iteration(&started.id).unwrap(), 1);
        let first = sup.capture(&started.id).unwrap();
        assert!(first.dom_text.contains("press"));
        let png = base64::engine::general_purpose::STANDARD
            .decode(
                first
                    .screenshot_data_url
                    .as_ref()
                    .unwrap()
                    .strip_prefix("data:image/png;base64,")
                    .unwrap(),
            )
            .unwrap();
        let reader = png::Decoder::new(std::io::Cursor::new(png))
            .read_info()
            .unwrap();
        assert_eq!((reader.info().width, reader.info().height), (1280, 800));
        assert!(first
            .screenshot_data_url
            .as_deref()
            .unwrap_or("")
            .starts_with("data:image/png;base64,"));
        sup.action(
            &started.id,
            &BrowserAction::PointerMove { x: 25.0, y: 15.0 },
        )
        .unwrap();
        sup.action(
            &started.id,
            &BrowserAction::PointerDown {
                button: crate::PointerButton::Primary,
            },
        )
        .unwrap();
        sup.action(
            &started.id,
            &BrowserAction::PointerUp {
                button: crate::PointerButton::Primary,
            },
        )
        .unwrap();
        let second = sup.capture(&started.id).unwrap();
        assert!(second.dom_text.contains("clicked"));
        assert_eq!(first.source_sha256, second.source_sha256);
        fs::write(
            app.join("index.html"),
            "<!doctype html><style>body{background:linear-gradient(45deg,#194c5b,#f0bb74)}</style><p>revised source</p>",
        )
        .unwrap();
        let revised = sup.capture(&started.id).unwrap();
        assert_ne!(first.source_sha256, revised.source_sha256);
        fs::write(
            app.join("index.html"),
            r#"<!doctype html><style>body{background:linear-gradient(45deg,#194c5b,#f0bb74)}</style><button id='b' onclick="this.textContent='clicked'">press</button>"#,
        )
        .unwrap();
        fs::write(app.join("motion.html"), "<!doctype html><style>body{background:linear-gradient(45deg,#e34141,#e8b875)}@media(prefers-reduced-motion:reduce){body{background:linear-gradient(45deg,#3475e3,#e8b875)}}</style><p>motion test</p>").unwrap();
        sup.action(
            &started.id,
            &BrowserAction::Navigate {
                path: "/motion.html".into(),
            },
        )
        .unwrap();
        let normal = sup.capture(&started.id).unwrap();
        sup.action(
            &started.id,
            &BrowserAction::SetReducedMotion { enabled: true },
        )
        .unwrap();
        let reduced = sup.capture(&started.id).unwrap();
        assert_ne!(normal.screenshot_data_url, reduced.screenshot_data_url);
        sup.action(
            &started.id,
            &BrowserAction::SetReducedMotion { enabled: false },
        )
        .unwrap();
        let restored = sup.capture(&started.id).unwrap();
        assert_eq!(normal.screenshot_data_url, restored.screenshot_data_url);
        // A click navigates independently of the typed route action. Do not
        // bind its pixels as task evidence when it leaves the preview port.
        fs::write(
            app.join("escape.html"),
            "<a href='http://example.invalid/'>leave</a>",
        )
        .unwrap();
        sup.action(
            &started.id,
            &BrowserAction::Navigate {
                path: "/escape.html".into(),
            },
        )
        .unwrap();
        sup.action(
            &started.id,
            &BrowserAction::PointerMove { x: 20.0, y: 15.0 },
        )
        .unwrap();
        sup.action(
            &started.id,
            &BrowserAction::PointerDown {
                button: crate::PointerButton::Primary,
            },
        )
        .unwrap();
        sup.action(
            &started.id,
            &BrowserAction::PointerUp {
                button: crate::PointerButton::Primary,
            },
        )
        .unwrap();
        assert_eq!(
            sup.capture(&started.id).unwrap_err(),
            PreviewError::UrlDenied
        );
        sup.action(&started.id, &BrowserAction::Navigate { path: "/".into() })
            .unwrap();
        assert!(sup.capture(&started.id).unwrap().dom_text.contains("press"));
        sup.record_iteration(
            &started.id,
            crate::IterationReceipt {
                iteration: 1,
                accepted: false,
                diff_id: "diff-1".into(),
                evidence_ids: vec![],
                failed_gates: vec![crate::ProductionGate::MobileViewport],
                reason: "mobile evidence missing".into(),
            },
        )
        .unwrap();
        assert_eq!(
            sup.production_report(&started.id).unwrap().receipts.len(),
            1
        );
        assert_eq!(sup.begin_iteration(&started.id).unwrap(), 2);
        sup.teardown(&started.id).unwrap();
        let _ = fs::remove_dir_all(w);
    }
}
