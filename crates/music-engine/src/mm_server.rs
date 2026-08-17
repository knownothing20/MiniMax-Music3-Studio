//! Supervisor for the primary `minimaxmusic.cpp` local runtime.
//!
//! The upstream Windows route is `buildcuda.cmd`, followed by `server.cmd`.
//! `server.cmd` launches `mm-server.exe` with `--models`, `--host`, and
//! `--port`; those are the only launch flags this supervisor emits. The
//! upstream server handles SIGINT/SIGTERM by stopping its HTTP listener,
//! cancelling the active pipeline, draining its worker and freeing models.

use std::{
    fs,
    io::{Read, Write},
    net::{IpAddr, SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8086;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmServerLaunchConfig {
    pub executable: PathBuf,
    pub models_root: PathBuf,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmServerLocation {
    /// Root of the bundled `minimaxmusic.cpp` runtime, not the Studio root.
    pub bundle_root: PathBuf,
    /// Explicit executable selected by settings. It takes priority over the
    /// verified paths that the upstream Windows scripts create.
    pub configured_executable: Option<PathBuf>,
    /// Explicit model directory selected by settings. The directory must
    /// already exist; the supervisor never creates or downloads weights.
    pub configured_models_root: Option<PathBuf>,
    pub host: Option<String>,
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOutcome {
    ReusedHealthyServer,
    Started,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopOutcome {
    pub was_running: bool,
    pub graceful: bool,
}

pub struct MmServerSupervisor {
    config: MmServerLaunchConfig,
    child: Option<Child>,
}

impl MmServerLocation {
    pub fn resolve(self) -> Result<MmServerLaunchConfig> {
        let bundle_root = canonical_directory(&self.bundle_root, "mm-server bundle root")?;
        let executable = match self.configured_executable {
            Some(path) => canonical_file(&path, "configured mm-server executable")?,
            None => locate_bundled_executable(&bundle_root)?,
        };
        let models_root = match self.configured_models_root {
            Some(path) => canonical_directory(&path, "configured MiniMax Music3 models root")?,
            None => canonical_directory(&bundle_root.join("models"), "bundled MiniMax Music3 models root")?,
        };
        let host = self.host.unwrap_or_else(|| DEFAULT_HOST.into());
        validate_loopback_host(&host)?;
        Ok(MmServerLaunchConfig {
            executable,
            models_root,
            host,
            port: self.port.unwrap_or(DEFAULT_PORT),
        })
    }
}

impl MmServerSupervisor {
    pub fn new(config: MmServerLaunchConfig) -> Result<Self> {
        canonical_file(&config.executable, "mm-server executable")?;
        canonical_directory(&config.models_root, "MiniMax Music3 models root")?;
        validate_loopback_host(&config.host)?;
        Ok(Self { config, child: None })
    }

    pub fn config(&self) -> &MmServerLaunchConfig {
        &self.config
    }

    pub fn is_healthy(&self, timeout: Duration) -> bool {
        health_check(&self.config.host, self.config.port, timeout)
    }

    /// Starts one owned server only when no healthy server is already listening
    /// on its loopback endpoint. A live owned child is never duplicated while
    /// it is still initialising.
    pub fn ensure_started(&mut self, readiness_timeout: Duration) -> Result<StartOutcome> {
        if self.is_healthy(Duration::from_millis(300)) {
            return Ok(StartOutcome::ReusedHealthyServer);
        }
        self.clear_exited_child()?;
        if self.child.is_some() {
            self.wait_until_healthy(readiness_timeout)?;
            return Ok(StartOutcome::Started);
        }

        let mut command = Command::new(&self.config.executable);
        command
            .arg("--models")
            .arg(&self.config.models_root)
            .arg("--host")
            .arg(&self.config.host)
            .arg("--port")
            .arg(self.config.port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_child_process(&mut command);
        let child = command
            .spawn()
            .with_context(|| format!("start mm-server {}", self.config.executable.display()))?;
        self.child = Some(child);
        if let Err(error) = self.wait_until_healthy(readiness_timeout) {
            let _ = self.stop(Duration::from_secs(1));
            return Err(error);
        }
        Ok(StartOutcome::Started)
    }

    /// Requests the shutdown mechanism implemented upstream. If the process
    /// does not exit within the grace period, it is forcibly stopped so Studio
    /// never leaves an orphaned local engine process behind.
    pub fn stop(&mut self, grace_period: Duration) -> Result<StopOutcome> {
        let Some(child) = self.child.as_mut() else {
            return Ok(StopOutcome { was_running: false, graceful: true });
        };
        if child.try_wait().context("inspect mm-server child")?.is_some() {
            self.child = None;
            return Ok(StopOutcome { was_running: false, graceful: true });
        }

        request_graceful_shutdown(child)?;
        let deadline = Instant::now() + grace_period;
        while Instant::now() < deadline {
            if child.try_wait().context("wait for mm-server shutdown")?.is_some() {
                self.child = None;
                return Ok(StopOutcome { was_running: true, graceful: true });
            }
            thread::sleep(Duration::from_millis(50));
        }
        child.kill().context("force-stop mm-server after grace period")?;
        child.wait().context("wait for force-stopped mm-server")?;
        self.child = None;
        Ok(StopOutcome { was_running: true, graceful: false })
    }

    fn clear_exited_child(&mut self) -> Result<()> {
        let exited = self
            .child
            .as_mut()
            .map(|child| child.try_wait().context("inspect mm-server child"))
            .transpose()?
            .flatten()
            .is_some();
        if exited {
            self.child = None;
        }
        Ok(())
    }

    fn wait_until_healthy(&mut self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.is_healthy(Duration::from_millis(300)) {
                return Ok(());
            }
            if let Some(child) = self.child.as_mut() {
                if let Some(status) = child.try_wait().context("inspect starting mm-server")? {
                    bail!("mm-server exited during startup with status {status}");
                }
            }
            if Instant::now() >= deadline {
                bail!(
                    "mm-server did not pass GET /health on {}:{} before startup timeout",
                    self.config.host,
                    self.config.port
                );
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for MmServerSupervisor {
    fn drop(&mut self) {
        let _ = self.stop(Duration::from_secs(2));
    }
}

fn locate_bundled_executable(bundle_root: &Path) -> Result<PathBuf> {
    // `server.cmd` from minimaxmusic.cpp puts `build\\Release` on PATH;
    // CMake also declares the build root as its runtime output directory.
    let executable = if cfg!(windows) { "mm-server.exe" } else { "mm-server" };
    let candidates = [
        bundle_root.join(executable),
        bundle_root.join("bin").join(executable),
        bundle_root.join("build").join("Release").join(executable),
        bundle_root.join("build").join(executable),
    ];
    candidates
        .iter()
        .find(|path| path.is_file())
        .map(|path| canonical_file(path, "bundled mm-server executable"))
        .transpose()?
        .with_context(|| {
            format!(
                "mm-server was not found below {}; expected a packaged binary or one produced by minimaxmusic.cpp buildcuda.cmd/buildall.cmd",
                bundle_root.display()
            )
        })
}

fn canonical_file(path: &Path, label: &str) -> Result<PathBuf> {
    if !path.is_file() {
        bail!("{label} is not a file: {}", path.display());
    }
    fs::canonicalize(path).with_context(|| format!("canonicalize {label}: {}", path.display()))
}

fn canonical_directory(path: &Path, label: &str) -> Result<PathBuf> {
    if !path.is_dir() {
        bail!("{label} is not a directory: {}", path.display());
    }
    fs::canonicalize(path).with_context(|| format!("canonicalize {label}: {}", path.display()))
}

fn validate_loopback_host(host: &str) -> Result<()> {
    let address: IpAddr = host
        .parse()
        .with_context(|| format!("mm-server host must be an IP address, got `{host}`"))?;
    if !address.is_loopback() {
        bail!("mm-server host must remain loopback-only, got `{host}`");
    }
    Ok(())
}

fn health_check(host: &str, port: u16, timeout: Duration) -> bool {
    let Ok(address) = host.parse::<IpAddr>() else {
        return false;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&SocketAddr::new(address, port), timeout) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    if stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut response = String::new();
    stream.read_to_string(&mut response).is_ok() && response.starts_with("HTTP/1.1 200")
}

#[cfg(windows)]
fn configure_child_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(windows))]
fn configure_child_process(_command: &mut Command) {}

#[cfg(windows)]
fn request_graceful_shutdown(child: &Child) -> Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::{Console::{GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT}, Threading::GetProcessId};
    let process_group = unsafe { GetProcessId(child.as_raw_handle()) };
    if unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, process_group) } == 0 {
        bail!("send CTRL_BREAK to mm-server process group failed")
    }
    Ok(())
}

#[cfg(not(windows))]
fn request_graceful_shutdown(child: &Child) -> Result<()> {
    let result = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    if result != 0 {
        bail!("send SIGTERM to mm-server process failed: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_directory(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("music-engine-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn resolves_packaged_windows_style_runtime_and_default_models() {
        let root = fresh_directory("runtime");
        let build = root.join("build").join("Release");
        fs::create_dir_all(&build).unwrap();
        fs::create_dir_all(root.join("models")).unwrap();
        let executable = build.join(if cfg!(windows) { "mm-server.exe" } else { "mm-server" });
        fs::write(&executable, b"test").unwrap();
        let config = MmServerLocation {
            bundle_root: root.clone(),
            configured_executable: None,
            configured_models_root: None,
            host: None,
            port: None,
        }
        .resolve()
        .unwrap();
        assert_eq!(config.host, DEFAULT_HOST);
        assert_eq!(config.port, DEFAULT_PORT);
        assert!(config.executable.ends_with(executable.file_name().unwrap()));
        assert!(config.models_root.ends_with("models"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_non_loopback_listener() {
        assert!(validate_loopback_host("0.0.0.0").is_err());
        assert!(validate_loopback_host("127.0.0.1").is_ok());
    }

    #[test]
    fn health_check_is_false_without_a_server() {
        assert!(!health_check("127.0.0.1", 65534, Duration::from_millis(10)));
    }
}
