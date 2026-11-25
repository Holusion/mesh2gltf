# mesh2gltf

A Rust command-line tool for converting very large OBJ, PLY and VRML meshes to glTF 2.0. It handles georeferenced coordinates and models [larger than available RAM](https://github.com/Holusion/mesh2gltf/wiki/Resource-usage-comparison), using a disk workspace.

## Install
Download a binary from latest [release](https://github.com/Holusion/mesh2gltf/releases) or install from a repository checkout with [Rust and Cargo](https://www.rust-lang.org/tools/install):

```sh
cargo install --path . --locked
```

## Convert a model

```sh
mesh2gltf model.obj
mesh2gltf scan.ply
mesh2gltf terrain.wrl --up-axis z -o terrain.gltf
mesh2gltf building.obj --center
mesh2gltf model.obj -o model.glb
```

A conversion writes a `.gltf` file, a sibling `.bin` geometry buffer and any copied or converted textures. An output ending in `.glb` is instead a single file holding the geometry and every texture, up to the 4 GiB a GLB can hold. By default, the output uses the input's name with a `.gltf` extension. The destination directory must already exist; existing output files are never overwritten. A failed conversion removes only output files it created.

```text
mesh2gltf [OPTIONS] <file.obj|file.ply|file.wrl|file.vrml>
mesh2gltf --inspect [--root DIR] [--trusted] [--exact-paths] <file.obj|file.mtl|file.ply>

-o, --output FILE      Destination .gltf, or .glb (default: input with .gltf extension)
    --center           Move the bounding-box center to 0, dropping the model's position
    --up-axis AXIS     Source up axis: y (default) or z
    --tmp-dir DIR      Workspace directory (default: the output's directory)
    --keep-tmp         Preserve the workspace for debugging
    --images MODE      auto (default), keep or copy; a .glb embeds them
    --no-color         Ignore vertex colors, keeping the material color on untextured faces
    --trusted          Let the model refer to files outside its folder
    --exact-paths      Follow references only to their written path, never by name
    --inspect          Print, as JSON, the files an .obj, .mtl or .ply refers to; convert nothing
    --root DIR         With --inspect: the folder references must stay in (default: the input's)
    --progress         Report reading and conversion progress on stderr
    --log-level LEVEL  Print stderr messages of this level and above: trace, debug, info
                       (default), warn or error
    --log-format FMT   Write stderr messages as text (default), or as JSON lines
-h, --help             Show help
```

Without arguments, mesh2gltf prints this help.

Vertex colors (OBJ `v x y z r g b`, PLY `red green blue`, VRML `Color` nodes) are only written on faces without a texture, where they replace the material's diffuse color (`Kd` or `diffuseColor`). A texture replaces them, so they are dropped on textured faces. `--no-color` drops them everywhere, which keeps the material's own color.

JPG and PNG textures are used as they are. Other formats, such as BMP, TIFF, TGA or EXR, are converted to PNG, which is lossless and keeps alpha. Textures stay in place when input and output share a folder and no conversion is needed; otherwise they are copied. A `.glb` embeds them, so `--images keep` is a command-line error there. Source images are never modified.

## Models from other people

By default, mesh2gltf only follows a file reference in a model (an OBJ `mtllib`, an MTL `map_Kd`, a PLY `comment TextureFile`, a VRML `url`) to a regular file inside the input's folder. A model can't read anything else on the machine, and a reference that fails gets the same message whether the file is missing or forbidden. A texture that can't be reached fails the conversion, as a missing one does. The output doesn't refer outside its own folder either, so `--images keep` is a command-line error (exit 2) if it would.

A reference may use `..` as long as it stays inside the folder. URLs and symlinks leading out are refused. An absolute path, which old exporters often write where a relative one belongs, is only used to look for a file of that name beside the file containing the reference. `--trusted` turns this off for a model you know: references may then point anywhere, including through symlinks, the lookup by name is only used when the absolute path doesn't exist, and `--images keep` may refer to any folder.

A relative path that leads nowhere, often because an upload or archive lost the folders it names, is looked for by name beside the file containing the reference too, as obj2gltf does. `textures/wall.png` in an MTL finds `wall.png` next to that MTL when there's no `textures` folder. The lookup only checks that one folder for that one name, and the written path always wins when it exists. Files found by name are listed in one counted warning. Two references that mean different paths, such as `a/wall.png` and `b/wall.png`, or `old/wall.png` and `wall.png`, can't both lead to one file this way: which file each meant would be a guess, so the conversion fails. `--exact-paths` turns off every lookup by name, absolute and relative: a reference must then lead to a file exactly where written.

### Checking an upload

`mesh2gltf --inspect FILE` reports the files an `.obj`, `.mtl` or `.ply` refers to, without converting anything or looking for those files, so each file of an upload can be inspected alone, in any order, as it arrives. It prints one JSON object on stdout:

```sh
$ mesh2gltf --inspect model.mtl
{"requires":[["textures/diffuse.jpg","diffuse.jpg"],["wall.png"]]}
```

Each entry of `requires` is one file, given as the paths the conversion will look for it at, in order: relative to the inspected file's folder, with forward slashes, and following the rules above, so `--trusted` and `--exact-paths` change it as they change the conversion. The file is found at the first of these paths that exists. Once every entry has at least one of its paths among the uploaded files, the model's references are complete. Here `C:\Users\bob\wall.png` in the MTL is met by `wall.png` beside it.

A conversion keeps references inside the input's folder, so a library in a folder below it may reach back up with `..`. Inspected alone, that library would take its own folder as the limit. `--root DIR` gives the model's folder instead, which must hold the inspected file: with `scan.obj` naming `mtl/m.mtl`, which names `../tex/d.jpg`, `mesh2gltf --inspect mtl/m.mtl --root .` requires `[["../tex/d.jpg","d.jpg"]]`. Paths stay relative to the inspected file's folder. `--root` doesn't apply to a conversion, nor with `--trusted`.

An OBJ requires the libraries its first `mtllib` names. Exporters write it before any geometry, so reading stops there, and only an OBJ without one is read to the end. An MTL requires the texture of every material it defines, used or not, since the conversion needs them all. A PLY requires its `comment TextureFile`s when it has texture coordinates, and only its header is read. VRML can't be inspected.

A reference the conversion would refuse, such as a URL, or a file the conversion would reject before following its references, such as a PLY point cloud or an unsupported `map_Kd` option, fails the inspection with the conversion's message and exit code. Two references meeting on one file found by name are only detected by the conversion, which can see which files exist.

Messages on stderr can be shown to whoever supplied the model. A file is shown by its name, never its folder, and a reference by what the model wrote. Names from a file are cut to a short length, and characters that could drive a terminal, read as markup or hide text are replaced by `?`. A problem a file repeats gives one warning, with its count and up to three examples, once the file has been read.

## Logs

Data goes to stdout (only `--inspect` writes any), and every message to stderr: what the conversion did, its progress with `--progress`, counted warnings, and the error that ends a failed run. `--log-level` and `--log-format` take meshtinker's values, so that a caller running both can pass the same ones and read both the same way:

- `--log-level <trace|debug|info|warn|error>` prints messages of this level and above; the default is `info`. These are eCorpus's task log levels. mesh2gltf writes only `info`, `warn` and `error` messages, so `trace` and `debug` show what `info` does. `warn` keeps warnings and the error, and `error` only the error.
- `--log-format json` writes each message as one JSON object per line:

```json
{"level":"info","message":"Wrote model.gltf (1 triangles, 1 meshes, 80 bytes of geometry)"}
{"level":"warn","message":"unsupported OBJ records omitted: 1, for example curv"}
{"level":"error","message":"Output .gltf, .glb or .bin already exists; choose a new output path","code":"environment"}
```

With `--progress`, a long stage reports how far it is about every 100 MB read or million faces, as an `info` message with two more fields, so that a caller can show a bar per stage rather than print each line:

```json
{"level":"info","message":"Converted 2000000 faces, 159999984 binary bytes (40%)","progress":0.4,"stage":"write"}
```

`progress` is the fraction of the stage done, from 0 to 1, and `stage` says which one: `read` (the input), `spool` (the OBJ mesh2gltf writes from a VRML input), `group` (sorting faces by object and material), `smooth` (generating normals) and `write` (the geometry). Stages come in that order, each when the model needs it, and a bar goes back to 0 with each new one. In text, the percentage ends the message.

`level` and `message` are always there; the message is the text line without its `Warning:` or `Error:`, cleaned in the same way. `code` is only on the error that ends a run: `rejected`, `usage`, `resource` or `environment`, as its exit code below, or `internal` for a panic. Other fields may come later, so parsers should ignore those they don't know. Both options are read before the rest of the command line, so a wrong command line is reported in the format asked for.

## Exit codes

| Code | Meaning |
| ---- | ------- |
| 0 | Success |
| 1 | The input was rejected, including an input that isn't `.obj`, `.ply`, `.wrl` or `.vrml`, a PLY without faces (point clouds aren't supported), and a model too large for a `.glb` |
| 2 | The command line is wrong, including `--inspect` with a conversion option such as `-o`, a `--root` that doesn't hold the input, `--images keep` with a `.glb`, or with an output folder that doesn't hold the input's images |
| 3 | A resource limit was reached: the disk is full, or a quota or file-size limit |
| 4 | Another problem with the machine, such as permissions, a missing output directory or an output that already exists |
| 101 | Internal error (a panic) |

Codes 3 and 4 mean the file itself may be fine, and a retry elsewhere can succeed. Every failure exits through the same cleanup. A file-size limit (`ulimit -f`) only gives code 3 if SIGXFSZ is ignored, for instance with `trap '' XFSZ` before starting mesh2gltf: otherwise the signal kills the process and leaves its workspace and partial output behind.

## Working with large or georeferenced models

- Keep the workspace on a real disk with ample free space. It defaults to the output directory; use `--tmp-dir` to choose another location. An SSD helps with large models. Temporary files are cleaned up automatically.
- Georeferenced models need no option. A model far from 0 compared with its size, such as a building in projected coordinates, has its positions stored relative to a rounded origin near its center, and each node's translation puts it back in place. glTF's float32 positions keep the model's detail, and files converted separately stay aligned. `--center` drops that position instead, moving the model's center to 0. The origin is recorded in the glTF metadata either way.
- Coordinates are assumed to be in metres. No coordinate-system reprojection or unit conversion is performed.
- Material conversion approximates the source appearance. Unsupported data is generally omitted with a warning; VRML support covers a subset of IndexedFaceSet scenes.

## Documentation

Run `mesh2gltf --help` for command help.

The [Wiki](https://github.com/Holusion/mesh2gltf/wiki) covers:

- [Conversion](https://github.com/Holusion/mesh2gltf/wiki/Conversion): supported formats, georeferencing, image formats and material approximations.
- [How it works](https://github.com/Holusion/mesh2gltf/wiki/How-it-works): the streaming pipeline, heap and memory-mapped files, and disk requirements.

## License

Apache-2.0. See [LICENSE](LICENSE).
