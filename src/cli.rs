//! The command line.
use crate::{images, Options, Res};
use std::path::{Path, PathBuf};
/// `None` when there is nothing to run: after `--help`, or with no arguments at all.
pub fn parse(args: impl Iterator<Item = String>) -> Res<Option<Options>> {
    let mut args = args.peekable();
    if args.peek().is_none() {
        help();
        return Ok(None);
    }
    let mut input = None;
    let mut output = None;
    let mut tmp_dir = None;
    let mut center = false;
    let mut z_up = false;
    let mut progress = false;
    let mut keep_tmp = false;
    let mut no_color = false;
    let mut trusted = false;
    let mut exact_paths = false;
    let mut inspect = false;
    // The first option given that only means something to a conversion.
    let mut converting = None;
    let mut images = images::Options::default();
    while let Some(arg) = args.next() {
        if matches!(
            arg.as_str(),
            "-o" | "--output"
                | "--tmp-dir"
                | "--center"
                | "--up-axis"
                | "--images"
                | "--progress"
                | "--keep-tmp"
                | "--no-color"
        ) || arg.starts_with("--images=")
        {
            converting.get_or_insert_with(|| arg.clone());
        }
        match arg.as_str() {
            "--help" | "-h" => {
                help();
                return Ok(None);
            }
            "-o" | "--output" => {
                output = Some(PathBuf::from(
                    args.next().ok_or("--output requires a path")?,
                ))
            }
            "--tmp-dir" => {
                tmp_dir = Some(PathBuf::from(
                    args.next().ok_or("--tmp-dir requires a path")?,
                ))
            }
            "--center" => center = true,
            "--up-axis" => {
                z_up = match args.next().as_deref() {
                    Some("y") => false,
                    Some("z") => true,
                    _ => return Err("--up-axis must be y or z".into()),
                }
            }
            "--images" => {
                images.mode = images::Mode::parse(
                    &args.next().ok_or("--images requires auto, keep or copy")?,
                )?
            }
            _ if arg.starts_with("--images=") => {
                images.mode = images::Mode::parse(arg.split_once('=').unwrap().1)?
            }
            "--progress" => progress = true,
            "--keep-tmp" => keep_tmp = true,
            "--no-color" => no_color = true,
            "--trusted" => trusted = true,
            "--exact-paths" => exact_paths = true,
            "--inspect" => inspect = true,
            _ if arg.starts_with('-') => return Err(format!("Unknown option: {arg}").into()),
            _ if input.is_none() => input = Some(PathBuf::from(arg)),
            _ => return Err(format!("Unexpected argument: {arg}").into()),
        }
    }
    let input = input.ok_or("Missing input file")?;
    if let Some(option) = converting.filter(|_| inspect) {
        return Err(format!("--inspect converts nothing, so it doesn't take {option}").into());
    }
    let output: PathBuf = output.unwrap_or_else(|| input.with_extension("gltf"));
    let glb = match output.extension().and_then(|s| s.to_str()) {
        Some("gltf") => false,
        Some("glb") => true,
        _ if inspect => false,
        _ => return Err("Output must have a .gltf or .glb extension".into()),
    };
    if glb && matches!(images.mode, images::Mode::Keep) {
        return Err("--images keep doesn't apply to a .glb, which embeds its images".into());
    }
    // Spools can exceed RAM, and the OS temporary directory is often RAM-backed (tmpfs).
    let tmp_dir = tmp_dir.unwrap_or_else(|| {
        let parent = output.parent().filter(|p| !p.as_os_str().is_empty());
        parent.unwrap_or(Path::new(".")).to_owned()
    });
    Ok(Some(Options {
        input,
        output,
        glb,
        tmp_dir,
        center,
        z_up,
        progress,
        keep_tmp,
        images,
        no_color,
        trusted,
        exact_paths,
        inspect,
    }))
}
fn help() {
    println!("Usage: mesh2gltf [OPTIONS] <file.obj|file.ply|file.wrl|file.vrml>\n       mesh2gltf --inspect [--trusted] [--exact-paths] <file.obj|file.mtl|file.ply>\n\nImport OBJ, PLY or VRML IndexedFaceSet geometry into glTF 2.0: .gltf + external .bin, or one .glb.\n\n  -o, --output FILE  Destination .gltf, or .glb (default: input with .gltf extension)\n      --center       Move the bounding-box center to 0, dropping the model's position\n      --up-axis AXIS Source up axis: y (default) or z; output is Y-up\n      --tmp-dir DIR  Disk-backed geometry spools (default: output directory)\n      --keep-tmp     Keep conversion workspace for debugging\n      --images MODE  auto (default), keep source references, or copy images; a .glb embeds them\n      --no-color     Ignore vertex colors, keeping the material color on untextured faces\n      --trusted      Let the model refer to files outside its folder\n      --exact-paths  Follow references only to their written path, never by name\n      --inspect      Print, as JSON, the files an .obj, .mtl or .ply refers to; convert nothing\n      --progress     Report conversion progress on stderr\n  -h, --help         Show help");
}
