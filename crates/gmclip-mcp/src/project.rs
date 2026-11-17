//! Project homes (R-STORE-4a). A project's home is `<library>/<name>.gmclip/`, holding
//! `owner.json {id, name, host, user, since, app_version}`; the live database is
//! `<app data>/projects/<id>.sqlite`, never in the home (the home may sit in a synced
//! folder; the live database never does).
//!
//! `owner.json` comes from the filesystem, so it is untrusted: it is read through the
//! root handle with a size cap, and its `id` must be a ULID before it names a file.

use std::io::Read;
use std::path::{Path, PathBuf};

use cap_std::fs::Dir;
use gmclip_model::Ulid;
use gmclip_proto::CmdError;
use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeInfo {
    pub id: String,
    pub name: String,
    pub host: String,
    pub user: String,
    pub since: i64,
    pub app_version: String,
}

fn bad_home(reason: &'static str) -> CmdError {
    CmdError::invalid("/path", reason)
}

/// Reads `owner.json` through a directory handle.
pub fn read_home(dir: &Dir) -> Result<HomeInfo, CmdError> {
    let mut f = dir
        .open("owner.json")
        .map_err(|_| bad_home("not a gmclip project home (no owner.json)"))?;
    let mut buf = Vec::new();
    f.by_ref()
        .take(64 * 1024 + 1)
        .read_to_end(&mut buf)
        .map_err(|_| bad_home("owner.json unreadable"))?;
    if buf.len() > 64 * 1024 {
        return Err(bad_home("owner.json too large"));
    }
    let info: HomeInfo =
        serde_json::from_slice(&buf).map_err(|_| bad_home("owner.json malformed"))?;
    Ulid::parse(&info.id).map_err(|_| bad_home("owner.json has no valid project id"))?;
    Ok(info)
}

/// The live database for a project id (the id is checked to be a ULID).
pub fn db_path(app: &Path, id: &str) -> Result<PathBuf, CmdError> {
    let id = Ulid::parse(id).map_err(|_| bad_home("not a project id"))?;
    let dir = app.join("projects");
    paths::mkdir_private(&dir).map_err(|_| CmdError::Internal {
        trace_id: "app-data".into(),
    })?;
    Ok(dir.join(format!("{id}.sqlite")))
}

fn new_ulid() -> Ulid {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0));
    let mut b = [0u8; 16];
    let _ = getrandom::fill(&mut b);
    let r = u128::from_le_bytes(b) & ((1u128 << 80) - 1);
    Ulid::from_parts(ms & ((1 << 48) - 1), r)
}

fn registry(app: &Path, id: &Ulid) -> PathBuf {
    app.join("projects").join(format!("{id}.home"))
}

/// Binds a project id to its home (app data, `projects/<id>.home`, written when the home
/// is created). A home whose `owner.json` claims an id recorded for another home is
/// refused, as is an id with a database but no recorded home: a planted `owner.json`
/// never reaches another project's database.
pub fn check_home(app: &Path, id: &str, home: &Path) -> Result<(), CmdError> {
    let id = Ulid::parse(id).map_err(|_| bad_home("not a project id"))?;
    let canon = home.canonicalize().unwrap_or_else(|_| home.to_owned());
    match std::fs::read(registry(app, &id)) {
        Ok(b) if b == canon.as_os_str().as_encoded_bytes() => Ok(()),
        Ok(_) => Err(bad_home("this project id belongs to another home")),
        Err(_) => Err(bad_home(
            "a project home not created on this Mac (import it)",
        )),
    }
}

/// Creates a project home with its `owner.json` and records it in app data (the
/// database is created on first open). A human action (tests, importers); agents cannot
/// create homes.
pub fn create_home(app: &Path, home: &Path, name: &str) -> std::io::Result<HomeInfo> {
    std::fs::create_dir_all(home)?;
    let info = HomeInfo {
        id: new_ulid().to_string(),
        name: name.to_owned(),
        host: gmclip_core::local_host(),
        user: std::env::var("USER").unwrap_or_default(),
        since: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0)),
        app_version: env!("CARGO_PKG_VERSION").into(),
    };
    std::fs::write(home.join("owner.json"), serde_json::to_vec_pretty(&info)?)?;
    let id = Ulid::parse(&info.id).map_err(|_| std::io::Error::other("id"))?;
    paths::mkdir_private(&app.join("projects"))?;
    std::fs::write(
        registry(app, &id),
        home.canonicalize()?.as_os_str().as_encoded_bytes(),
    )?;
    Ok(info)
}

/// A new home's `owner.json` (a fresh id, made on this host), bound to no home yet: a
/// consolidated copy (L.06) is bound where it is opened ([`adopt_home`]).
pub fn new_home_info(name: &str) -> HomeInfo {
    HomeInfo {
        id: new_ulid().to_string(),
        name: name.to_owned(),
        host: gmclip_core::local_host(),
        user: std::env::var("USER").unwrap_or_default(),
        since: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0)),
        app_version: env!("CARGO_PKG_VERSION").into(),
    }
}

/// Binds project `id` to `home` on this Mac (a folder made elsewhere — a consolidated copy
/// — opened here by a person). Refused when the id is already bound to another home, so a
/// planted `owner.json` still never reaches another project's database.
pub fn adopt_home(app: &Path, home: &Path, id: &str) -> Result<(), CmdError> {
    let id = Ulid::parse(id).map_err(|_| bad_home("not a project id"))?;
    let canon = home
        .canonicalize()
        .map_err(|_| bad_home("no folder there"))?;
    match std::fs::read(registry(app, &id)) {
        Ok(b) if b == canon.as_os_str().as_encoded_bytes() => Ok(()),
        Ok(_) => Err(bad_home("this project id belongs to another home")),
        Err(_) => {
            paths::mkdir_private(&app.join("projects")).map_err(|_| CmdError::Internal {
                trace_id: "app-data".into(),
            })?;
            std::fs::write(registry(app, &id), canon.as_os_str().as_encoded_bytes()).map_err(|_| {
                CmdError::Internal {
                    trace_id: "app-data".into(),
                }
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hostile_id_never_names_a_file() {
        let app = std::env::temp_dir().join(format!("gmclip-proj-{}", std::process::id()));
        assert!(db_path(&app, "../../.ssh/known_hosts").is_err());
        let p = db_path(&app, "01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap();
        assert!(p.starts_with(app.join("projects")));
        let _ = std::fs::remove_dir_all(&app);
    }

    /// L.06: a folder made elsewhere is bound where it is opened, once; the same id at
    /// another folder is refused (a planted owner.json never reaches the first's data).
    #[test]
    fn a_home_made_elsewhere_is_adopted_once() {
        let root = std::env::temp_dir().join(format!("gmclip-adopt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (app, a, b) = (
            root.join("app"),
            root.join("a.gmclip"),
            root.join("b.gmclip"),
        );
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let info = new_home_info("copy");
        assert!(check_home(&app, &info.id, &a).is_err(), "not yet bound");
        adopt_home(&app, &a, &info.id).unwrap();
        check_home(&app, &info.id, &a).unwrap();
        adopt_home(&app, &a, &info.id).unwrap();
        assert!(
            adopt_home(&app, &b, &info.id).is_err(),
            "the id belongs to a.gmclip"
        );
        assert!(adopt_home(&app, &a, "../../x").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
