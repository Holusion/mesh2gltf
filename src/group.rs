//! Group the face spool by primitive key, (object, material, UV layout, vertex colours), on disk.
//! Objects are already contiguous, since every `o` (or `g` before any `o`) starts a new one.
//! Materials can interleave inside an object; only then is the spool rewritten, by a counting
//! sort into a mapped file. File order is kept within each key.
use crate::obj::{self, Face, Scene};
use crate::{disk::map, Res};
use memmap2::{Mmap, MmapMut};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::BufReader,
    path::Path,
};
/// What the faces of a primitive share.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub object: usize,
    pub material: usize,
    pub uv: bool,
    /// The faces show vertex colours in place of the material's diffuse colour.
    pub colored: bool,
}
/// The `vc` spool, when the model has vertex colours.
pub fn colors(scene: &Scene, dir: &Path) -> Res<Option<Mmap>> {
    scene.colors.then(|| map(&dir.join("vc"))).transpose()
}
/// The key of `face`, and whether its texture hides vertex colours it has.
///
/// Vertex colours replace the diffuse colour, and an RGB texture replaces them, in the order of
/// VRML97's lighting model (ISO/IEC 14772-1, 4.14.3, table 4.6) and X3D's. OBJ defines no order.
/// glTF would multiply `COLOR_0` into a texture, which darkens one by the colours that
/// photogrammetry exporters bake from it. A face shows colours when any of its positions has one.
pub fn key(scene: &Scene, colors: Option<&[u8]>, face: &Face) -> Res<(Key, bool)> {
    let uv = face.corners[0].vt != 0;
    let textured = uv && scene.materials[face.material].texture.is_some();
    let mut has_colors = false;
    if let Some(spool) = colors {
        for c in &face.corners {
            if obj::color(spool, c.v, scene.counts[0])?.is_some() {
                has_colors = true;
                break;
            }
        }
    }
    let key = Key {
        object: face.object,
        material: face.material,
        uv,
        colored: has_colors && !textured,
    };
    Ok((key, has_colors && textured))
}
pub struct Group {
    pub faces: u64,
    pub corners: u64,
    bytes: u64,
}
/// Validate every face and return the keys in first-appearance order, with the spool's faces
/// rearranged so that each key's faces are contiguous and in that order.
pub fn group(scene: &Scene, dir: &Path, progress: bool) -> Res<Vec<Group>> {
    let path = dir.join("faces");
    let colors = colors(scene, dir)?;
    let mut faces = BufReader::with_capacity(256 * 1024, File::open(&path)?);
    let mut groups: Vec<Group> = Vec::new();
    let mut keys = HashMap::new();
    let mut current = usize::MAX;
    let mut interleaved = false;
    let mut count = 0u64;
    let mut untextured = 0u64;
    let mut hidden_colors = 0u64;
    while let Some(face) = obj::read_face(&mut faces)? {
        count += 1;
        let corners = &face.corners;
        let uv = corners[0].vt != 0;
        let normal = corners[0].vn != 0;
        if corners
            .iter()
            .any(|c| (c.vt != 0) != uv || (c.vn != 0) != normal)
        {
            return Err(format!("Face {count} mixes missing and present UVs/normals").into());
        }
        untextured += u64::from(scene.materials[face.material].texture.is_some() && !uv);
        let (key, hidden) = key(scene, colors.as_deref(), &face)?;
        hidden_colors += u64::from(hidden);
        let id = *keys.entry(key).or_insert_with(|| {
            groups.push(Group {
                faces: 0,
                corners: 0,
                bytes: 0,
            });
            groups.len() - 1
        });
        if id != current {
            interleaved |= groups[id].bytes > 0;
            current = id;
        }
        groups[id].faces += 1;
        groups[id].corners += corners.len() as u64;
        groups[id].bytes += 12 + 24 * corners.len() as u64;
    }
    if untextured > 0 {
        crate::warn!(code: "missing-texture-coordinates", "{untextured} faces with a textured material have no UVs; they use an untextured copy of it");
    }
    if hidden_colors > 0 {
        crate::warn!(code: "textured-vertex-colors", "vertex colors on {hidden_colors} textured faces are dropped, since the texture replaces them");
    }
    if !interleaved {
        return Ok(groups);
    }
    if progress {
        crate::info!("Grouping {count} faces by object and material");
    }
    let sorted_path = dir.join("faces-grouped");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&sorted_path)?;
    file.set_len(groups.iter().map(|g| g.bytes).sum())?;
    // SAFETY: the file was just created in the private workspace and is only accessed through
    // this mapping. Every byte is written before the mapping is dropped.
    let mut sorted = unsafe { MmapMut::map_mut(&file)? };
    let mut cursors = Vec::with_capacity(groups.len());
    let mut offset = 0;
    for g in &groups {
        cursors.push(usize::try_from(offset)?);
        offset += g.bytes;
    }
    let mut faces = BufReader::with_capacity(256 * 1024, File::open(&path)?);
    let mut record = Vec::new();
    let mut done = 0u64;
    while let Some(face) = obj::read_face(&mut faces)? {
        done += 1;
        if progress && done.is_multiple_of(1_000_000) {
            let message = format!("Grouping: moved {done} faces");
            crate::diagnostics::progress("group", done, count, &message);
        }
        let id = keys[&key(scene, colors.as_deref(), &face)?.0];
        record.clear();
        obj::encode_face(
            &mut record,
            face.object,
            face.material,
            face.flat,
            &face.corners,
        );
        sorted[cursors[id]..cursors[id] + record.len()].copy_from_slice(&record);
        cursors[id] += record.len();
    }
    // No msync: the page cache already serves the replay, and eviction writes the pages back.
    drop(sorted);
    drop(file);
    drop(faces);
    fs::rename(&sorted_path, &path)?;
    Ok(groups)
}
