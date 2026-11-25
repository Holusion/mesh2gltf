//! PLY (Stanford polygon file) decoding into the same spools as OBJ. The header is text in every
//! PLY. ASCII and binary bodies, in either byte order, share everything but the reading of one
//! value.
//!
//! Only meshes are read: a file without faces, such as a point cloud, is rejected.
use crate::{
    dedup::Dedup,
    diagnostics::{safe, shown, Tally},
    failure::Context,
    fixed::{fixed_from_float, parse_decimal_fixed, BBox},
    material::Material,
    obj::{encode_face, srgb_to_linear, Corner, Scene, MAX_FACE},
    references::References,
    Failure, Res,
};
use std::{
    fs::{self, File},
    io::{self, BufRead, BufReader, BufWriter, ErrorKind, Read, Seek, Write},
    path::Path,
};
/// Bounds the header, which is read as text before anything is known about the file.
const MAX_HEADER: u64 = 1024 * 1024;
/// Longer than any number an exporter writes.
const MAX_TOKEN: usize = 256;
#[derive(Clone, Copy, PartialEq)]
enum Type {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}
impl Type {
    /// Both the spec's names and the sized ones that VTK and others write.
    fn parse(name: &str) -> Res<Self> {
        Ok(match name {
            "char" | "int8" => Self::I8,
            "uchar" | "uint8" => Self::U8,
            "short" | "int16" => Self::I16,
            "ushort" | "uint16" => Self::U16,
            "int" | "int32" => Self::I32,
            "uint" | "uint32" => Self::U32,
            "float" | "float32" => Self::F32,
            "double" | "float64" => Self::F64,
            _ => return Err(format!("Unknown PLY property type {}", safe(name)).into()),
        })
    }
    fn size(self) -> usize {
        match self {
            Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::F64 => 8,
        }
    }
    /// A full colour channel. `uchar` channels span 0–255, as in three.js's PLYLoader and
    /// MeshLab; other integer types span their own range, and floating-point ones `[0,1]`.
    fn full(self) -> f64 {
        match self {
            Self::I8 => i8::MAX.into(),
            Self::U8 => u8::MAX.into(),
            Self::I16 => i16::MAX.into(),
            Self::U16 => u16::MAX.into(),
            Self::I32 => i32::MAX.into(),
            Self::U32 => u32::MAX.into(),
            Self::F32 | Self::F64 => 1.,
        }
    }
}
struct Property {
    name: String,
    ty: Type,
    /// The type of a list's item count, which comes before its items.
    list: Option<Type>,
}
struct Element {
    name: String,
    count: u64,
    properties: Vec<Property>,
}
#[derive(Clone, Copy, PartialEq)]
enum Format {
    Ascii,
    Little,
    Big,
}
struct Header {
    format: Format,
    elements: Vec<Element>,
    /// `comment TextureFile` names, which a face's `texnumber` indexes.
    textures: Vec<String>,
}
fn header(input: &mut impl BufRead) -> Res<Header> {
    let mut limited = input.take(MAX_HEADER);
    let mut raw = Vec::new();
    // Comments are free text in whatever encoding the exporter used.
    let mut line = move || -> Res<Option<String>> {
        raw.clear();
        if limited.read_until(b'\n', &mut raw)? == 0 {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&raw).trim().to_owned()))
    };
    if line()?.as_deref() != Some("ply") {
        return Err("Not a PLY file: the first line isn't \"ply\"".into());
    }
    let mut format = None;
    let mut elements: Vec<Element> = Vec::new();
    let mut textures = Vec::new();
    loop {
        let text = line()?.ok_or("PLY header has no end_header in its first MiB")?;
        let mut words = text.split_whitespace();
        let invalid = || format!("Invalid PLY header line: {}", safe(&text));
        match words.next() {
            Some("format") => {
                format = Some(match words.next() {
                    Some("ascii") => Format::Ascii,
                    Some("binary_little_endian") => Format::Little,
                    Some("binary_big_endian") => Format::Big,
                    _ => return Err(invalid().into()),
                })
            }
            Some("element") => {
                let (Some(name), Some(count), None) = (words.next(), words.next(), words.next())
                else {
                    return Err(invalid().into());
                };
                elements.push(Element {
                    name: name.into(),
                    count: count.parse().map_err(|_| invalid())?,
                    properties: Vec::new(),
                });
            }
            Some("property") => {
                let element = elements.last_mut().ok_or_else(invalid)?;
                let words: Vec<_> = words.collect();
                let property = match words[..] {
                    ["list", count, item, name] => {
                        let count = Type::parse(count)?;
                        if matches!(count, Type::F32 | Type::F64) {
                            return Err(invalid().into());
                        }
                        Property {
                            name: name.into(),
                            ty: Type::parse(item)?,
                            list: Some(count),
                        }
                    }
                    [ty, name] => Property {
                        name: name.into(),
                        ty: Type::parse(ty)?,
                        list: None,
                    },
                    _ => return Err(invalid().into()),
                };
                element.properties.push(property);
            }
            // MeshLab and other VCG-based exporters name textures this way.
            Some("comment") => {
                let mut rest = text["comment".len()..].trim_start().splitn(2, ' ');
                if rest
                    .next()
                    .is_some_and(|k| k.eq_ignore_ascii_case("TextureFile"))
                {
                    textures.extend(rest.next().map(|name| name.trim().to_owned()));
                }
            }
            Some("obj_info") | None => {}
            Some("end_header") => break,
            Some(_) => return Err(invalid().into()),
        }
    }
    Ok(Header {
        format: format.ok_or("PLY header has no format line")?,
        elements,
        textures,
    })
}
fn ended(e: io::Error) -> Failure {
    if e.kind() == ErrorKind::UnexpectedEof {
        "PLY data ends before the elements its header declares".into()
    } else {
        e.into()
    }
}
/// The data after the header: the one place ASCII and binary differ.
struct Body<R> {
    input: R,
    format: Format,
    token: Vec<u8>,
}
impl<R: BufRead> Body<R> {
    /// The next whitespace-separated word of an ASCII body. Line ends are whitespace like any
    /// other, as an element's values are all on its own line anyway.
    fn token(&mut self) -> Res<&str> {
        self.token.clear();
        loop {
            let buf = self.input.fill_buf()?;
            if buf.is_empty() {
                break;
            }
            let mut used = 0;
            let mut done = false;
            for &b in buf {
                used += 1;
                if !b.is_ascii_whitespace() {
                    if self.token.len() == MAX_TOKEN {
                        return Err(format!("PLY value exceeds {MAX_TOKEN} bytes").into());
                    }
                    self.token.push(b);
                } else if !self.token.is_empty() {
                    done = true;
                    break;
                }
            }
            self.input.consume(used);
            if done {
                break;
            }
        }
        if self.token.is_empty() {
            return Err(ended(ErrorKind::UnexpectedEof.into()));
        }
        Ok(std::str::from_utf8(&self.token).map_err(|_| "Invalid PLY value")?)
    }
    /// The next value as an `f64`, which holds every PLY integer exactly.
    fn value(&mut self, ty: Type) -> Res<f64> {
        if self.format == Format::Ascii {
            let token = self.token()?;
            return token
                .parse()
                .map_err(|_| format!("Invalid PLY value {}", safe(token)).into());
        }
        let mut bytes = [0; 8];
        let b = &mut bytes[..ty.size()];
        self.input.read_exact(b).map_err(ended)?;
        if self.format == Format::Big {
            b.reverse();
        }
        Ok(match ty {
            Type::I8 => (b[0] as i8).into(),
            Type::U8 => b[0].into(),
            Type::I16 => i16::from_le_bytes([b[0], b[1]]).into(),
            Type::U16 => u16::from_le_bytes([b[0], b[1]]).into(),
            Type::I32 => i32::from_le_bytes(bytes[..4].try_into().unwrap()).into(),
            Type::U32 => u32::from_le_bytes(bytes[..4].try_into().unwrap()).into(),
            Type::F32 => f32::from_le_bytes(bytes[..4].try_into().unwrap()).into(),
            Type::F64 => f64::from_le_bytes(bytes),
        })
    }
    /// A coordinate in fixed point: from its decimal digits in ASCII, as OBJ coordinates are, and
    /// from its exact binary value otherwise.
    fn coordinate(&mut self, ty: Type) -> Res<i64> {
        match self.format {
            Format::Ascii => parse_decimal_fixed(self.token()?),
            _ => fixed_from_float(self.value(ty)?),
        }
    }
    /// A list length, or an index below `limit`.
    fn whole(&mut self, ty: Type, limit: u64) -> Res<u64> {
        let n = self.value(ty)?;
        if n.fract() != 0. || n < 0. || n >= limit as f64 {
            return Err(format!("PLY index or count out of range: {n}").into());
        }
        Ok(n as u64)
    }
    fn skip(&mut self, p: &Property) -> Res<()> {
        let n = match p.list {
            Some(count) => self.whole(count, u64::MAX)?,
            None => 1,
        };
        if self.format == Format::Ascii {
            for _ in 0..n {
                self.token()?;
            }
            return Ok(());
        }
        let bytes = n.saturating_mul(p.ty.size() as u64);
        if io::copy(&mut self.input.by_ref().take(bytes), &mut io::sink())? < bytes {
            return Err(ended(ErrorKind::UnexpectedEof.into()));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Role {
    Position(usize),
    Normal(usize),
    Color(usize),
    Alpha,
    Uv(usize),
    Corners,
    /// Per-corner texture coordinates, `u v` for each corner in turn, as MeshLab writes them.
    Wedges,
    TexNumber,
    Skip,
}
/// What `p` of `element` means. Per-vertex texture coordinate names are three.js PLYLoader's.
fn role(element: &str, p: &Property) -> Role {
    match (element, p.name.as_str(), p.list.is_some()) {
        ("vertex", "x", false) => Role::Position(0),
        ("vertex", "y", false) => Role::Position(1),
        ("vertex", "z", false) => Role::Position(2),
        ("vertex", "nx", false) => Role::Normal(0),
        ("vertex", "ny", false) => Role::Normal(1),
        ("vertex", "nz", false) => Role::Normal(2),
        ("vertex", "red", false) => Role::Color(0),
        ("vertex", "green", false) => Role::Color(1),
        ("vertex", "blue", false) => Role::Color(2),
        ("vertex", "alpha", false) => Role::Alpha,
        ("vertex", "s" | "u" | "texture_u" | "texture_s", false) => Role::Uv(0),
        ("vertex", "t" | "v" | "texture_v" | "texture_t", false) => Role::Uv(1),
        // `vertex_index` is the older name, as in three.js and VTK.
        ("face", "vertex_indices" | "vertex_index", true) => Role::Corners,
        ("face", "texcoord", true) => Role::Wedges,
        ("face", "texnumber", false) => Role::TexNumber,
        _ => Role::Skip,
    }
}
/// The roles of `element`'s properties, with partial groups (a normal without `nz`) skipped.
fn roles(element: &Element, unsupported: &mut Tally) -> Vec<Role> {
    let mut roles: Vec<_> = element
        .properties
        .iter()
        .map(|p| role(&element.name, p))
        .collect();
    let complete = |roles: &[Role], group: fn(usize) -> Role, n: usize| {
        (0..n).all(|i| roles.contains(&group(i)))
    };
    let partial: Vec<_> = [
        (Role::Normal as fn(usize) -> Role, 3),
        (Role::Color, 3),
        (Role::Uv, 2),
    ]
    .into_iter()
    .filter(|&(group, n)| !complete(&roles, group, n))
    .flat_map(|(group, n)| (0..n).map(group))
    .collect();
    let wedges = roles.contains(&Role::Wedges);
    for (role, p) in roles.iter_mut().zip(&element.properties) {
        // Per-corner texture coordinates take precedence over per-vertex ones.
        if partial.contains(role) || (wedges && matches!(role, Role::Uv(_))) {
            *role = Role::Skip;
        }
        if *role == Role::Skip {
            unsupported.add(&format!("{}.{}", element.name, p.name));
        }
    }
    roles
}
fn write_all<const N: usize>(out: &mut impl Write, values: [[u8; 8]; N]) -> io::Result<()> {
    values.iter().try_for_each(|v| out.write_all(v))
}
/// A PLY header checked as a mesh: what each property of each element means, and the materials
/// its textures make, after the default one.
struct Mesh {
    header: Header,
    roles: Vec<Vec<Role>>,
    materials: Vec<Material>,
}
/// Reads the header of `input` from `reader`, which is left at the body, and follows its
/// textures with `references`.
fn mesh(input: &Path, reader: &mut impl BufRead, references: &References) -> Res<Mesh> {
    let header = header(reader).with_context(|| shown(input))?;
    let mut unsupported =
        Tally::coded("unsupported-property", "unsupported PLY properties omitted");
    let mut other_elements =
        Tally::coded("unsupported-element", "unsupported PLY elements omitted");
    let reject = |message: &str| -> Failure { format!("{}: {message}", shown(input)).into() };
    let elements = &header.elements;
    let find = |name: &'static str| elements.iter().filter(move |e| e.name == name);
    if find("face").all(|e| e.count == 0) {
        return Err(reject(
            "PLY file has no faces; point clouds are not supported",
        ));
    }
    let (Some(_), None) = (find("vertex").next(), find("vertex").nth(1)) else {
        return Err(reject("PLY file needs exactly one vertex element"));
    };
    if find("face").nth(1).is_some() {
        return Err(reject("PLY file has more than one face element"));
    }
    let roles: Vec<_> = header
        .elements
        .iter()
        .map(|e| match e.name.as_str() {
            "vertex" | "face" => roles(e, &mut unsupported),
            _ => {
                other_elements.add(&e.name);
                vec![Role::Skip; e.properties.len()]
            }
        })
        .collect();
    let has = |role: Role| roles.iter().flatten().any(|r| *r == role);
    if !(0..3).all(|i| has(Role::Position(i))) {
        return Err(reject("PLY vertices lack x, y or z"));
    }
    if !has(Role::Corners) {
        return Err(reject("PLY faces lack vertex_indices"));
    }
    let mut materials = vec![Material::new("default")];
    if has(Role::Uv(0)) || has(Role::Wedges) {
        let base = input.parent().unwrap_or(Path::new("."));
        for name in &header.textures {
            let mut m = Material::new(name);
            // The texture is the whole colour, with no diffuse factor to darken it.
            m.kd = [1.; 3];
            let file = references.resolve(base, name);
            m.texture = Some(file.with_context(|| format!("Texture {}", safe(name)))?);
            materials.push(m);
        }
    } else if !header.textures.is_empty() {
        crate::warn!("PLY textures omitted: the file has no texture coordinates");
    }
    Ok(Mesh {
        header,
        roles,
        materials,
    })
}
/// Follows the textures of `input` with `references`, for `--inspect`. Only the header is read.
pub fn inspect(input: &Path, references: &References) -> Res<()> {
    let file = File::open(input).with_context(|| shown(input))?;
    mesh(input, &mut BufReader::new(file), references)?;
    Ok(())
}
/// `keep_colors` false reads vertex colours past, as if the file had none.
pub fn spool(
    input: &Path,
    dir: &Path,
    progress: bool,
    references: &References,
    keep_colors: bool,
) -> Res<Scene> {
    let file = File::open(input).with_context(|| shown(input))?;
    let mut body = Body {
        input: BufReader::with_capacity(256 * 1024, file),
        format: Format::Ascii,
        token: Vec::new(),
    };
    let Mesh {
        header,
        roles,
        materials,
    } = mesh(input, &mut body.input, references)?;
    body.format = header.format;
    let elements = &header.elements;
    let find = |name: &'static str| elements.iter().filter(move |e| e.name == name);
    // `mesh` checked there is exactly one.
    let vertex = find("vertex").next().unwrap();
    let has = |role: Role| roles.iter().flatten().any(|r| *r == role);
    let normals = has(Role::Normal(0));
    let colors = has(Role::Color(0));
    let vertex_uvs = has(Role::Uv(0));
    let wedges = has(Role::Wedges);
    let mut v = BufWriter::new(File::create(dir.join("v"))?);
    let mut vt = BufWriter::new(File::create(dir.join("vt"))?);
    let mut vn = BufWriter::new(File::create(dir.join("vn"))?);
    let mut vc = (colors && keep_colors)
        .then(|| File::create(dir.join("vc")).map(BufWriter::new))
        .transpose()?;
    let mut faces = BufWriter::new(File::create(dir.join("faces"))?);
    // Corners are merged by index, so equal per-corner coordinates must share one: a position's
    // UVs are deduplicated by value, exactly as faces are later.
    let uv_dir = dir.join("ply-uv");
    let mut uv_dedup = if wedges {
        fs::create_dir(&uv_dir)?;
        let faces = find("face").next().map_or(0, |e| e.count);
        let mut dedup = Dedup::create(&uv_dir, vertex.count, faces)?;
        dedup.reset()?;
        Some(dedup)
    } else {
        None
    };
    let mut bbox = BBox::new();
    let mut missing_normals = !normals;
    let mut uv_count = if vertex_uvs { vertex.count } else { 0 };
    let mut translucent = 0u64;
    let mut clamped = 0u64;
    let mut short_faces = 0u64;
    let mut mismatched_uvs = 0u64;
    let mut bad_texnumbers = 0u64;
    let mut corners = Vec::new();
    let mut wedge = Vec::new();
    let mut record = Vec::new();
    let mut done = 0u64;
    let total = header.elements.iter().map(|e| e.count).sum();
    for (element, roles) in header.elements.iter().zip(&roles) {
        for i in 0..element.count {
            done += 1;
            if progress && done.is_multiple_of(1 << 22) {
                let read = body.input.stream_position()?;
                let message = format!("Reading PLY: {}", crate::format::fmt_bytes(read));
                crate::diagnostics::progress("read", done, total, &message);
            }
            let result: Res<()> = (|| {
                let mut position = [0i64; 3];
                let mut normal = [0f64; 3];
                let mut color = [0f64; 3];
                let mut uv = [0f64; 2];
                let mut texnumber = None;
                corners.clear();
                wedge.clear();
                for (p, role) in element.properties.iter().zip(roles) {
                    match *role {
                        Role::Position(k) => position[k] = body.coordinate(p.ty)?,
                        Role::Normal(k) => normal[k] = body.value(p.ty)?,
                        Role::Color(k) => {
                            let c = body.value(p.ty)? / p.ty.full();
                            if !(0. ..=1.).contains(&c) {
                                if !c.is_finite() {
                                    return Err("Non-finite PLY color".into());
                                }
                                clamped += 1;
                            }
                            color[k] = c.clamp(0., 1.);
                        }
                        Role::Alpha => translucent += u64::from(body.value(p.ty)? < p.ty.full()),
                        Role::Uv(k) => uv[k] = body.value(p.ty)?,
                        Role::Corners => {
                            let n = body.whole(p.list.unwrap(), u64::MAX)?;
                            if n > MAX_FACE as u64 {
                                return Err(format!("Face exceeds {MAX_FACE} corners").into());
                            }
                            for _ in 0..n {
                                let v = body.whole(p.ty, vertex.count)? + 1;
                                corners.push(Corner { v, vt: 0, vn: 0 });
                            }
                        }
                        Role::Wedges => {
                            let n = body.whole(p.list.unwrap(), 2 * MAX_FACE as u64 + 1)?;
                            for _ in 0..n {
                                wedge.push(body.value(p.ty)?);
                            }
                        }
                        Role::TexNumber => texnumber = Some(body.value(p.ty)?),
                        Role::Skip => body.skip(p)?,
                    }
                }
                if element.name == "vertex" {
                    bbox.add(position);
                    write_all(&mut v, position.map(i64::to_le_bytes))?;
                    if normals {
                        missing_normals |= normal == [0.; 3];
                        write_all(&mut vn, normal.map(f64::to_le_bytes))?;
                    }
                    if let Some(vc) = vc.as_mut() {
                        write_all(vc, color.map(|c| srgb_to_linear(c).to_le_bytes()))?;
                    }
                    if vertex_uvs {
                        if uv.iter().any(|t| !t.is_finite()) {
                            return Err("Non-finite PLY texture coordinate".into());
                        }
                        write_all(&mut vt, uv.map(f64::to_le_bytes))?;
                    }
                } else if element.name == "face" {
                    if corners.len() < 3 {
                        short_faces += 1;
                        return Ok(());
                    }
                    for c in &mut corners {
                        c.vn = if normals { c.v } else { 0 };
                        c.vt = if vertex_uvs { c.v } else { 0 };
                    }
                    if let Some(dedup) = uv_dedup.as_mut() {
                        if wedge.iter().any(|t| !t.is_finite()) {
                            return Err("Non-finite PLY texture coordinate".into());
                        }
                        if wedge.len() != 2 * corners.len() {
                            mismatched_uvs += 1;
                            wedge.clear();
                        }
                        for (c, t) in corners.iter_mut().zip(wedge.chunks(2)) {
                            // Keyed by the float32 values glTF stores.
                            let bits = |x: f64| u64::from((x as f32).to_bits());
                            let key = Corner {
                                v: c.v,
                                vt: bits(t[0]) << 32 | bits(t[1]),
                                vn: 0,
                            };
                            let next = u32::try_from(uv_count)
                                .map_err(|_| "More than 2^32 PLY texture coordinates")?;
                            c.vt = match dedup.find_or_insert(key, next)? {
                                Some(id) => u64::from(id) + 1,
                                None => {
                                    write_all(&mut vt, [t[0], t[1]].map(f64::to_le_bytes))?;
                                    uv_count += 1;
                                    uv_count
                                }
                            };
                        }
                    }
                    let mut material = 0;
                    // A face without UVs gets an untextured copy, as an OBJ face does.
                    if materials.len() > 1 {
                        let n = texnumber.unwrap_or(0.);
                        if n.fract() == 0. && n >= 0. && n < (materials.len() - 1) as f64 {
                            material = n as usize + 1;
                        } else {
                            bad_texnumbers += 1;
                        }
                    }
                    record.clear();
                    encode_face(&mut record, 0, material, false, &corners);
                    faces.write_all(&record)?;
                }
                Ok(())
            })();
            result.with_context(|| format!("{}: {} {i}", shown(input), safe(&element.name)))?;
        }
    }
    drop(uv_dedup);
    if wedges {
        fs::remove_dir_all(&uv_dir)?;
    }
    if short_faces > 0 {
        crate::warn!(code: "too-few-corners", "omitted {short_faces} faces with fewer than three corners");
    }
    if mismatched_uvs > 0 {
        crate::warn!("{mismatched_uvs} faces whose texcoord list doesn't match their corners are left untextured");
    }
    if bad_texnumbers > 0 {
        crate::warn!(
            "{bad_texnumbers} faces with a texnumber naming no TextureFile are left untextured"
        );
    }
    if translucent > 0 && keep_colors {
        crate::warn!("vertex color alpha is omitted ({translucent} translucent vertices)");
    }
    if clamped > 0 && keep_colors {
        crate::warn!("{clamped} vertex color channels outside [0,1] are clamped");
    }
    for w in [&mut v, &mut vt, &mut vn, &mut faces] {
        w.flush()?;
    }
    if let Some(vc) = vc.as_mut() {
        vc.flush()?;
    }
    Ok(Scene {
        bbox,
        missing_normals,
        colors: vc.is_some(),
        objects: vec!["default".into()],
        materials,
        counts: [
            vertex.count,
            uv_count,
            if normals { vertex.count } else { 0 },
        ],
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn parse(text: &str) -> Res<Header> {
        header(&mut text.as_bytes())
    }
    #[test]
    fn header_reads_elements_lists_and_textures() {
        let h = parse(
            "ply\r\nformat binary_big_endian 1.0\r\ncomment TextureFile my tex.png\r\nobj_info x\r\n\
             element vertex 3\r\nproperty float32 x\r\nelement face 1\r\n\
             property list uchar int vertex_indices\r\nend_header\r\n",
        )
        .unwrap();
        assert!(h.format == Format::Big);
        assert_eq!(h.textures, ["my tex.png"]);
        let names: Vec<_> = h
            .elements
            .iter()
            .map(|e| (e.name.as_str(), e.count))
            .collect();
        assert_eq!(names, [("vertex", 3), ("face", 1)]);
        assert!(h.elements[1].properties[0].list == Some(Type::U8));
        assert!(h.elements[1].properties[0].ty == Type::I32);
    }
    #[test]
    fn header_rejects_what_it_cannot_lay_out() {
        for bad in [
            "solid\n",
            "ply\nformat binary_middle_endian 1.0\nend_header\n",
            "ply\nformat ascii 1.0\nproperty float x\nend_header\n",
            "ply\nformat ascii 1.0\nelement vertex 1\nproperty half x\nend_header\n",
            "ply\nformat ascii 1.0\nelement face 1\nproperty list float int v\nend_header\n",
            "ply\nformat ascii 1.0\nelement vertex -1\nend_header\n",
            "ply\nformat ascii 1.0\nelement vertex 1\nbogus\nend_header\n",
            "ply\nelement vertex 1\nend_header\n",
            "ply\nformat ascii 1.0\n",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn ascii_and_binary_values_agree() {
        let read = |format, bytes: &[u8], ty| {
            let mut body = Body {
                input: bytes,
                format,
                token: Vec::new(),
            };
            body.value(ty).unwrap()
        };
        assert_eq!(read(Format::Ascii, b"  \r\n-3 ", Type::I8), -3.);
        assert_eq!(read(Format::Little, &[0xfd], Type::I8), -3.);
        assert_eq!(read(Format::Little, &[1, 2], Type::U16), 513.);
        assert_eq!(read(Format::Big, &[1, 2], Type::U16), 258.);
        assert_eq!(read(Format::Big, &1.5f64.to_be_bytes(), Type::F64), 1.5);
        assert_eq!(
            read(Format::Little, &u32::MAX.to_le_bytes(), Type::U32),
            4294967295.
        );
        let mut body = Body {
            input: &b"12345678.123456789012 7"[..],
            format: Format::Ascii,
            token: Vec::new(),
        };
        // ASCII coordinates keep their decimal digits, whatever their declared type.
        assert_eq!(body.coordinate(Type::F32).unwrap(), 123_456_781_234_567_890);
        assert!(body.whole(Type::U8, 7).is_err());
        assert!(body.value(Type::U8).is_err());
    }
}
