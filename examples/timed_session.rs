//! Bounded live diagnostic session; normal shutdown removes its WG service/routes.
//! cargo run --release --example timed_session -- <config.toml> <seconds>
use anyhow::{Result, ensure};
use std::{path::Path, time::Duration};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(args.len() == 2, "expected: <config.toml> <seconds>");
    let seconds: u64 = args[1].parse()?;
    ensure!(
        (1..=600).contains(&seconds),
        "duration must be 1..600 seconds"
    );
    xxtab::app::run(Path::new(&args[0]), "run", async {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(seconds)) => {},
            result = tokio::signal::ctrl_c() => result?,
        }
        Ok(())
    })
    .await
}
