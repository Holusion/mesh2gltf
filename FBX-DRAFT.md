# FBX (`.fbx`) import: how to decide the implementation

Working note for the `fbx-support` branch, which starts from the COLLADA plan because it shares
its scene layer (`COLLADA-DRAFT.md`, "Shared scene layer"). Delete it before merging.

This draft doesn't plan the importer yet. It plans the evaluation that chooses how to build it.

## Constraints the choice must respect

- **Stream, never hold the model.** Heap bounded by one face plus per-object records. FBX inputs
  are large: of the 766 `.fbx` files on the shared drive, 92 are 100 MB–1 GB and 12 are over
  1 GB, the largest 3.1 GB. Compressed arrays make the decoded geometry larger still.
- **One binary, no feature flags.** Whatever FBX needs is built for every user on Linux, macOS and
  Windows, with `RUSTFLAGS='-D warnings'`.
- **Small code** is a goal of its own, and the default path stays simple: no FBX-specific options.
- **Blender's importer is the reference** for placement and appearance, as for COLLADA.
- The scene layer (nodes, root `Basis`, per-instance geometry matrix, shared meshes) already
  exists for COLLADA. FBX adds an importer, not a second scene model.

## Candidates

| | Approach | Status |
| --- | --- | --- |
| A | **Our own `fbx.rs`**: binary and ASCII record reader, index pass, streaming geometry, FBX semantics | Candidate |
| B | **Mixed**: `ufbx` loads the scene with geometry skipped (transforms, axes, units, materials, textures, connections); our own reader streams the geometry arrays from the file | Candidate, if it passes the checks below |
| C | **`ufbx` full load** | Out: it holds every vertex and index array in RAM. Measured only to say how far it is from the rule |
| D | **`fbx2gltf`** (Autodesk FBX SDK) | Out: whole scene in RAM, float32 positions with no origin, a proprietary SDK, and its own glTF. Usable only as another reference |

What decides between A and B is how much FBX meaning the real files use. If they only use plain
translation, rotation and scale with standard axes, A is small. If pivots, pre- and
post-rotations, rotation orders and geometric transforms are common, `ufbx`'s tested handling of
them is worth a C dependency.

## Evaluation

All of it runs outside the repository's code: scripts and builds in a scratch folder, outputs on
a real disk (not `/tmp`, which is a small tmpfs), sample files read-only.

### 1. Corpus survey: all 766 files

The list is the output of `find SHARED -iname '*.fbx' -type f -printf '%s\t%p\n'`.
A throwaway reader walks each file:

- **Binary:** each record stores its end offset, so arrays are skipped without being read. Only
  array headers (type, length, encoding, compressed size) are recorded, which makes the walk fast
  even on 3 GB.
- **ASCII:** scanned as text, with arrays counted, not stored.

Recorded per file:

| Group | Fields |
| --- | --- |
| File | Binary or ASCII, FBX version (6.x has a different layout from 7.x), size, creator application and version |
| Global settings | `UpAxis`, `UpAxisSign`, `FrontAxis`, `FrontAxisSign`, `CoordAxis`, `CoordAxisSign` (handedness), `UnitScaleFactor`, `OriginalUnitScaleFactor` |
| Objects | Counts of `Geometry` by type (`Mesh`, `Shape`, `NurbsCurve`, `Line`, other), `Model` by type, `Material` by `ShadingModel`, `Texture`, `Video` (embedded content size), `Deformer` (`Skin`, `Cluster`, `BlendShape`), `AnimationCurve` |
| Geometry | `Vertices` type (`d` or `f`), largest array (elements, raw and compressed bytes), share of compressed arrays, total decoded size, `PolygonVertexIndex` length, largest polygon, edges-only geometries |
| Layers | `MappingInformationType` × `ReferenceInformationType` for normals, UVs, colors and materials; number of UV sets; color layers; per-polygon material use |
| Transforms | Files using `PreRotation`, `PostRotation`, `RotationPivot`, `ScalingPivot`, `RotationOffset`, `ScalingOffset`, a non-default `RotationOrder`, a non-default `InheritType`, and `GeometricTranslation`/`Rotation`/`Scaling` |
| Reuse | Models sharing a geometry, and how many of those bind different materials |
| Coordinates | Sampled position magnitudes and node translations (georeferencing) |
| Textures | Relative or absolute file names, embedded, missing |

Result: a feature-usage table over the corpus (files using each feature, by exporter), and the
size distribution of the decoded geometry.

### 2. Representative set: about 20 files

Picked from the survey: the largest file per exporter; at least one file per transform feature
in use; ASCII and FBX 6 if present; embedded textures; a left-handed or non-Y-up axis system;
models sharing a geometry with different materials; skinned meshes. Picks and reasons go in the
results.

### 3. `ufbx` measurement

- Fetch `ufbx` at a pinned release tag into its own folder, and build a small C harness against
  it. Read `ufbx.h` for the real option names rather than assuming them: skipping geometry,
  keeping the raw node tree, memory limits.
- On the representative set, plus the five largest files:
  - **Full load (C):** peak RSS (`/usr/bin/time -v`) and time, compared with file size and decoded
    geometry size.
  - **Geometry skipped (B):** peak RSS and time. B passes only if peak RSS tracks the object count,
    not the geometry size, for example staying in the low hundreds of MB on the 3 GB file.
  - **Identity mapping (B):** whether each `ufbx` mesh can be matched to its FBX object id in the
    file, which our geometry reader needs. Cross-check against the ids the survey reader finds.
- **Placement:** per-mesh world bounding boxes from `ufbx`, from Blender's FBX importer run
  headless, and from a direct transform composition. Disagreements are listed by feature.
- **Rust build:** the `ufbx` Rust bindings (version, maintenance, how the C is compiled), a clean
  build with `-D warnings` and clippy on Linux, added binary size and build time. macOS and
  Windows can't be built here, so known reports for them are noted instead.

### 4. Estimating our own importer

- The decoding cost: streaming zlib decompression throughput on the largest compressed arrays,
  and lockstep reading of `PolygonVertexIndex` with UV, normal and material layers.
- Lines per feature, for every feature used by more than a handful of files. The record reader,
  index pass, layers and materials are needed in both A and B; only the transform stack, axes,
  units and object graph differ.
- The transform stack, if the survey shows it in use: Autodesk's documented order
  (`T · Roff · Rp · Rpre · R · Rpost⁻¹ · Rp⁻¹ · Soff · Sp · S · Sp⁻¹`, rotation order, inherit
  type), checked against Blender on the files that use it.

### 5. Decision rule

| Outcome | Choice |
| --- | --- |
| The pivot and pre/post-rotation transforms and the unusual axes are rare in the corpus, and A's estimate is around 1,000 lines or less | **A** |
| B fails the memory or identity check | **A** |
| B passes, the corpus uses the transform stack widely, and B is at least ~40% less of our own code than A | **B** |
| In between | **A**: no C dependency, one language, and the project's preference for less to maintain |

These thresholds are proposals for you to adjust before the decision is taken.

## Deliverable

A "Results" section in this file: the feature-usage table, the representative set, the `ufbx`
measurements, placement comparisons, both line estimates, and a recommendation with its reasons.
Committed on `fbx-support`. No importer code until you've chosen.

## Expected scope of the importer, either way

Static meshes with their transforms, reuse, axes and units; Lambert and Phong materials through
the shared Phong constructor; diffuse textures, external or embedded (an embedded one is written
to the workspace and then goes through `images.rs` as usual); skinned meshes in bind pose. Omitted
with a counted warning: animation, blend shapes, NURBS and curves, cameras, lights, extra UV sets.
Per-corner vertex colors depend on the survey: COLLADA skips them, so FBX would bring them to
the shared layer if they are common.

## Results

Night run of 2026-11-19, in the `fbx-support` worktree. Scripts, the `ufbx` checkout, builds and
data are in its gitignored `out/`: `out/fbxsurvey` (survey reader and direct placement, Rust),
`out/harness` (C harness), `out/scripts`, `out/data`.

**Recommendation: A, our own `fbx.rs`.** B fails the memory check outright, and the identity
check on FBX 6; and even where it would work, it saves about a third of our own code, under the
40 % the rule asks for. Details below.

### 1. Corpus survey

766 paths; 762 FBX files read (58.7 GB). The other 4 are macOS `._` resource forks named `.fbx`.
740 binary (FBX 6.1 to 7.7, mostly 7.4), 22 ASCII, 19 FBX 6. 11 files first failed in the survey
reader on byte arrays (`c`), which an FBX SDK 2016 photogrammetry export writes; read after a fix.

| Exporter | Files | GB | Largest |
| --- | --- | --- | --- |
| 3ds Max | 202 | 21.1 | 2.36 GB |
| Blender | 188 | 3.9 | 427 MB |
| FBX SDK (no application named) | 179 | 20.0 | 3.14 GB (ASCII) |
| Maya | 104 | 1.8 | 90 MB |
| Revit | 50 | 8.4 | 2.66 GB |
| Scaniverse, Unreal, Cinema 4D, Houdini, Assimp, Modo | 39 | 3.5 | 662 MB |

Feature usage, in files (of 762), with the main exporters:

| Feature | Files | Mainly |
| --- | --- | --- |
| `PreRotation` | 247 | 3ds Max 192 |
| Non-uniform scale | 140 | Blender, 3ds Max |
| Negative scale | 90 | Revit, 3ds Max |
| `GeometricTranslation` / `Rotation` / `Scaling` | 85 / 31 / 12 | 3ds Max |
| `RotationPivot`, `ScalingPivot` | 68 | Maya 63 |
| `PostRotation` | 22 | 3ds Max |
| `RotationOrder` other than XYZ | 4 | |
| `InheritType` other than RSrs (0 or 2) | 7 (858 models) | Blender, Maya |
| Not Y up | 108 | Revit 50 (Z up, front −Y) |
| Unusual axis systems | 13 | 9 photogrammetry files Z up, X front; 2 Y up, X front, −Z right; 1 Blender Z up, −X front, −Y right; 1 Z up, +Y front, −X right |
| Unit not centimetres | 231 | m 61, ft 55, in 43, mm 37 |
| Per-polygon materials (`ByPolygon`) | 374 | |
| Normals `ByPolygonVertex` / `ByVertice` / `ByPolygon` | 644 / 89 / 50 | |
| More than one UV set | 104 | |
| Vertex colours | 48 | `ByPolygonVertex IndexToDirect` 24, `ByVertice` 21, `AllSame` 3 |
| Polygons over 8 corners | 183 | Largest 4,836 corners; 3 files over mesh2gltf's 4,096 |
| Geometry shared by several models | 63 (26,612 geometries) | Revit 49 |
| …of which with other materials | 1 (282 instances) | Cinema 4D |
| Embedded textures | 102 | |
| Textures missing beside the file | 211 | |
| Skins / blend shapes / animation | 37 / 12 / 92 | |
| Coordinates over 10⁵ | 32 | 3ds Max |
| ASCII / FBX 6 | 22 / 19 | |

Across all models: `InheritType` 1 on 346,026, geometric transforms on 200,745, `PreRotation` on
58,134, pivots on 511, `PostRotation` on 29.

Decoded geometry per file: median 3 MB, p90 94 MB, p99 745 MB, largest 2.18 GB. The largest
single array is 24 MB: big files are many objects, not one huge mesh. 45 % of arrays are
compressed.

### 2. Representative set

31 files, in `out/data/representative.tsv`: the largest per exporter (11), the five largest
overall, one per transform feature in use (post-rotation, rotation order, pivots, geometric
rotation and scaling, negative scale), ASCII 7 and 6, binary FBX 6, embedded textures, three
unusual axis systems, shared geometry with other materials, a skinned mesh, polygons over 4,096
corners, and Revit's Z-up feet. Missed: a file with `InheritType` 0 or 2, which the picking script
looked up under the wrong key.

### 3. `ufbx` (v0.23.1, commit `26a482ae`)

Peak RSS by `getrusage` and 0.1 s sampling, one run at a time. None reached the 75 % cap.

| File | Size | Meshes | Full load | Geometry skipped | Skipped + `retain_dom` |
| --- | --- | --- | --- | --- | --- |
| FBX SDK F1 (ASCII) | 3.14 GB | 43,130 | 2.1 GB, 36 s | 435 MB | 798 MB |
| Revit F2 | 2.66 GB | 174,050 | 6.7 GB, 29 s | 1.8 GB | **4.9 GB** |
| 3ds Max F3 | 2.36 GB | 5,806 | 7.7 GB, 21 s | 44 MB | 130 MB |
| 3ds Max F4 | 2.27 GB | 3,403 | 8.7 GB, 21 s | 48 MB | 128 MB |
| FBX SDK F5 | 2.14 GB | 144,763 | 8.2 GB, 22 s | 3.8 GB | **8.3 GB** |
| 3ds Max F6 | 1.96 GB | 6,423 | 5.3 GB | 42 MB | 125 MB |
| F7 (negative scale) | 229 MB | 17,371 | 458 MB | 235 MB | 535 MB |
| Others (24 files) | ≤ 662 MB | ≤ 12,090 | ≤ 1.1 GB | ≤ 167 MB | ≤ 455 MB |

- **Full load (C):** 2 to 4 times the file, up to 8.7 GB. Out, as planned.
- **Memory check for B: fails.** With geometry skipped, memory follows the object count, at 10–30 KB
  per mesh, and `retain_dom`, which B needs for the ids, doubles it. Two files reach 4.9 and
  8.3 GB, as much as a full load. "Low hundreds of MB" holds only for files with few objects.
- **Identity check: 100 % on FBX 7, 0 % on FBX 6.** Every mesh's raw node carries its FBX id on
  all 28 FBX 7 files; FBX 6 (binary and ASCII) identifies objects by name, so B would need a
  second matching scheme there.
- **Rust bindings:** `ufbx` 0.11.5 (MIT or Unlicense) bundles exactly ufbx 0.23.1 and compiles it
  with `cc`. A probe loading with `ignore_geometry` and `retain_dom` builds clean with
  `RUSTFLAGS='-D warnings'` and clippy `-D warnings` on Linux. It adds about 20 s of build time
  (one 33,000-line C file) and about 475 KB to an LTO release binary. macOS and Windows weren't
  built.

### Placement

Scene bounds in glTF axes and metres, as the largest per-axis difference relative to the scene's
size. "Direct" is the survey tool's composition: Autodesk's order, rotation order, pre- and
post-rotation, pivots and offsets, geometric transform, property templates, axes and unit, in
208 lines (`out/fbxsurvey/src/place.rs`).

- **Direct composition against `ufbx`:** equal within 6×10⁻⁵ on all 18 FBX 7 files it ran on,
  including post-rotation, rotation order, pivots, geometric rotation and scaling, negative scale,
  Revit's Z-up feet and the three unusual axis systems. Not run on the 9 files over 500 MB, or on
  ASCII and FBX 6, which the prototype doesn't read.
- **Blender 5.2 against `ufbx`:** equal on 5 files (3ds Max, Assimp, Blender, Maya, Modo), with
  triangle counts within 0.03 %. Different on Cinema 4D's largest: 7.79 M triangles against 6.28 M,
  and bounds 5.7 times the size apart; not investigated. Blender refuses ASCII FBX, so it can't
  check the largest file. Blender's peak memory was 15 GB on the 2.36 GB 3ds Max file.
- **Stopped:** Claude Code stopped the Blender runs, while Revit's 2.66 GB file was importing,
  because the machine ran low on memory. The other 22 Blender comparisons weren't made, and the
  run wasn't restarted, as Claude Code asked.

### 4. Our own importer

- **Decoding cost:** streaming zlib (flate2, miniz_oxide) decoded 15.0 GB of arrays in 122 s on one
  thread: 123 MB/s. The corpus's largest file has 2.18 GB of arrays, about 18 s. `PolygonVertexIndex`
  read in lockstep with its layers is the same pattern as COLLADA's `<vcount>` and `<p>`: no
  measured obstacle.
- **The transform stack is small**, at 208 lines in the prototype, and matches `ufbx` everywhere it
  was compared. Its risk is in exporter quirks, not in the formula: `InheritType` 0 and 2, which
  the prototype treats as 1, were not exercised.

Lines of our own code, with the prototypes as measures (record reader 80 lines, ASCII tree 105,
placement 208):

| Part | A | B |
| --- | --- | --- |
| Binary record reader, array headers, 64-bit offsets | 120 | 120 |
| ASCII reader, streaming (the largest file is ASCII) | 250 | 250 |
| Index pass: objects, templates, properties, connections | 200 | — (ufbx) |
| Transform stack, axes, unit | 210 | — (ufbx) |
| Geometry: polygons, layers in lockstep, per-polygon materials, zlib | 300 | 300 |
| Materials, textures, embedded content | 150 | — (ufbx) |
| Skins in bind pose, blend shape base | 50 | — (ufbx) |
| FBX 6: objects by name, geometry in models, `Properties60` | 120 | 60 (name matching) |
| Glue from ufbx's scene to the scene layer, id matching | — | 250 |
| Warnings and errors | 60 | 40 |
| **Total** | **≈ 1,460** | **≈ 1,020**, plus a C dependency of 33,000 lines |

B is about 30 % less of our own code.

### 5. Decision rule

| Row | Outcome | Holds? |
| --- | --- | --- |
| A if the pivot and pre/post-rotation transforms and unusual axes are rare, and A is ~1,000 lines | Not rare: `PreRotation` in 32 % of files, pivots in 9 %, not Y up in 14 %, 13 unusual axis systems; A is ~1,460 | No |
| **A if B fails the memory or identity check** | Memory: 4.9 and 8.3 GB with ids. Identity: 0 % on FBX 6 | **Yes** |
| B if it passes, the stack is widely used, and B saves ≥ ~40 % | B fails; and saves ~30 % | No |
| A in between | | |

**A.**

Where the thresholds might move:

- The memory check should come first and be absolute, as the project's constraint is: it decided
  this on its own. "Low hundreds of MB" could be stated per object, for example under 2 KB per
  object, which mesh2gltf's COLLADA first pass (4–5 KB per part) doesn't meet either.
- Row 1's "~1,000 lines" undercounts FBX: an ASCII reader that streams is needed whatever the
  choice, since the largest file is ASCII. About 1,500 is the honest figure for A.
- "Transform stack widely used" was meant as a proxy for risk. The prototype shows the formula is
  small and right; the risk is in exporters' quirks, which the corpus can test.

### What the survey adds to the importer's scope

- **ASCII must stream:** the largest file (3.1 GB) is ASCII, and Blender can't read it.
- **Polygons over 4,096 corners:** 3 Maya files. mesh2gltf's face limit rejects them today.
- **Missing textures:** 211 files would fail under the kept rule. Worth reconsidering for FBX,
  where textures often stay on the author's machine and 102 files embed theirs anyway.
- **Embedded textures:** 102 files. `images.rs` needs to take a texture written to the workspace.
- **Per-corner vertex colours:** 24 files, which the per-position `vc` spool can't hold.
- **Per-polygon materials** (374 files) fit the face spool as they are.
- **Shared geometry with other materials:** one file. The scene layer's first-binding rule, with
  its warning, is enough.
- **Heap per object:** files with 145,000–174,000 objects will test the per-object records and
  the glTF JSON writer, which already takes 417 MB for COLLADA's 27,000 parts.
