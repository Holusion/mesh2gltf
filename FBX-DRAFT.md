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
