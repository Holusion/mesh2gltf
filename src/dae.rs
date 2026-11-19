//! COLLADA (`.dae`) decoding into the same spools as OBJ and PLY, plus a node tree.
//!
//! COLLADA refers to things by `#id`, forward as well as back, and exporters order its libraries
//! as they like: SketchUp writes the visual scene before the geometry it instances. A first pass
//! over a read-only map of the file therefore keeps only small records: where each big array
//! lies in the file, each primitive's inputs, the materials and the node tree. A second pass then
//! decodes the arrays straight from the map into the spools, so that however large a
//! `<float_array>` or `<p>` is, it never reaches the heap.
//!
//! Supported: COLLADA 1.4.1 `triangles` and `polylist` with VERTEX, NORMAL and one TEXCOORD set.
//! Other primitives and inputs are omitted with one counted warning each.
use crate::{
    diagnostics::{safe, shown, Tally},
    failure::Context,
    fixed::{parse_decimal_fixed, BBox},
    material::Material,
    obj::{encode_face, Corner, Scene, MAX_FACE},
    references::References,
    scene::{multiply, Basis, Graph, Instance, Matrix, Node, IDENTITY},
    Res,
};
use memmap2::Mmap;
use quick_xml::{
    events::{BytesStart, Event},
    Reader, XmlVersion,
};
use std::{
    collections::HashMap,
    fs::File,
    io::{BufWriter, Write},
    ops::Range,
    path::Path,
};
/// Bounds the text of an element read whole: a matrix, a colour, a file name.
const MAX_TEXT: usize = 1024 * 1024;
/// Bounds the node tree once `instance_node` references are expanded, which can multiply it.
const MAX_NODES: usize = 1 << 22;
/// Elements whose text is read whole, as opposed to the big arrays, which are only located.
const SMALL_TEXT: &[&str] = &["matrix", "translate", "rotate", "scale", "up_axis"];

/// `<accessor>`: how the records of a source sit in its array.
struct Accessor {
    array: String,
    count: u64,
    stride: u64,
    offset: u64,
    /// The positions, within a record, of the named `<param>`s, which are the values used.
    components: Vec<u64>,
}
struct Input {
    semantic: String,
    offset: u64,
    set: u64,
    /// The id of a `<source>`, or of the `<vertices>` for VERTEX.
    source: String,
}
struct Primitive {
    kind: String,
    count: u64,
    material: Option<String>,
    inputs: Vec<Input>,
    vcount: Option<Range<usize>>,
    p: Option<Range<usize>>,
}
struct Geometry {
    name: String,
    /// The inputs of `<vertices>`, which VERTEX stands for.
    vertices: Vec<Input>,
    primitives: Vec<Primitive>,
}
struct Instanced {
    url: String,
    /// `instance_material symbol → target`.
    bindings: Vec<(String, String)>,
}
struct DaeNode {
    name: String,
    parent: Option<usize>,
    /// The node's transforms, composed in document order: each applies before the ones above.
    matrix: Matrix,
    geometries: Vec<Instanced>,
}
struct VisualScene {
    name: String,
    roots: Vec<usize>,
}
/// Everything the first pass keeps: small records, and byte ranges for the big arrays.
#[derive(Default)]
struct Document {
    basis: Option<Basis>,
    arrays: HashMap<String, (Range<usize>, u64)>,
    sources: HashMap<String, Accessor>,
    geometries: Vec<Geometry>,
    geometry_ids: HashMap<String, usize>,
    nodes: Vec<DaeNode>,
    visual_scenes: HashMap<String, VisualScene>,
    first_visual_scene: Option<String>,
    scene: Option<String>,
}
/// `#id` → `id`. A reference to another document can't be followed.
fn local(url: &str) -> Res<&str> {
    url.strip_prefix('#')
        .ok_or_else(|| format!("COLLADA reference outside the document: {}", safe(url)).into())
}
fn attribute(e: &BytesStart, name: &str) -> Res<Option<String>> {
    for a in e.attributes() {
        let a = a?;
        if a.key.local_name().as_ref() == name {
            return Ok(Some(
                a.normalized_value(XmlVersion::Implicit1_0)?.into_owned(),
            ));
        }
    }
    Ok(None)
}
fn number<T: std::str::FromStr>(e: &BytesStart, name: &str) -> Res<Option<T>> {
    attribute(e, name)?
        .map(|v| {
            v.trim()
                .parse()
                .map_err(|_| format!("Invalid COLLADA {name}: {}", safe(&v)).into())
        })
        .transpose()
}
/// Whitespace-separated values of an array, read from the file map.
struct Values<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Values<'a> {
    fn new(map: &'a [u8], range: &Range<usize>) -> Self {
        Self {
            bytes: &map[range.clone()],
            at: 0,
        }
    }
    fn next_token(&mut self) -> Res<Option<&'a str>> {
        let b = self.bytes;
        while self.at < b.len() && b[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
        let start = self.at;
        while self.at < b.len() && !b[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
        if start == self.at {
            return Ok(None);
        }
        Ok(Some(std::str::from_utf8(&b[start..self.at])?))
    }
    fn token(&mut self, what: &str) -> Res<&'a str> {
        self.next_token()?
            .ok_or_else(|| format!("COLLADA {what} ends before its count").into())
    }
    fn index(&mut self, what: &str) -> Res<u64> {
        let t = self.token(what)?;
        t.parse()
            .map_err(|_| format!("Invalid COLLADA index: {}", safe(t)).into())
    }
}
fn floats(text: &str, n: usize, what: &str) -> Res<Vec<f64>> {
    let values: Vec<f64> = text
        .split_ascii_whitespace()
        .map(|t| t.parse::<f64>().ok().filter(|v| v.is_finite()))
        .collect::<Option<_>>()
        .ok_or_else(|| format!("Invalid COLLADA {what}: {}", safe(text)))?;
    if values.len() != n {
        return Err(format!("COLLADA {what} needs {n} values: {}", safe(text)).into());
    }
    Ok(values)
}
/// A COLLADA `<matrix>`, row-major, as a column-major matrix.
fn row_major(text: &str) -> Res<Matrix> {
    let m = floats(text, 16, "matrix")?;
    Ok(std::array::from_fn(|i| m[(i % 4) * 4 + i / 4]))
}
/// A node transform element as a matrix: `<matrix>` is row-major, `<rotate>` an axis and an
/// angle in degrees.
fn transform_matrix(kind: &str, text: &str) -> Res<Matrix> {
    let mut m = IDENTITY;
    match kind {
        "matrix" => m = row_major(text)?,
        "translate" => m[12..15].copy_from_slice(&floats(text, 3, kind)?),
        "scale" => {
            let s = floats(text, 3, kind)?;
            for i in 0..3 {
                m[i * 5] = s[i];
            }
        }
        _ => {
            let r = floats(text, 4, kind)?;
            let length = (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt();
            if r[3] == 0. {
                return Ok(m);
            }
            if length == 0. {
                return Err("COLLADA rotate has no axis".into());
            }
            let [x, y, z] = [r[0] / length, r[1] / length, r[2] / length];
            let (sin, cos) = r[3].to_radians().sin_cos();
            let t = 1. - cos;
            // Rodrigues' rotation, by columns.
            m = [
                t * x * x + cos,
                t * x * y + sin * z,
                t * x * z - sin * y,
                0.,
                t * x * y - sin * z,
                t * y * y + cos,
                t * y * z + sin * x,
                0.,
                t * x * z + sin * y,
                t * y * z - sin * x,
                t * z * z + cos,
                0.,
                0.,
                0.,
                0.,
                1.,
            ];
        }
    }
    Ok(m)
}
/// The first pass. Big arrays are located, not read.
fn index(map: &[u8], unsupported: &mut Tally) -> Res<Document> {
    let mut doc = Document::default();
    let mut reader = Reader::from_reader(map);
    reader.config_mut().expand_empty_elements = true;
    let mut stack: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut span = 0..0;
    let mut geometry: Option<usize> = None;
    let mut source: Option<String> = None;
    let mut primitive: Option<Primitive> = None;
    let mut array: Option<(String, u64)> = None;
    let mut node_stack: Vec<usize> = Vec::new();
    let mut visual_scene: Option<String> = None;
    let mut instance: Option<Instanced> = None;
    let position = |reader: &Reader<&[u8]>| reader.buffer_position() as usize;
    loop {
        let event = reader.read_event().map_err(|e| {
            format!(
                "Invalid COLLADA XML near byte {}: {e}",
                reader.error_position()
            )
        })?;
        match event {
            Event::Start(e) => {
                let name = e.local_name().as_ref().to_owned();
                let parent = stack.last().map(String::as_str);
                if parent.is_none() && name != "COLLADA" {
                    return Err("Not a COLLADA document: the root element isn't COLLADA".into());
                }
                match (parent, name.as_str()) {
                    (Some("COLLADA"), "asset") => {}
                    (Some("asset"), "unit") if stack.len() == 2 => {
                        let unit: f64 = number(&e, "meter")?.unwrap_or(1.);
                        if !(unit.is_finite() && unit > 0.) {
                            return Err("Invalid COLLADA unit".into());
                        }
                        doc.basis
                            .get_or_insert(Basis {
                                axes: Basis::Y_UP,
                                unit: 1.,
                            })
                            .unit = unit;
                    }
                    (Some("library_geometries"), "geometry") => {
                        let id = attribute(&e, "id")?.unwrap_or_default();
                        let name = attribute(&e, "name")?.unwrap_or_else(|| id.clone());
                        doc.geometry_ids.insert(id, doc.geometries.len());
                        doc.geometries.push(Geometry {
                            name,
                            vertices: Vec::new(),
                            primitives: Vec::new(),
                        });
                    }
                    (Some("geometry"), "mesh") => geometry = Some(doc.geometries.len() - 1),
                    (Some("geometry"), other) if other != "extra" && other != "asset" => {
                        unsupported.add(other);
                    }
                    (Some("mesh"), "source") => source = attribute(&e, "id")?,
                    (Some("source"), "float_array") if source.is_some() => {
                        let id = attribute(&e, "id")?.unwrap_or_default();
                        array = Some((id, number(&e, "count")?.unwrap_or(0)));
                    }
                    (Some("technique_common"), "accessor") if source.is_some() => {
                        let accessor = Accessor {
                            array: local(&attribute(&e, "source")?.unwrap_or_default())?.into(),
                            count: number(&e, "count")?.ok_or("COLLADA accessor has no count")?,
                            stride: number(&e, "stride")?.unwrap_or(1),
                            offset: number(&e, "offset")?.unwrap_or(0),
                            components: Vec::new(),
                        };
                        doc.sources.insert(source.clone().unwrap(), accessor);
                    }
                    (Some("accessor"), "param") if source.is_some() => {
                        let accessor = doc.sources.get_mut(source.as_ref().unwrap()).unwrap();
                        let at = accessor.components.len() as u64;
                        // Unnamed params are skipped values, and take a place in the record.
                        accessor
                            .components
                            .push(if attribute(&e, "name")?.is_some() {
                                at
                            } else {
                                u64::MAX
                            });
                    }
                    (Some("vertices"), "input") if geometry.is_some() => {
                        let input = read_input(&e)?;
                        doc.geometries[geometry.unwrap()].vertices.push(input);
                    }
                    (Some("mesh"), kind) if !matches!(kind, "source" | "vertices" | "extra") => {
                        primitive = Some(Primitive {
                            kind: kind.into(),
                            count: number(&e, "count")?.unwrap_or(0),
                            material: attribute(&e, "material")?,
                            inputs: Vec::new(),
                            vcount: None,
                            p: None,
                        });
                    }
                    (Some(_), "input") if primitive.is_some() => {
                        let input = read_input(&e)?;
                        primitive.as_mut().unwrap().inputs.push(input);
                    }
                    (Some("library_visual_scenes"), "visual_scene") => {
                        let id = attribute(&e, "id")?.unwrap_or_default();
                        let name = attribute(&e, "name")?.unwrap_or_else(|| id.clone());
                        doc.first_visual_scene.get_or_insert(id.clone());
                        doc.visual_scenes.insert(
                            id.clone(),
                            VisualScene {
                                name,
                                roots: Vec::new(),
                            },
                        );
                        visual_scene = Some(id);
                    }
                    (Some("visual_scene" | "node"), "node") if visual_scene.is_some() => {
                        if doc.nodes.len() == MAX_NODES {
                            return Err("COLLADA scene has too many nodes".into());
                        }
                        let id = attribute(&e, "id")?;
                        let name = attribute(&e, "name")?
                            .or(id)
                            .unwrap_or_else(|| "node".into());
                        let parent = node_stack.last().copied();
                        if parent.is_none() {
                            let scene = visual_scene.as_ref().unwrap();
                            doc.visual_scenes
                                .get_mut(scene)
                                .unwrap()
                                .roots
                                .push(doc.nodes.len());
                        }
                        node_stack.push(doc.nodes.len());
                        doc.nodes.push(DaeNode {
                            name,
                            parent,
                            matrix: IDENTITY,
                            geometries: Vec::new(),
                        });
                    }
                    (Some("node"), "instance_geometry") if !node_stack.is_empty() => {
                        instance = Some(Instanced {
                            url: local(&attribute(&e, "url")?.unwrap_or_default())?.into(),
                            bindings: Vec::new(),
                        });
                    }
                    (Some("technique_common"), "instance_material") if instance.is_some() => {
                        let symbol = attribute(&e, "symbol")?.unwrap_or_default();
                        let target = attribute(&e, "target")?.unwrap_or_default();
                        let target = local(&target)?.to_owned();
                        instance.as_mut().unwrap().bindings.push((symbol, target));
                    }
                    (Some("node"), other)
                        if !node_stack.is_empty() && other.starts_with("instance_") =>
                    {
                        unsupported.add(other);
                    }
                    (Some("scene"), "instance_visual_scene") => {
                        doc.scene = Some(local(&attribute(&e, "url")?.unwrap_or_default())?.into());
                    }
                    (Some("COLLADA"), library)
                        if library.starts_with("library_")
                            && !matches!(
                                library,
                                "library_geometries" | "library_visual_scenes"
                            ) =>
                    {
                        unsupported.add(library);
                    }
                    _ => {}
                }
                stack.push(name);
                text.clear();
                span = position(&reader)..position(&reader);
            }
            Event::Text(t) => {
                span.end = position(&reader);
                if stack
                    .last()
                    .is_some_and(|n| SMALL_TEXT.contains(&n.as_str()))
                {
                    if text.len() + t.len() > MAX_TEXT {
                        return Err("COLLADA element text exceeds 1 MiB".into());
                    }
                    text.push_str(&t);
                }
            }
            Event::GeneralRef(_) | Event::CData(_) | Event::Comment(_) => {
                span.end = position(&reader);
            }
            Event::End(_) => {
                let name = stack.pop().unwrap_or_default();
                let parent = stack.last().map(String::as_str);
                match (parent, name.as_str()) {
                    (Some("asset"), "up_axis") if stack.len() == 2 => {
                        let axes = match text.trim() {
                            "Y_UP" => Basis::Y_UP,
                            "Z_UP" => Basis::Z_UP,
                            // X up, −Y right: a quarter turn about Z.
                            "X_UP" => [[0., -1., 0.], [1., 0., 0.], [0., 0., 1.]],
                            other => {
                                return Err(
                                    format!("Invalid COLLADA up_axis {}", safe(other)).into()
                                )
                            }
                        };
                        doc.basis
                            .get_or_insert(Basis {
                                axes: Basis::Y_UP,
                                unit: 1.,
                            })
                            .axes = axes;
                    }
                    (_, "float_array") => {
                        if let Some((id, count)) = array.take() {
                            doc.arrays.insert(id, (span.clone(), count));
                        }
                    }
                    (Some("mesh"), "source") => source = None,
                    (Some(_), "vcount") if primitive.is_some() => {
                        primitive.as_mut().unwrap().vcount = Some(span.clone());
                    }
                    (Some(_), "p") if primitive.is_some() => {
                        let p = primitive.as_mut().unwrap();
                        if p.p.replace(span.clone()).is_some() {
                            // `<polygons>` has one `<p>` per polygon; it is omitted anyway.
                            p.p = None;
                        }
                    }
                    (Some("mesh"), _) if primitive.is_some() => {
                        let p = primitive.take().unwrap();
                        doc.geometries[geometry.unwrap()].primitives.push(p);
                    }
                    (Some("geometry"), "mesh") => geometry = None,
                    (Some("node"), transform @ ("matrix" | "translate" | "rotate" | "scale"))
                        if !node_stack.is_empty() =>
                    {
                        let m = transform_matrix(transform, &text)?;
                        let node = &mut doc.nodes[*node_stack.last().unwrap()];
                        node.matrix = multiply(&node.matrix, &m);
                    }
                    (Some("node"), transform @ ("lookat" | "skew")) if !node_stack.is_empty() => {
                        // Ignoring it would misplace everything below the node.
                        return Err(
                            format!("COLLADA {transform} transforms are not supported").into()
                        );
                    }
                    (Some("node"), "instance_geometry") => {
                        if let (Some(i), Some(&n)) = (instance.take(), node_stack.last()) {
                            doc.nodes[n].geometries.push(i);
                        }
                    }
                    (_, "node") if visual_scene.is_some() => {
                        node_stack.pop();
                    }
                    (Some("library_visual_scenes"), "visual_scene") => visual_scene = None,
                    _ => {}
                }
                text.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err("COLLADA document ends before its last element closes".into());
    }
    Ok(doc)
}
fn read_input(e: &BytesStart) -> Res<Input> {
    Ok(Input {
        semantic: attribute(e, "semantic")?.unwrap_or_default(),
        offset: number(e, "offset")?.unwrap_or(0),
        set: number(e, "set")?.unwrap_or(0),
        source: local(&attribute(e, "source")?.unwrap_or_default())?.into(),
    })
}
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Role {
    Position,
    Uv,
    Normal,
}
/// The decode pass's output side: the spools and what has been written to them.
struct Spools {
    v: BufWriter<File>,
    vt: BufWriter<File>,
    vn: BufWriter<File>,
    faces: BufWriter<File>,
    counts: [u64; 3],
    /// Where each source spooled so far starts, as the index before its first record, and how
    /// many records it has.
    bases: HashMap<(String, Role), (u64, u64)>,
}
impl Spools {
    /// Spools `source` for `role` once, and returns its base and record count.
    fn source(
        &mut self,
        doc: &Document,
        map: &[u8],
        id: &str,
        role: Role,
        bbox: &mut BBox,
        local: &mut BBox,
    ) -> Res<(u64, u64)> {
        if let Some(found) = self.bases.get(&(id.to_owned(), role)) {
            return Ok(*found);
        }
        let accessor = doc
            .sources
            .get(id)
            .ok_or_else(|| format!("COLLADA source {} is missing", safe(id)))?;
        let (range, array_count) = doc
            .arrays
            .get(&accessor.array)
            .ok_or_else(|| format!("COLLADA array {} is missing", safe(&accessor.array)))?;
        let width = match role {
            Role::Position | Role::Normal => 3,
            Role::Uv => 2,
        };
        // The record positions of the first named params: X Y Z, S T, or whatever names they use.
        let mut wanted: Vec<u64> = accessor
            .components
            .iter()
            .copied()
            .filter(|c| *c != u64::MAX)
            .take(width)
            .collect();
        if accessor.components.is_empty() {
            wanted = (0..width as u64).collect();
        }
        if wanted.len() < width || accessor.stride < accessor.components.len() as u64 {
            return Err(format!("COLLADA source {} has too few components", safe(id)).into());
        }
        let needed = accessor
            .count
            .checked_mul(accessor.stride)
            .and_then(|n| n.checked_add(accessor.offset))
            .ok_or("COLLADA accessor overflows")?;
        if needed > *array_count {
            return Err(format!("COLLADA source {} is longer than its array", safe(id)).into());
        }
        let mut values = Values::new(map, range);
        for _ in 0..accessor.offset {
            values.token("float_array")?;
        }
        let base = self.counts[role as usize];
        let mut tokens = vec![""; accessor.stride as usize];
        for _ in 0..accessor.count {
            for t in &mut tokens {
                *t = values.token("float_array")?;
            }
            match role {
                Role::Position => {
                    let mut p = [0; 3];
                    for (p, c) in p.iter_mut().zip(&wanted) {
                        *p = parse_decimal_fixed(tokens[*c as usize])?;
                    }
                    bbox.add(p);
                    local.add(p);
                    for n in p {
                        self.v.write_all(&n.to_le_bytes())?;
                    }
                }
                Role::Normal | Role::Uv => {
                    let out = if role == Role::Normal {
                        &mut self.vn
                    } else {
                        &mut self.vt
                    };
                    for c in &wanted {
                        let t = tokens[*c as usize];
                        let v: f64 = t
                            .parse()
                            .ok()
                            .filter(|v: &f64| v.is_finite())
                            .ok_or_else(|| format!("Invalid COLLADA value: {}", safe(t)))?;
                        out.write_all(&v.to_le_bytes())?;
                    }
                }
            }
        }
        self.counts[role as usize] += accessor.count;
        let found = (base, accessor.count);
        self.bases.insert((id.to_owned(), role), found);
        Ok(found)
    }
}
/// Where one attribute of a corner comes from: which value of the corner's indices, the source's
/// base in its spool, and its record count.
#[derive(Clone, Copy)]
struct Attribute {
    offset: usize,
    base: u64,
    count: u64,
}
impl Attribute {
    fn index(&self, indices: &[u64]) -> Res<u64> {
        let i = indices[self.offset];
        if i >= self.count {
            return Err(format!("COLLADA index {i} outside its source of {}", self.count).into());
        }
        Ok(self.base + i + 1)
    }
}
struct Warnings {
    unsupported: Tally,
    inputs: Tally,
    unbound: Tally,
    bindings: Tally,
    short_faces: u64,
}
pub fn spool(input: &Path, dir: &Path, progress: bool, references: &References) -> Res<Scene> {
    let _ = references;
    let file = File::open(input).with_context(|| shown(input))?;
    // SAFETY: the input is only read. Another process truncating it during the import would
    // raise SIGBUS, as a full disk does on the workspace's sparse maps.
    let map = unsafe { Mmap::map(&file)? };
    let mut warnings = Warnings {
        unsupported: Tally::new("unsupported COLLADA content omitted"),
        inputs: Tally::new("unsupported COLLADA inputs omitted"),
        unbound: Tally::new("COLLADA material symbols without a binding, using the default"),
        bindings: Tally::new(
            "COLLADA geometries instanced with another material binding, keeping the first",
        ),
        short_faces: 0,
    };
    let doc = index(&map, &mut warnings.unsupported).with_context(|| shown(input))?;
    if progress {
        crate::say!(
            "Indexed COLLADA: {} geometries, {} nodes",
            doc.geometries.len(),
            doc.nodes.len()
        );
    }
    let scene = decode(&doc, &map, dir, &mut warnings).with_context(|| shown(input))?;
    if warnings.short_faces > 0 {
        crate::say!(
            "Warning: omitted {} faces with fewer than three corners",
            warnings.short_faces
        );
    }
    Ok(scene)
}
/// The second pass: the node tree, then each instanced geometry, in file order.
fn decode(doc: &Document, map: &[u8], dir: &Path, warnings: &mut Warnings) -> Res<Scene> {
    let scene_id = doc
        .scene
        .as_ref()
        .or(doc.first_visual_scene.as_ref())
        .ok_or("COLLADA document has no visual scene")?;
    let visual = doc
        .visual_scenes
        .get(scene_id)
        .ok_or_else(|| format!("COLLADA visual scene {} is missing", safe(scene_id)))?;
    // Each node, and the geometry each instance names, with the first binding of each.
    let mut nodes: Vec<Node> = Vec::new();
    let mut instanced: Vec<Option<usize>> = Vec::new();
    let mut bindings: HashMap<usize, &[(String, String)]> = HashMap::new();
    let mut pending: Vec<(usize, Option<u32>)> =
        visual.roots.iter().rev().map(|&r| (r, None)).collect();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); doc.nodes.len()];
    for (i, n) in doc.nodes.iter().enumerate() {
        if let Some(p) = n.parent {
            children[p].push(i);
        }
    }
    while let Some((n, parent)) = pending.pop() {
        let source = &doc.nodes[n];
        let at = u32::try_from(nodes.len())?;
        nodes.push(Node {
            name: source.name.clone(),
            parent,
            matrix: source.matrix,
            mesh: None,
        });
        instanced.push(None);
        for (k, g) in source.geometries.iter().enumerate() {
            let geometry = *doc
                .geometry_ids
                .get(&g.url)
                .ok_or_else(|| format!("COLLADA geometry {} is missing", safe(&g.url)))?;
            let first = bindings.entry(geometry).or_insert(&g.bindings);
            if *first != g.bindings.as_slice() {
                warnings.bindings.add(&doc.geometries[geometry].name);
            }
            // A node's first instance is the node's mesh, and the others are child nodes.
            if k > 0 {
                nodes.push(Node {
                    name: doc.geometries[geometry].name.clone(),
                    parent: Some(at),
                    matrix: IDENTITY,
                    mesh: None,
                });
                instanced.push(None);
            }
            *instanced.last_mut().unwrap() = Some(geometry);
            if nodes.len() > MAX_NODES {
                return Err("COLLADA scene has too many nodes".into());
            }
        }
        for &c in children[n].iter().rev() {
            pending.push((c, Some(at)));
        }
    }
    // Objects in file order, so that the decode pass reads the map front to back.
    let mut used: Vec<usize> = bindings.keys().copied().collect();
    used.sort_unstable();
    let mut object_of = HashMap::new();
    let mut objects = vec!["default".to_owned()];
    for &g in &used {
        object_of.insert(g, u32::try_from(objects.len())?);
        objects.push(doc.geometries[g].name.clone());
    }
    for (node, geometry) in nodes.iter_mut().zip(&instanced) {
        if let Some(g) = geometry {
            node.mesh = Some(Instance {
                object: object_of[g],
                geometry: IDENTITY,
            });
        }
    }
    let mut spools = Spools {
        v: BufWriter::new(File::create(dir.join("v"))?),
        vt: BufWriter::new(File::create(dir.join("vt"))?),
        vn: BufWriter::new(File::create(dir.join("vn"))?),
        faces: BufWriter::new(File::create(dir.join("faces"))?),
        counts: [0; 3],
        bases: HashMap::new(),
    };
    let materials = vec![Material::new("default")];
    let mut bbox = BBox::new();
    let mut bounds = vec![BBox::new(); objects.len()];
    let mut missing_normals = false;
    for &g in &used {
        let object = object_of[&g];
        let geometry = &doc.geometries[g];
        let mut local = BBox::new();
        for primitive in &geometry.primitives {
            if let Some(symbol) = &primitive.material {
                if !bindings[&g].iter().any(|(s, _)| s == symbol) {
                    warnings.unbound.add(symbol);
                }
            }
            let material = 0;
            missing_normals |= decode_primitive(
                doc,
                map,
                geometry,
                primitive,
                object,
                material,
                &mut spools,
                &mut bbox,
                &mut local,
                warnings,
            )
            .with_context(|| format!("Geometry {}", safe(&geometry.name)))?;
        }
        bounds[object as usize] = local;
    }
    for w in [
        &mut spools.v,
        &mut spools.vt,
        &mut spools.vn,
        &mut spools.faces,
    ] {
        w.flush()?;
    }
    Ok(Scene {
        bbox,
        missing_normals,
        colors: false,
        objects,
        materials,
        counts: spools.counts,
        graph: Some(Graph {
            name: visual.name.clone(),
            nodes,
            basis: doc.basis.unwrap_or(Basis {
                axes: Basis::Y_UP,
                unit: 1.,
            }),
            bounds,
        }),
    })
}
/// Writes the faces of one primitive. Returns whether they lack normals.
#[allow(clippy::too_many_arguments)]
fn decode_primitive(
    doc: &Document,
    map: &[u8],
    geometry: &Geometry,
    primitive: &Primitive,
    object: u32,
    material: u32,
    spools: &mut Spools,
    bbox: &mut BBox,
    local: &mut BBox,
    warnings: &mut Warnings,
) -> Res<bool> {
    let kind = primitive.kind.as_str();
    if !matches!(kind, "triangles" | "polylist") {
        warnings.unsupported.add(kind);
        return Ok(false);
    }
    let stride = primitive
        .inputs
        .iter()
        .map(|i| i.offset + 1)
        .max()
        .unwrap_or(0) as usize;
    // Each input, with the value of a corner's indices it reads: VERTEX stands for the inputs of
    // `<vertices>`, which all read the VERTEX index.
    let mut position = None;
    let mut normal = None;
    let mut uvs: Vec<(&Input, usize)> = Vec::new();
    for input in &primitive.inputs {
        let own = [(input, input.offset as usize)];
        let expanded: Vec<_> = if input.semantic == "VERTEX" {
            geometry
                .vertices
                .iter()
                .map(|v| (v, input.offset as usize))
                .collect()
        } else {
            own.to_vec()
        };
        for (input, at) in expanded {
            match input.semantic.as_str() {
                "POSITION" => position = Some((input, at)),
                "NORMAL" => normal = Some((input, at)),
                "TEXCOORD" => uvs.push((input, at)),
                other => warnings.inputs.add(other),
            }
        }
    }
    // One UV set: the lowest.
    uvs.sort_by_key(|(input, _)| input.set);
    for (extra, _) in uvs.iter().skip(1) {
        warnings.inputs.add(&format!("TEXCOORD set {}", extra.set));
    }
    let uv = uvs.first().copied();
    let (position, at) = position.ok_or("COLLADA primitive has no positions")?;
    let mut attribute = |input: &Input, offset: usize, role: Role| -> Res<Attribute> {
        let (base, count) = spools.source(doc, map, &input.source, role, bbox, local)?;
        Ok(Attribute {
            offset,
            base,
            count,
        })
    };
    let position = attribute(position, at, Role::Position)?;
    let normal = normal
        .map(|(n, at)| attribute(n, at, Role::Normal))
        .transpose()?;
    let uv = uv.map(|(u, at)| attribute(u, at, Role::Uv)).transpose()?;
    let Some(p) = &primitive.p else {
        return Ok(normal.is_none());
    };
    let mut indices_of = Values::new(map, p);
    let mut sizes = primitive.vcount.as_ref().map(|r| Values::new(map, r));
    if kind == "polylist" && sizes.is_none() && primitive.count > 0 {
        return Err("COLLADA polylist has no vcount".into());
    }
    let mut indices = vec![0u64; stride];
    let mut corners = Vec::new();
    let mut record = Vec::new();
    for _ in 0..primitive.count {
        let n = match &mut sizes {
            Some(s) => s.index("vcount")?,
            None => 3,
        };
        corners.clear();
        for _ in 0..n {
            for i in &mut indices {
                *i = indices_of.index("p")?;
            }
            if corners.len() == MAX_FACE {
                return Err(format!("Face exceeds {MAX_FACE} corners").into());
            }
            corners.push(Corner {
                v: position.index(&indices)?,
                vt: uv.map(|a| a.index(&indices)).transpose()?.unwrap_or(0),
                vn: normal.map(|a| a.index(&indices)).transpose()?.unwrap_or(0),
            });
        }
        if corners.len() < 3 {
            warnings.short_faces += 1;
            continue;
        }
        record.clear();
        encode_face(
            &mut record,
            object as usize,
            material as usize,
            false,
            &corners,
        );
        spools.faces.write_all(&record)?;
    }
    if indices_of.next_token()?.is_some() {
        return Err("COLLADA p has more indices than its count".into());
    }
    if let Some(s) = &mut sizes {
        if s.next_token()?.is_some() {
            return Err("COLLADA vcount has more values than its count".into());
        }
    }
    Ok(normal.is_none())
}
