//! Per-user GUI profiles. Imported originals are never modified.
use crate::{config::Config, wgconfig};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const TEMPLATE: &str = include_str!("../examples/xxtab.toml");
pub const WG_TEMPLATE: &str = include_str!("../examples/wg.conf.example");
const MAX_FILE: u64 = 128 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub id: String,
}
#[derive(Default, Serialize, Deserialize)]
pub struct Catalog {
    pub profiles: Vec<Profile>,
    pub selected: usize,
}
pub struct Store {
    pub root: PathBuf,
    pub catalog: Catalog,
}
#[derive(Serialize, Deserialize)]
pub struct Draft {
    pub name: String,
    pub tunnel: String,
    pub wireguard: String,
}

pub fn read_text(path: &Path) -> Result<String> {
    ensure!(
        std::fs::metadata(path)?.len() <= MAX_FILE,
        "配置文件超过 128 KiB"
    );
    let text = std::fs::read_to_string(path).context("无法读取 UTF-8 配置文件")?;
    Ok(text.trim_start_matches('\u{feff}').replace("\r\n", "\n"))
}
fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().context("缺少配置目录")?)?;
    file.write_all(data)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}
impl Store {
    pub fn open(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&root)?;
        let path = root.join("profiles.json");
        let catalog: Catalog = if path.exists() {
            serde_json::from_str(&read_text(&path)?)
                .context("配置列表损坏，请保留 profiles.json 后检查")?
        } else {
            Catalog::default()
        };
        ensure!(catalog.profiles.len() <= 100, "最多支持 100 份配置");
        for p in &catalog.profiles {
            ensure!(
                !p.id.is_empty() && p.id.bytes().all(|c| c.is_ascii_hexdigit()),
                "配置目录标识不合法"
            );
        }
        Ok(Self { root, catalog })
    }
    pub fn path(&self, index: usize) -> Result<PathBuf> {
        let profile = self.catalog.profiles.get(index).context("请选择配置")?;
        Ok(self.root.join(&profile.id).join("xxtab.toml"))
    }
    pub fn load(&self, index: usize) -> Result<Draft> {
        let path = self.path(index)?;
        Ok(Draft {
            name: self.catalog.profiles[index].name.clone(),
            tunnel: read_text(&path)?,
            wireguard: read_text(&path.with_file_name("wg.conf"))?,
        })
    }
    pub fn persist(&self) -> Result<()> {
        atomic_write(
            &self.root.join("profiles.json"),
            &serde_json::to_vec_pretty(&self.catalog)?,
        )
    }
    pub fn save(&mut self, index: Option<usize>, draft: &Draft) -> Result<usize> {
        ensure!(
            !draft.name.trim().is_empty()
                && draft.name.chars().count() <= 60
                && !draft.name.chars().any(char::is_control),
            "配置名称需要 1–60 个字符"
        );
        ensure!(
            draft.tunnel.len() <= MAX_FILE as usize && draft.wireguard.len() <= MAX_FILE as usize,
            "配置过大"
        );
        let tunnel = normalize(&draft.tunnel, None)?;
        let cfg = Config::parse(&tunnel, &self.root.join("xxtab.toml"))?;
        let wg = cfg.wireguard.as_ref().context("缺少 [wireguard] 配置段")?;
        wgconfig::prepare(&draft.wireguard, cfg.listen, wg.mtu)
            .context("WireGuard 配置未通过检查")?;
        let index = index.unwrap_or(self.catalog.profiles.len());
        ensure!(
            index <= self.catalog.profiles.len() && index < 100,
            "配置数量已达到上限"
        );
        // A complete new version is written first; only then atomically publish its catalog entry.
        // A failed or interrupted save cannot leave the active TOML and WG file out of sync.
        let id = format!(
            "{:x}",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        );
        let dir = self.root.join(&id);
        std::fs::create_dir(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        atomic_write(&dir.join("wg.conf"), draft.wireguard.as_bytes())?;
        atomic_write(&dir.join("xxtab.toml"), tunnel.as_bytes())?;
        let old = self.catalog.profiles.get(index).cloned();
        let previous_selection = self.catalog.selected;
        let profile = Profile {
            name: draft.name.trim().into(),
            id,
        };
        if index == self.catalog.profiles.len() {
            self.catalog.profiles.push(profile);
        } else {
            self.catalog.profiles[index] = profile;
        }
        self.catalog.selected = index;
        if let Err(error) = self.persist() {
            if let Some(old) = old {
                self.catalog.profiles[index] = old;
            } else {
                self.catalog.profiles.pop();
            }
            self.catalog.selected = previous_selection;
            return Err(error);
        }
        // Retain previous complete versions for manual recovery.
        // Small profiles are disk data, not retained in the running process.
        Ok(index)
    }
}

/// Normalize the managed WG path; imported CA and executable paths retain their meaning.
fn normalize(text: &str, source: Option<&Path>) -> Result<String> {
    let mut value: toml::Value =
        toml::from_str(text).map_err(|_| anyhow::anyhow!("隧道 TOML 语法错误，请检查字段和值"))?;
    let table = value.as_table_mut().context("隧道配置必须是 TOML 表")?;
    if let Some(source) = source {
        let base = source.parent().context("配置没有父目录")?;
        if let Some(ca) = table.get_mut("ca_file") {
            let path = PathBuf::from(ca.as_str().context("ca_file 必须是路径文本")?);
            *ca = toml::Value::String(base.join(path).to_string_lossy().into_owned());
        }
    } else if let Some(ca) = table.get("ca_file").and_then(toml::Value::as_str) {
        ensure!(
            Path::new(ca).is_absolute(),
            "界面配置中的 ca_file 请使用绝对路径；导入已有配置会自动转换"
        );
    }
    let wg = table
        .get_mut("wireguard")
        .and_then(toml::Value::as_table_mut)
        .context("缺少 [wireguard] 配置段")?;
    wg.insert("config".into(), toml::Value::String("wg.conf".into()));
    toml::to_string_pretty(&value).context("无法生成 TOML 配置")
}
pub fn import(path: &Path) -> Result<Draft> {
    let path = std::fs::canonicalize(path)?;
    let text = read_text(&path)?;
    let name = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    if path
        .extension()
        .is_some_and(|s| s.eq_ignore_ascii_case("toml"))
    {
        let cfg = Config::parse(&text, &path)?;
        let wg = cfg.wireguard.context("导入的 TOML 缺少 [wireguard]")?;
        Ok(Draft {
            name,
            tunnel: normalize(&text, Some(&path))?,
            wireguard: read_text(&wg.config)?,
        })
    } else {
        Ok(Draft {
            name,
            tunnel: TEMPLATE.into(),
            wireguard: text,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wg() -> String {
        use base64::Engine;
        let key = base64::engine::general_purpose::STANDARD.encode([1; 32]);
        format!(
            "[Interface]\nPrivateKey={key}\nAddress=10.0.0.2/32\n[Peer]\nPublicKey={key}\nAllowedIPs=10.0.0.1/32\n"
        )
    }
    #[test]
    fn import_and_save_leave_originals_unchanged_and_pair_profiles() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("source.conf"), wg()).unwrap();
        let tunnel =
            "server='ws://127.0.0.1:8080'\nca_file='ca.pem'\n[wireguard]\nconfig='source.conf'";
        std::fs::write(dir.path().join("source.toml"), tunnel).unwrap();
        let mut draft = import(&dir.path().join("source.toml")).unwrap();
        let mut store = Store::open(dir.path().join("profiles")).unwrap();
        let index = store.save(None, &draft).unwrap();
        let first = store.path(index).unwrap();
        draft.name = "新版".into();
        store.save(Some(index), &draft).unwrap();
        assert_ne!(first, store.path(index).unwrap());
        assert_eq!(store.catalog.profiles.len(), 1);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("source.toml")).unwrap(),
            tunnel
        );
        let cfg = Config::load(&store.path(index).unwrap()).unwrap();
        assert!(cfg.ca_file.unwrap().is_absolute());
        assert_eq!(read_text(&cfg.wireguard.unwrap().config).unwrap(), wg());
        assert_eq!(
            Store::open(store.root).unwrap().catalog.profiles[0].name,
            "新版"
        );
    }
    #[test]
    fn invalid_draft_cannot_replace_saved_profile() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().into()).unwrap();
        let mut draft = Draft {
            name: "test".into(),
            tunnel: TEMPLATE.into(),
            wireguard: wg(),
        };
        store.save(None, &draft).unwrap();
        let path = store.path(0).unwrap();
        draft.wireguard.push_str("PostUp = bad\n");
        assert!(store.save(Some(0), &draft).is_err());
        assert_eq!(store.path(0).unwrap(), path);
    }
}
