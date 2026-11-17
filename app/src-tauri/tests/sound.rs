//! Reel-tools parity: gain over a span (RT's program gain envelopes), part gain, hiding
//! the camera over a span (a hide splits the parts at its edges). On AJ's h79 reel.

use std::path::Path;

use gmclip_app::core::{AppCore, Dirs};

/// `h79/h79-v4.mp4` in the lab media folder.
fn video() -> &'static str {
    static P: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| gmclip_model::lab::media_str("h79/h79-v4.mp4"));
    P.as_str()
}

#[test]
fn gain_spans_part_gain_and_camera_hides() {
    if !Path::new(video()).is_file() {
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-snd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );

    gmclip_app::sound::gain_span(&c, &pid, &cut, 300, 450, 10.0).unwrap();
    let pl = c.planned(&pid, &cut).unwrap().plan;
    let env = &pl.audio.envelopes.as_ref().unwrap()[0];
    eprintln!("{:?}", env);
    assert_eq!(env.gain_db, 10.0);
    assert_eq!(env.out_samples.start(), 300 * 1600);
    assert_eq!(env.out_samples.end(), 450 * 1600);
    assert_eq!(env.ramp_in_samples, 2400);
    let sv = gmclip_app::sound::view(&c, &pid, &cut).unwrap();
    assert_eq!(sv["gains"][0]["from"], 300);
    assert_eq!(sv["gains"][0]["to"], 450);

    let part = sv["parts"][0]["id"].as_str().unwrap().to_owned();
    gmclip_app::sound::part_gain(&c, &pid, &part, -3.0).unwrap();
    let pl = c.planned(&pid, &cut).unwrap().plan;
    assert_eq!(pl.parts[0].audio.gain_db, Some(-3.0));

    gmclip_app::sound::hide_cam(&c, &pid, &cut, 600, 900).unwrap();
    let sv = gmclip_app::sound::view(&c, &pid, &cut).unwrap();
    assert_eq!(
        sv["parts"].as_array().unwrap().len(),
        3,
        "split at both edges: {sv}"
    );
    assert_eq!(sv["hides"][0]["from"], 600);
    assert_eq!(sv["hides"][0]["to"], 900);
    assert!(c.planned(&pid, &cut).is_ok(), "the cut still plans");

    let hide = sv["hides"][0]["id"].as_str().unwrap().to_owned();
    gmclip_app::sound::hide_delete(&c, &pid, &cut, &hide).unwrap();
    let g = sv["gains"][0]["id"].as_str().unwrap().to_owned();
    gmclip_app::sound::gain_delete(&c, &pid, &g).unwrap();
    let sv = gmclip_app::sound::view(&c, &pid, &cut).unwrap();
    assert!(
        sv["hides"].as_array().unwrap().is_empty() && sv["gains"].as_array().unwrap().is_empty()
    );
    // a hide that does not fall on part edges: the planner says which part, the app
    // keeps the last plan
    let sv = gmclip_app::sound::view(&c, &pid, &cut).unwrap();
    let part = sv["parts"][0]["id"].as_str().unwrap().to_owned();
    let p = c.project(&pid).unwrap();
    let src = {
        let doc = p.handle.snapshot().doc.clone();
        let pt: &gmclip_model::entities::Part = doc
            .get(gmclip_app::planner::ulid_id::<gmclip_model::entities::Part>(&part).unwrap())
            .unwrap();
        match pt.media {
            gmclip_model::entities::MediaRef::Source(s) => s.0.to_string(),
            gmclip_model::entities::MediaRef::Group(_) => unreachable!(),
        }
    };
    let raw = |t: i64| serde_json::json!({"raw": {"time": {"clock": "src", "source": src, "t": t}, "reason": "test"}});
    c.call(&pid, "cut_edit", serde_json::json!({"cut": cut, "ops": [
        {"op": "hide_roles_add", "from": raw(705_600_000), "to": raw(2 * 705_600_000), "roles": [{"region": "cam"}]}]}))
        .unwrap();
    let e = c.planned(&pid, &cut).unwrap_err();
    eprintln!("{}", e.message);
    assert!(
        e.message
            .contains("part 1: a hide that starts or ends inside a part"),
        "{}",
        e.message
    );
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// Decision 0051: a 44.1 kHz M4A laid over the whole reel is stored as an asset, planned
/// as a ducked `/1` overlay, and the export's mix (through the real decoders) hears it;
/// changed and removed from the panel's view.
#[cfg(target_os = "macos")]
#[test]
fn music_over_the_reel() {
    if !Path::new(video()).is_file() {
        return;
    }
    let root = std::env::temp_dir().join(format!("gmclip-music-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    // 4 s of a 330 Hz tone, stereo, through AudioToolbox's AAC
    let pcm: Vec<f32> = (0..44_100 * 4)
        .flat_map(|i| {
            let v = (0.3 * (std::f64::consts::TAU * 330.0 * f64::from(i) / 44_100.0).sin()) as f32;
            [v, v]
        })
        .collect();
    let wav = root.join("bed.wav");
    gmclip_media::audio::wav::write_f32(std::fs::File::create(&wav).unwrap(), 44_100, 2, &pcm)
        .unwrap();
    let m4a = root.join("bed.m4a");
    let st = std::process::Command::new("/usr/bin/afconvert")
        .args(["-f", "m4af", "-d", "aac@44100", "-b", "128000"])
        .arg(&wav)
        .arg(&m4a)
        .status()
        .unwrap();
    assert!(st.success());
    let c = AppCore::new(Dirs {
        app_data: root.join("app"),
        library: root.join("lib"),
        exports: root.join("exports"),
    });
    let v = gmclip_app::video::open_video(&c, Path::new(video())).unwrap();
    let (pid, cut) = (
        v["project"].as_str().unwrap().to_owned(),
        v["cut"].as_str().unwrap().to_owned(),
    );
    assert!(
        gmclip_app::sound::music_add(&c, &pid, &cut, wav.with_extension("txt").to_str().unwrap())
            .is_err()
    );
    gmclip_app::sound::music_add(&c, &pid, &cut, m4a.to_str().unwrap()).unwrap();
    let sv = gmclip_app::sound::view(&c, &pid, &cut).unwrap();
    assert_eq!(sv["sounds"][0]["name"], "bed.m4a");
    assert_eq!(sv["sounds"][0]["duck"], true);
    assert_eq!(sv["sounds"][0]["from"], 0);
    let plan = c.planned(&pid, &cut).unwrap().plan;
    assert_eq!(plan.schema, gmclip_plan::consts::SchemaVersion::V1);
    let o = &plan.audio.overlays.as_ref().unwrap()[0];
    assert_eq!((o.out_samples.start(), o.gain_db), (0, -18.0));
    assert!(o.duck.is_some());
    let src = plan.sources.iter().find(|s| s.id == o.source).unwrap();
    assert!(
        src.path.ends_with(".m4a") && Path::new(&src.path).is_file(),
        "{}",
        src.path
    );
    // the export's mix through the real decoders: the music is in it, 2 s in
    let mix = |p: &gmclip_plan::Plan| {
        let mut inputs = gmclip_render::export::DemuxInputs::new(None);
        let mut m = gmclip_render::export::VoiceMix::open(p, &mut inputs).unwrap();
        let mut out = vec![0.0f32; 2 * 48_000];
        m.read(48_000, &mut out).unwrap();
        out
    };
    let mut bare = (*plan).clone();
    bare.audio.overlays = None;
    bare.audio.speech = None;
    let (with, without) = (mix(&plan), mix(&bare));
    let rms = (with
        .iter()
        .zip(&without)
        .map(|(a, b)| f64::from(a - b).powi(2))
        .sum::<f64>()
        / with.len() as f64)
        .sqrt();
    eprintln!("the music in the mix: {rms:.4} RMS");
    // a 0.3 tone at −18 dB is ≈ 0.027 RMS, less where the voice ducks it
    assert!(
        (0.005..0.04).contains(&rms),
        "the music's RMS in the mix: {rms}"
    );
    let id = sv["sounds"][0]["id"].as_str().unwrap().to_owned();
    c.call(
        &pid,
        "audio_overlay_set",
        serde_json::json!({"overlay": id, "gain_db": -30, "no_duck": true}),
    )
    .unwrap();
    let sv = gmclip_app::sound::view(&c, &pid, &cut).unwrap();
    assert_eq!(
        (
            sv["sounds"][0]["gain_db"].as_f64(),
            sv["sounds"][0]["duck"].as_bool()
        ),
        (Some(-30.0), Some(false))
    );
    c.call(
        &pid,
        "audio_overlay_delete",
        serde_json::json!({"overlay": id}),
    )
    .unwrap();
    let sv = gmclip_app::sound::view(&c, &pid, &cut).unwrap();
    assert!(sv["sounds"].as_array().unwrap().is_empty());
    c.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
