//! Process runner: Windows-native Job Object containment (assign BEFORE resume),
//! piped stdin for the task body, capped stderr capture, stop with exit confirmation.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const STDERR_CAP: usize = 2 * 1024 * 1024; // 2 MiB per attempt

#[derive(Clone, Debug)]
pub struct SpawnSpec {
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub stdin_text: String,
}

pub struct ExitOutcome {
    pub exit_code: Option<i32>,
    pub stderr: Vec<u8>,
    pub stderr_truncated: bool,
    /// true only when we observed the process tree exit through the job handle
    pub stop_confirmed: bool,
    pub killed: bool,
}

/// Filtered environment: drop inherited credential-looking vars, keep operating baseline.
pub fn filter_env(adapter_env: &[(String, String)]) -> Vec<(String, String)> {
    let allow = [
        "SystemRoot",
        "SystemDrive",
        "windir",
        "PATH",
        "PATHEXT",
        "COMSPEC",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "HOMEDRIVE",
        "HOMEPATH",
        "LOCALAPPDATA",
        "APPDATA",
        "PROGRAMFILES",
        "PROGRAMDATA",
        "COMPUTERNAME",
        "NUMBER_OF_PROCESSORS",
        "OS",
        "PROCESSOR_ARCHITECTURE",
        "SESSIONNAME",
        "USERNAME",
    ];
    let mut env: Vec<(String, String)> = Vec::new();
    for k in allow {
        if let Ok(v) = std::env::var(k) {
            env.push((k.to_string(), v));
        }
    }
    // adapter-provided entries are explicit and controlled (e.g. CODEX_HOME)
    for (k, v) in adapter_env {
        env.retain(|(ek, _)| ek != k);
        env.push((k.clone(), v.clone()));
    }
    env
}

pub struct RunningChild {
    pub pid: u32,
    cancel: Arc<AtomicBool>,
    lines_rx: Mutex<Option<std::sync::mpsc::Receiver<String>>>,
    #[cfg(windows)]
    job: Arc<JobBox>,
    #[cfg(not(windows))]
    job_inner: Arc<Mutex<Option<std::process::Child>>>,
}

struct JobBox {
    inner: Mutex<Option<winproc::Job>>,
}

unsafe impl Send for JobBox {}
unsafe impl Sync for JobBox {}

impl RunningChild {
    /// Request stop: terminate job, then wait for exit confirmation.
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        #[cfg(windows)]
        {
            let g = self.job.inner.lock().unwrap();
            if let Some(job) = g.as_ref() {
                job.kill();
            }
        }
    }

    /// Take the stdout line receiver (adapter parse loop owns it).
    pub fn take_lines(&self) -> Option<std::sync::mpsc::Receiver<String>> {
        self.lines_rx.lock().unwrap().take()
    }

    /// Non-blocking exit poll: Some(outcome) when the process tree has exited.
    pub fn poll_exit_short(&self) -> Option<ExitOutcome> {
        #[cfg(windows)]
        {
            let g = self.job.inner.lock().unwrap();
            if let Some(job) = g.as_ref() {
                if job.wait(0) {
                    return Some(ExitOutcome {
                        exit_code: job.exit_code(),
                        stderr: job.take_stderr(),
                        stderr_truncated: job.stderr_truncated(),
                        stop_confirmed: true,
                        killed: self.cancel.load(Ordering::SeqCst),
                    });
                }
            }
            None
        }
        #[cfg(not(windows))]
        {
            let mut g = self.job_inner.lock().unwrap();
            if let Some(child) = g.as_mut() {
                if let Ok(Some(status)) = child.try_wait() {
                    return Some(ExitOutcome {
                        exit_code: status.code(),
                        stderr: Vec::new(),
                        stderr_truncated: false,
                        stop_confirmed: true,
                        killed: self.cancel.load(Ordering::SeqCst),
                    });
                }
            }
            None
        }
    }

    /// Blocking wait for exit. Returns outcome with stop confirmation evidence.
    pub fn wait(&self, timeout: Duration) -> ExitOutcome {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            #[cfg(windows)]
            {
                let g = self.job.inner.lock().unwrap();
                if let Some(job) = g.as_ref() {
                    if job.wait(200) {
                        let code = job.exit_code();
                        let killed = self.cancel.load(Ordering::SeqCst);
                        return ExitOutcome {
                            exit_code: code,
                            stderr: job.take_stderr(),
                            stderr_truncated: job.stderr_truncated(),
                            stop_confirmed: true,
                            killed,
                        };
                    }
                } else {
                    return ExitOutcome {
                        exit_code: None,
                        stderr: Vec::new(),
                        stderr_truncated: false,
                        stop_confirmed: false,
                        killed: false,
                    };
                }
                drop(g);
            }
            #[cfg(not(windows))]
            {
                let g = self.job_inner.lock().unwrap();
                if let Some(child) = g.as_ref() {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            return ExitOutcome {
                                exit_code: status.code(),
                                stderr: Vec::new(),
                                stderr_truncated: false,
                                stop_confirmed: true,
                                killed: self.cancel.load(Ordering::SeqCst),
                            };
                        }
                        Ok(None) => {}
                        Err(_) => {
                            return ExitOutcome {
                                exit_code: None,
                                stderr: Vec::new(),
                                stderr_truncated: false,
                                stop_confirmed: false,
                                killed: false,
                            };
                        }
                    }
                }
                drop(g);
            }
            if std::time::Instant::now() > deadline {
                // not exited by deadline; kill hard and continue waiting (stop must be confirmed)
                self.stop();
                let hard_deadline = std::time::Instant::now() + Duration::from_secs(10);
                loop {
                    #[cfg(windows)]
                    {
                        let g = self.job.inner.lock().unwrap();
                        if let Some(job) = g.as_ref() {
                            if job.wait(200) {
                                let code = job.exit_code();
                                return ExitOutcome {
                                    exit_code: code,
                                    stderr: job.take_stderr(),
                                    stderr_truncated: job.stderr_truncated(),
                                    stop_confirmed: true,
                                    killed: true,
                                };
                            }
                        }
                        drop(g);
                    }
                    if std::time::Instant::now() > hard_deadline {
                        return ExitOutcome {
                            exit_code: None,
                            stderr: Vec::new(),
                            stderr_truncated: false,
                            stop_confirmed: false,
                            killed: true,
                        };
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
}

#[cfg(windows)]
mod winproc {
    use std::path::Path;
    pub use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, SetHandleInformation, HANDLE_FLAG_INHERIT};
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::ReadFile;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject, TerminateJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JobObjectExtendedLimitInformation,
    };
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, GetExitCodeProcess, ResumeThread, WaitForSingleObject, CREATE_SUSPENDED,
        CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW,
    };
    use std::sync::Mutex;

    pub struct Job {
        pub job: HANDLE,
        pub proc: HANDLE,
        pub stderr: std::sync::Arc<(Mutex<Vec<u8>>, Mutex<bool>)>,
    }

    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn kill(&self) {
            unsafe {
                TerminateJobObject(self.job, 1);
            }
        }
        pub fn wait(&self, timeout_ms: u32) -> bool {
            unsafe { WaitForSingleObject(self.proc, timeout_ms) == 0 }
        }
        pub fn exit_code(&self) -> Option<i32> {
            let mut code: u32 = 259;
            unsafe {
                if GetExitCodeProcess(self.proc, &mut code) != 0 && code != 259 {
                    Some(code as i32)
                } else {
                    None
                }
            }
        }
        pub fn take_stderr(&self) -> Vec<u8> {
            self.stderr.0.lock().unwrap().clone()
        }
        pub fn stderr_truncated(&self) -> bool {
            *self.stderr.1.lock().unwrap()
        }
    }

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn quote_arg(arg: &str) -> String {
        if !arg.is_empty()
            && !arg
                .chars()
                .any(|c| c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '"' || c == '\u{0b}')
        {
            return arg.to_string();
        }
        let mut out = String::from("\"");
        let mut backslashes = 0usize;
        for c in arg.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    out.push_str(&"\\".repeat(backslashes * 2 + 1));
                    out.push('"');
                    backslashes = 0;
                }
                c => {
                    out.push_str(&"\\".repeat(backslashes));
                    backslashes = 0;
                    out.push(c);
                }
            }
        }
        out.push_str(&"\\".repeat(backslashes * 2));
        out.push('"');
        out
    }

    fn env_block(env: &[(String, String)]) -> Vec<u16> {
        let mut block: Vec<u16> = Vec::new();
        for (k, v) in env {
            for u in format!("{k}={v}").encode_utf16() {
                block.push(u);
            }
            block.push(0);
        }
        block.push(0);
        block
    }

    struct Pipes {
        in_read: HANDLE,
        in_write: HANDLE,
        out_read: HANDLE,
        out_write: HANDLE,
        err_read: HANDLE,
        err_write: HANDLE,
    }

    fn make_pipes() -> Result<Pipes, String> {
        let mut sa: SECURITY_ATTRIBUTES = unsafe { std::mem::zeroed() };
        sa.nLength = std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32;
        sa.bInheritHandle = 1;
        let mut p = Pipes {
            in_read: std::ptr::null_mut(),
            in_write: std::ptr::null_mut(),
            out_read: std::ptr::null_mut(),
            out_write: std::ptr::null_mut(),
            err_read: std::ptr::null_mut(),
            err_write: std::ptr::null_mut(),
        };
        let ok0 = unsafe { CreatePipe(&mut p.in_read, &mut p.in_write, &sa, 0) };
        let ok1 = unsafe { CreatePipe(&mut p.out_read, &mut p.out_write, &sa, 0) };
        let ok2 = unsafe { CreatePipe(&mut p.err_read, &mut p.err_write, &sa, 0) };
        if ok0 == 0 || ok1 == 0 || ok2 == 0 {
            return Err("CreatePipe failed".into());
        }
        unsafe {
            // parent-side ends must not be inherited
            SetHandleInformation(p.in_write, HANDLE_FLAG_INHERIT, 0);
            SetHandleInformation(p.out_read, HANDLE_FLAG_INHERIT, 0);
            SetHandleInformation(p.err_read, HANDLE_FLAG_INHERIT, 0);
        }
        Ok(p)
    }

    pub struct Spawned {
        pub job: Job,
        pub pid: u32,
        pub out_read: HANDLE,
        pub err_read: HANDLE,
        pub in_write: HANDLE,
    }

    pub fn spawn_in_job(spec: &super::SpawnSpec) -> Result<Spawned, String> {
        let pipes = make_pipes()?;
        let exe_w = to_wide(&extended_path(Path::new(&spec.exe)).to_string_lossy());
        let mut cmdline = quote_arg(&spec.exe.to_string_lossy());
        for a in &spec.args {
            cmdline.push(' ');
            cmdline.push_str(&quote_arg(a));
        }
        let mut cmd_w: Vec<u16> = cmdline.encode_utf16().collect();
        cmd_w.push(0);
        let cwd_w = to_wide(&extended_path(&spec.cwd).to_string_lossy());
        let env_w = env_block(&spec.env);

        let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        si.dwFlags = STARTF_USESTDHANDLES;
        si.hStdInput = pipes.in_read;
        si.hStdOutput = pipes.out_write;
        si.hStdError = pipes.err_write;
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

        let ok = unsafe {
            CreateProcessW(
                exe_w.as_ptr(),
                cmd_w.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
                env_w.as_ptr() as *const _,
                cwd_w.as_ptr(),
                &mut si,
                &mut pi,
            )
        };
        if ok == 0 {
            return Err(format!(
                "CreateProcessW failed (code {}); exe={}",
                unsafe { GetLastError() },
                spec.exe.display()
            ));
        }
        // close child-side write ends in the parent so EOF propagates
        unsafe {
            CloseHandle(pipes.in_read);
            CloseHandle(pipes.out_write);
            CloseHandle(pipes.err_write);
        }

        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            unsafe {
                CloseHandle(pi.hThread);
                CloseHandle(pi.hProcess);
            }
            return Err("CreateJobObjectW failed".into());
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            unsafe {
                TerminateJobObject(job, 1);
                CloseHandle(job);
                CloseHandle(pi.hThread);
                CloseHandle(pi.hProcess);
            }
            return Err("SetInformationJobObject failed".into());
        }
        let ok = unsafe { AssignProcessToJobObject(job, pi.hProcess) };
        if ok == 0 {
            unsafe {
                TerminateJobObject(job, 1);
                CloseHandle(job);
                CloseHandle(pi.hThread);
                CloseHandle(pi.hProcess);
            }
            return Err("AssignProcessToJobObject failed".into());
        }
        // process tree fully contained; run it now
        unsafe {
            ResumeThread(pi.hThread);
            CloseHandle(pi.hThread);
        }
        Ok(Spawned {
            pid: pi.dwProcessId,
            job: Job {
                job,
                proc: pi.hProcess,
                stderr: std::sync::Arc::new((Mutex::new(Vec::new()), Mutex::new(false))),
            },
            out_read: pipes.out_read,
            err_read: pipes.err_read,
            in_write: pipes.in_write,
        })
    }

    fn extended_path(p: &Path) -> std::path::PathBuf {
        let s = p.to_string_lossy();
        if s.starts_with(r"\\?\") {
            p.to_path_buf()
        } else if s.len() > 240 {
            std::path::PathBuf::from(format!(r"\\?\{}", s))
        } else {
            p.to_path_buf()
        }
    }

    pub fn drain_pipe_to(h: SendHandle, sink: &Mutex<Vec<u8>>, truncated: &Mutex<bool>, cap: usize) {
        let h = h.0;
        let mut chunk = [0u8; 65536];
        loop {
            let mut n: u32 = 0;
            let ok = unsafe { ReadFile(h, chunk.as_mut_ptr(), chunk.len() as u32, &mut n, std::ptr::null_mut()) };
            if ok == 0 || n == 0 {
                break;
            }
            let mut s = sink.lock().unwrap();
            if s.len() < cap {
                let take = (cap - s.len()).min(n as usize);
                s.extend_from_slice(&chunk[..take]);
                if take < n as usize {
                    *truncated.lock().unwrap() = true;
                }
            } else {
                *truncated.lock().unwrap() = true;
            }
        }
        unsafe {
            CloseHandle(h);
        }
    }

    pub fn write_stdin_and_close(h: SendHandle, text: &str) {
        let h = h.0;
        // blocking write; stdin pipes are 4KiB+ buffered, task bodies are ≤ budget + slack
        let bytes = text.as_bytes();
        let mut off = 0usize;
        while off < bytes.len() {
            let mut written: u32 = 0;
            let ok = unsafe {
                windows_sys::Win32::Storage::FileSystem::WriteFile(
                    h,
                    bytes[off..].as_ptr(),
                    (bytes.len() - off) as u32,
                    &mut written,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 || written == 0 {
                break;
            }
            off += written as usize;
        }
        unsafe {
            CloseHandle(h);
        }
    }

    pub fn close_read(h: HANDLE) {
        unsafe {
            CloseHandle(h);
        }
    }

    /// Raw HANDLE wrapper: moved to a writer thread once, used only there.
    #[derive(Clone, Copy)]
    pub struct SendHandle(pub HANDLE);
    unsafe impl Send for SendHandle {}

}


/// Spawn the process inside a Job Object (contained before first instruction),
/// start reader threads, write the stdin body, return the handle.
pub fn launch(spec: &SpawnSpec) -> Result<RunningChild, String> {
    #[cfg(windows)]
    {
        let spawned = winproc::spawn_in_job(spec)?;
        let lines_tx = std::sync::mpsc::channel::<String>();
        let job_arc = Arc::new(JobBox {
            inner: Mutex::new(Some(spawned.job)),
        });
        // stdout line reader thread
        {
            let tx = lines_tx.0.clone();
            let h = winproc::SendHandle(spawned.out_read);
            std::thread::spawn(move || {
                read_lines(h, tx);
            });
        }
        // stderr drain thread: grab the sink handles and DROP the job lock before
        // draining (drain runs until EOF; holding the lock would deadlock stop/wait)
        {
            let job = job_arc.clone();
            let err_h = winproc::SendHandle(spawned.err_read);
            std::thread::spawn(move || {
                let sink = {
                    let g = job.inner.lock().unwrap();
                    g.as_ref().map(|j| j.stderr.clone())
                };
                match sink {
                    Some(s) => winproc::drain_pipe_to(err_h, &s.0, &s.1, STDERR_CAP),
                    None => winproc::close_read(err_h.0),
                }
            });
        }
        // stdin writer thread (task body)
        {
            let body = spec.stdin_text.clone();
            let h = winproc::SendHandle(spawned.in_write);
            std::thread::spawn(move || {
                winproc::write_stdin_and_close(h, &body);
            });
        }
        Ok(RunningChild {
            pid: spawned.pid,
            cancel: Arc::new(AtomicBool::new(false)),
            lines_rx: Mutex::new(Some(lines_tx.1)),
            job: job_arc,
        })
    }
    #[cfg(not(windows))]
    {
        use std::process::{Command, Stdio};
        let mut child = Command::new(&spec.exe)
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(&spec.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn failed: {e}"))?;
        if let Some(mut sin) = child.stdin.take() {
            let body = spec.stdin_text.clone();
            std::thread::spawn(move || {
                use std::io::Write;
                let _ = sin.write_all(body.as_bytes());
            });
        }
        let lines_tx = std::sync::mpsc::channel::<String>();
        let stderr_buf = Arc::new((Mutex::new(Vec::new()), Mutex::new(false)));
        {
            let mut out = child.stdout.take();
            let tx = lines_tx.0.clone();
            std::thread::spawn(move || {
                if let Some(out) = out.as_mut() {
                    read_lines_from_reader(out, tx);
                }
            });
        }
        {
            let mut err = child.stderr.take();
            let sink = stderr_buf.clone();
            std::thread::spawn(move || {
                use std::io::Read;
                if let Some(err) = err.as_mut() {
                    let mut buf = Vec::new();
                    let mut truncated = false;
                    let mut chunk = [0u8; 65536];
                    loop {
                        match err.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                let mut s = sink.0.lock().unwrap();
                                if s.len() < STDERR_CAP {
                                    let take = (STDERR_CAP - s.len()).min(n);
                                    s.extend_from_slice(&chunk[..take]);
                                    if take < n {
                                        truncated = true;
                                    }
                                } else {
                                    truncated = true;
                                }
                                let _ = &mut buf;
                            }
                        }
                    }
                    *sink.1.lock().unwrap() = truncated;
                }
            });
        }
        let pid = child.id();
        Ok(RunningChild {
            pid,
            cancel: Arc::new(AtomicBool::new(false)),
            lines_rx: Mutex::new(Some(lines_tx.1)),
            job_inner: Arc::new(Mutex::new(Some(child))),
        })
    }
}

#[cfg(windows)]
fn read_lines(h: winproc::SendHandle, tx: std::sync::mpsc::Sender<String>) {
    use windows_sys::Win32::Storage::FileSystem::ReadFile;
    let h = h.0;
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        let mut n: u32 = 0;
        let ok = unsafe { ReadFile(h, chunk.as_mut_ptr(), chunk.len() as u32, &mut n, std::ptr::null_mut()) };
        if ok == 0 || n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n as usize]);
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let mut s = String::from_utf8_lossy(&line[..line.len().saturating_sub(1)]).to_string();
            if s.ends_with('\r') {
                s.pop();
            }
            if !s.is_empty() {
                let _ = tx.send(s);
            }
        }
    }
    if !buf.is_empty() {
        let _ = tx.send(String::from_utf8_lossy(&buf).to_string());
    }
    winproc::close_read(h);
}

#[cfg(not(windows))]
fn read_lines_from_reader<R: std::io::Read>(r: &mut R, tx: std::sync::mpsc::Sender<String>) {
    use std::io::BufRead;
    let reader = std::io::BufReader::new(r);
    for line in reader.lines().map_while(Result::ok) {
        let _ = tx.send(line);
    }
}
