# COLLADA (`.dae`) import: draft

Working note for the `collada-support` branch. Delete it before merging: the README and wiki
take over once the behaviour exists.

## Decisions

FBX support is planned on the `fbx-support` branch (`FBX-DRAFT.md`). It shares this branch's
scene layer, so the layer below is shaped for both formats.

1. XML is read with **`quick-xml` over a memory map of the input**.
2. The COLLADA **node tree is kept** in the glTF; transforms aren't baked into positions.
3. **`<unit meter>` is applied** as a scale on the root node, as three.js's ColladaLoader and
   Blender's importer do.
4. **`<up_axis>` is followed**: `Z_UP` gives the same rotation as `--up-axis z`, and an explicit
   `--up-axis` overrides it with a note.
5. **Missing textures keep the current rule**: an unreachable texture fails the conversion.
6. **Identical materials are merged only if it is cheap**: see "Duplicate materials".
7. **Blender's COLLADA importer is the reference** for appearance and placement; three.js only
   where it plainly makes more sense.
8. **One scene layer for COLLADA and FBX**: see "Shared scene layer".
9. **No feature flags and no scene-related options.** One binary. A second binary, for scenes,
   only if scene-specific options ever become necessary; the default path stays simple.

## What the samples contain

The survey covers the 22 paths in `collada.txt`. The four D13–16 are identical copies and
D19 is a directory, so there are 18 distinct files, from 878 B to 2.8 GB.
It was produced by a streaming scanner (`survey_dae.py`, kept outside the repository) that
records tags, attributes and small texts and samples the first 3,000 numbers of each array.

| Sample | Size | Exporter | Up, unit | Shape of the data |
| --- | --- | --- | --- | --- |
| D11 | 2.8 GB | FreeCAD (pycollada)¹ | Z, m | 20,272 geometries, nodes, materials and effects, one each; identity nodes; `triangles` VERTEX+NORMAL |
| D8 | 1.8 GB | FreeCAD (pycollada) | Z, m | Same pattern, 27,023 of each |
| D6 | 945 MB | FreeCAD (pycollada) | Z, m | Same pattern, 955 of each |
| D3 | 442 MB | FBX COLLADA exporter | Y, 0.001 (named "centimeter") | 287 geometries, VERTEX+NORMAL+TEXCOORD; node matrices up to 24,722 units; 11 significant digits; 60,942 stacked `<texture>` layers in one `diffuse` |
| D9 | 226 MB | FreeCAD (pycollada) | Z, m | 1 geometry, 2.6 M positions; largest text node 115 MB (`<p>`) |
| D4 | 176 MB | SketchUp 17 | Z, inch | 94,511 geometries, 71,480 of them `lines` only; 11,185 nodes, 5,145 `instance_node`, depth 8; positions up to 1.2·10⁷ in (315 km) |
| D2 | 34 MB | LightWave | Y, 0.01 | **Truncated** inside a skin controller: no visual scene, no `</COLLADA>` |
| D17 | 29 MB | Blender 2.75 | Z, m | `polylist` (triangles only), 5 `lines`; `transparency` 0.055 with no `<transparent>` |
| D7 | 8.9 MB | SketchUp 17 | Z, inch | `triangles`, TEXCOORD without a `set` bound through `bind_vertex_input`; `RGB_ZERO` |
| D22 | 5.6 MB | Blender 2.74 | Z, m | `polylist`, `double_sided` 1 (GOOGLEEARTH, MAX3D) |
| D1 | 2.7 MB | SketchUp 17 | Z, inch | `library_nodes` + `instance_node`, `constant` and `lambert`, `lines` |
| D12 | 2.0 MB | SketchUp 7 | Z, inch | `triangles`, `lambert`, 2 textures |
| D21 | 1.3 MB | Assimp | Y, 0.01 | `polylist` with every input at offset 0, including TANGENT and BINORMAL |
| D10 | 950 kB | FreeCAD (pycollada) | Z, m | 4 geometries |
| D5 | 895 kB | (none) | no `asset` values | **Skinned**: 5 geometries reachable only through `instance_controller`; 127 joint nodes, depth 13 |
| D13–16 | 758 kB | Blender 2.79 | Z, m | 1 geometry, cameras and lights |
| D20 | 104 kB | FBX COLLADA exporter | Y, 0.0254 (named "centimeter") | Two TEXCOORD sets; no materials at all |
| D18 | 878 B | FBX COLLADA exporter | Y, 0.01 | **Empty** visual scene |

¹ No `authoring_tool`, but FreeCAD's `Part__Feature` ids and pycollada's output style.

What this settles:

- **All 1.4.1.** No 1.5 constructs (`instance_image`, `<init_from><ref>`) are needed yet.
- **Primitives:** only `triangles`, `polylist` (largest polygon 32 corners) and `lines`. No
  `polygons`, `ph`, `tristrips` or `trifans`. Those are omitted with a counted warning in the
  first version; `polygons` without holes is cheap to add later on the `polylist` path.
- **Inputs:** VERTEX, NORMAL and TEXCOORD, at separate offsets or all at offset 0. `<vertices>`
  holds POSITION alone or POSITION+NORMAL (SketchUp), or POSITION+NORMAL+TEXCOORD (D5).
  No COLOR anywhere, so vertex colors are left out of the first version. TANGENT and BINORMAL
  (Assimp) and a second TEXCOORD set (FBX) are omitted with a counted warning.
- **Scale:** the largest single text node is 115 MB and the largest accessor 2.6 M records, well
  inside `u32`. The three biggest files hold 1–27 k small geometries. Heap is dominated by the
  per-geometry, per-node and per-material records: 10⁵ of them in D4 and D8.
- **Library order** differs: SketchUp writes `library_visual_scenes` before
  `library_geometries`, everyone else after. That confirms the two passes.
- **Reuse** happens only through `instance_node` (SketchUp components). No geometry is
  instanced twice directly, and no geometry gets two different material bindings.
- **Transforms:** only `<matrix>`. No `translate`, `rotate`, `scale`, `lookat` or `skew`.
- **Units:** 7 samples aren't in metres (inch, cm, mm), so applying the unit matters.
  The `meter` attribute is authoritative: FBX writes `name="centimeter"` with
  `meter="0.001000"` and `meter="0.025400"`.
- **Georeferencing:** D3 has 11-digit positions and large translations in its node matrices;
  D4 has geometry 315 km out. Both go through the fixed-point path and the far origin.
- **Materials:** `profile_COMMON` only: `phong` (FreeCAD, Blender, FBX, Assimp), `lambert`
  (SketchUp, LightWave), `constant` (SketchUp). Every `shininess` is an exponent (0 to 1024);
  no exporter uses a 0–1 scale. Textures are only in `diffuse`.
- **Texture references:** `newparam surface/sampler2D` (Blender, SketchUp), an image id named
  directly in `<texture texture>` (FBX), `bind_vertex_input UVSET0` (SketchUp), and
  `texcoord="CHANNEL0"` with no binding (FBX). URIs: relative with `%20`, `file:///abs/...`,
  and `file://C:/Users/...` (two slashes, a Windows path, unescaped spaces).
- **Missing textures:** D13–16 (its images are in `Textures/`, not beside the file),
  D1 (its texture folder is missing) and D3 (a path on the author's desktop).
  D5's absolute `.tga` paths are found by name beside the file.

## Architecture

A new `src/dae.rs` that spools straight into the existing `v`, `vt`, `vn` and `faces` spools
and returns a `Scene`, like `ply.rs`. Everything after `group::group` is unchanged except the
node writer in `gltf.rs`.

### Index pass

`quick-xml`'s slice reader runs over a read-only mapping of the input. Text events borrow from
the mapping, so a 115 MB `<p>` costs no heap, and `buffer_position()` gives byte ranges. One pass
records:

- Every `<source>` in a `<mesh>`: id, byte range of its `<float_array>`, accessor `count`,
  `stride`, `offset` and which params are used.
- Every `<vertices>`: its inputs.
- Every primitive: kind, `count`, `material` symbol, inputs (semantic, offset, set, source),
  byte ranges of `<vcount>` and `<p>`.
- Materials, effects and images, reduced to a `Material` and an image URI as they stream past.
  Only the first `<texture>` of a channel is kept: D3's 60,942 stacked layers are counted, not
  stored.
- Nodes: parent, composed matrix, `instance_geometry` (url plus `symbol → material` bindings),
  `instance_node`, `instance_controller`. The `<scene>`'s `instance_visual_scene` picks the scene.
- Controllers: a skin's `source` geometry and `bind_shape_matrix`.

A truncated document (D2) or broken XML is an input error, exit 1.

### Decode pass

1. Resolve `#id` references. A dangling one is an error, since it leaves geometry unplaced. A
   cycle of `instance_node` is an error.
2. For each geometry used by the scene, in file order:
   - Each source an input uses is parsed from its byte range into the matching global spool,
     once, and its base index recorded. Positions go through `parse_decimal_fixed`, which
     already accepts the `1.9e-10` forms LightWave writes.
   - For each `triangles` or `polylist` primitive, `<p>` is read, and `<vcount>` in lockstep with a
     second cursor on the same mapping. Corners become `Corner { v, vt, vn }` plus the bases.
     Faces are written with the geometry as the object and the bound material.
   - `lines` and other primitives are counted and omitted.
3. A `<p>` shorter or longer than its `count`, a `vcount` total that disagrees with `<p>`, or an
   index outside its source is an error: geometry would be wrong.

Objects stay contiguous and each primitive has one material, so `group.rs` sees the same shape of
face spool as for OBJ.

### Shared scene layer

This layer is written once and used by COLLADA, FBX (planned in `FBX-DRAFT.md` on the
`fbx-support` branch) and, later, VRML `Transform` and `USE`. It is shaped for FBX from the
start, so that FBX adds an importer and no second scene model.

`Scene` gains a node list and a root basis:

```rust
pub struct Node {
    pub name: String,
    pub parent: Option<u32>,
    /// Local transform, column-major like glTF, composed in f64.
    pub matrix: [f64; 16],
    pub mesh: Option<Instance>,
}
pub struct Instance {
    /// The geometry: an object of the face spool.
    pub object: u32,
    /// Applied to the geometry only, not to the node's children: COLLADA's `bind_shape_matrix`,
    /// FBX's geometric transform. Identity otherwise.
    pub geometry: [f64; 16],
}
pub struct Basis {
    /// Source axes to glTF's: any signed permutation, so FBX's axis systems, left-handed ones
    /// included, fit as well as COLLADA's `Y_UP` and `Z_UP`.
    pub axes: [[f64; 3]; 3],
    /// Metres per source unit: `<unit meter>`, FBX `UnitScaleFactor / 100` (centimetres per unit).
    pub unit: f64,
}
```

- **Root node:** one node carrying `Basis` (rotation or mirror, and the unit scale), with the
  scene's top-level nodes as children. An explicit `--up-axis y|z` replaces the declared axes, with
  a note. A left-handed source gives a root with a negative determinant; glTF then reverses the
  winding, as its spec requires, so faces keep their side. `--center` adds the translation that
  moves the scene-space bounding-box center to 0.
- **Mesh child node:** a node with an instance gets a child holding the mesh, with matrix
  `geometry · T(origin)`. The origin restores fixed-point positions under any matrix
  (`M·G·(p′ + o)`), and the same child carries COLLADA's bind shape and FBX's geometric
  transform. No second mechanism is needed for either.
- **Origin:** positions stay relative to the global fixed-point origin, computed over every
  geometry's local bounding box. The scene-space box for `--center` and the reported bounds is
  each geometry's box through its full matrix chain: a conservative box, not an exact one.
- **Reuse:** several nodes may instance one object, and its glTF mesh is shared. COLLADA's
  `instance_node` and FBX's models sharing a geometry both end up here. A library subtree is
  expanded into new glTF nodes, since glTF nodes have one parent. D4 is the test for node
  count after expansion.
- **Material binding:** faces record the material bound by the first instance of their object.
  A later instance with a different binding keeps it, with one counted warning in every importer.
  No COLLADA sample does this. FBX binds materials per model, so the FBX evaluation counts how
  often it happens. If it matters, the fix is extra glTF meshes sharing the same accessors.
- **Pruning:** nodes whose whole subtree carries no triangles are dropped: cameras, lights,
  SketchUp's line-only geometries.
- **Flat formats:** OBJ, PLY and VRML have no graph (`Scene::graph` is `None`), and `gltf.rs`
  keeps writing one node per object for them, with `--up-axis z` and the origin on each node.
  Routing them through the graph would add a root node and change their output, so they stay on
  the existing path, byte-identical (checked on real OBJ and PLY files and a VRML file).
  Implemented in `src/scene.rs`.
- **No scene-related flags.** The layer adds no option. One binary stays the default. If
  scene-specific options ever become necessary, the split is a binary for simple imports and one
  for scenes, sharing the core, rather than flags on the simple path.

COLLADA specifics on top of the layer:

- Each `<node>` becomes a `Node`, its `<matrix>` elements composed in order.
- `instance_controller`: the skin's source geometry, with `bind_shape_matrix` as the instance's
  `geometry` matrix, which is its bind pose. Joints, weights and animations are omitted with a
  warning. D5 is the test; its placement is checked against Blender.
- `Z_UP` and `Y_UP` are the two `Basis` values COLLADA can declare; `X_UP` is a third.

### Materials

`instance_material symbol → target` → `material` → `instance_effect` → `effect`. `profile_COMMON`
fills the existing `Material`, so the Phong→PBR policy, wiki Materials page and
`extras.sourcePhong` carry over:

**The reference.** Blender 5.2, the only Blender on the machine, no longer has a COLLADA
importer (Blender removed it in 5.0). The reference is therefore Blender 4.5's importer read
from its source (`source/blender/io/collada/Materials.cpp`, `MaterialNode`), with three.js
0.186's ColladaLoader (`ColladaComposer.js`) as the second opinion. Renders against Blender 4.5
are still to do, once a 4.5 build is installed.

| COLLADA | `Material` | Following |
| --- | --- | --- |
| `diffuse` color / first `<texture>` | `kd` / `texture` with `kd = 1`: the texture replaces the colour. Neither: black | Blender |
| `specular` | Black or absent: specular off (`illum 1`). Otherwise the default dielectric specular, not its value | Blender |
| `shininess` | `ns`, a Phong exponent, mapped to roughness as for MTL. Absent: roughness 0.5 | three.js, which uses it; Blender ignores it pending a rework, with roughness 0.5 |
| `emission` | `ke` | Both |
| `transparent` + `transparency` | Alpha = `transparent.a × transparency`, each 1 when absent | Blender (three.js agrees) |
| `RGB_ZERO` | Alpha = 1 − luminance(`transparent.rgb`) × `transparency` | The 1.4.1 spec and three.js (for grey): Blender's `1 − a × t` makes SketchUp's grey glass invisible |
| `reflectivity` | Ignored | three.js: Blender makes it metalness, which would make every D3 material metal; mesh2gltf infers no metal |
| `<constant>` | Unlit (`illum 0`) | three.js (MeshBasicMaterial) and the spec |
| `<lambert>` | No specular (`illum 1`) | Both |
| `double_sided` in `<extra>` (GOOGLEEARTH, MAX3D) | `double_sided` | three.js; Blender has no backface culling to set |
| Colour space | Linear, as for MTL | Blender; three.js reads them as sRGB |
| `ambient`, `reflective`, `bump`, `index_of_refraction`, textures on other channels, stacked diffuse textures, other profiles | Omitted, counted | |

Consequences on the samples: D17's stones, `transparency 0.055` with no `<transparent>`, are
nearly invisible in both Blender and three.js, and so here. FreeCAD's `specular 1 1 1` with
`shininess 0` gives a matte part with ordinary specular.

`material.rs` needed no change: the fields COLLADA sets are already public. FBX can use them
the same way.

Texture lookup: `<texture texture="X">` goes through `newparam sampler2D` → `surface` →
`init_from`, and falls back to an `<image>` with id `X` (FBX). The TEXCOORD set is the lowest:
every sample binds set 0, or has one set. `<init_from>`: `file:` and its slashes go, escapes
are decoded, then `References::resolve`, whose lookup by name beside the model covers
`file://C:/Users/...` and D5's absolute paths. An image id that names nothing omits the
texture with a counted warning; a file that can't be reached fails the conversion.

### Out of scope for the first version

`.zae` and `.kmz` archives, gzip-compressed `.dae`, external document references
(`other.dae#id`), COLLADA 1.5, `polygons`/`tristrips`/`trifans`, vertex colors, a second UV set,
skinning and animation, physics and kinematics.

## Code changes

| File | Change | Size |
| --- | --- | --- |
| `src/dae.rs` (new) | Index pass, reference resolution, decode pass, effects | ~800–1000 lines |
| `src/main.rs`, `src/cli.rs` | `.dae` dispatch, help, `up_axis` default | small |
| `src/gltf.rs` | Shared scene layer: node list, root `Basis`, mesh child nodes with `geometry · T(origin)`, shared meshes, pruning | ~150 lines |
| `src/obj.rs` | `Scene` gains `nodes` and `basis`; OBJ, PLY and VRML fill the flat ones | small |
| `src/material.rs` | Phong-style constructor shared by COLLADA and FBX | ~30 lines |
| `src/fixed.rs` | Bounding box through a matrix chain, for `--center` and reported bounds | small |
| `Cargo.toml` | `quick-xml` | 1 line |
| `tests/import.rs` | One small inline `.dae` per mapping above | |
| `tests/hostile.rs` | Truncation, dangling and cyclic references, `<p>` overruns, entity expansion, huge attributes | |
| README | Formats, input list, exit codes, `--up-axis` default | |
| Wiki | Geometry and format support → COLLADA; Materials → COLLADA materials; Canonical glTF profile → scene, root node, origin; Georeferenced coordinates → units and up axis; Disk-backed conversion → the two passes and the input map | |

## Validation

Every sample gets an expected outcome before the run:

| Expected | Samples |
| --- | --- |
| Rejected, exit 1 | D2 (truncated), D18 (no geometry) |
| Converts | the 13 other distinct files |
| Fails on a missing texture (rule kept) | D13–16, D1, D3 |

For each conversion:

1. The Khronos validator (`tools/validate_gltf.cjs`) reports no errors.
2. Triangle count, scene-space bounds in metres, and node, mesh, material and texture counts
   match a reference import in Blender. three.js's ColladaLoader is the second opinion.
3. Rendered comparison for textures, transparency and winding: SketchUp files, D17, D22,
   D5, and one FreeCAD file.
4. `tools/monitor_import.py` on D11, D8 and D6: flat heap, and time and
   workspace use in line with an OBJ of the same size.

## Duplicate materials (conditional)

FreeCAD writes one material per part: 27,023 for D8, most of them likely identical.
Merging materials that are equal in value is done **only if it is cheap**:

- under 100 lines,
- shared by every importer (after spooling, on `Scene::materials`, remapping the face spool's
  material index during grouping rather than with an extra pass),
- little to no time or memory cost: one hash of each material's effective values, which are
  small per-material records already in memory.

The material name then becomes the first source name, and the merge is reported in one line. If
it can't meet those limits, materials stay one-to-one with the source, and that is recorded here.
Measure on D8 first: if the 27,023 materials collapse to a handful, it is worth doing.

## Implementation order

Each step ends with `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and
`cargo test --locked` passing, and one commit.

1. The shared scene layer's types (`Node`, `Instance`, `Basis`) in `Scene` and the node writer in
   `gltf.rs`, with OBJ, PLY and VRML giving the flat list. Output must be
   byte-identical for existing tests.
2. `dae.rs` index pass and decode pass for `triangles` and `polylist`, default material, identity
   nodes: the FreeCAD files convert.
3. Node matrices, root `Basis` (general axes, unit), mesh child nodes, `--center`. Test the
   basis with a mirrored axis system too, although COLLADA can't declare one: FBX will.
4. `instance_node` expansion and pruning: the SketchUp files convert.
5. Effects, images and texture binding.
6. `instance_controller` in bind pose: D5.
7. Material deduplication, if it meets the limits above.
8. Run every sample against the Validation table; README and wiki updates.
