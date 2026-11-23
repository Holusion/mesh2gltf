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
mod ply;
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
    /// One self-contained `.glb` rather than a `.gltf` beside its `.bin` and images.
    pub glb: bool,
    pub tmp_dir: PathBuf,
    pub center: bool,
    pub z_up: bool,
    pub progress: bool,
    pub keep_tmp: bool,
    pub images: images::Options,
    /// Ignore vertex colours, so that untextured faces show their material's diffuse colour.
    pub no_color: bool,
    /// Follow file references anywhere, rather than only inside the input's directory.
    pub trusted: bool,
    /// Follow references only to their written path, never by name beside the referencing file.
    pub exact_paths: bool,
    /// Report the files the input refers to instead of converting it.
    pub inspect: bool,
}
fn run(mut opts: Options) -> Res<()> {
    if opts.inspect {
        return inspect(&opts);
    }
    opts.images.outside_references = opts.trusted;
    opts.images.embed = opts.glb;
    let ext = opts
        .input
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(ext.as_str(), "obj" | "ply" | "wrl" | "vrml") {
        return Err(format!("Unsupported input format: .{}", diagnostics::safe(&ext)).into());
    }
    // Not `exists`, which follows a link and says no if it leads nowhere.
    let taken = |path: &Path| fs::symlink_metadata(path).is_ok();
    if taken(&opts.output) || (!opts.glb && taken(&opts.output.with_extension("bin"))) {
        let message = "Output .gltf, .glb or .bin already exists; choose a new output path";
        return Err(JobDirectory(io::Error::new(ErrorKind::AlreadyExists, message)).into());
    }
    // With `--tmp-dir` elsewhere, a missing output directory would only show after the import.
    fs::metadata(parent(&opts.output))
        .map_err(JobDirectory)
        .with_context(|| "Output directory".into())?;
    let references = References::for_input(parent(&opts.input), opts.trusted, !opts.exact_paths)
        .with_context(|| "Input directory".into())?;
    let workspace = Workspace::create(&opts.tmp_dir, opts.keep_tmp)?;
    workspace.explain(convert(&opts, &ext, references, &workspace.path))
}
/// Prints `{"requires": [[path, ...], ...]}`: for each file the input refers to, the paths it is
/// looked for at, in order, relative to the input's folder. It is met by the first that exists.
/// Only the input is read, so each file of a model can be inspected alone, in any order, and the
/// files it names need not exist yet.
fn inspect(opts: &Options) -> Res<()> {
    let input = &opts.input;
    let references = References::for_input(parent(input), opts.trusted, !opts.exact_paths)
        .with_context(|| "Input directory".into())?
        .listing();
    let ext = input.extension().and_then(|s| s.to_str()).unwrap_or("");
    match ext.to_ascii_lowercase().as_str() {
        "obj" => obj::inspect(input, &references)?,
        "mtl" => {
            let mut warnings = material::Warnings::new();
            material::load(
                input,
                &references,
                Origin::Model,
                &mut Default::default(),
                &mut warnings,
            )?
        }
        "ply" => ply::inspect(input, &references)?,
        _ => {
            let ext = diagnostics::safe(ext);
            return Err(format!("--inspect reads .obj, .mtl or .ply, not .{ext}").into());
        }
    }
    // A texture shared by several materials is one requirement.
    let mut requires = Vec::new();
    for tries in references.listed() {
        if !requires.contains(&tries) {
            requires.push(tries);
        }
    }
    println!("{}", serde_json::json!({ "requires": requires }));
    Ok(())
}
fn convert(opts: &Options, ext: &str, references: References, workspace: &Path) -> Res<()> {
    let scene = match ext {
        "ply" => ply::spool(
            &opts.input,
            workspace,
            opts.progress,
            &references,
            !opts.no_color,
        )?,
        "obj" => obj::spool(
            &opts.input,
            workspace,
            opts.progress,
            &references,
            Origin::Model,
            !opts.no_color,
        )?,
        _ => {
            let intermediate = workspace.join("source.obj");
            vrml::convert(&opts.input, &intermediate, workspace, opts.progress)?;
            let origin = Origin::GeneratedFrom(&opts.input);
            let colors = !opts.no_color;
            obj::spool(
                &intermediate,
                workspace,
                opts.progress,
                &references,
                origin,
                colors,
            )?
        }
    };
    // Every reference has been followed: the files found by name are reported now.
    drop(references);
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
