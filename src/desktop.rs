//! One-shot, unprivileged JSON bridge for the native macOS frontend.
use crate::profiles::{self, Draft, Store};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    path::PathBuf,
};

#[derive(Deserialize)]
struct Request {
    root: PathBuf,
    #[serde(flatten)]
    command: Command,
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Command {
    List,
    Template,
    Load { index: usize },
    Select { index: usize },
    Save { index: Option<usize>, draft: Draft },
    Import { path: PathBuf },
    Dependencies,
}
fn execute(request: Request) -> Result<Value> {
    let mut store = Store::open(request.root)?;
    match request.command {
        Command::List => Ok(serde_json::to_value(&store.catalog)?),
        Command::Template => Ok(serde_json::to_value(Draft {
            name: "新配置".into(),
            tunnel: profiles::TEMPLATE.into(),
            wireguard: profiles::WG_TEMPLATE.into(),
        })?),
        Command::Load { index } => Ok(serde_json::to_value(store.load(index)?)?),
        Command::Select { index } => {
            store.path(index)?;
            store.catalog.selected = index;
            store.persist()?;
            Ok(json!({}))
        }
        Command::Save { index, draft } => Ok(json!({"index": store.save(index, &draft)?})),
        Command::Import { path } => Ok(serde_json::to_value(profiles::import(&path)?)?),
        Command::Dependencies => {
            #[cfg(target_os = "macos")]
            {
                Ok(json!({"ready": crate::macos::dependencies_ready()}))
            }
            #[cfg(not(target_os = "macos"))]
            {
                Ok(json!({"ready": false}))
            }
        }
    }
}
pub fn serve() -> Result<()> {
    let mut input = Vec::new();
    std::io::stdin()
        .take(512 * 1024 + 1)
        .read_to_end(&mut input)?;
    let result = (|| {
        ensure!(input.len() <= 512 * 1024, "desktop request too large");
        let request = serde_json::from_slice(&input)
            .map_err(|_| anyhow::anyhow!("invalid desktop request"))?;
        execute(request)
    })();
    let response = match result {
        Ok(value) => json!({"ok": true, "data": value}),
        Err(error) => json!({"ok": false, "error": format!("{error:#}")}),
    };
    serde_json::to_writer(std::io::stdout().lock(), &response)
        .context("cannot write desktop response")?;
    std::io::stdout().write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bridge_round_trip_and_rejected_save_preserve_catalog() {
        use base64::Engine;
        let dir = tempfile::tempdir().unwrap();
        let call = |command| {
            execute(Request {
                root: dir.path().into(),
                command,
            })
        };
        let key = base64::engine::general_purpose::STANDARD.encode([1; 32]);
        let draft = Draft {
            name: "macOS".into(),
            tunnel: profiles::TEMPLATE.into(),
            wireguard: format!(
                "[Interface]\nPrivateKey={key}\nAddress=10.1.1.2/32\n[Peer]\nPublicKey={key}\nAllowedIPs=10.1.1.1/32\n"
            ),
        };
        assert_eq!(
            call(Command::Save { index: None, draft }).unwrap()["index"],
            0
        );
        let saved = call(Command::Load { index: 0 }).unwrap();
        assert_eq!(saved["name"], "macOS");
        let mut invalid: Draft = serde_json::from_value(saved).unwrap();
        invalid.tunnel = "private-secret invalid TOML".into();
        let error = call(Command::Save {
            index: Some(0),
            draft: invalid,
        })
        .unwrap_err();
        assert!(!format!("{error:#}").contains("private-secret"));
        assert_eq!(
            call(Command::List).unwrap()["profiles"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(call(Command::Select { index: 99 }).is_err());
    }
}
