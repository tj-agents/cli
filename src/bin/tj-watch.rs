#![cfg_attr(windows, windows_subsystem = "windows")]

#[allow(dead_code)]
#[path = "../live.rs"]
mod live;
#[allow(dead_code)]
#[path = "../paths.rs"]
mod paths;
#[allow(dead_code)]
#[path = "../watch.rs"]
mod watch;

fn main() -> anyhow::Result<()> {
    watch::run_forever().map_err(anyhow::Error::from)
}
