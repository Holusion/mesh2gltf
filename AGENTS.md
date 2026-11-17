# Goals

`mesh2gltf` is a minimal Rust CLI that imports very large OBJ and VRML meshes, often georeferenced and larger than the machine's RAM plus swap, into simple glTF 2.0: JSON with an external `.bin`, with the textures kept, copied or converted. It has a single command, `mesh2gltf [OPTIONS] <file>`. Keeping the code small to maintain is a goal of its own; resource use is already minimal.

The README contains the command reference. The GitHub wiki has two sections: Conversion (format support, georeferencing, images and material approximations) and How it works (streaming, heap/mmap/disk use). Its sources are maintained in the separate `git@github.com:Holusion/mesh2gltf.wiki.git` repository, not in this repository. Keep both in sync with user-visible changes at the appropriate level of detail.

# Constraints

- **Stream, never hold the model.** Input is read line by line or chunk by chunk. Geometry lives in disk spools that are written sequentially and then memory-mapped. Heap use is bounded by one face plus small per-object and per-material records. Any whole-model structure in memory is a bug.
- **Workspaces go on a real disk.** Spools default to the output's directory, not the OS temporary directory, which is often RAM-backed (tmpfs).
- **Output is faithful.** Don't chunk, cap or duplicate geometry to save memory; use disk-backed structures instead. The only primitive split is at the 32-bit index limit. Coordinates go through the fixed-point path, so `--center` loses no digits.
- **Real exports are messy.** Prefer omitting unsupported data with one counted warning over aborting a long import. Reserve errors for input that can't be interpreted, or that would silently give wrong geometry.
- **Interop choices are credited.** When behaviour follows another implementation (three.js, Blender, earcut, obj2gltf, a spec), name it in the code comment or relevant wiki page. That way the choice can be traced, and later swapped.

# Code layout

All modules sit directly in `src/`:

- `main.rs` declares the modules, holds `Res`, `Options` and `run`, and maps failures to exit codes. `cli.rs` parses arguments and prints help.
- `vrml.rs` turns VRML into an intermediate OBJ and MTL in the workspace. `obj.rs` spools OBJ geometry to disk, and `material.rs` reads MTL.
- `triangulate.rs`, `dedup.rs`, `smooth.rs` and `group.rs` triangulate faces, deduplicate corners, generate normals and group faces by material. `fixed.rs` holds the fixed-point coordinates.
- `gltf.rs` writes the glTF and its `.bin`, and `images.rs` places, checks and converts textures.
- `disk.rs` (workspaces, file mappings and published outputs), `references.rs` (confinement), `failure.rs` (exit-code classes), `diagnostics.rs` (safe stderr) and `format.rs` support the rest.

Input is OBJ and VRML only. Rewriting an existing glTF, and measuring one, are out of scope.

# Conventions

- Comments and docs describe current behaviour and the reasons for it. They don't describe development history or removed features.
- Every behaviour change comes with tests and a documentation update. Update the README for command options, the Conversion wiki section for conversion behaviour, and How it works for implementation details.
- Before committing, run:

  ```sh
  cargo fmt --check
  cargo clippy --all-targets -- -D warnings
  cargo test --locked
  ```

  CI builds with `RUSTFLAGS='-D warnings'` on Linux, macOS and Windows.
- For output changes, run Khronos's glTF validator (`tools/validate_gltf.cjs`; the Development wiki page has the setup). Use `tools/monitor_import.py` for memory measurements on real imports.
