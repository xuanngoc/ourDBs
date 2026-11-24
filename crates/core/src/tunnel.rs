//! SSH tunnel through the system `ssh` binary (`ssh -N -L`).
//!
//! The system ssh reads ~/.ssh/config, keys and the ssh-agent, so every host
//! alias that works in a terminal works here. Password prompts are not
//! supported (BatchMode=yes): use a key or the agent.

use std::{
    io::Read,
    net::{SocketAddr, TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, bail};

pub struct Tunnel {
    child: Child,
    pub local_port: u16,
}

/// Split "user@host:port" into ("user@host", Some(port)). Aliases and
/// "user@host" come back unchanged.
fn split_dest(dest: &str) -> (String, Option<u16>) {
    let dest = dest.trim();
    if let Some((left, right)) = dest.rsplit_once(':')
        && let Ok(p) = right.parse::<u16>()
        && !left.contains(':')
    {
        return (left.to_string(), Some(p));
    }
    (dest.to_string(), None)
}

/// Replace a leading "~/" with the home folder.
pub fn expand_home(path: &str) -> String {
    match (path.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{home}/{rest}"),
        _ => path.to_string(),
    }
}

impl Tunnel {
    /// Open a tunnel to `remote_host:remote_port` through `dest` and wait
    /// until the local port accepts connections (max 20 s). Blocking: call it
    /// from `spawn_blocking`.
    pub fn open(
        dest: &str,
        key: Option<&str>,
        remote_host: &str,
        remote_port: u16,
    ) -> anyhow::Result<Tunnel> {
        let local_port = {
            let l = TcpListener::bind("127.0.0.1:0").context("no free local port")?;
            l.local_addr()?.port()
        };
        let (dest, ssh_port) = split_dest(dest);
        if dest.is_empty() {
            bail!("SSH destination is empty");
        }
        let mut cmd = Command::new("ssh");
        cmd.arg("-N")
            .args(["-o", "BatchMode=yes"])
            .args(["-o", "ExitOnForwardFailure=yes"])
            .args(["-o", "ServerAliveInterval=30"])
            .args(["-o", "ConnectTimeout=15"])
            .args(["-o", "ControlMaster=no", "-o", "ControlPath=none"])
            .arg("-L")
            .arg(format!(
                "127.0.0.1:{local_port}:{remote_host}:{remote_port}"
            ));
        if let Some(p) = ssh_port {
            cmd.arg("-p").arg(p.to_string());
        }
        if let Some(key) = key.map(str::trim).filter(|k| !k.is_empty()) {
            let path = expand_home(key);
            if !std::path::Path::new(&path).exists() {
                bail!("SSH key file not found: {path}");
            }
            cmd.arg("-i").arg(path).args(["-o", "IdentitiesOnly=yes"]);
        }
        cmd.arg(&dest)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().context("cannot start ssh")?;

        let addr = SocketAddr::from(([127, 0, 0, 1], local_port));
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = child.try_wait()? {
                let mut err = String::new();
                if let Some(mut e) = child.stderr.take() {
                    let _ = e.read_to_string(&mut err);
                }
                bail!("ssh exited ({status}): {}", err.trim());
            }
            if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
                return Ok(Tunnel { child, local_port });
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                bail!("ssh tunnel to {dest} did not open within 20 s");
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::split_dest;

    #[test]
    fn dest() {
        assert_eq!(split_dest("bastion"), ("bastion".into(), None));
        assert_eq!(
            split_dest("ec2-user@1.2.3.4:2222"),
            ("ec2-user@1.2.3.4".into(), Some(2222))
        );
        assert_eq!(split_dest(" me@host "), ("me@host".into(), None));
    }
}
