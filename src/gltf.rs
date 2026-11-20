//! The glTF file and its `.bin`, or one GLB holding both and the images. A geometry pass
//! streams faces into the binary payload in primitives of bounded memory, whatever the size of
//! an object; an appearance pass then describes their materials and textures.
use crate::{
    dedup::Dedup,
    diagnostics::{shown, Tally},
    disk::{map, parent, Outputs},
    failure::Context,
    fixed::{format_fixed_as_decimal, format_triple, SCALE},
    group::{self, Group, Key},
    images::Plan,
    obj::{self, Scene},
    triangulate, Res,
};
use serde_json::{json, Map, Value};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};
/// glTF reserves each index type's maximum value for primitive restart: a primitive with
/// `UNSIGNED_SHORT` indices holds at most 65,535 vertices, one with `UNSIGNED_INT` 2^32 - 1.
pub const SHORT_VERTICES: u64 = u16::MAX as u64;
pub const MAX_VERTICES: u64 = u32::MAX as u64;
pub const COORDINATE_CONVENTION: &str = "Source units preserved (assumed metres); no CRS inferred";
/// Floats per vertex up to the end of the UV, and where the colour starts (11 when absent).
fn layout(uv: bool, color: bool) -> (usize, usize, u64) {
    let uv_end = if uv { 8 } else { 6 };
    let color_start = if color { 8 } else { 11 };
    (
        uv_end,
        color_start,
        ((uv_end + 11 - color_start) * 4) as u64,
    )
}
/// The open primitive. Its vertices stream into the payload as they are first referenced, its
/// triangles into the index spool as u32, narrowed to u16 on close when the vertex count allows.
struct Primitive {
    key: Key,
    start: u64,
    vertices: u64,
    triangles: u64,
    min: [f32; 3],
    max: [f32; 3],
    indices: BufWriter<File>,
}
struct Document {
    accessors: Vec<Value>,
    views: Vec<Value>,
    meshes: Vec<Value>,
    offset: u64,
    triangles: u64,
    index_spool: PathBuf,
}
impl Document {
    fn accessor(
        &mut self,
        view: usize,
        offset: u64,
        component: u32,
        count: u64,
        kind: &str,
        bounds: Option<([f32; 3], [f32; 3])>,
    ) -> usize {
        let mut a = json!({"bufferView":view,"byteOffset":offset,"componentType":component,"count":count,"type":kind});
        if let Some((min, max)) = bounds {
            a["min"] = json!(min);
            a["max"] = json!(max);
        }
        let id = self.accessors.len();
        self.accessors.push(a);
        id
    }
    fn open(&mut self, dedup: &mut Dedup, key: Key) -> Res<Primitive> {
        dedup.reset()?;
        Ok(Primitive {
            key,
            start: self.offset,
            vertices: 0,
            triangles: 0,
            min: [f32::INFINITY; 3],
            max: [f32::NEG_INFINITY; 3],
            indices: BufWriter::with_capacity(256 * 1024, File::create(&self.index_spool)?),
        })
    }
    fn vertex(&mut self, p: &mut Primitive, v: &[f32; 11], out: &mut impl Write) -> Res<u32> {
        let index = u32::try_from(p.vertices)?;
        let (uv_end, color_start, stride) = layout(p.key.uv, p.key.colored);
        for n in v[..uv_end].iter().chain(&v[color_start..]) {
            out.write_all(&n.to_le_bytes())?;
        }
        for ((min, max), x) in p.min.iter_mut().zip(&mut p.max).zip(v) {
            *min = min.min(*x);
            *max = max.max(*x);
        }
        p.vertices += 1;
        self.offset += stride;
        Ok(index)
    }
    fn close(&mut self, p: Primitive, out: &mut impl Write) -> Res<()> {
        if p.triangles == 0 {
            // Vertices are only written for triangles, so the payload holds nothing for it.
            return Ok(());
        }
        let (uv_end, _, stride) = layout(p.key.uv, p.key.colored);
        let view = self.views.len();
        self.views.push(json!({"buffer":0,"byteOffset":p.start,"byteLength":p.vertices * stride,"byteStride":stride,"target":34962}));
        let position = self.accessor(view, 0, 5126, p.vertices, "VEC3", Some((p.min, p.max)));
        let normal = self.accessor(view, 12, 5126, p.vertices, "VEC3", None);
        let mut attributes = json!({"POSITION":position,"NORMAL":normal});
        if p.key.uv {
            attributes["TEXCOORD_0"] =
                json!(self.accessor(view, 24, 5126, p.vertices, "VEC2", None));
        }
        if p.key.colored {
            let offset = uv_end as u64 * 4;
            attributes["COLOR_0"] =
                json!(self.accessor(view, offset, 5126, p.vertices, "VEC3", None));
        }
        let mut spool = p.indices.into_inner().map_err(|e| e.into_error())?;
        spool.flush()?;
        drop(spool);
        let mut spool = BufReader::with_capacity(256 * 1024, File::open(&self.index_spool)?);
        self.triangles += p.triangles;
        let short = p.vertices <= SHORT_VERTICES;
        let count = p.triangles * 3;
        let mut index = [0; 4];
        for _ in 0..count {
            spool.read_exact(&mut index)?;
            out.write_all(if short { &index[..2] } else { &index })?;
        }
        let bytes = count * if short { 2 } else { 4 };
        let index_view = self.views.len();
        self.views
            .push(json!({"buffer":0,"byteOffset":self.offset,"byteLength":bytes,"target":34963}));
        self.offset += bytes;
        // Keep the next vertex view 4-byte aligned after 16-bit indices.
        let padding = self.offset.next_multiple_of(4) - self.offset;
        out.write_all(&[0; 4][..padding as usize])?;
        self.offset += padding;
        let component = if short { 5123 } else { 5125 };
        let indices = self.accessor(index_view, 0, component, count, "SCALAR", None);
        self.meshes[p.key.object]["primitives"]
            .as_array_mut()
            .unwrap()
            .push(
                json!({"attributes":attributes,"indices":indices,"material":p.key.material,"mode":4}),
            );
        Ok(())
    }
}
/// Writes `options.output`, with its `.bin` unless it is a GLB, from the spooled and grouped
/// scene, placing textures as `images` plans them.
pub fn write(
    scene: Scene,
    groups: &[Group],
    images: HashMap<PathBuf, Plan>,
    dir: &Path,
    options: &crate::Options,
) -> Res<()> {
    write_with_limit(scene, groups, images, dir, options, MAX_VERTICES)
}
/// `max_vertices` is only lowered by tests, to exercise splitting at the u32 index limit.
fn write_with_limit(
    scene: Scene,
    groups: &[Group],
    images: HashMap<PathBuf, Plan>,
    dir: &Path,
    options: &crate::Options,
    max_vertices: u64,
) -> Res<()> {
    let output = &options.output;
    let (center, z_up) = (options.center, options.z_up);
    let transparency = crate::images::scan_transparency(&images);
    // Positions are stored relative to `origin`. A model far from 0 keeps its place through a
    // node translation; `--center` drops it instead.
    let (origin, translated) = if center {
        (scene.bbox.center().ok_or("No vertices to center")?, false)
    } else if let Some(origin) = scene.bbox.far_origin() {
        (origin, true)
    } else {
        ([0; 3], false)
    };
    crate::say!("Origin: {}", format_triple(origin));
    crate::say!(
        "Bounds: {} (size {})",
        scene.bbox.range_str().unwrap_or_default(),
        scene.bbox.size_str().unwrap_or_default()
    );
    let max_corners = groups.iter().map(|g| g.corners).max().unwrap_or(0);
    let mut document = geometry(
        &scene,
        max_corners,
        origin,
        dir,
        options.progress,
        max_vertices,
    )?;
    if document.offset == 0 {
        return Err("Input contains no supported triangle geometry".into());
    }
    if translated {
        crate::say!("Note: the model is far from 0 for its size; positions are relative to the origin, which each node's translation restores (--center drops it)");
    }
    let transparent = transparency
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))?;
    let mut outputs = Outputs::default();
    let (mut look, contents) = appearance(
        &scene,
        &mut document.meshes,
        &images,
        &transparent,
        parent(output),
        &mut outputs,
        options.glb,
    )?;
    let geometry_bytes = document.offset;
    let embedded = if options.glb {
        embed_images(&mut document, look.get_mut("images"), contents)?
    } else {
        Vec::new()
    };
    // glTF applies the rotation first, so the translation is the origin in output axes. Parsed
    // from its decimal text, it is the `f64` a JSON reader gets from the origin.
    let translation = translated.then(|| {
        let o = if z_up {
            [origin[0], origin[2], -origin[1]]
        } else {
            origin
        };
        o.map(|v| format_fixed_as_decimal(v).parse::<f64>().unwrap())
    });
    let mut meshes = Vec::new();
    let mut nodes = Vec::new();
    for mesh in document.meshes {
        if !mesh["primitives"].as_array().unwrap().is_empty() {
            let mut node = json!({"name":mesh["name"],"mesh":meshes.len()});
            if z_up {
                let q = std::f64::consts::FRAC_1_SQRT_2;
                node["rotation"] = json!([-q, 0., 0., q]);
            }
            if let Some(t) = translation {
                node["translation"] = json!(t);
            }
            nodes.push(node);
            meshes.push(mesh);
        }
    }
    let mesh_count = meshes.len();
    let bin_path = output.with_extension("bin");
    let origin_strings = origin.map(format_fixed_as_decimal);
    // A GLB's buffer is its binary chunk, which has no URI.
    let mut buffer = json!({"byteLength":document.offset});
    if !options.glb {
        buffer["uri"] = json!(uri(&bin_path.file_name().unwrap().to_string_lossy()));
    }
    let mut root = json!({"asset":{"version":"2.0","generator":concat!("mesh2gltf ",env!("CARGO_PKG_VERSION"))},
        "scene":0,"scenes":[{"nodes":(0..nodes.len()).collect::<Vec<_>>()}],"nodes":nodes,"meshes":meshes,
        "buffers":[buffer],"bufferViews":document.views,"accessors":document.accessors,
        "extras":{"mesh2gltf":{"origin":origin_strings,"sourceBounds":scene.bbox.record(),"centered":center,"sourceUpAxis":if z_up {"z"} else {"y"},"coordinateConvention":COORDINATE_CONVENTION}}});
    root.as_object_mut().unwrap().extend(look);
    let payload = dir.join("payload.bin");
    if options.glb {
        write_glb(
            output,
            &root,
            &payload,
            &embedded,
            document.offset,
            &mut outputs,
        )?;
    } else {
        let mut dst = BufWriter::new(outputs.create(&bin_path)?);
        std::io::copy(&mut File::open(&payload)?, &mut dst)?;
        dst.flush()?;
        drop(dst);
        let mut dst = BufWriter::new(outputs.create(output)?);
        serde_json::to_writer_pretty(&mut dst, &root)?;
        dst.flush()?;
        drop(dst);
    }
    outputs.commit();
    crate::say!(
        "Wrote {} ({} triangles, {mesh_count} meshes, {} bytes of geometry)",
        shown(output),
        document.triangles,
        geometry_bytes
    );
    Ok(())
}
/// Streams every face into `payload.bin` in the workspace, triangulated and deduplicated, in one
/// primitive per run of faces sharing a `group::Key`.
fn geometry(
    scene: &Scene,
    max_corners: u64,
    origin: [i64; 3],
    dir: &Path,
    progress: bool,
    max_vertices: u64,
) -> Res<Document> {
    let attrs = [
        map(&dir.join("v"))?,
        map(&dir.join("vt"))?,
        map(&dir.join("vn"))?,
    ];
    let colors = group::colors(scene, dir)?;
    let smooth = if scene.missing_normals {
        Some(crate::smooth::generate(scene, &attrs[0], dir, progress)?)
    } else {
        None
    };
    let mut dedup = Dedup::create(dir, scene.counts[0], max_corners)?;
    let mut faces = BufReader::with_capacity(256 * 1024, File::open(dir.join("faces"))?);
    let mut document = Document {
        accessors: Vec::new(),
        views: Vec::new(),
        meshes: scene
            .objects
            .iter()
            .map(|n| json!({"name":n,"primitives":[]}))
            .collect(),
        offset: 0,
        triangles: 0,
        index_spool: dir.join("indices"),
    };
    // Build payload in the private workspace before touching output paths.
    let mut binary = BufWriter::with_capacity(256 * 1024, File::create(dir.join("payload.bin"))?);
    let mut open: Option<Primitive> = None;
    let mut face_count = 0u64;
    let mut skipped_degenerate = 0u64;
    let mut forced_polygons = 0u64;
    let mut repaired_normals = 0u64;
    let mut splits = 0u64;
    let mut local = Vec::new();
    while let Some(face) = obj::read_face(&mut faces)? {
        face_count += 1;
        let (key, _) = group::key(scene, colors.as_deref(), &face)?;
        let obj::Face { flat, corners, .. } = face;
        // Faces were validated by grouping: UV and normal presence is uniform per face.
        let uv = key.uv;
        let normal = corners[0].vn != 0;
        if open.as_ref().is_none_or(|p| p.key != key) {
            if let Some(p) = open.take() {
                document.close(p, &mut binary)?;
            }
            open = Some(document.open(&mut dedup, key)?);
        }
        let mut positions = Vec::with_capacity(corners.len());
        let mut vertices = Vec::with_capacity(corners.len());
        for c in &corners {
            let raw = obj::fetch::<3>(&attrs[0], c.v, scene.counts[0])?.map(i64::from_le_bytes);
            let p = std::array::from_fn(|i| {
                ((raw[i] as i128 - origin[i] as i128) as f64) / (SCALE as f64)
            });
            positions.push(p);
            let mut v = [0f32; 11];
            for i in 0..3 {
                v[i] = p[i] as f32;
            }
            if normal {
                let n = obj::fetch::<3>(&attrs[2], c.vn, scene.counts[2])?.map(f64::from_le_bytes);
                let length = n.iter().map(|x| x * x).sum::<f64>().sqrt();
                if !length.is_finite() {
                    return Err("Zero/invalid source normal".into());
                }
                if length > 0. {
                    for i in 0..3 {
                        v[i + 3] = (n[i] / length) as f32;
                    }
                } else {
                    repaired_normals += 1;
                }
            }
            if uv {
                let t = obj::fetch::<2>(&attrs[1], c.vt, scene.counts[1])?.map(f64::from_le_bytes);
                v[6] = t[0] as f32;
                v[7] = (1. - t[1]) as f32;
            }
            if let Some(colors) = colors.as_deref().filter(|_| key.colored) {
                // A position without a colour, on a face where others have one, is white.
                let rgb = obj::color(colors, c.v, scene.counts[0])?.unwrap_or([1.; 3]);
                for i in 0..3 {
                    v[i + 8] = rgb[i] as f32;
                }
            }
            if v.iter().any(|x| !x.is_finite()) {
                return Err("Attribute exceeds float32 range".into());
            }
            vertices.push(v);
        }
        // Translate relative to first corner for stable polygon calculations wherever the face lies.
        let anchor = positions[0];
        for p in &mut positions {
            for i in 0..3 {
                p[i] -= anchor[i];
            }
        }
        let Some(polygon) = triangulate::triangulate(&positions) else {
            skipped_degenerate += 1;
            continue;
        };
        forced_polygons += u64::from(polygon.forced);
        let face_normal = polygon.normal;
        // A corner taking its face's normal, under `s off` or where the smooth normal cancels
        // out, has a normal of its own: never shared.
        let mut own = vec![false; corners.len()];
        for ((corner, v), own) in corners.iter().zip(&mut vertices).zip(&mut own) {
            if v[3..6].iter().any(|n| *n != 0.) {
                continue;
            }
            let sum = if flat {
                [0.; 3]
            } else {
                obj::fetch::<3>(smooth.as_ref().unwrap(), corner.v, scene.counts[0])?
                    .map(f64::from_le_bytes)
            };
            let length = sum.iter().map(|x| x * x).sum::<f64>().sqrt();
            let normal = if length > 0. && length.is_finite() {
                sum.map(|x| (x / length) as f32)
            } else {
                *own = true;
                face_normal
            };
            v[3..6].copy_from_slice(&normal);
        }
        let mut p = open.take().unwrap();
        // The only split: where u32 indices run out. A face adds at most its corner count.
        if p.vertices + corners.len() as u64 > max_vertices {
            splits += 1;
            document.close(p, &mut binary)?;
            p = document.open(&mut dedup, key)?;
        }
        local.clear();
        local.resize(corners.len(), None);
        for triangle in polygon.triangles {
            for i in triangle {
                let index = match local[i] {
                    Some(index) => index,
                    None => {
                        let next = u32::try_from(p.vertices)?;
                        let found = if own[i] {
                            None
                        } else {
                            dedup.find_or_insert(corners[i], next)?
                        };
                        let index = match found {
                            Some(index) => index,
                            None => document.vertex(&mut p, &vertices[i], &mut binary)?,
                        };
                        local[i] = Some(index);
                        index
                    }
                };
                p.indices.write_all(&index.to_le_bytes())?;
            }
            p.triangles += 1;
        }
        open = Some(p);
        if progress && face_count.is_multiple_of(1_000_000) {
            crate::say!(
                "Converted {face_count} faces, {} binary bytes",
                document.offset
            );
        }
    }
    if let Some(p) = open.take() {
        document.close(p, &mut binary)?;
    }
    if splits > 0 {
        crate::say!("Warning: {splits} primitives reached the {max_vertices}-vertex limit of 32-bit indices and were split");
    }
    if repaired_normals > 0 {
        crate::say!("Warning: generated smooth normals for {repaired_normals} corners with zero-length source normals");
    }
    if skipped_degenerate > 0 {
        crate::say!("Warning: skipped {skipped_degenerate} faces with zero area");
    }
    if forced_polygons > 0 {
        crate::say!("Warning: triangulated {forced_polygons} self-intersecting or otherwise non-simple faces by forced ear clipping; their triangles may overlap");
    }
    binary.flush()?;
    Ok(document)
}
/// An image placed in a GLB's binary chunk: its file, offset and length.
type Embedded = (PathBuf, u64, u64);
/// Places each image after the geometry, 4-byte aligned, in a buffer view of its own, and
/// points the image at it. `contents` holds the image files in the order of `images`.
fn embed_images(
    document: &mut Document,
    images: Option<&mut Value>,
    contents: Vec<PathBuf>,
) -> Res<Vec<Embedded>> {
    let images = images.map_or(&mut [][..], |v| v.as_array_mut().unwrap());
    let mut placed = Vec::new();
    for (image, content) in images.iter_mut().zip(contents) {
        let length = fs::metadata(&content)
            .with_context(|| format!("Texture {}", shown(&content)))?
            .len();
        let offset = document.offset.next_multiple_of(4);
        image["bufferView"] = json!(document.views.len());
        document
            .views
            .push(json!({"buffer":0,"byteOffset":offset,"byteLength":length}));
        document.offset = offset + length;
        placed.push((content, offset, length));
    }
    Ok(placed)
}
/// Writes the GLB container (glTF 2.0, "GLB File Format Specification"): a 12-byte header, the
/// JSON chunk padded with spaces, then the binary chunk, which is the geometry payload followed by
/// the embedded images, padded with zeros. Every length is known before the first byte is
/// written, so the file is written in one sequential pass and only the JSON is held in memory.
fn write_glb(
    path: &Path,
    root: &Value,
    payload: &Path,
    images: &[Embedded],
    buffer_length: u64,
    outputs: &mut Outputs,
) -> Res<()> {
    let mut json = serde_json::to_vec(root)?;
    json.resize(json.len().next_multiple_of(4), b' ');
    let bin = buffer_length.next_multiple_of(4);
    let total = 12 + 8 + json.len() as u64 + 8 + bin;
    // GLB lengths are 32-bit; a .gltf's external .bin has no such limit.
    let total = u32::try_from(total).map_err(|_| {
        format!("The model needs {total} bytes, more than the 4 GiB a .glb can hold; write a .gltf instead")
    })?;
    let mut out = BufWriter::with_capacity(256 * 1024, outputs.create(path)?);
    out.write_all(b"glTF")?;
    out.write_all(&2u32.to_le_bytes())?;
    out.write_all(&total.to_le_bytes())?;
    out.write_all(&(json.len() as u32).to_le_bytes())?;
    out.write_all(b"JSON")?;
    out.write_all(&json)?;
    out.write_all(&(bin as u32).to_le_bytes())?;
    out.write_all(b"BIN\0")?;
    let mut written = std::io::copy(&mut File::open(payload)?, &mut out)?;
    for (content, offset, length) in images {
        out.write_all(&[0; 4][..(offset - written) as usize])?;
        let mut file =
            File::open(content).with_context(|| format!("Texture {}", shown(content)))?;
        // Its length is already in the JSON: had the file shrunk since, every later byte
        // would shift.
        if std::io::copy(&mut (&mut file).take(*length), &mut out)? != *length {
            return Err(format!("Texture {} changed during the conversion", shown(content)).into());
        }
        written = offset + length;
    }
    out.write_all(&[0; 4][..(bin - written) as usize])?;
    out.flush()?;
    Ok(())
}
/// Materials, textures, images and samplers for the primitives of `meshes`, whose material
/// indices it points at the written materials, with the file of each image in order. Copies the
/// images the output doesn't refer to in place, unless it `embed`s them, which leaves them
/// without a URI for `embed_images`.
fn appearance(
    scene: &Scene,
    meshes: &mut [Value],
    images: &HashMap<PathBuf, Plan>,
    transparent: &HashSet<PathBuf>,
    folder: &Path,
    outputs: &mut Outputs,
    embed: bool,
) -> Res<(Map<String, Value>, Vec<PathBuf>)> {
    let mut textures = Vec::new();
    let mut image_list = Vec::new();
    let mut contents = Vec::new();
    let mut samplers = Vec::new();
    let mut image_ids = HashMap::new();
    let mut sampler_ids = HashMap::new();
    // An image sampled with both wrap modes is one image and two textures.
    let mut texture_ids = HashMap::new();
    let mut materials = Vec::new();
    let mut extensions = BTreeSet::new();
    // A primitive without UVs cannot sample its material's texture, and glTF forbids it from
    // referencing one. It gets a copy of the material without the texture, as in obj2gltf
    // (`splitIncompatibleMaterials`), so its colour factors still apply. A primitive whose
    // vertex colours replace the diffuse colour (see `group::key`) gets a copy with a white base
    // colour, since glTF multiplies `COLOR_0` into it.
    let variant = |p: &Value| {
        let material = p["material"].as_u64().unwrap() as usize;
        let attributes = &p["attributes"];
        let textured =
            scene.materials[material].texture.is_some() && attributes.get("TEXCOORD_0").is_some();
        (material, textured, attributes.get("COLOR_0").is_some())
    };
    let used_materials: BTreeSet<(usize, bool, bool)> = meshes
        .iter()
        .flat_map(|mesh| mesh["primitives"].as_array().unwrap().iter().map(variant))
        .collect();
    let mut material_ids = HashMap::new();
    let mut invisible = BTreeSet::new();
    let mut invisible_names = Tally::new(
        "fully transparent materials, which will be invisible (d 0 or Tr 1; check whether the exporter writes Tr as opacity)",
    );
    for &(old_id, textured, colored) in &used_materials {
        let m = &scene.materials[old_id];
        material_ids.insert((old_id, textured, colored), materials.len());
        let mut blend = false;
        let texture = if let Some(path) = m.texture.as_ref().filter(|_| textured) {
            let path =
                fs::canonicalize(path).with_context(|| format!("Texture {}", shown(path)))?;
            let plan = &images[&path];
            blend = transparent.contains(&path);
            let image = match image_ids.get(&path) {
                Some(id) => *id,
                None => {
                    if embed {
                        image_list.push(json!({"mimeType":plan.mime_type()}));
                    } else {
                        plan.publish(folder, outputs)?;
                        image_list.push(json!({"uri":plan.uri}));
                    }
                    contents.push(plan.content.clone());
                    image_ids.insert(path.clone(), image_list.len() - 1);
                    image_list.len() - 1
                }
            };
            let sampler = *sampler_ids.entry(m.clamp).or_insert_with(|| {
                // CLAMP_TO_EDGE for `map_Kd -clamp on`, otherwise REPEAT, glTF's default.
                let wrap = if m.clamp { 33071 } else { 10497 };
                samplers.push(json!({"wrapS":wrap,"wrapT":wrap}));
                samplers.len() - 1
            });
            Some(*texture_ids.entry((path, m.clamp)).or_insert_with(|| {
                textures.push(json!({"source":image,"sampler":sampler}));
                textures.len() - 1
            }))
        } else {
            None
        };
        let mut value = m.json(texture, &mut extensions);
        // Kept literal, but flagged: a fully dissolved material is almost always an exporter
        // writing `Tr` as opacity, and silently invisible objects would leave no clue why.
        // Glass (`illum 4`/`9`) keeps alpha 1, so `d 0` there, clear glass, is not flagged.
        if value["pbrMetallicRoughness"]["baseColorFactor"][3] == 0. && invisible.insert(old_id) {
            invisible_names.add(&m.name);
        }
        if colored {
            value["name"] = json!(format!("{}-vertex-colors", m.name));
            for i in 0..3 {
                value["pbrMetallicRoughness"]["baseColorFactor"][i] = json!(1.);
            }
        } else if m.texture.is_some() && !textured {
            value["name"] = json!(format!("{}-untextured", m.name));
        }
        if blend {
            value["alphaMode"] = json!("BLEND");
        }
        materials.push(value);
    }
    for mesh in meshes.iter_mut() {
        for primitive in mesh["primitives"].as_array_mut().unwrap() {
            primitive["material"] = json!(material_ids[&variant(primitive)]);
        }
    }
    let mut look = Map::new();
    look.insert("materials".into(), json!(materials));
    // Without the transform a texture lands in the wrong place, so a viewer that cannot apply
    // it must refuse the model rather than show it wrong. Other extensions degrade gracefully.
    if extensions.contains("KHR_texture_transform") {
        look.insert(
            "extensionsRequired".into(),
            json!(["KHR_texture_transform"]),
        );
    }
    look.insert("extensionsUsed".into(), json!(extensions));
    if !image_list.is_empty() {
        look.insert("images".into(), json!(image_list));
        look.insert("textures".into(), json!(textures));
        look.insert("samplers".into(), json!(samplers));
    }
    Ok((look, contents))
}
/// URI path component escaping (not filesystem escaping).
pub fn uri(name: &str) -> String {
    let mut out = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{material::Origin, references::References};
    /// Primitive vertex counts after importing `obj` with a lowered vertex limit.
    fn import_with_limit(obj: &str, limit: u64) -> Vec<u64> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mesh2gltf-split-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let input = dir.join("in.obj");
        fs::write(&input, obj).unwrap();
        let output = dir.join("out.gltf");
        let options = crate::Options {
            input: input.clone(),
            output: output.clone(),
            tmp_dir: dir.clone(),
            ..Default::default()
        };
        let scene = obj::spool(
            &input,
            &dir,
            false,
            &References::unrestricted(),
            Origin::Model,
            true,
        )
        .unwrap();
        let groups = crate::group::group(&scene, &dir, false).unwrap();
        write_with_limit(scene, &groups, HashMap::new(), &dir, &options, limit).unwrap();
        let root: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        let primitives = root["meshes"][0]["primitives"].as_array().unwrap();
        let accessors = &root["accessors"];
        primitives
            .iter()
            .map(|p| {
                let position = p["attributes"]["POSITION"].as_u64().unwrap() as usize;
                accessors[position]["count"].as_u64().unwrap()
            })
            .collect()
    }
    #[test]
    fn a_glb_refuses_what_its_lengths_cannot_describe() {
        let dir = std::env::temp_dir().join(format!("mesh2gltf-glb-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let (payload, image, glb) = (dir.join("payload"), dir.join("image"), dir.join("out.glb"));
        fs::write(&payload, [0; 8]).unwrap();
        fs::write(&image, [1; 3]).unwrap();
        let root = json!({"asset":{"version":"2.0"}});
        let mut outputs = Outputs::default();
        let error = write_glb(&glb, &root, &payload, &[], 1 << 32, &mut outputs).unwrap_err();
        assert!(error.to_string().contains("4 GiB"), "{error}");
        assert!(!glb.exists());
        // An image shorter than the length its buffer view records.
        let images = [(image, 8, 5)];
        assert!(write_glb(&glb, &root, &payload, &images, 13, &mut outputs).is_err());
        drop(outputs);
        assert!(!glb.exists());
        fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn primitives_split_only_at_the_index_limit() {
        let obj = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3\nf 1 3 4\n";
        assert_eq!(import_with_limit(obj, 6), [4]);
        // The second face could add three vertices, which would pass the limit.
        assert_eq!(import_with_limit(obj, 5), [3, 3]);
    }
}
