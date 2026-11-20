//! Source image policy and optional decoding/encoding, one image at a time.
use crate::{
    diagnostics::shown,
    disk::{parent, Outputs},
    failure::{Context, Usage},
    Failure, Res,
};
use crate::{gltf::uri, material::Material};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Component, Path, PathBuf},
    thread::{self, JoinHandle},
};
#[derive(Clone, Copy, Default)]
pub enum Mode {
    #[default]
    Auto,
    Keep,
    Copy,
}
impl Mode {
    pub fn parse(s: &str) -> Res<Self> {
        match s {
            "auto" => Ok(Self::Auto),
            "keep" => Ok(Self::Keep),
            "copy" => Ok(Self::Copy),
            _ => Err("--images must be auto, keep or copy".into()),
        }
    }
}
#[derive(Default)]
pub struct Options {
    pub mode: Mode,
    /// Let the output refer to images outside its own folder: `run` sets this from
    /// `--trusted`.
    pub outside_references: bool,
    /// The output carries its images in its binary chunk, as a `.glb` does: none is copied or
    /// referred to by URI. `run` sets this from the output's extension.
    pub embed: bool,
}
pub struct Plan {
    pub content: PathBuf,
    pub publish_name: Option<String>,
    /// Empty when the output embeds its images.
    pub uri: String,
}
impl Plan {
    /// The MIME type a `.glb` records for the embedded image: `prepare` leaves only JPG and PNG.
    pub fn mime_type(&self) -> &'static str {
        let png = self
            .content
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("png"));
        if png {
            "image/png"
        } else {
            "image/jpeg"
        }
    }
    /// Copies the image into the output's `folder`, unless the output refers to it in place.
    pub fn publish(&self, folder: &Path, outputs: &mut Outputs) -> Res<()> {
        if let Some(name) = &self.publish_name {
            let mut copy = outputs.create(&folder.join(name))?;
            io::copy(&mut File::open(&self.content)?, &mut copy)?;
        }
        Ok(())
    }
}
fn relative_uri(source: &Path, destination: &Path) -> Res<String> {
    let from: Vec<_> = destination.components().collect();
    let to: Vec<_> = source.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return Err(
            "Cannot reference source images across filesystem roots; use --images copy".into(),
        );
    }
    let mut parts = vec!["..".to_owned(); from.len() - common];
    for component in &to[common..] {
        if let Component::Normal(name) = component {
            parts.push(uri(&name.to_string_lossy()));
        }
    }
    Ok(parts.join("/"))
}
/// The URI of an image left where it is. A published model doesn't refer outside its own folder
/// unless the user vouches for it, and `--images auto` copies rather than getting here.
fn kept_uri(source: &Path, destination: &Path, options: &Options) -> Res<String> {
    if !options.outside_references && !source.starts_with(destination) {
        // Confined images are all beneath the input's folder, so this needs an output folder that
        // doesn't hold the input's: the command line's doing, not the file's.
        let message = "--images keep would refer outside the output's folder; use --images copy, or --trusted to allow it";
        return Err(Usage(message.into()).into());
    }
    relative_uri(source, destination)
}
pub fn prepare(
    options: &Options,
    input: &Path,
    output: &Path,
    workspace: &Path,
    materials: &[Material],
) -> Res<HashMap<PathBuf, Plan>> {
    let destination = fs::canonicalize(parent(output))?;
    let same_folder = fs::canonicalize(parent(input))? == destination;
    let stem = output
        .file_stem()
        .ok_or("Invalid output filename")?
        .to_string_lossy();
    let mut plans = HashMap::new();
    for material in materials {
        let Some(path) = &material.texture else {
            continue;
        };
        let source = fs::canonicalize(path).with_context(|| format!("Texture {}", shown(path)))?;
        if plans.contains_key(&source) {
            continue;
        }
        let ext = source
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        // glTF core only carries JPG and PNG; anything else becomes a PNG, lossless and with alpha.
        let convert = !matches!(ext.as_str(), "jpg" | "jpeg" | "png");
        let copy = convert
            || match options.mode {
                Mode::Auto => !same_folder,
                Mode::Keep => false,
                Mode::Copy => true,
            };
        let output_ext = if convert { "png" } else { &ext };
        let name = format!("{stem}_texture_{}.{output_ext}", plans.len());
        let content = if convert {
            let converted = workspace.join(&name);
            convert_image(&source, &converted)
                .with_context(|| format!("Converting texture {}", shown(&source)))?;
            converted
        } else {
            source.clone()
        };
        check_image(&content).with_context(|| format!("Texture {}", shown(&source)))?;
        let uri = if options.embed {
            String::new()
        } else if copy {
            uri(&name)
        } else {
            kept_uri(&source, &destination, options)?
        };
        plans.insert(
            source,
            Plan {
                content,
                publish_name: (copy && !options.embed).then_some(name),
                uri,
            },
        );
    }
    Ok(plans)
}
/// Conversion writes PNG, which is lossless and keeps alpha.
fn convert_image(source: &Path, destination: &Path) -> Res<()> {
    let decoded = image::ImageReader::open(source)?
        .with_guessed_format()?
        .decode()?;
    // PNG has no float samples, so an EXR or HDR texture is clamped to 0..1 and stored in 8 bits.
    let decoded = match decoded.color() {
        image::ColorType::Rgb32F => decoded.to_rgb8().into(),
        image::ColorType::Rgba32F => decoded.to_rgba8().into(),
        _ => decoded,
    };
    let mut output = Recording {
        inner: BufWriter::new(File::create(destination)?),
        error: None,
    };
    let encoded = decoded
        .write_with_encoder(image::codecs::png::PngEncoder::new(&mut output))
        .map_err(Failure::from)
        .and_then(|()| Ok(output.flush()?));
    match output.error {
        Some(error) => Err(error.into()),
        None => encoded,
    }
}
/// Keeps the cause of a failed write: the PNG encoder passes on only its message, which would
/// make a full disk look like a broken texture.
struct Recording<W> {
    inner: W,
    error: Option<io::Error>,
}
impl<W: Write> Recording<W> {
    fn record<T>(&mut self, result: io::Result<T>) -> io::Result<T> {
        if let Err(e) = &result {
            let copy = e.raw_os_error().map(io::Error::from_raw_os_error);
            self.error
                .get_or_insert(copy.unwrap_or_else(|| e.kind().into()));
        }
        result
    }
}
impl<W: Write> Write for Recording<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let result = self.inner.write(buf);
        self.record(result)
    }
    fn flush(&mut self) -> io::Result<()> {
        let result = self.inner.flush();
        self.record(result)
    }
}
/// The source paths of textures with any pixel below full opacity, scanned on a background
/// thread: inflating a large PNG takes seconds, and only the material JSON needs the answer.
pub fn scan_transparency(plans: &HashMap<PathBuf, Plan>) -> JoinHandle<Res<HashSet<PathBuf>>> {
    let jobs: Vec<_> = plans
        .iter()
        .map(|(source, plan)| (source.clone(), plan.content.clone()))
        .collect();
    thread::spawn(move || {
        let mut transparent = HashSet::new();
        for (source, content) in jobs {
            let alpha = texture_alpha(&content);
            if alpha.with_context(|| format!("Texture {}", shown(&source)))? {
                transparent.insert(source);
            }
        }
        Ok(transparent)
    })
}
/// Check the signature and, for PNG, the header chunks without reading any pixel, so a broken
/// texture fails the import before the geometry work.
fn check_image(path: &Path) -> Res<Option<png::Reader<BufReader<File>>>> {
    let mut file = BufReader::new(File::open(path)?);
    let png = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("png"));
    if !png {
        let mut magic = [0; 2];
        file.read_exact(&mut magic)?;
        if magic != [0xff, 0xd8] {
            return Err("JPEG texture has an invalid signature".into());
        }
        return Ok(None);
    }
    let mut decoder = png::Decoder::new(file);
    // Palette and tRNS transparency become an alpha channel; 16-bit samples stay exact.
    decoder.set_transformations(png::Transformations::EXPAND);
    Ok(Some(decoder.read_info()?))
}
/// Whether any pixel is less than fully opaque. An alpha channel alone proves nothing: baking and
/// photogrammetry tools routinely write RGBA PNGs whose alpha is 255 everywhere, and blending
/// those would cost sorting artefacts and a transparent pass for nothing. PNG rows are scanned
/// one at a time, so even a 16k texture costs only a row of memory.
fn texture_alpha(path: &Path) -> Res<bool> {
    let Some(mut reader) = check_image(path)? else {
        return Ok(false);
    };
    let (color, depth) = reader.output_color_type();
    if !matches!(color, png::ColorType::GrayscaleAlpha | png::ColorType::Rgba) {
        return Ok(false);
    }
    let sample = if depth == png::BitDepth::Sixteen {
        2
    } else {
        1
    };
    let pixel = color.samples() * sample;
    while let Some(row) = reader.next_row()? {
        if row
            .data()
            .chunks_exact(pixel)
            .any(|p| p[pixel - sample..].iter().any(|b| *b != 0xff))
        {
            return Ok(true);
        }
    }
    Ok(false)
}
#[cfg(test)]
mod tests {
    use super::*;
    use png::{BitDepth, ColorType};
    #[test]
    fn the_transparency_scan_keeps_the_cause_of_a_failure() {
        let missing = PathBuf::from("no/such/texture.png");
        let plan = Plan {
            content: missing.clone(),
            publish_name: None,
            uri: String::new(),
        };
        let plans = HashMap::from([(missing, plan)]);
        let error = scan_transparency(&plans).join().unwrap().unwrap_err();
        let cause = std::error::Error::source(&*error).unwrap();
        assert!(cause.is::<std::io::Error>(), "{error}");
        assert!(
            error.to_string().starts_with("Texture texture.png: "),
            "{error}"
        );
    }
    fn encode(
        dir: &Path,
        name: &str,
        color: ColorType,
        depth: BitDepth,
        setup: impl FnOnce(&mut png::Encoder<'_, BufWriter<File>>),
        data: &[u8],
    ) -> PathBuf {
        let path = dir.join(name);
        let mut encoder = png::Encoder::new(BufWriter::new(File::create(&path).unwrap()), 2, 1);
        encoder.set_color(color);
        encoder.set_depth(depth);
        setup(&mut encoder);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(data).unwrap();
        writer.finish().unwrap();
        path
    }
    #[test]
    fn alpha_means_a_pixel_below_full_opacity() {
        let dir = std::env::temp_dir().join(format!("mesh2gltf-alpha-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let (eight, sixteen) = (BitDepth::Eight, BitDepth::Sixteen);
        let none = |_: &mut png::Encoder<'_, BufWriter<File>>| {};
        let cases = [
            ("rgb", ColorType::Rgb, eight, vec![1, 2, 3, 4, 5, 6], false),
            (
                "rgba-opaque",
                ColorType::Rgba,
                eight,
                vec![1, 2, 3, 255, 4, 5, 6, 255],
                false,
            ),
            (
                "rgba-254",
                ColorType::Rgba,
                eight,
                vec![1, 2, 3, 255, 4, 5, 6, 254],
                true,
            ),
            (
                "ga-zero",
                ColorType::GrayscaleAlpha,
                eight,
                vec![255, 255, 0, 0],
                true,
            ),
            (
                "rgba16-opaque",
                ColorType::Rgba,
                sixteen,
                [255; 16].to_vec(),
                false,
            ),
        ];
        for (name, color, depth, data, expected) in cases {
            let path = encode(&dir, &format!("{name}.png"), color, depth, none, &data);
            assert_eq!(texture_alpha(&path).unwrap(), expected, "{name}");
        }
        // The low byte of a 16-bit alpha counts too.
        let mut data = [255; 16];
        data[15] = 254;
        let path = encode(
            &dir,
            "rgba16-low.png",
            ColorType::Rgba,
            sixteen,
            none,
            &data,
        );
        assert!(texture_alpha(&path).unwrap());
        // tRNS: only entries that pixels actually use matter.
        let palette = |e: &mut png::Encoder<'_, BufWriter<File>>| {
            e.set_palette(vec![0, 0, 0, 255, 255, 255]);
            e.set_trns(vec![255, 0]);
        };
        let path = encode(
            &dir,
            "indexed-opaque.png",
            ColorType::Indexed,
            eight,
            palette,
            &[0, 0],
        );
        assert!(!texture_alpha(&path).unwrap());
        let path = encode(
            &dir,
            "indexed-clear.png",
            ColorType::Indexed,
            eight,
            palette,
            &[0, 1],
        );
        assert!(texture_alpha(&path).unwrap());
        let key = |e: &mut png::Encoder<'_, BufWriter<File>>| e.set_trns(vec![0, 7]);
        let path = encode(
            &dir,
            "gray-key.png",
            ColorType::Grayscale,
            eight,
            key,
            &[3, 7],
        );
        assert!(texture_alpha(&path).unwrap());
        let path = encode(
            &dir,
            "gray-unkeyed.png",
            ColorType::Grayscale,
            eight,
            key,
            &[3, 8],
        );
        assert!(!texture_alpha(&path).unwrap());
        fs::write(dir.join("photo.jpg"), [0xff, 0xd8, 0xff]).unwrap();
        assert!(!texture_alpha(&dir.join("photo.jpg")).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }
}
