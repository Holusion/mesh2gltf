//! Import orchestration. Geometry lives on disk, never in a whole-model structure.
#![deny(clippy::print_stderr)]

mod cli;
mod dedup;
mod diagnostics;
mod disk;
mod failure;
mod fixed;
mod format;
mod gltf;
mod group;
mod images;
mod material;
mod obj;
mod references;
mod smooth;
mod triangulate;
mod vrml;

use disk::{parent, Workspace};
use failure::{Context, JobDirectory};
use material::Origin;
use references::References;
use std::{
    env, fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    process::ExitCode,
};

/// `Send`, so that a thread can hand its failure back with its cause intact.
pub type Failure = Box<dyn std::error::Error + Send + Sync>;
pub type Res<T> = Result<T, Failure>;

fn main() -> ExitCode {
    diagnostics::report_panics();
    let result = match cli::parse(env::args().skip(1)) {
        Ok(Some(options)) => run(options),
        Ok(None) => Ok(()),
        Err(e) => Err(failure::Usage(e).into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            crate::say!("Error: {e}");
            ExitCode::from(failure::classify(&*e) as u8)
        }
    }
}
#[derive(Default)]
pub struct Options {
    pub input: PathBuf,
    pub output: PathBuf,
    pub tmp_dir: PathBuf,
    pub center: bool,
    pub z_up: bool,
    pub progress: bool,
    pub keep_tmp: bool,
    pub images: images::Options,
    /// Drop OBJ vertex colours instead of writing `COLOR_0`/`_COLOR`.
    pub no_color: bool,
    /// Follow file references anywhere, rather than only inside the input's directory.
    pub trusted: bool,
}
fn run(mut opts: Options) -> Res<()> {
    opts.images.outside_references = opts.trusted;
    let ext = opts
        .input
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(ext.as_str(), "obj" | "wrl" | "vrml") {
        return Err(format!("Unsupported input format: .{}", diagnostics::safe(&ext)).into());
    }
    // Not `exists`, which follows a link and says no if it leads nowhere.
    let taken = |path: &Path| fs::symlink_metadata(path).is_ok();
    if taken(&opts.output) || taken(&opts.output.with_extension("bin")) {
        let message = "Output .gltf or .bin already exists; choose a new output path";
        return Err(JobDirectory(io::Error::new(ErrorKind::AlreadyExists, message)).into());
    }
    // With `--tmp-dir` elsewhere, a missing output directory would only show after the import.
    fs::metadata(parent(&opts.output))
        .map_err(JobDirectory)
        .with_context(|| "Output directory".into())?;
    let references = References::for_input(parent(&opts.input), opts.trusted)
        .with_context(|| "Input directory".into())?;
    let workspace = Workspace::create(&opts.tmp_dir, opts.keep_tmp)?;
    workspace.explain(convert(&opts, &ext, references, &workspace.path))
}
fn convert(opts: &Options, ext: &str, references: References, workspace: &Path) -> Res<()> {
    let source = if ext == "obj" {
        opts.input.clone()
    } else {
        let intermediate = workspace.join("source.obj");
        vrml::convert(&opts.input, &intermediate, workspace, opts.progress)?;
        intermediate
    };
    let origin = if ext == "obj" {
        Origin::Model
    } else {
        Origin::GeneratedFrom(&opts.input)
    };
    let scene = obj::spool(
        &source,
        workspace,
        opts.progress,
        &references,
        origin,
        !opts.no_color,
    )?;
    // Faces are checked before the slow work of converting textures and writing geometry.
    let groups = group::group(&scene, workspace, opts.progress)?;
    let images = images::prepare(
        &opts.images,
        &opts.input,
        &opts.output,
        workspace,
        &scene.materials,
    )?;
    gltf::write(scene, &groups, images, workspace, opts)
}
