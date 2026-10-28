//! A typed wrapper over the `docker` command line.
//!
//! The command line rather than the Engine API over its unix socket: the
//! whole surface this lab needs is nine verbs, `docker` is already installed
//! wherever the lab can run at all, and it is the same idiom the rest of the
//! repository's tooling uses for `dpkg-deb`, `strip` and `cargo`. A Docker
//! API client crate would add an async HTTP dependency, an API-version
//! negotiation, and a second way for the lab to break that has nothing to do
//! with Mattermost.
//!
//! What is *not* a compose file, and the reason the earlier note in `lib.rs`
//! ruled one out, is the container definitions: they are Rust values below,
//! so the readiness ladder, the seeding and the containers cannot drift apart
//! from each other the way a YAML file and the code that waits on it do.

use std::ffi::OsStr;
use std::process::{Command, Stdio};

use crate::{Error, Result};

/// One container to run, as `docker run` arguments.
#[derive(Debug, Default)]
pub(crate) struct Container {
    pub(crate) name: String,
    pub(crate) image: String,
    /// `KEY=value`, passed as `--env`.
    pub(crate) env: Vec<String>,
    /// `(host, container)` port numbers, bound to loopback only.
    pub(crate) ports: Vec<(u16, u16)>,
    /// `(volume name, mount point)`.
    pub(crate) volumes: Vec<(String, String)>,
    pub(crate) network: String,
    /// The name other containers on the network resolve it by.
    pub(crate) alias: String,
    /// `KEY=value`, passed as `--label`, so `down` can find everything
    /// without knowing the names.
    pub(crate) labels: Vec<String>,
}

/// Run `docker` and return its stdout, trimmed.
fn docker<I, S>(args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<String> = args
        .into_iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect();
    let out = Command::new("docker")
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error(format!("could not run docker: {e}")))?;
    if !out.status.success() {
        return Err(Error(format!(
            "docker {} failed with {}:\n{}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Fail early with something actionable rather than nine times with
/// "could not run docker".
pub(crate) fn require_daemon() -> Result<()> {
    docker(["version", "--format", "{{.Server.Version}}"]).map_err(|e| {
        Error(format!(
            "{e}\nThe lab needs a running Docker daemon and an account in the `docker` group. \
             Check with `docker version`."
        ))
    })?;
    Ok(())
}

pub(crate) fn pull(image: &str) -> Result<()> {
    if docker(["image", "inspect", "--format", "{{.Id}}", image]).is_ok() {
        return Ok(());
    }
    println!("pulling {image} (first run only)");
    docker(["pull", image]).map(|_| ())
}

pub(crate) fn network_create(name: &str, label: &str) -> Result<()> {
    if docker(["network", "inspect", "--format", "{{.Id}}", name]).is_ok() {
        return Ok(());
    }
    docker(["network", "create", "--label", label, name]).map(|_| ())
}

pub(crate) fn volume_create(name: &str, label: &str) -> Result<()> {
    docker(["volume", "create", "--label", label, name]).map(|_| ())
}

/// Is there a container by this name, and is it running?
pub(crate) fn state(name: &str) -> Option<bool> {
    docker(["inspect", "--format", "{{.State.Running}}", name])
        .ok()
        .map(|s| s == "true")
}

pub(crate) fn start_existing(name: &str) -> Result<()> {
    docker(["start", name]).map(|_| ())
}

pub(crate) fn run(spec: &Container) -> Result<()> {
    let mut args: Vec<String> = vec![
        "run".into(),
        "--detach".into(),
        "--name".into(),
        spec.name.clone(),
        "--network".into(),
        spec.network.clone(),
        "--network-alias".into(),
        spec.alias.clone(),
    ];
    // No restart policy on purpose. A lab that comes back by itself after a
    // reboot is a Mattermost listening on this machine's 8065 forever,
    // which is not what anybody meant by "throwaway". `mmlab up` starts it
    // again in a second.
    for label in &spec.labels {
        args.push("--label".into());
        args.push(label.clone());
    }
    for kv in &spec.env {
        args.push("--env".into());
        args.push(kv.clone());
    }
    for (host, inside) in &spec.ports {
        // 127.0.0.1 explicitly: a lab server with no real passwords on it
        // should not be reachable from the coffee shop's wifi because
        // Docker helpfully punched through the firewall.
        args.push("--publish".into());
        args.push(format!("127.0.0.1:{host}:{inside}"));
    }
    for (volume, at) in &spec.volumes {
        args.push("--volume".into());
        args.push(format!("{volume}:{at}"));
    }
    args.push(spec.image.clone());
    docker(args).map(|_| ())
}

/// The ids of everything of one kind carrying the lab's label.
///
/// `kind` is "container", "volume" or "network" -- the three things `up`
/// creates. Listing by label rather than by name is what lets `down` clean up
/// after a rename, or after a half-finished `up`.
pub(crate) fn labelled(kind: &str, label: &str) -> Result<Vec<String>> {
    let filter = format!("label={label}");
    let args: Vec<&str> = match kind {
        "container" => vec!["ps", "--all", "--filter", &filter, "--quiet"],
        other => vec![other, "ls", "--filter", &filter, "--quiet"],
    };
    let out = docker(args)?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

pub(crate) fn remove_containers(ids: &[String]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut args = vec!["rm".to_owned(), "--force".into(), "--volumes".into()];
    args.extend(ids.iter().cloned());
    docker(args).map(|_| ())
}

pub(crate) fn remove(kind: &str, ids: &[String]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut args = vec![kind.to_owned(), "rm".into()];
    args.extend(ids.iter().cloned());
    docker(args).map(|_| ())
}

pub(crate) fn logs(name: &str, lines: u32) -> Result<String> {
    let out = Command::new("docker")
        .args(["logs", "--tail", &lines.to_string(), name])
        .output()
        .map_err(|e| Error(format!("could not run docker logs: {e}")))?;
    // The server logs to stderr, so both streams matter and neither alone is
    // the answer.
    Ok(format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}
