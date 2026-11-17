//! A2.17 R-LINT-1: AJ's imported RT reels lint clean — h79 v4 (loc_4fadb756) with no
//! warning, and the calibration over the other imported goldens of that stream reported
//! (rules that fire there are listed, for calibration). Lab: the lab media.

use std::path::Path;

use gmclip_app::core::{AppCore, Dirs};

#[test]
fn imported_goldens_lint_clean() {
    if !gmclip_model::lab::has(&["preview"]) {
        eprintln!("skipped: no lab media");
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-lintg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let mut all = Vec::new();
    for vid in ["loc_4fadb756", "loc_086da141", "loc_bd89f38b"] {
        let Ok(imp) = gmclip_app::labfixture::open_project(&c, vid) else {
            continue;
        };
        let pid = imp["id"].as_str().unwrap().to_owned();
        let p = c.project(&pid).unwrap();
        let doc = p.handle.snapshot().doc.clone();
        for r in imp["fixture"]["cuts"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let (Some(hook), Some(cut)) = (r["hook_id"].as_str(), r["cut"].as_str()) else {
                continue;
            };
            let cid = gmclip_app::planner::ulid_id::<gmclip_model::entities::Cut>(cut).unwrap();
            let f = gmclip_core::lint::lint_cut(&doc, cid);
            eprintln!(
                "{hook}: {:?}",
                f.iter().map(|x| (&x.rule, &x.message)).collect::<Vec<_>>()
            );
            all.push((hook.to_owned(), f));
        }
    }
    // h79 v4: the imported RenderPlan (M0's import of AJ's reel)
    let made = c.create_project("h79").unwrap();
    let pid = made["id"].as_str().unwrap().to_owned();
    let r = c
        .import(
            &pid,
            &gmclip_app::labfixture::plan_copy(
                &Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../schemas/examples/h79-v4.renderplan.json"),
                &root.join("plans"),
            )
            .unwrap(),
        )
        .unwrap();
    let cut = r["cut"].as_str().unwrap();
    let doc = c.project(&pid).unwrap().handle.snapshot().doc.clone();
    let cid = gmclip_app::planner::ulid_id::<gmclip_model::entities::Cut>(cut).unwrap();
    let f = gmclip_core::lint::lint_cut(&doc, cid);
    eprintln!(
        "h79 v4: {:?}",
        f.iter().map(|x| (&x.rule, &x.message)).collect::<Vec<_>>()
    );
    all.push(("h79 v4".into(), f));
    // R-LINT-1: no warning or error on any approved golden (info is the calibrated level)
    let loud: Vec<(String, String)> = all
        .iter()
        .flat_map(|(h, f)| {
            f.iter()
                .filter(|x| x.level != gmclip_core::lint::Level::Info)
                .map(move |x| (h.clone(), x.rule.clone()))
        })
        .collect();
    eprintln!("{} goldens; warnings or errors: {loud:?}", all.len());
    assert!(loud.is_empty(), "{loud:?}");
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
