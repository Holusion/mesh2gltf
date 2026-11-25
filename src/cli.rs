//! The command line.
use crate::{
    diagnostics::{Format, Level},
    images, Options, Res,
};
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
    let mut root = None;
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
            "--log-level" | "--log-format" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("{arg} requires a value"))?;
                log_option(&arg, &value)?
            }
            _ if arg.starts_with("--log-level=") || arg.starts_with("--log-format=") => {
                let (option, value) = arg.split_once('=').unwrap();
                log_option(option, value)?
            }
            "--root" => root = Some(PathBuf::from(args.next().ok_or("--root requires a path")?)),
            _ if arg.starts_with('-') => return Err(format!("Unknown option: {arg}").into()),
            _ if input.is_none() => input = Some(PathBuf::from(arg)),
            _ => return Err(format!("Unexpected argument: {arg}").into()),
        }
    }
    let input = input.ok_or("Missing input file")?;
    if let Some(option) = converting.filter(|_| inspect) {
        return Err(format!("--inspect converts nothing, so it doesn't take {option}").into());
    }
    if root.is_some() && !inspect {
        return Err("--root only applies to --inspect".into());
    }
    if root.is_some() && trusted {
        return Err(
            "--root doesn't apply with --trusted, which lets references go anywhere".into(),
        );
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
        root,
    }))
}
/// `--log-level` and `--log-format`, read before the command line is checked, so that a wrong one
/// is reported as they ask, as meshtinker does. The last valid value of each wins; a wrong value
/// keeps the default, and [`parse`] then reports it.
pub fn log_settings(args: &[String]) -> (Level, Format) {
    let mut settings = (Level::Info, Format::Text);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let (option, value) = match arg.split_once('=') {
            Some((option, value)) => (option, Some(value)),
            None => (arg.as_str(), None),
        };
        // An option's value, such as `-o --log-level`, is still taken for the option: the
        // command line is then wrong anyway, and only the format of its error is at stake.
        let value = || value.or_else(|| args.clone().next().map(String::as_str));
        match option {
            "--log-level" => settings.0 = value().and_then(Level::parse).unwrap_or(settings.0),
            "--log-format" => settings.1 = value().and_then(Format::parse).unwrap_or(settings.1),
            _ => {}
        }
    }
    settings
}
fn log_option(option: &str, value: &str) -> Res<()> {
    let valid = match option {
        "--log-level" => Level::parse(value).is_some(),
        _ => Format::parse(value).is_some(),
    };
    match (valid, option) {
        (true, _) => Ok(()),
        (false, "--log-level") => {
            Err("--log-level must be trace, debug, info, warn or error".into())
        }
        (false, _) => Err("--log-format must be text or json".into()),
    }
}
fn help() {
    println!(
        "\
Usage: mesh2gltf [OPTIONS] <file.obj|file.ply|file.wrl|file.vrml>
       mesh2gltf --inspect [--root DIR] [--trusted] [--exact-paths] <file.obj|file.mtl|file.ply>

Import OBJ, PLY or VRML IndexedFaceSet geometry into glTF 2.0: .gltf + external .bin, or one .glb.

  -o, --output FILE      Destination .gltf, or .glb (default: input with .gltf extension)
      --center           Move the bounding-box center to 0, dropping the model's position
      --up-axis AXIS     Source up axis: y (default) or z; output is Y-up
      --tmp-dir DIR      Disk-backed geometry spools (default: output directory)
      --keep-tmp         Keep conversion workspace for debugging
      --images MODE      auto (default), keep source references, or copy images; a .glb embeds them
      --no-color         Ignore vertex colors, keeping the material color on untextured faces
      --trusted          Let the model refer to files outside its folder
      --exact-paths      Follow references only to their written path, never by name
      --inspect          Print, as JSON, the files an .obj, .mtl or .ply refers to; convert nothing
      --root DIR         With --inspect: the folder references must stay in (default: the input's)
      --progress         Report conversion progress on stderr
      --log-level LEVEL  Print stderr messages of this level and above: trace, debug, info
                         (default), warn or error
      --log-format FMT   Write stderr messages as text (default), or as JSON lines
  -h, --help             Show help"
    );
}
