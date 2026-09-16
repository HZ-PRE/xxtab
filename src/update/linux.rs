//! Optional desktop dialogs; headless hosts need no extra runtime dependencies.
use anyhow::Result;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

struct Dialog(Child);

impl Dialog {
    async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.0.try_wait()? {
                return Ok(status);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

impl Drop for Dialog {
    fn drop(&mut self) {
        // Do not leave a dialog process running after the tunnel/CLI exits.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(super) async fn notify(message: &str) -> Result<()> {
    let desktop = ["DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()));
    if !desktop {
        return Ok(());
    }
    // Only use installed desktop tools, without a shell or a new GUI dependency.
    for (program, arguments) in [
        (
            "zenity",
            vec![
                "--info",
                "--no-markup",
                "--title=xxtab 更新",
                "--text",
                message,
            ],
        ),
        (
            "kdialog",
            vec!["--title", "xxtab 更新", "--msgbox", message],
        ),
    ] {
        match Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => {
                let status = Dialog(child).wait().await?;
                // Closing the dialog is also a successful notification.
                if status.success() || status.code() == Some(1) {
                    return Ok(());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    // The CLI already printed the full update instructions.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn dialog_wait_does_not_block_tunnel_and_drop_reaps_child() {
        let mut dialog = Dialog(Command::new("/bin/sleep").arg("30").spawn().unwrap());
        let id = dialog.0.id();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), dialog.wait())
                .await
                .is_err()
        );
        assert!(dialog.0.try_wait().unwrap().is_none());
        drop(dialog);
        assert!(!std::path::Path::new(&format!("/proc/{id}")).exists());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn closed_dialog_completes() {
        let mut dialog = Dialog(Command::new("/bin/true").spawn().unwrap());
        assert!(dialog.wait().await.unwrap().success());
    }
}
