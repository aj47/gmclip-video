//! The model register and cache (SPEC §9 R-ML-3, §15.3; SR R-LOAD-1/2).
//!
//! Every file gmclip loads at runtime (model weights and the ONNX Runtime dylib) has a
//! row here with its SHA-256 and size. A file loads only from an **absolute** path under
//! the cache root (or, in a release, the signed bundle) and only after its hash equals
//! the pin; a symlink is refused rather than followed. Nothing in this module talks to
//! the network: the CDN downloader lands with the CDN (B13). Until then the lab cache
//! is filled by `tools/fetch_models.sh` (dev-only, curl + `shasum`), and a user's
//! "use a local copy" goes through [`ModelCache::install_local_copy`], which refuses
//! a file whose hash is not the pin ("This file is not the model gmclip ships").
//!
//! `spec/model-register.md` is the human-readable register; a test keeps its hashes
//! equal to the ones below.

use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use crate::MlError;
use crate::sha256;

/// One pinned file.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PinnedFile {
    pub name: &'static str,
    pub sha256: &'static str,
    pub size: u64,
    /// Where the lab fetch takes it from (never fetched by the app; R-ML-3's CDN replaces it).
    pub upstream: &'static str,
}

/// One register row.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ModelEntry {
    pub id: &'static str,
    /// Directory under the cache root.
    pub dir: &'static str,
    pub weights_licence: &'static str,
    pub code_licence: &'static str,
    pub source: &'static str,
    pub files: &'static [PinnedFile],
}

macro_rules! hf {
    ($f:literal) => {
        concat!(
            "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce/",
            $f
        )
    };
}

/// Parakeet TDT 0.6B v3, int8 ONNX export (encoder + decoder/joint + vocabulary).
pub const PARAKEET_TDT_V3_INT8: ModelEntry = ModelEntry {
    id: "parakeet-tdt-0.6b-v3-int8",
    dir: "models/parakeet-tdt-0.6b-v3-int8",
    weights_licence: "CC-BY-4.0",
    code_licence: "MIT OR Apache-2.0 (parakeet-rs 0.4.1)",
    source: "nvidia/parakeet-tdt-0.6b-v3, ONNX by istupakov/parakeet-tdt-0.6b-v3-onnx @ 8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce",
    files: &[
        PinnedFile {
            name: "encoder-model.int8.onnx",
            sha256: "6139d2fa7e1b086097b277c7149725edbab89cc7c7ae64b23c741be4055aff09",
            size: 652_183_999,
            upstream: hf!("encoder-model.int8.onnx"),
        },
        PinnedFile {
            name: "decoder_joint-model.int8.onnx",
            sha256: "eea7483ee3d1a30375daedc8ed83e3960c91b098812127a0d99d1c8977667a70",
            size: 18_202_004,
            upstream: hf!("decoder_joint-model.int8.onnx"),
        },
        PinnedFile {
            name: "vocab.txt",
            sha256: "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
            size: 93_939,
            upstream: hf!("vocab.txt"),
        },
        PinnedFile {
            name: "config.json",
            sha256: "666903c76b9798caf2c210afd4f6cd60b08a8dbf9800ec8d7a3bc0d2148ac466",
            size: 97,
            upstream: hf!("config.json"),
        },
    ],
};

/// The forced-alignment pieces for the same model (decision 0006): NeMo's preprocessor
/// exported to ONNX and the fp32 decoder/joint (the int8 joint quantises activations per
/// call, so a joint grid would not equal frame-by-frame steps). The encoder and the
/// vocabulary come from [`PARAKEET_TDT_V3_INT8`]. A directory of its own, so parakeet-rs
/// (which prefers an fp32 joint when it sees one) keeps decoding with the int8 one.
pub const PARAKEET_ALIGNER: ModelEntry = ModelEntry {
    id: "parakeet-tdt-0.6b-v3-aligner",
    dir: "models/parakeet-tdt-0.6b-v3-aligner",
    weights_licence: "CC-BY-4.0",
    code_licence: "gmclip (own TDT forward algorithm)",
    source: "nvidia/parakeet-tdt-0.6b-v3, ONNX by istupakov/parakeet-tdt-0.6b-v3-onnx @ 8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce",
    files: &[
        PinnedFile {
            name: "nemo128.onnx",
            sha256: "a9fde1486ebfcc08f328d75ad4610c67835fea58c73ba57e3209a6f6cf019e9f",
            size: 139_764,
            upstream: hf!("nemo128.onnx"),
        },
        PinnedFile {
            name: "decoder_joint-model.onnx",
            sha256: "e978ddf6688527182c10fde2eb4b83068421648985ef23f7a86be732be8706c1",
            size: 72_520_893,
            upstream: hf!("decoder_joint-model.onnx"),
        },
    ],
};

/// Silero VAD v6.2.1 (MIT).
pub const SILERO_VAD_V6_2_1: ModelEntry = ModelEntry {
    id: "silero-vad-v6.2.1",
    dir: "models/silero-vad-v6.2.1",
    weights_licence: "MIT",
    code_licence: "MIT",
    source: "snakers4/silero-vad tag v6.2.1, src/silero_vad/data/silero_vad.onnx",
    files: &[
        PinnedFile {
            name: "silero_vad.onnx",
            sha256: "1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3",
            size: 2_327_524,
            upstream: "https://raw.githubusercontent.com/snakers4/silero-vad/v6.2.1/src/silero_vad/data/silero_vad.onnx",
        },
        PinnedFile {
            name: "LICENSE",
            sha256: "2e63e9a38b6e8fc0c7bc37ce174caca1862870856c6daf5697cfb785e925520b",
            size: 1_075,
            upstream: "https://raw.githubusercontent.com/snakers4/silero-vad/v6.2.1/LICENSE",
        },
    ],
};

/// ONNX Runtime 1.30.0, Microsoft's macOS arm64 release (MIT). One per process.
pub const ORT_1_30_0_MACOS_ARM64: ModelEntry = ModelEntry {
    id: "onnxruntime-1.30.0-osx-arm64",
    dir: "models/onnxruntime-1.30.0",
    weights_licence: "n/a",
    code_licence: "MIT (+ ThirdPartyNotices.txt)",
    source: "github.com/microsoft/onnxruntime release v1.30.0 (commit f2c39fe2f838cf35ce7da92824f5a5e3ee6e88a7), onnxruntime-osx-arm64-1.30.0.tgz sha256 6ebb5062a934537c352937821f9fe9718e7de1a2db1122a93dd363ffd53a7012",
    files: &[PinnedFile {
        name: "libonnxruntime.1.30.0.dylib",
        sha256: "bcc9110f9d638a119de2db7afb3ba9a1da8085f0cb3401e1c48ae1caf450b6fa",
        size: 43_879_424,
        upstream: "https://github.com/microsoft/onnxruntime/releases/download/v1.30.0/onnxruntime-osx-arm64-1.30.0.tgz",
    }],
};

/// ONNX Runtime 1.30.0, Microsoft's Windows x64 release (MIT; CPU execution provider).
/// The DLL is Authenticode-signed by Microsoft Corporation (checked when pinned).
pub const ORT_1_30_0_WINDOWS_X64: ModelEntry = ModelEntry {
    id: "onnxruntime-1.30.0-win-x64",
    dir: "models/onnxruntime-1.30.0-win-x64",
    weights_licence: "n/a",
    code_licence: "MIT (+ ThirdPartyNotices.txt)",
    source: "github.com/microsoft/onnxruntime release v1.30.0 (commit f2c39fe2f838cf35ce7da92824f5a5e3ee6e88a7), onnxruntime-win-x64-1.30.0.zip sha256 c6ba983baf5681af108599675d2a89c2d145512d02de28aed0bff177cd0ba949",
    files: &[PinnedFile {
        name: "onnxruntime.dll",
        sha256: "7e39e2bdbba836d98071ef28620735ba36a47c554cf794585269aecc50fab0da",
        size: 16_462_648,
        upstream: "https://github.com/microsoft/onnxruntime/releases/download/v1.30.0/onnxruntime-win-x64-1.30.0.zip",
    }],
};

/// Every ONNX Runtime build in the register; a machine needs only its own
/// ([`crate::runtime::platform_entry`]).
pub const ORT_BUILDS: &[&ModelEntry] = &[&ORT_1_30_0_MACOS_ARM64, &ORT_1_30_0_WINDOWS_X64];

/// Does this machine need `e`? Every model does; of the runtimes, only this platform's.
pub fn needed_here(e: &ModelEntry) -> bool {
    if ORT_BUILDS
        .iter()
        .any(|o| std::ptr::eq(*o, e) || o.id == e.id)
    {
        crate::runtime::platform_entry().is_some_and(|p| p.id == e.id)
    } else {
        true
    }
}

/// all-MiniLM-L6-v2 (Apache-2.0): 384-d sentence embeddings for search by meaning (L.02),
/// the fp32 ONNX export in the upstream repository and its WordPiece vocabulary.
pub const MINILM_L6_V2: ModelEntry = ModelEntry {
    id: "all-minilm-l6-v2",
    dir: "models/all-minilm-l6-v2",
    weights_licence: "Apache-2.0",
    code_licence: "gmclip (own WordPiece tokenizer and mean pooling)",
    source: "sentence-transformers/all-MiniLM-L6-v2 @ 1110a243fdf4706b3f48f1d95db1a4f5529b4d41",
    files: &[
        PinnedFile {
            name: "model.onnx",
            sha256: "6fd5d72fe4589f189f8ebc006442dbb529bb7ce38f8082112682524616046452",
            size: 90_405_214,
            upstream: "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/1110a243fdf4706b3f48f1d95db1a4f5529b4d41/onnx/model.onnx",
        },
        PinnedFile {
            name: "vocab.txt",
            sha256: "07eced375cec144d27c900241f3e339478dec958f92fddbc551f295c992038a3",
            size: 231_508,
            upstream: "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/1110a243fdf4706b3f48f1d95db1a4f5529b4d41/vocab.txt",
        },
    ],
};

/// Every register row.
pub const REGISTER: &[&ModelEntry] = &[
    &PARAKEET_TDT_V3_INT8,
    &PARAKEET_ALIGNER,
    &SILERO_VAD_V6_2_1,
    &ORT_1_30_0_MACOS_ARM64,
    &ORT_1_30_0_WINDOWS_X64,
    &MINILM_L6_V2,
];

/// The cache root: `<OS cache dir>/gmclip` (`~/Library/Caches/gmclip` on macOS).
pub fn default_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let p = if cfg!(target_os = "macos") {
        home?.join("Library/Caches/gmclip")
    } else if cfg!(windows) {
        PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("gmclip")
    } else {
        match std::env::var_os("XDG_CACHE_HOME") {
            Some(x) => PathBuf::from(x).join("gmclip"),
            None => home?.join(".cache/gmclip"),
        }
    };
    p.is_absolute().then_some(p)
}

/// A model whose files all matched their pins.
#[derive(Clone, Debug)]
pub struct VerifiedModel {
    pub entry: &'static ModelEntry,
    pub dir: PathBuf,
}

impl VerifiedModel {
    pub fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

/// The model cache.
#[derive(Clone, Debug)]
pub struct ModelCache {
    root: PathBuf,
}

/// Text files a model directory may hold besides its pins (never loaded).
pub const DOC_FILES: &[&str] = &[
    "LICENSE",
    "NOTICE",
    "MODEL_CARD.md",
    "ThirdPartyNotices.txt",
];

/// Creates `dir` (and parents) owner-only (0700 on Unix; SR R-PROTO-4).
fn create_private_dir(dir: &Path) -> Result<(), MlError> {
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir)
        .map_err(|_| MlError::Load("cannot create the cache"))
}

/// The user-facing refusal for a wrong local copy (R-LOAD-2).
pub const NOT_OUR_MODEL: &str = "This file is not the model gmclip ships";

fn regular_file_len(p: &Path) -> Option<u64> {
    let m = fs::symlink_metadata(p).ok()?;
    m.file_type().is_file().then_some(m.len())
}

impl ModelCache {
    /// `root` must be absolute (R-LOAD-1: nothing loads from a relative path).
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, MlError> {
        let root = root.into();
        if !root.is_absolute() {
            return Err(MlError::Load("cache root is not absolute"));
        }
        Ok(Self { root })
    }

    pub fn default_cache() -> Result<Self, MlError> {
        Self::new(default_root().ok_or(MlError::Load("no cache directory"))?)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn dir(&self, e: &ModelEntry) -> PathBuf {
        self.root.join(e.dir)
    }

    /// Checks every file of `e`: a regular file (not a symlink), the pinned size, the
    /// pinned SHA-256.
    ///
    /// The directory may hold nothing else but licence texts ([`DOC_FILES`]): a loader
    /// that picks files by name (parakeet-rs prefers `encoder-model.onnx` over the pinned
    /// `.int8.` file, and falls back to any `encoder*.onnx`) must find only pinned files.
    pub fn verify(&self, e: &'static ModelEntry) -> Result<VerifiedModel, MlError> {
        self.verify_until(e, &|| false)
    }

    /// [`ModelCache::verify`], stopping with [`MlError::Cancelled`] when `stop` says so
    /// (checked every MiB of the hashing).
    pub fn verify_until(
        &self,
        e: &'static ModelEntry,
        stop: &dyn Fn() -> bool,
    ) -> Result<VerifiedModel, MlError> {
        let dir = self.dir(e);
        if fs::symlink_metadata(&dir)
            .map(|m| !m.file_type().is_dir())
            .unwrap_or(true)
        {
            return Err(MlError::ModelMissing(e.id.to_owned()));
        }
        for entry in
            fs::read_dir(&dir).map_err(|_| MlError::Load("cannot read the model directory"))?
        {
            let entry = entry.map_err(|_| MlError::Load("cannot read the model directory"))?;
            let name = entry.file_name();
            let name = name.to_str().unwrap_or("");
            if !e.files.iter().any(|f| f.name == name) && !DOC_FILES.contains(&name) {
                return Err(MlError::Refused("unpinned file in the model directory"));
            }
        }
        for f in e.files {
            check_file(&dir.join(f.name), f, stop)?;
        }
        Ok(VerifiedModel { entry: e, dir })
    }

    /// "Use a local copy" (R-LOAD-2): copies `src` in as `name` only if its hash is the
    /// pin. Written to a temp file, re-hashed, then renamed into place.
    pub fn install_local_copy(
        &self,
        e: &'static ModelEntry,
        name: &str,
        src: &Path,
    ) -> Result<PathBuf, MlError> {
        let pin = e
            .files
            .iter()
            .find(|f| f.name == name)
            .ok_or(MlError::Load("not a file of this model"))?;
        if regular_file_len(src) != Some(pin.size) {
            return Err(MlError::Refused(NOT_OUR_MODEL));
        }
        let (h, _) = sha256::file_hex(src).map_err(|_| MlError::Load("cannot read the copy"))?;
        if h != pin.sha256 {
            return Err(MlError::Refused(NOT_OUR_MODEL));
        }
        let dir = self.dir(e);
        create_private_dir(&dir)?;
        let dst = dir.join(pin.name);
        // a staging name outside the model directory (which must hold only pins), created
        // fresh: never through a link someone left at that path
        let tmp = self.root.join(format!(".{}.{}.partial", e.id, pin.name));
        let _ = fs::remove_file(&tmp);
        {
            let mut out = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .map_err(|_| MlError::Load("cannot copy"))?;
            let mut inp = fs::File::open(src).map_err(|_| MlError::Load("cannot copy"))?;
            std::io::copy(&mut inp, &mut out).map_err(|_| MlError::Load("cannot copy"))?;
            out.flush().map_err(|_| MlError::Load("cannot copy"))?;
        }
        if let Err(err) = check_file(&tmp, pin, &|| false) {
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }
        fs::rename(&tmp, &dst).map_err(|_| MlError::Load("cannot install"))?;
        Ok(dst)
    }

    /// Installs a model archive (plain ustar, as R-ML-3's CDN will serve it) safely
    /// (R-LOAD-2): entries with an absolute path, a `..` component, a symlink or hard
    /// link, or any type but file/directory are refused; only files named in the
    /// register row are extracted (plus `LICENSE`, `NOTICE`, `MODEL_CARD.md`); the
    /// extracted total may not exceed 1.5 × `declared_size`. Everything lands in a
    /// staging directory and is hash-checked before it is moved into place.
    pub fn install_archive(
        &self,
        e: &'static ModelEntry,
        archive: &Path,
        declared_size: u64,
    ) -> Result<VerifiedModel, MlError> {
        let dir = self.dir(e);
        let staging = self.root.join(format!(".{}.staging", e.id));
        let _ = fs::remove_dir_all(&staging);
        create_private_dir(&staging)?;
        let result = (|| {
            let f = fs::File::open(archive).map_err(|_| MlError::Load("cannot open archive"))?;
            extract_ustar(f, &staging, e, declared_size)?;
            for pin in e.files {
                check_file(&staging.join(pin.name), pin, &|| false)?;
            }
            // the directory is replaced whole: no file of an older install survives
            let _ = fs::remove_dir_all(&dir);
            create_private_dir(&dir)?;
            for entry in fs::read_dir(&staging).map_err(|_| MlError::Load("staging"))? {
                let entry = entry.map_err(|_| MlError::Load("staging"))?;
                fs::rename(entry.path(), dir.join(entry.file_name()))
                    .map_err(|_| MlError::Load("cannot install"))?;
            }
            Ok(())
        })();
        let _ = fs::remove_dir_all(&staging);
        result?;
        self.verify(e)
    }
}

fn check_file(p: &Path, pin: &PinnedFile, stop: &dyn Fn() -> bool) -> Result<(), MlError> {
    match regular_file_len(p) {
        None => return Err(MlError::ModelMissing(pin.name.to_owned())),
        Some(n) if n != pin.size => return Err(MlError::Refused(NOT_OUR_MODEL)),
        Some(_) => {}
    }
    let (h, _) = sha256::file_hex_until(p, stop)
        .map_err(|_| MlError::Load("cannot read a model file"))?
        .ok_or(MlError::Cancelled)?;
    if h != pin.sha256 {
        return Err(MlError::Refused(NOT_OUR_MODEL));
    }
    Ok(())
}

/// Files an archive may carry besides the pinned ones (R-ML-3).
const ARCHIVE_EXTRAS: &[&str] = DOC_FILES;

fn octal(field: &[u8]) -> Result<u64, MlError> {
    let s = field
        .iter()
        .take_while(|&&b| b != 0 && b != b' ')
        .copied()
        .collect::<Vec<u8>>();
    let s = std::str::from_utf8(&s).map_err(|_| MlError::Refused("archive header"))?;
    let s = s.trim_start_matches(' ');
    if s.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(s, 8).map_err(|_| MlError::Refused("archive header"))
}

fn cstr(field: &[u8]) -> Result<&str, MlError> {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    std::str::from_utf8(&field[..end]).map_err(|_| MlError::Refused("archive name"))
}

/// A safe relative path: no root, prefix, `..` or `.` tricks, no backslashes.
fn safe_relative(name: &str) -> Result<PathBuf, MlError> {
    if name.is_empty() || name.contains('\\') || name.contains(':') {
        return Err(MlError::Refused("archive path"));
    }
    let p = Path::new(name);
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(MlError::Refused("archive path"));
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(MlError::Refused("archive path"));
    }
    Ok(out)
}

fn extract_ustar(
    mut r: impl Read,
    dst: &Path,
    e: &ModelEntry,
    declared_size: u64,
) -> Result<(), MlError> {
    // 1.5 × the declared size (R-LOAD-2), and never more than the pins plus 1 MiB of
    // licence text, whatever the caller declared
    let pinned: u64 = e.files.iter().map(|f| f.size).sum();
    let cap = (declared_size.saturating_mul(3) / 2).min(pinned + (1 << 20));
    let mut total = 0u64;
    let mut header = [0u8; 512];
    let mut zero_blocks = 0;
    loop {
        if r.read_exact(&mut header).is_err() {
            return Err(MlError::Refused("archive truncated"));
        }
        if header.iter().all(|&b| b == 0) {
            zero_blocks += 1;
            if zero_blocks == 2 {
                return Ok(());
            }
            continue;
        }
        zero_blocks = 0;
        // checksum: sum of the header with the checksum field as spaces
        let want = octal(&header[148..156])?;
        let got: u64 = header
            .iter()
            .enumerate()
            .map(|(i, &b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    u64::from(b)
                }
            })
            .sum();
        if want != got {
            return Err(MlError::Refused("archive checksum"));
        }
        if &header[257..262] != b"ustar" {
            return Err(MlError::Refused("not a ustar archive"));
        }
        let name = cstr(&header[0..100])?;
        let prefix = cstr(&header[345..500])?;
        let full = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        let size = octal(&header[124..136])?;
        let kind = header[156];
        let rel = safe_relative(&full)?;
        match kind {
            b'5' => {
                // directories carry no data; nothing is created for them
                if size != 0 {
                    return Err(MlError::Refused("archive directory with data"));
                }
                continue;
            }
            b'0' | 0 => {}
            // '1' hard link, '2' symlink, and every extension header (pax, GNU long
            // names) are refused outright.
            _ => return Err(MlError::Refused("archive entry type")),
        }
        total = total.saturating_add(size);
        if total > cap {
            return Err(MlError::Refused("archive larger than declared"));
        }
        // flat model archives: allow a single leading directory, keep the file name
        let file_name = rel
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or(MlError::Refused("archive path"))?
            .to_owned();
        if rel.components().count() > 2 {
            return Err(MlError::Refused("archive path"));
        }
        let pin = e.files.iter().find(|f| f.name == file_name);
        if pin.is_some_and(|p| p.size != size) {
            return Err(MlError::Refused(NOT_OUR_MODEL));
        }
        let wanted = pin.is_some() || ARCHIVE_EXTRAS.contains(&file_name.as_str());
        let padded = size.div_ceil(512) * 512;
        if !wanted {
            // skip its data
            std::io::copy(&mut (&mut r).take(padded), &mut std::io::sink())
                .map_err(|_| MlError::Refused("archive truncated"))?;
            continue;
        }
        let out_path = dst.join(&file_name);
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&out_path)
            .map_err(|_| MlError::Refused("archive repeats a file"))?;
        let copied = std::io::copy(&mut (&mut r).take(size), &mut out)
            .map_err(|_| MlError::Load("cannot extract"))?;
        if copied != size {
            return Err(MlError::Refused("archive truncated"));
        }
        out.flush().map_err(|_| MlError::Load("cannot extract"))?;
        std::io::copy(&mut (&mut r).take(padded - size), &mut std::io::sink())
            .map_err(|_| MlError::Refused("archive truncated"))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static TINY: ModelEntry = ModelEntry {
        id: "tiny",
        dir: "models/tiny",
        weights_licence: "MIT",
        code_licence: "MIT",
        source: "test",
        files: &[PinnedFile {
            name: "w.bin",
            // sha256("weights")
            sha256: "fdbd8fe5c5d9e5d5c5d2df7d2b2a0e7b5b0a7dbf2f0d2d38b8d3b70b1d6c0bd0",
            size: 7,
            upstream: "",
        }],
    };

    fn tiny() -> &'static ModelEntry {
        // the pin is computed, so the test does not depend on a hand-copied digest
        static E: std::sync::OnceLock<ModelEntry> = std::sync::OnceLock::new();
        static F: std::sync::OnceLock<[PinnedFile; 1]> = std::sync::OnceLock::new();
        let files = F.get_or_init(|| {
            let h: &'static str = Box::leak(sha256::hex(b"weights").into_boxed_str());
            [PinnedFile {
                sha256: h,
                ..TINY.files[0]
            }]
        });
        E.get_or_init(|| ModelEntry { files, ..TINY })
    }

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "gmclip-ml-models-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// A ustar archive from `(name, typeflag, data)` entries.
    fn ustar(entries: &[(&str, u8, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, kind, data) in entries {
            let mut h = [0u8; 512];
            h[..name.len()].copy_from_slice(name.as_bytes());
            h[100..107].copy_from_slice(b"0000644");
            h[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
            h[136..147].copy_from_slice(b"00000000000");
            h[156] = *kind;
            if *kind == b'2' {
                h[157..163].copy_from_slice(b"/etc/x");
            }
            h[257..263].copy_from_slice(b"ustar\0");
            h[263..265].copy_from_slice(b"00");
            h[148..156].copy_from_slice(b"        ");
            let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
            h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
            out.extend_from_slice(&h);
            out.extend_from_slice(data);
            out.resize(out.len().div_ceil(512) * 512, 0);
        }
        out.extend_from_slice(&[0u8; 1024]);
        out
    }

    #[test]
    fn relative_roots_are_refused() {
        assert!(ModelCache::new("models").is_err());
    }

    #[test]
    fn local_copy_needs_the_pinned_hash() {
        let root = scratch("copy");
        let cache = ModelCache::new(&root).unwrap();
        let wrong = root.join("wrong.bin");
        fs::write(&wrong, b"weighTs").unwrap();
        assert_eq!(
            cache
                .install_local_copy(tiny(), "w.bin", &wrong)
                .unwrap_err(),
            MlError::Refused(NOT_OUR_MODEL)
        );
        assert!(cache.verify(tiny()).is_err());
        let right = root.join("right.bin");
        fs::write(&right, b"weights").unwrap();
        cache.install_local_copy(tiny(), "w.bin", &right).unwrap();
        cache.verify(tiny()).unwrap();
        // an unpinned model file beside the pins is refused (a loader might pick it)
        fs::write(cache.dir(tiny()).join("encoder-model.onnx"), b"x").unwrap();
        assert_eq!(
            cache.verify(tiny()).unwrap_err(),
            MlError::Refused("unpinned file in the model directory")
        );
        fs::remove_file(cache.dir(tiny()).join("encoder-model.onnx")).unwrap();
        fs::write(cache.dir(tiny()).join("LICENSE"), b"MIT").unwrap();
        cache.verify(tiny()).unwrap();
        // a file swapped after install is caught at load
        fs::write(cache.dir(tiny()).join("w.bin"), b"WEIGHTS").unwrap();
        assert_eq!(
            cache.verify(tiny()).unwrap_err(),
            MlError::Refused(NOT_OUR_MODEL)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_model_is_not_followed() {
        let root = scratch("link");
        let cache = ModelCache::new(&root).unwrap();
        let real = root.join("real.bin");
        fs::write(&real, b"weights").unwrap();
        fs::create_dir_all(cache.dir(tiny())).unwrap();
        std::os::unix::fs::symlink(&real, cache.dir(tiny()).join("w.bin")).unwrap();
        assert!(matches!(
            cache.verify(tiny()),
            Err(MlError::ModelMissing(_))
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn archives_install_safely() {
        let root = scratch("tar");
        let cache = ModelCache::new(&root).unwrap();
        let ok = root.join("ok.tar");
        fs::write(
            &ok,
            ustar(&[
                ("tiny/", b'5', b""),
                ("tiny/w.bin", b'0', b"weights"),
                ("tiny/LICENSE", b'0', b"MIT"),
                ("tiny/other.txt", b'0', b"skipped"),
            ]),
        )
        .unwrap();
        cache.install_archive(tiny(), &ok, 20).unwrap();
        assert!(cache.dir(tiny()).join("LICENSE").exists());
        assert!(!cache.dir(tiny()).join("other.txt").exists());

        type Entry<'a> = (&'a str, u8, &'a [u8]);
        let bad: &[(&str, Vec<Entry<'_>>, u64)] = &[
            ("traversal", vec![("../w.bin", b'0', b"weights")], 20),
            ("absolute", vec![("/tmp/w.bin", b'0', b"weights")], 20),
            (
                "nested-dotdot",
                vec![("tiny/../../w.bin", b'0', b"weights")],
                20,
            ),
            ("symlink", vec![("tiny/w.bin", b'2', b"")], 20),
            ("hardlink", vec![("tiny/w.bin", b'1', b"")], 20),
            ("pax", vec![("tiny/w.bin", b'x', b"")], 20),
            ("too-big", vec![("tiny/w.bin", b'0', b"weights")], 4),
            ("wrong-hash", vec![("tiny/w.bin", b'0', b"weighTs")], 20),
            (
                "repeat",
                vec![
                    ("tiny/w.bin", b'0', b"weights"),
                    ("w.bin", b'0', b"weights"),
                ],
                20,
            ),
        ];
        for (tag, entries, declared) in bad {
            let root2 = scratch(tag);
            let cache2 = ModelCache::new(&root2).unwrap();
            let p = root2.join("a.tar");
            fs::write(&p, ustar(entries)).unwrap();
            assert!(
                matches!(
                    cache2.install_archive(tiny(), &p, *declared),
                    Err(MlError::Refused(_))
                ),
                "{tag}"
            );
            assert!(!cache2.dir(tiny()).join("w.bin").exists(), "{tag}");
            assert!(
                !root2.join("w.bin").exists() && !root2.parent().unwrap().join("w.bin").exists(),
                "{tag}"
            );
            let _ = fs::remove_dir_all(root2);
        }
        // truncated
        let t = root.join("t.tar");
        let mut bytes = ustar(&[("tiny/w.bin", b'0', b"weights")]);
        bytes.truncate(600);
        fs::write(&t, bytes).unwrap();
        assert!(cache.install_archive(tiny(), &t, 20).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn register_rows_are_well_formed() {
        let mut ids = std::collections::BTreeSet::new();
        for e in REGISTER {
            assert!(ids.insert(e.id), "duplicate id {}", e.id);
            assert!(!e.files.is_empty());
            for f in e.files {
                assert_eq!(f.sha256.len(), 64);
                assert!(
                    f.sha256
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                );
                assert!(f.size > 0);
                safe_relative(f.name).unwrap();
            }
        }
    }

    /// Every model is needed everywhere; of the runtimes, exactly this platform's pin.
    #[test]
    fn a_machine_needs_only_its_own_runtime() {
        let here: Vec<&str> = ORT_BUILDS
            .iter()
            .filter(|e| needed_here(e))
            .map(|e| e.id)
            .collect();
        let want: Vec<&str> = crate::runtime::platform_entry()
            .map(|e| e.id)
            .into_iter()
            .collect();
        assert_eq!(here, want);
        for e in REGISTER {
            if !ORT_BUILDS.iter().any(|o| o.id == e.id) {
                assert!(needed_here(e), "{}", e.id);
            }
        }
    }

    #[test]
    fn the_spec_register_lists_every_pin() {
        let md = include_str!("../../../spec/model-register.md");
        for e in REGISTER {
            for f in e.files {
                assert!(
                    md.contains(f.sha256),
                    "spec/model-register.md lacks {} {}",
                    e.id,
                    f.name
                );
            }
        }
    }
}
