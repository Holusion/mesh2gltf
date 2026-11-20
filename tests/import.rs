use serde_json::Value;
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
static SEQ: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "mesh2gltf-integration-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn file(&self, name: &str, body: &str) -> PathBuf {
        let p = self.0.join(name);
        fs::write(&p, body).unwrap();
        p
    }
    fn import(&self, input: &str, center: bool) -> std::process::Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_mesh2gltf"));
        cmd.arg(self.0.join(input))
            .arg("-o")
            .arg(self.0.join("out.gltf"))
            .arg("--tmp-dir")
            .arg(&self.0);
        if center {
            cmd.arg("--center");
        }
        cmd.output().unwrap()
    }
    fn result(&self) -> (Value, Vec<u8>) {
        (
            serde_json::from_slice(&fs::read(self.0.join("out.gltf")).unwrap()).unwrap(),
            fs::read(self.0.join("out.bin")).unwrap(),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn floats(root: &Value, bin: &[u8], accessor: usize, components: usize) -> Vec<Vec<f32>> {
    let a = &root["accessors"][accessor];
    let v = &root["bufferViews"][a["bufferView"].as_u64().unwrap() as usize];
    let offset = v["byteOffset"].as_u64().unwrap_or(0) as usize
        + a["byteOffset"].as_u64().unwrap_or(0) as usize;
    let stride = v["byteStride"].as_u64().unwrap_or((components * 4) as u64) as usize;
    (0..a["count"].as_u64().unwrap() as usize)
        .map(|i| {
            (0..components)
                .map(|j| {
                    f32::from_le_bytes(
                        bin[offset + i * stride + j * 4..offset + i * stride + j * 4 + 4]
                            .try_into()
                            .unwrap(),
                    )
                })
                .collect()
        })
        .collect()
}
#[test]
fn exact_center_scientific_notation_and_ranges() {
    let f = Fixture::new();
    f.file("in.obj","o georeferenced\nv 6.512345678901234e5 6862000 120\nv 651234.5698901234 6862000 120\nv 651234.5678901234 6862000.002 120\nf -3 -2 -1\n");
    let result = f.import("in.obj", true);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, bin) = f.result();
    assert_eq!(
        root["extras"]["mesh2gltf"]["origin"][0],
        "651234.5688901234"
    );
    // `--center` drops the position rather than restoring it.
    assert_eq!(root["extras"]["mesh2gltf"]["centered"], true);
    assert!(root["nodes"][0].get("translation").is_none());
    assert_eq!(
        root["buffers"][0]["byteLength"].as_u64().unwrap(),
        bin.len() as u64
    );
    let primitive = &root["meshes"][0]["primitives"][0];
    let p = floats(
        &root,
        &bin,
        primitive["attributes"]["POSITION"].as_u64().unwrap() as usize,
        3,
    );
    assert!((p[0][0] + 0.001).abs() < 1e-8);
    assert!((p[1][0] - 0.001).abs() < 1e-8);
    let normals = floats(
        &root,
        &bin,
        primitive["attributes"]["NORMAL"].as_u64().unwrap() as usize,
        3,
    );
    assert_eq!(normals[0], vec![0., 0., 1.]);
    for view in root["bufferViews"].as_array().unwrap() {
        let start = view["byteOffset"].as_u64().unwrap();
        let length = view["byteLength"].as_u64().unwrap();
        assert_eq!(start % 4, 0);
        assert!(start + length <= bin.len() as u64);
    }
}
#[test]
fn georeferenced_model_keeps_its_place() {
    let f = Fixture::new();
    // A 4 mm triangle in Lambert-93, where float32 resolves 0.5 m.
    f.file("in.obj", "o a\nv 651234.5678 6862001.234 120.5\nv 651234.5718 6862001.234 120.5\nv 651234.5678 6862001.238 120.5\nf 1 2 3\no b\nf 3 2 1\n");
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("far from 0"));
    let (root, bin) = f.result();
    let extras = &root["extras"]["mesh2gltf"];
    assert_eq!(
        extras["origin"],
        serde_json::json!(["651234.57", "6862001.236", "120.5"])
    );
    assert_eq!(extras["centered"], false);
    for node in root["nodes"].as_array().unwrap() {
        assert_eq!(
            node["translation"],
            serde_json::json!([651234.57, 6862001.236, 120.5])
        );
    }
    let primitive = &root["meshes"][0]["primitives"][0];
    let p = floats(
        &root,
        &bin,
        primitive["attributes"]["POSITION"].as_u64().unwrap() as usize,
        3,
    );
    // Positions keep the source's 0.1 mm digits relative to the origin.
    let expected = [
        [-0.0022, -0.002, 0.],
        [0.0018, -0.002, 0.],
        [-0.0022, 0.002, 0.],
    ];
    for (p, e) in p.iter().zip(expected) {
        for i in 0..3 {
            assert!((p[i] as f64 - e[i]).abs() < 1e-7, "{p:?} vs {e:?}");
        }
    }
}
#[test]
fn georeferenced_z_up_translation_is_in_output_axes() {
    let f = Fixture::new();
    f.file(
        "in.obj",
        "v 651000 6862000 120\nv 651010 6862000 120\nv 651000 6862010 120\nf 1 2 3\n",
    );
    let result = Command::new(env!("CARGO_BIN_EXE_mesh2gltf"))
        .arg(f.0.join("in.obj"))
        .args(["--up-axis", "z", "-o"])
        .arg(f.0.join("out.gltf"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, _) = f.result();
    assert_eq!(
        root["extras"]["mesh2gltf"]["origin"],
        serde_json::json!(["651010", "6862010", "120"])
    );
    assert_eq!(
        root["nodes"][0]["translation"],
        serde_json::json!([651010.0, 120.0, -6862010.0])
    );
}
#[test]
fn local_model_has_no_translation() {
    let f = Fixture::new();
    f.file(
        "in.obj",
        "v 2000 3000 0\nv 5000 3000 0\nv 2000 4000 120\nf 1 2 3\n",
    );
    assert!(f.import("in.obj", false).status.success());
    let (root, _) = f.result();
    assert_eq!(
        root["extras"]["mesh2gltf"]["origin"],
        serde_json::json!(["0", "0", "0"])
    );
    assert!(root["nodes"][0].get("translation").is_none());
}
#[test]
fn seams_materials_and_explicit_meshes() {
    let f = Fixture::new();
    f.file("in.mtl","newmtl paint\nKd 0.3 0.4 0.5\nKs 0.2 0.1 0.04\nNs 126\nTr 0.8\nd 0.7\nnewmtl metal\nPm 1\nPr 0.2\n");
    f.file("in.obj","mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 0 1\nvt .5 .5\nvn 0 0 2\no first\nusemtl paint\nf 1/1/1 2/2/1 3/3/1\nf 1/4/1 2/2/1 3/3/1\no second\nusemtl metal\nf 1/1/1 2/2/1 3/3/1\n");
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, bin) = f.result();
    assert_eq!(root["meshes"].as_array().unwrap().len(), 2);
    let p = &root["meshes"][0]["primitives"][0];
    let accessor = p["attributes"]["POSITION"].as_u64().unwrap() as usize;
    assert_eq!(root["accessors"][accessor]["count"], 4);
    let uv = floats(
        &root,
        &bin,
        p["attributes"]["TEXCOORD_0"].as_u64().unwrap() as usize,
        2,
    );
    assert_eq!(uv[0], vec![0., 1.]);
    let paint = &root["materials"][p["material"].as_u64().unwrap() as usize];
    assert_eq!(paint["pbrMetallicRoughness"]["metallicFactor"], 0.);
    assert_eq!(paint["alphaMode"], "BLEND");
    assert_eq!(paint["pbrMetallicRoughness"]["baseColorFactor"][3], 0.7);
    let a = &root["meshes"][1]["primitives"][0];
    let metal = &root["materials"][a["material"].as_u64().unwrap() as usize];
    assert_eq!(metal["pbrMetallicRoughness"]["metallicFactor"], 1.);
}
/// Each primitive's vertex count and index component type, in output order.
fn primitive_shapes(root: &Value) -> Vec<(u64, u64)> {
    root["meshes"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["primitives"].as_array().unwrap())
        .map(|p| {
            let accessor = |id: &Value| &root["accessors"][id.as_u64().unwrap() as usize];
            (
                accessor(&p["attributes"]["POSITION"])["count"]
                    .as_u64()
                    .unwrap(),
                accessor(&p["indices"])["componentType"].as_u64().unwrap(),
            )
        })
        .collect()
}
#[test]
fn repeated_triangles_share_one_primitive_and_vertices() {
    let f = Fixture::new();
    let mut file = fs::File::create(f.0.join("in.obj")).unwrap();
    writeln!(file, "v 0 0 0\nv 1 0 0\nv 0 1 0\no huge").unwrap();
    for _ in 0..65537 {
        writeln!(file, "f 1 2 3").unwrap();
    }
    drop(file);
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, _) = f.result();
    assert_eq!(primitive_shapes(&root), [(3, 5123)]);
    let p = &root["meshes"][0]["primitives"][0];
    let indices = &root["accessors"][p["indices"].as_u64().unwrap() as usize];
    assert_eq!(indices["count"], 65537 * 3);
}
/// A fan of `n - 1` triangles around an apex, using all `n` positions.
fn fan(f: &Fixture, n: usize) {
    let mut file = fs::File::create(f.0.join("in.obj")).unwrap();
    writeln!(file, "v 0 0 1").unwrap();
    for k in 1..n {
        writeln!(file, "v {k} 0 0").unwrap();
    }
    for k in 2..n {
        writeln!(file, "f 1 {k} {}", k + 1).unwrap();
    }
}
#[test]
fn index_width_follows_vertex_count() {
    // 65,535 vertices still fit 16-bit indices: 65,535 itself is the reserved restart value.
    for (n, component) in [(65535, 5123), (65536, 5125)] {
        let f = Fixture::new();
        fan(&f, n);
        let result = f.import("in.obj", false);
        assert!(result.status.success());
        let (root, bin) = f.result();
        assert_eq!(primitive_shapes(&root), [(n as u64, component)]);
        assert_eq!(
            root["buffers"][0]["byteLength"].as_u64().unwrap(),
            bin.len() as u64
        );
        for view in root["bufferViews"].as_array().unwrap() {
            assert_eq!(view["byteOffset"].as_u64().unwrap() % 4, 0);
        }
    }
}
#[test]
fn shuffled_faces_and_interleaved_materials_keep_exact_vertex_counts() {
    // A 40 x 40 grid of quads in a scrambled order, alternating two materials every quad.
    let f = Fixture::new();
    f.file("in.mtl", "newmtl a\nnewmtl b\n");
    let mut file = fs::File::create(f.0.join("in.obj")).unwrap();
    writeln!(file, "mtllib in.mtl").unwrap();
    for y in 0..41 {
        for x in 0..41 {
            writeln!(file, "v {x} {y} 0").unwrap();
        }
    }
    let at = |x: usize, y: usize| y * 41 + x + 1;
    let mut corners = [
        std::collections::BTreeSet::new(),
        std::collections::BTreeSet::new(),
    ];
    for i in 0..1600 {
        let q = (i * 997) % 1600;
        let (x, y) = (q % 40, q / 40);
        let material = (x + y) % 2;
        let quad = [at(x, y), at(x + 1, y), at(x + 1, y + 1), at(x, y + 1)];
        corners[material].extend(quad);
        writeln!(file, "usemtl {}", ["a", "b"][material]).unwrap();
        writeln!(file, "f {} {} {} {}", quad[0], quad[1], quad[2], quad[3]).unwrap();
    }
    drop(file);
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, _) = f.result();
    // One primitive per material, in order of first use, each holding exactly the positions
    // its faces use. The first face is the corner quad, which uses material a.
    let shapes = primitive_shapes(&root);
    assert_eq!(shapes.len(), 2);
    let material = |i: usize| {
        root["meshes"][0]["primitives"][i]["material"]
            .as_u64()
            .unwrap()
    };
    assert_eq!(root["materials"][material(0) as usize]["name"], "a");
    for (i, shape) in shapes.iter().enumerate() {
        let name = &root["materials"][material(i) as usize]["name"];
        let set = &corners[if name == "a" { 0 } else { 1 }];
        assert_eq!(*shape, (set.len() as u64, 5123));
    }
}
#[test]
fn vrml_material_defaults_and_georeferencing() {
    let f = Fixture::new();
    f.file("in.wrl","#VRML V2.0 utf8\nDEF terrain Shape { appearance Appearance { material Material {} } geometry IndexedFaceSet { coordIndex [0,1,2,-1] coord Coordinate { point [651000 6862000 120,651001 6862000 120,651000 6862001 120] } } }");
    let result = f.import("in.wrl", true);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, _) = f.result();
    assert_eq!(root["meshes"][0]["name"], "terrain");
    let p = &root["meshes"][0]["primitives"][0];
    let m = &root["materials"][p["material"].as_u64().unwrap() as usize];
    assert_eq!(
        m["extras"]["sourcePhong"]["Ks"],
        serde_json::json!([0., 0., 0.])
    );
    assert_eq!(m["extras"]["sourcePhong"]["Ns"], 25.6);
}
#[test]
fn errors_leave_no_output_and_do_not_overwrite() {
    let f = Fixture::new();
    f.file("in.obj", "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 9\n");
    assert!(!f.import("in.obj", true).status.success());
    assert!(!f.0.join("out.bin").exists());
    assert!(!f.0.join("out.gltf").exists());
    f.file("out.bin", "existing");
    assert!(!f.import("in.obj", false).status.success());
    assert_eq!(fs::read_to_string(f.0.join("out.bin")).unwrap(), "existing");
    let result = Command::new(env!("CARGO_BIN_EXE_mesh2gltf"))
        .args(["unknown", "in.obj"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    let result = Command::new(env!("CARGO_BIN_EXE_mesh2gltf"))
        .args(["--typo"])
        .output()
        .unwrap();
    assert!(!result.status.success());
}
#[test]
fn workspace_defaults_to_output_folder_and_is_removed() {
    let f = Fixture::new();
    f.file("in.obj", "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n");
    let entries = |dir: &str| {
        let mut names: Vec<String> = fs::read_dir(f.0.join(dir))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    };
    fs::create_dir(f.0.join("clean")).unwrap();
    assert!(import_images(&f, "clean/out.gltf", &[]).status.success());
    assert_eq!(entries("clean"), ["out.bin", "out.gltf"]);
    fs::create_dir(f.0.join("kept")).unwrap();
    assert!(import_images(&f, "kept/out.gltf", &["--keep-tmp"])
        .status
        .success());
    let kept = entries("kept");
    assert_eq!(kept.len(), 3);
    assert!(f.0.join("kept").join(&kept[0]).join("faces").is_file());
}
#[test]
fn unsupported_vrml_transform_is_an_error() {
    let f = Fixture::new();
    f.file(
        "in.wrl",
        "#VRML V2.0 utf8\nTransform { translation 651000 6862000 0 children [] }",
    );
    let result = f.import("in.wrl", true);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Non-identity"));
}

#[test]
fn z_up_rotation_and_texture_publication() {
    let f = Fixture::new();
    // One translucent texel is enough to need blending.
    image::RgbaImage::from_fn(2, 2, |x, y| {
        image::Rgba([9, 9, 9, if x + y == 2 { 254 } else { 255 }])
    })
    .save(f.0.join("albedo.png"))
    .unwrap();
    let png = fs::read(f.0.join("albedo.png")).unwrap();
    f.file("in.mtl", "newmtl paint\nKd 1 1 1\nmap_Kd albedo.png\n");
    f.file("in.obj", "mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 0 1\nusemtl paint\nf 1/1 2/2 3/3\n");
    let result = Command::new(env!("CARGO_BIN_EXE_mesh2gltf"))
        .arg(f.0.join("in.obj"))
        .args(["--up-axis", "z", "--images", "copy", "-o"])
        .arg(f.0.join("with spaces.gltf"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let root: Value =
        serde_json::from_slice(&fs::read(f.0.join("with spaces.gltf")).unwrap()).unwrap();
    assert_eq!(root["extras"]["mesh2gltf"]["sourceUpAxis"], "z");
    assert!(
        (root["nodes"][0]["rotation"][0].as_f64().unwrap() + std::f64::consts::FRAC_1_SQRT_2).abs()
            < 1e-15
    );
    assert_eq!(root["buffers"][0]["uri"], "with%20spaces.bin");
    assert_eq!(root["images"][0]["uri"], "with%20spaces_texture_0.png");
    assert_eq!(root["materials"][0]["alphaMode"], "BLEND");
    assert_eq!(
        fs::read(f.0.join("with spaces_texture_0.png")).unwrap(),
        png
    );
}

#[test]
fn opaque_alpha_channel_keeps_material_opaque() {
    let f = Fixture::new();
    image::RgbaImage::from_pixel(2, 2, image::Rgba([9, 9, 9, 255]))
        .save(f.0.join("albedo.png"))
        .unwrap();
    f.file("in.mtl", "newmtl paint\nmap_Kd albedo.png\n");
    f.file(
        "in.obj",
        "mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nusemtl paint\nf 1/1 2/1 3/1\n",
    );
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, _) = f.result();
    assert_eq!(root["materials"][0]["alphaMode"], "OPAQUE");
}

#[test]
fn texture_error_cleans_previously_copied_files() {
    let f = Fixture::new();
    image::RgbaImage::new(1, 1)
        .save(f.0.join("valid.png"))
        .unwrap();
    f.file(
        "in.mtl",
        "newmtl first\nmap_Kd valid.png\nnewmtl missing\nmap_Kd absent.png\n",
    );
    f.file("in.obj", "mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nusemtl first\nf 1/1 2/1 3/1\nusemtl missing\nf 1/1 2/1 3/1\n");
    let result = f.import("in.obj", false);
    assert!(!result.status.success());
    assert!(!f.0.join("out_texture_0.png").exists());
    assert!(!f.0.join("out.gltf").exists());
    assert!(!f.0.join("out.bin").exists());
}

#[test]
fn smooth_normals_cross_material_and_uv_seams() {
    let f = Fixture::new();
    f.file("in.mtl", "newmtl first\nnewmtl second\n");
    f.file("in.obj", "mtllib in.mtl\ns 1\nv 0 0 0\nv 1 0 0\nv 0 1 0\nv 0 0 2\nvt 0 0\nvt .5 .5\nusemtl first\nf 1/1 2/1 3/1\nusemtl second\nf 1/2 4/2 2/2\n");
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!String::from_utf8_lossy(&result.stderr).contains("smoothing groups"));
    let (root, bin) = f.result();
    let primitives = root["meshes"][0]["primitives"].as_array().unwrap();
    assert_eq!(primitives.len(), 2);
    for primitive in primitives {
        let normals = floats(
            &root,
            &bin,
            primitive["attributes"]["NORMAL"].as_u64().unwrap() as usize,
            3,
        );
        let shared = &normals[0];
        assert!(shared[0].abs() < 1e-6);
        assert!((shared[1] - 2. / 5f32.sqrt()).abs() < 1e-6);
        assert!((shared[2] - 1. / 5f32.sqrt()).abs() < 1e-6);
    }
}

#[test]
fn s_off_faces_are_flat_and_weigh_on_no_neighbour() {
    // Face 1 faces +z, face 2 faces +y; they share positions 1 and 2.
    for flat in ["s off", "s 0", "s OFF"] {
        let f = Fixture::new();
        f.file(
            "in.obj",
            &format!("v 0 0 0\nv 1 0 0\nv 0 1 0\nv 0 0 2\n{flat}\nf 1 2 3\ns 1\nf 1 4 2\n"),
        );
        let result = f.import("in.obj", false);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let (root, bin) = f.result();
        let p = &root["meshes"][0]["primitives"][0];
        let normals = floats(
            &root,
            &bin,
            p["attributes"]["NORMAL"].as_u64().unwrap() as usize,
            3,
        );
        // The flat face keeps its own corners; the smooth one is smoothed only with itself.
        assert_eq!(normals.len(), 6, "{flat}");
        let up = normals.iter().filter(|n| **n == vec![0., 0., 1.]).count();
        let side = normals.iter().filter(|n| **n == vec![0., 1., 0.]).count();
        assert_eq!((up, side), (3, 3), "{flat}: {normals:?}");
    }
}

#[test]
fn smooth_default_preserves_authored_normals() {
    let f = Fixture::new();
    f.file(
        "in.obj",
        "v 0 0 0\nv 1 0 0\nv 0 1 0\nv 0 0 1\nvn 2 0 0\nf 1//1 2//1 3//1\nf 1 4 2\n",
    );
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, bin) = f.result();
    let primitive = &root["meshes"][0]["primitives"][0];
    let normals = floats(
        &root,
        &bin,
        primitive["attributes"]["NORMAL"].as_u64().unwrap() as usize,
        3,
    );
    assert_eq!(normals[0], vec![1., 0., 0.]);
    assert!((normals[3][1] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    assert!((normals[3][2] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
}

#[test]
fn vrml_defaults_to_smooth_normals_with_zero_crease_angle() {
    let f = Fixture::new();
    f.file("in.wrl", "#VRML V2.0 utf8\nShape { geometry IndexedFaceSet { creaseAngle 0 coord Coordinate { point [0 0 0,1 0 0,0 1 0,0 0 1] } coordIndex [0,1,2,-1,0,3,1,-1] } }");
    let result = f.import("in.wrl", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, bin) = f.result();
    let primitive = &root["meshes"][0]["primitives"][0];
    let normals = floats(
        &root,
        &bin,
        primitive["attributes"]["NORMAL"].as_u64().unwrap() as usize,
        3,
    );
    assert!((normals[0][1] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    assert!((normals[0][2] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
}

#[test]
fn smooth_normals_on_a_primitive_with_32_bit_indices() {
    let f = Fixture::new();
    let mut source = fs::File::create(f.0.join("in.obj")).unwrap();
    for i in 0..22000 {
        writeln!(
            source,
            "v {} 0 0\nv {} 0 0\nv {} 1 0",
            i * 3,
            i * 3 + 1,
            i * 3
        )
        .unwrap();
    }
    writeln!(source, "v 0 0 1").unwrap();
    for i in 0..22000 {
        writeln!(source, "f {} {} {}", i * 3 + 1, i * 3 + 2, i * 3 + 3).unwrap();
    }
    // Revisit positions 1 and 2 after 66,000 other vertices.
    writeln!(source, "f 1 66001 2").unwrap();
    drop(source);
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, bin) = f.result();
    assert_eq!(primitive_shapes(&root), [(66001, 5125)]);
    let primitive = &root["meshes"][0]["primitives"][0];
    let attribute = |name: &str| primitive["attributes"][name].as_u64().unwrap() as usize;
    let positions = floats(&root, &bin, attribute("POSITION"), 3);
    let normals = floats(&root, &bin, attribute("NORMAL"), 3);
    let mut occurrences = 0;
    for (position, normal) in positions.iter().zip(normals) {
        if position == &[0., 0., 0.] {
            occurrences += 1;
            assert!((normal[1] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
            assert!((normal[2] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        }
    }
    assert_eq!(occurrences, 1);
}
fn textured_fixture(extension: &str) -> Fixture {
    let f = Fixture::new();
    let image = image::RgbImage::from_fn(4, 4, |x, y| {
        image::Rgb([(x * 60) as u8, (y * 60) as u8, 128])
    });
    image.save(f.0.join(format!("source.{extension}"))).unwrap();
    f.file(
        "in.mtl",
        &format!("newmtl paint\nmap_Kd source.{extension}\n"),
    );
    f.file(
        "in.obj",
        "mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nusemtl paint\nf 1/1 2/1 3/1\n",
    );
    f
}
fn import_images(f: &Fixture, output: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_mesh2gltf"))
        .arg(f.0.join("in.obj"))
        .arg("-o")
        .arg(f.0.join(output))
        .args(args)
        .output()
        .unwrap()
}
fn image_uri(f: &Fixture, output: &str) -> String {
    let root: Value = serde_json::from_slice(&fs::read(f.0.join(output)).unwrap()).unwrap();
    root["images"][0]["uri"].as_str().unwrap().to_owned()
}
#[test]
fn image_defaults_keep_in_same_folder_and_copy_in_different_folder() {
    let f = textured_fixture("png");
    let result = import_images(&f, "same.gltf", &[]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(image_uri(&f, "same.gltf"), "source.png");
    assert!(!f.0.join("same_texture_0.png").exists());
    fs::create_dir(f.0.join("destination")).unwrap();
    let result = import_images(&f, "destination/out.gltf", &[]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(image_uri(&f, "destination/out.gltf"), "out_texture_0.png");
    assert_eq!(
        fs::read(f.0.join("source.png")).unwrap(),
        fs::read(f.0.join("destination/out_texture_0.png")).unwrap()
    );
    let result = import_images(
        &f,
        "destination/keep.gltf",
        &["--images", "keep", "--trusted"],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(image_uri(&f, "destination/keep.gltf"), "../source.png");
}
#[test]
fn unsupported_image_is_converted_to_png() {
    let f = textured_fixture("bmp");
    let original = fs::read(f.0.join("source.bmp")).unwrap();
    let result = import_images(&f, "converted.gltf", &[]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(image_uri(&f, "converted.gltf"), "converted_texture_0.png");
    assert_eq!(
        image::open(f.0.join("source.bmp")).unwrap().to_rgb8(),
        image::open(f.0.join("converted_texture_0.png"))
            .unwrap()
            .to_rgb8()
    );
    assert_eq!(fs::read(f.0.join("source.bmp")).unwrap(), original);
}
#[test]
fn jpg_and_png_are_used_as_they_are() {
    for extension in ["jpg", "png"] {
        let f = textured_fixture(extension);
        let result = import_images(&f, "out.gltf", &[]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(image_uri(&f, "out.gltf"), format!("source.{extension}"));
    }
}
/// The JSON and binary chunk of a GLB, after checking its container.
fn read_glb(path: &std::path::Path) -> (Value, Vec<u8>) {
    let glb = fs::read(path).unwrap();
    let word = |at: usize| u32::from_le_bytes(glb[at..at + 4].try_into().unwrap()) as usize;
    assert_eq!(&glb[..4], b"glTF");
    assert_eq!(word(4), 2);
    assert_eq!(word(8), glb.len());
    let json_length = word(12);
    assert_eq!(&glb[16..20], b"JSON");
    assert_eq!(json_length % 4, 0);
    let json = &glb[20..20 + json_length];
    let bin_at = 20 + json_length;
    let bin_length = word(bin_at);
    assert_eq!(&glb[bin_at + 4..bin_at + 8], b"BIN\0");
    assert_eq!(bin_length % 4, 0);
    assert_eq!(bin_at + 8 + bin_length, glb.len());
    let root: Value = serde_json::from_slice(json).unwrap();
    (root, glb[bin_at + 8..].to_vec())
}
#[test]
fn glb_holds_the_geometry_and_images_in_one_file() {
    let f = textured_fixture("jpg");
    let jpg = fs::read(f.0.join("source.jpg")).unwrap();
    image::RgbImage::from_pixel(3, 1, image::Rgb([200, 10, 10]))
        .save(f.0.join("second.bmp"))
        .unwrap();
    f.file(
        "in.mtl",
        "newmtl paint\nmap_Kd source.jpg\nnewmtl other\nmap_Kd second.bmp\n",
    );
    f.file("in.obj", "mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nusemtl paint\nf 1/1 2/1 3/1\nusemtl other\nf 1/1 3/1 2/1\n");
    fs::create_dir(f.0.join("destination")).unwrap();
    let before = fs::read_dir(&f.0).unwrap().count();
    let result = import_images(&f, "destination/out.glb", &["--images", "copy"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    // No .bin, copied texture or workspace left beside it or beside the input.
    let written: Vec<_> = fs::read_dir(f.0.join("destination"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(written, ["out.glb"]);
    assert_eq!(fs::read_dir(&f.0).unwrap().count(), before);
    let (root, bin) = read_glb(&f.0.join("destination/out.glb"));
    let buffer = &root["buffers"][0];
    assert!(buffer.get("uri").is_none(), "{buffer}");
    let byte_length = buffer["byteLength"].as_u64().unwrap() as usize;
    assert_eq!(byte_length.next_multiple_of(4), bin.len());
    let positions = root["meshes"][0]["primitives"][0]["attributes"]["POSITION"]
        .as_u64()
        .unwrap() as usize;
    let mut corners = floats(&root, &bin, positions, 3);
    corners.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(corners, [[0., 0., 0.], [0., 1., 0.], [1., 0., 0.]]);
    let image = |i: usize| {
        let image = &root["images"][i];
        assert!(image.get("uri").is_none(), "{image}");
        let view = &root["bufferViews"][image["bufferView"].as_u64().unwrap() as usize];
        let offset = view["byteOffset"].as_u64().unwrap() as usize;
        assert_eq!(offset % 4, 0);
        let length = view["byteLength"].as_u64().unwrap() as usize;
        (
            image["mimeType"].as_str().unwrap().to_owned(),
            bin[offset..offset + length].to_vec(),
        )
    };
    assert_eq!(image(0), ("image/jpeg".to_owned(), jpg));
    let (mime, png) = image(1);
    assert_eq!(mime, "image/png");
    assert_eq!(
        image::load_from_memory(&png).unwrap().to_rgb8(),
        image::open(f.0.join("second.bmp")).unwrap().to_rgb8()
    );
}
#[test]
fn glb_refuses_images_keep_and_an_existing_file() {
    let f = textured_fixture("png");
    let result = import_images(&f, "out.glb", &["--images", "keep"]);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.status.code(), Some(2), "{stderr}");
    assert!(!f.0.join("out.glb").exists());
    // A .bin of the same name is no output of a .glb, so it doesn't stand in the way.
    f.file("out.bin", "unrelated");
    let result = import_images(&f, "out.glb", &[]);
    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let glb = fs::read(f.0.join("out.glb")).unwrap();
    let result = import_images(&f, "out.glb", &[]);
    assert_eq!(result.status.code(), Some(4));
    assert_eq!(fs::read(f.0.join("out.glb")).unwrap(), glb);
    assert_eq!(fs::read(f.0.join("out.bin")).unwrap(), b"unrelated");
}
#[test]
fn files_whose_folders_were_lost_are_found_by_name() {
    // As an upload of loose files leaves them: every folder the references name is gone.
    let f = textured_fixture("png");
    f.file("in.mtl", "newmtl paint\nmap_Kd textures/source.png\n");
    f.file(
        "in.obj",
        "mtllib materials/in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nusemtl paint\nf 1/1 2/1 3/1\n",
    );
    let result = import_images(&f, "out.gltf", &[]);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert!(
        stderr.contains("Warning: references found by name beside the file that makes them, not at their written path: 2, for example materials/in.mtl, textures/source.png"),
        "{stderr}"
    );
    assert_eq!(image_uri(&f, "out.gltf"), "source.png");
    let result = import_images(&f, "exact.gltf", &["--exact-paths"]);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("mtllib materials/in.mtl: not found, or outside the model's folder"),
        "{stderr}"
    );
    // Absolute paths too are only looked for where written.
    f.file(
        "in.mtl",
        "newmtl paint\nmap_Kd C:\\Users\\bob\\source.png\n",
    );
    f.file(
        "in.obj",
        "mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nusemtl paint\nf 1/1 2/1 3/1\n",
    );
    let result = import_images(&f, "absolute.gltf", &[]);
    assert!(result.status.success());
    let result = import_images(&f, "exact.gltf", &["--exact-paths"]);
    assert_eq!(result.status.code(), Some(1));
    assert!(!f.0.join("exact.gltf").exists());
}
#[test]
fn textures_meeting_on_one_file_by_name_are_refused() {
    let f = textured_fixture("png");
    f.file(
        "in.mtl",
        "newmtl a\nmap_Kd a/source.png\nnewmtl b\nmap_Kd b/source.png\n",
    );
    f.file(
        "in.obj",
        "mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nusemtl a\nf 1/1 2/1 3/1\nusemtl b\nf 1/1 3/1 2/1\n",
    );
    let result = import_images(&f, "out.glb", &[]);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("Texture b/source.png: found only by name, as source.png, which a/source.png also leads to; the files can't be told apart"),
        "{stderr}"
    );
    assert!(!f.0.join("out.glb").exists());
}
#[test]
fn conversion_preserves_alpha() {
    let f = textured_fixture("tga");
    image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 0]))
        .save(f.0.join("source.tga"))
        .unwrap();
    let result = import_images(&f, "png.gltf", &[]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        image::open(f.0.join("png_texture_0.png"))
            .unwrap()
            .to_rgba8()
            .get_pixel(0, 0)
            .0,
        [255, 0, 0, 0]
    );
}
#[test]
fn float_exr_textures_are_clamped_to_eight_bit_png() {
    let f = textured_fixture("png");
    f.file("in.mtl", "newmtl paint\nmap_Kd source.exr\n");
    image::Rgb32FImage::from_fn(2, 1, |x, _| {
        image::Rgb(if x == 0 {
            [0.0, 0.5, 1.0]
        } else {
            [2.0, -1.0, 0.25]
        })
    })
    .save(f.0.join("source.exr"))
    .unwrap();
    let result = import_images(&f, "exr.gltf", &[]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(image_uri(&f, "exr.gltf"), "exr_texture_0.png");
    let png = image::open(f.0.join("exr_texture_0.png")).unwrap();
    assert_eq!(png.color(), image::ColorType::Rgb8);
    let png = png.to_rgb8();
    assert_eq!(png.get_pixel(0, 0).0, [0, 128, 255]);
    assert_eq!(png.get_pixel(1, 0).0, [255, 0, 64]);
}

#[test]
fn awkward_polygons_triangulate_instead_of_aborting() {
    let f = Fixture::new();
    // A triangle written as a quad, a keyhole (a square with a square hole), four collinear
    // corners, and a self-intersecting pentagon.
    f.file(
        "in.obj",
        "v 0 0 0\nv 4 0 0\nv 4 4 0\nv 0 4 0\nv 1 1 0\nv 1 3 0\nv 3 3 0\nv 3 1 0\n\
         v 1 0 0\nv 2 0 0\nv 3 0 0\n\
         v 1 2 1\nv 2 2 1\nv 1 3 1\nv 4 0 1\nv 0 3 1\n\
         f 1 2 3 3\nf 1 2 3 4 1 5 6 7 8 5\nf 1 9 10 11\nf 12 13 14 15 16\n",
    );
    let result = f.import("in.obj", false);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert!(
        stderr.contains("skipped 1 faces with zero area"),
        "{stderr}"
    );
    assert!(
        stderr.contains("triangulated 1 self-intersecting"),
        "{stderr}"
    );
    let (root, _) = f.result();
    let p = &root["meshes"][0]["primitives"][0];
    // 1 + 8 + 2: the pentagon's third triangle has no area.
    assert_eq!(
        root["accessors"][p["indices"].as_u64().unwrap() as usize]["count"],
        3 * 11
    );
}

fn mesh_names(root: &Value) -> Vec<&str> {
    root["meshes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect()
}

#[test]
fn groups_are_objects_until_the_first_o() {
    let f = Fixture::new();
    // A group-only file: an empty header group, a nameless group, and a repeated name.
    f.file(
        "in.obj",
        "g header\nv 0 0 0\nv 1 0 0\nv 0 1 0\ng a\nf 1 2 3\ng b\nf 1 2 3\ng\nf 1 2 3\ng a\nf 1 2 3\n",
    );
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, _) = f.result();
    assert_eq!(mesh_names(&root), ["a", "b", "default", "a"]);
    let nodes: Vec<_> = root["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["name"].as_str())
        .collect();
    assert_eq!(nodes, ["a", "b", "default", "a"]);
    // Once an `o` names objects, groups no longer split them.
    let f = Fixture::new();
    f.file(
        "in.obj",
        "v 0 0 0\nv 1 0 0\nv 0 1 0\ng lead\nf 1 2 3\no house\ng roof\nf 1 2 3\ng walls\nf 1 2 3\n",
    );
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, _) = f.result();
    assert_eq!(mesh_names(&root), ["lead", "house"]);
    let house = &root["meshes"][1]["primitives"][0];
    assert_eq!(
        root["accessors"][house["indices"].as_u64().unwrap() as usize]["count"],
        6
    );
}

#[test]
fn faces_without_uvs_get_an_untextured_copy_of_their_material() {
    let f = textured_fixture("png");
    f.file("in.mtl", "newmtl paint\nKd 0.5 0.25 1\nmap_Kd source.png\n");
    f.file(
        "in.obj",
        "mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nusemtl paint\nf 1/1 2/1 3/1\nf 1 2 3\nf 3 2 1\n",
    );
    let result = f.import("in.obj", false);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert!(
        stderr.contains("2 faces with a textured material have no UVs"),
        "{stderr}"
    );
    let (root, _) = f.result();
    let primitives = root["meshes"][0]["primitives"].as_array().unwrap();
    assert_eq!(primitives.len(), 2);
    for p in primitives {
        let material = &root["materials"][p["material"].as_u64().unwrap() as usize];
        let pbr = &material["pbrMetallicRoughness"];
        assert_eq!(
            pbr["baseColorFactor"],
            serde_json::json!([0.5, 0.25, 1., 1.])
        );
        if p["attributes"].get("TEXCOORD_0").is_some() {
            assert_eq!(material["name"], "paint");
            assert!(pbr.get("baseColorTexture").is_some());
        } else {
            assert_eq!(material["name"], "paint-untextured");
            assert!(pbr.get("baseColorTexture").is_none());
        }
    }
    assert_eq!(root["materials"].as_array().unwrap().len(), 2);
    assert!(root.get("extensionsRequired").is_none());
}

#[test]
fn trailing_backslash_continues_statements_but_not_comments() {
    let f = Fixture::new();
    // A comment ending in a backslash keeps the next vertex; a face spans three lines, with
    // whitespace after a backslash; the last face ends the file mid-continuation.
    f.file(
        "in.obj",
        "# exported to C:\\models\\\nv 0 0 0\nv 1 0 0\nv 1 1 \\\n 0\nv 0 1 0\nf 1 2 \\  \n 3 \\\n 4\nf 1 2 3 \\",
    );
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, _) = f.result();
    let p = &root["meshes"][0]["primitives"][0];
    assert_eq!(
        root["accessors"][p["attributes"]["POSITION"].as_u64().unwrap() as usize]["count"],
        4
    );
    assert_eq!(
        root["accessors"][p["indices"].as_u64().unwrap() as usize]["count"],
        9
    );
    // Errors name the line a continued statement starts on.
    let f = Fixture::new();
    f.file("bad.obj", "v 0 0 0\nf 1 \\\n 2 x\n");
    let result = f.import("bad.obj", false);
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("bad.obj:2:"), "{stderr}");
}

#[test]
fn map_kd_options_become_texture_transform_and_clamp() {
    let f = Fixture::new();
    image::RgbImage::new(2, 2)
        .save(f.0.join("wood grain.png"))
        .unwrap();
    f.file(
        "in.mtl",
        "newmtl plain\nmap_Kd -s 1 1 1 -o 0 0 0 -blendu off -cc on wood grain.png\n\
         newmtl tiled\nmap_Kd -s 2 4 1 -o 0.25 -0.5 0 -clamp on -bm 1 -imfchan r -texres 512 -mm 0 2 -t 0.1 wood grain.png\n",
    );
    f.file(
        "in.obj",
        "mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nusemtl plain\nf 1/1 2/1 3/1\nusemtl tiled\nf 1/1 2/1 3/1\n",
    );
    let result = f.import("in.obj", false);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert!(
        stderr.contains("unsupported map_Kd options ignored: 2, for example -mm, -t"),
        "{stderr}"
    );
    let (root, _) = f.result();
    // One image, sampled by two textures: repeat for `plain`, clamp for `tiled`.
    assert_eq!(root["images"].as_array().unwrap().len(), 1);
    assert_eq!(
        root["samplers"],
        serde_json::json!([{"wrapS":10497,"wrapT":10497},{"wrapS":33071,"wrapT":33071}])
    );
    let info = |m: usize| &root["materials"][m]["pbrMetallicRoughness"]["baseColorTexture"];
    let sampler =
        |m: usize| root["textures"][info(m)["index"].as_u64().unwrap() as usize]["sampler"].clone();
    assert_eq!(root["materials"][0]["name"], "plain");
    assert_eq!(sampler(0), 0);
    assert_eq!(sampler(1), 1);
    // Identity options add nothing; `-s 2 4 -o 0.25 -0.5` samples at 2u + 0.25 and, with V
    // flipped, 4v + 1 - 4 + 0.5.
    assert!(info(0).get("extensions").is_none());
    assert_eq!(
        info(1)["extensions"]["KHR_texture_transform"],
        serde_json::json!({"offset": [0.25, -2.5], "scale": [2., 4.]})
    );
    assert_eq!(
        root["extensionsUsed"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| *e == "KHR_texture_transform")
            .count(),
        1
    );
    assert_eq!(
        root["extensionsRequired"],
        serde_json::json!(["KHR_texture_transform"])
    );
    // An option outside the spec's list leaves no way to find the path.
    let f = Fixture::new();
    f.file("in.mtl", "newmtl x\nmap_Kd -zoom 2 a.png\n");
    f.file("in.obj", "mtllib in.mtl\nv 0 0 0\n");
    let result = f.import("in.obj", false);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Unsupported map_Kd option -zoom"));
}

#[test]
fn fully_transparent_materials_warn_once_unless_glass() {
    let f = textured_fixture("png");
    f.file(
        "in.mtl",
        "newmtl ghost\nTr 1\nmap_Kd source.png\nnewmtl glass\nd 0\nillum 4\nnewmtl unused\nd 0\n",
    );
    // `ghost` is used with and without UVs, so it is written twice, but warned once.
    f.file(
        "in.obj",
        "mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nusemtl ghost\nf 1/1 2/1 3/1\nf 1 2 3\nusemtl glass\nf 1 2 3\n",
    );
    let result = f.import("in.obj", false);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert_eq!(
        stderr.matches("fully transparent materials").count(),
        1,
        "{stderr}"
    );
    assert!(
        stderr.contains("fully transparent materials") && stderr.contains("for example ghost"),
        "{stderr}"
    );
    // The output stays literal.
    let (root, _) = f.result();
    assert_eq!(
        root["materials"][0]["pbrMetallicRoughness"]["baseColorFactor"][3],
        0.
    );
}

#[test]
fn keywords_match_whatever_their_case() {
    let f = Fixture::new();
    f.file(
        "in.mtl",
        "NEWMTL Paint\nKD 0.5 0.25 1\nTR 0.5\nMAP_KS shine.png\n",
    );
    f.file(
        "in.obj",
        "MTLLIB in.mtl\nV 0 0 0\nV 1 0 0\nV 0 1 0\nVN 0 0 1\nO Thing\nUSEMTL Paint\nF 1//1 2//1 3//1\nCSTYPE bezier\n",
    );
    let result = f.import("in.obj", false);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    // Unsupported keywords are still reported as written.
    assert!(
        stderr.contains("unsupported MTL properties ignored: 1, for example MAP_KS"),
        "{stderr}"
    );
    assert!(
        stderr.contains("unsupported OBJ records omitted: 1, for example CSTYPE"),
        "{stderr}"
    );
    let (root, _) = f.result();
    assert_eq!(root["meshes"][0]["name"], "Thing");
    let material = &root["materials"][0];
    assert_eq!(material["name"], "Paint");
    assert_eq!(
        material["pbrMetallicRoughness"]["baseColorFactor"],
        serde_json::json!([0.5, 0.25, 1., 0.5])
    );
}

#[test]
fn faces_with_fewer_than_three_corners_are_omitted() {
    let f = Fixture::new();
    f.file(
        "in.obj",
        "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2\nf 3\nf\nf 1 2 3\n",
    );
    let result = f.import("in.obj", false);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert!(
        stderr.contains("omitted 3 faces with fewer than three corners"),
        "{stderr}"
    );
    let (root, _) = f.result();
    let p = &root["meshes"][0]["primitives"][0];
    assert_eq!(
        root["accessors"][p["indices"].as_u64().unwrap() as usize]["count"],
        3
    );
    // Their indices are still checked.
    f.file("bad.obj", "v 0 0 0\nf 1 2\n");
    assert!(!f.import("bad.obj", false).status.success());
}

#[test]
fn zero_source_normals_are_smoothed_and_degenerate_triangles_skipped() {
    let f = Fixture::new();
    f.file("in.obj","v 0 0 0\nv 1 0 0\nv 0 1 0\nv 0 0 1\nvn 0 0 0\nf 1//1 2//1 3//1\nf 1//1 4//1 2//1\nf 1//1 1//1 2//1\n");
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("zero-length source normals"));
    assert!(String::from_utf8_lossy(&result.stderr).contains("skipped 1 faces with zero area"));
    let (root, bin) = f.result();
    let p = &root["meshes"][0]["primitives"][0];
    assert_eq!(
        root["accessors"][p["indices"].as_u64().unwrap() as usize]["count"],
        6
    );
    let normals = floats(
        &root,
        &bin,
        p["attributes"]["NORMAL"].as_u64().unwrap() as usize,
        3,
    );
    assert!((normals[0][1] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    assert!((normals[0][2] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
}

/// A primitive's material name, vertex stride and colours, if it has any.
type Summary = (String, u64, Option<Vec<Vec<f32>>>);
fn colored_primitives(root: &Value, bin: &[u8]) -> Vec<Summary> {
    let mut found = Vec::new();
    for mesh in root["meshes"].as_array().unwrap() {
        for p in mesh["primitives"].as_array().unwrap() {
            let attributes = p["attributes"].as_object().unwrap();
            assert!(
                attributes
                    .keys()
                    .all(|k| k == "COLOR_0" || !k.contains("COLOR")),
                "{attributes:?}"
            );
            let position = attributes["POSITION"].as_u64().unwrap() as usize;
            let view = root["accessors"][position]["bufferView"].as_u64().unwrap() as usize;
            let colors = attributes
                .get("COLOR_0")
                .map(|c| floats(root, bin, c.as_u64().unwrap() as usize, 3));
            found.push((
                root["materials"][p["material"].as_u64().unwrap() as usize]["name"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
                root["bufferViews"][view]["byteStride"].as_u64().unwrap(),
                colors,
            ));
        }
    }
    found
}

#[test]
fn vertex_colors_replace_the_diffuse_color_and_give_way_to_textures() {
    let f = Fixture::new();
    image::RgbImage::new(1, 1)
        .save(f.0.join("albedo.png"))
        .unwrap();
    f.file(
        "in.mtl",
        "newmtl plain\nKd 0.5 0.5 0.5\nnewmtl photo\nKd 0.5 0.5 0.5\nmap_Kd albedo.png\n",
    );
    // The first position has no colour, nor do the last three.
    f.file("in.obj", "mtllib in.mtl\nv 0 0 0\nv 1 0 0 1 0.5 0\nv 0 1 0 0.2 0.04045 1 0.5\nv 0 0 1\nv 1 0 1\nv 0 1 1\nvt 0 0\nvt 1 0\nvt 0 1\nusemtl plain\nf 1 2 3\nf 4 5 6\nf 1/1 2/2 3/3\nusemtl photo\nf 1/1 2/2 3/3\nf 1 2 3\n");
    let result = f.import("in.obj", false);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert!(stderr.contains("vertex color alpha is omitted"), "{stderr}");
    assert!(
        stderr.contains("vertex colors on 1 textured faces are dropped"),
        "{stderr}"
    );
    let (root, bin) = f.result();
    let primitives = colored_primitives(&root, &bin);
    let summary: Vec<_> = primitives
        .iter()
        .map(|(name, stride, colors)| (name.as_str(), *stride, colors.is_some()))
        .collect();
    // Colours on a face replace `Kd`, so its material is a white copy; a face whose positions
    // have none keeps the material, and a texture drops them.
    assert_eq!(
        summary,
        [
            ("plain-vertex-colors", 36, true),
            ("plain", 24, false),
            ("plain-vertex-colors", 44, true),
            ("photo", 32, false),
            ("photo-vertex-colors", 36, true),
        ]
    );
    for (_, _, colors) in &primitives {
        let Some(colors) = colors else { continue };
        // A position without a colour, on a face where others have one, is white.
        assert_eq!(colors[0], vec![1., 1., 1.]);
        assert_eq!(colors[1][0], 1.);
        assert!((colors[1][1] - 0.21404114).abs() < 1e-6);
        assert_eq!(colors[1][2], 0.);
        assert!((colors[2][0] - 0.03310477).abs() < 1e-6);
        assert!((colors[2][1] - 0.04045 / 12.92).abs() < 1e-7);
    }
    let materials: Vec<_> = root["materials"].as_array().unwrap().iter().collect();
    assert_eq!(materials.len(), 4);
    for m in materials {
        let pbr = &m["pbrMetallicRoughness"];
        let colored = m["name"].as_str().unwrap().ends_with("-vertex-colors");
        let rgb = if colored { 1. } else { 0.5 };
        assert_eq!(
            pbr["baseColorFactor"],
            serde_json::json!([rgb, rgb, rgb, 1.])
        );
        assert_eq!(pbr.get("baseColorTexture").is_some(), m["name"] == "photo");
        assert_eq!(
            m["extras"]["sourcePhong"]["Kd"],
            serde_json::json!([0.5, 0.5, 0.5])
        );
    }
    // --no-color drops them everywhere, and their warnings with them: materials keep `Kd`.
    let result = Command::new(env!("CARGO_BIN_EXE_mesh2gltf"))
        .arg(f.0.join("in.obj"))
        .arg("--no-color")
        .arg("-o")
        .arg(f.0.join("plain.gltf"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert!(!stderr.contains("color"), "{stderr}");
    let root: Value = serde_json::from_slice(&fs::read(f.0.join("plain.gltf")).unwrap()).unwrap();
    let bin = fs::read(f.0.join("plain.bin")).unwrap();
    let names: Vec<_> = colored_primitives(&root, &bin)
        .into_iter()
        .map(|(name, _, colors)| {
            assert!(colors.is_none());
            name
        })
        .collect();
    assert_eq!(names, ["plain", "plain", "photo", "photo-untextured"]);
}

#[test]
fn vrml_colors_follow_the_same_rule() {
    let f = Fixture::new();
    image::RgbImage::new(1, 1)
        .save(f.0.join("albedo.png"))
        .unwrap();
    let triangle = "coord Coordinate { point [0 0 0, 1 0 0, 0 1 0] } coordIndex [0 1 2 -1]";
    let blue = "appearance Appearance { material Material { diffuseColor 0 0 1 } }";
    let shapes = [
        // Colours before the coordinates they belong to.
        format!("DEF before Shape {{ {blue} geometry IndexedFaceSet {{ color Color {{ color [1 0 0, 0 1 0, 0 0 1] }} {triangle} }} }}"),
        format!("DEF indexed Shape {{ {blue} geometry IndexedFaceSet {{ {triangle} colorIndex [0 1 2 -1] color Color {{ color [1 0 0, 0 1 0, 0 0 1] }} }} }}"),
        format!("DEF plain Shape {{ {blue} geometry IndexedFaceSet {{ {triangle} }} }}"),
        format!("DEF perface Shape {{ {blue} geometry IndexedFaceSet {{ {triangle} colorPerVertex FALSE color Color {{ color [1 0 0] }} }} }}"),
        format!("DEF reindexed Shape {{ {blue} geometry IndexedFaceSet {{ {triangle} colorIndex [2 1 0 -1] color Color {{ color [1 0 0, 0 1 0, 0 0 1] }} }} }}"),
        format!("DEF textured Shape {{ appearance Appearance {{ texture ImageTexture {{ url \"albedo.png\" }} }} geometry IndexedFaceSet {{ {triangle} texCoord TextureCoordinate {{ point [0 0, 1 0, 0 1] }} color Color {{ color [1 0 0, 0 1 0, 0 0 1] }} }} }}"),
    ];
    f.file(
        "in.wrl",
        &format!("#VRML V2.0 utf8\n{}\n", shapes.join("\n")),
    );
    let result = f.import("in.wrl", false);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert!(
        stderr.contains("colors omitted on 2 VRML shapes"),
        "{stderr}"
    );
    assert!(
        stderr.contains("vertex colors on 1 textured faces are dropped"),
        "{stderr}"
    );
    let (root, bin) = f.result();
    let primitives = colored_primitives(&root, &bin);
    let colored: Vec<_> = primitives
        .iter()
        .map(|(name, _, colors)| (name.split("_mtl_").next().unwrap(), colors.is_some()))
        .collect();
    assert_eq!(
        colored,
        [
            ("before", true),
            ("indexed", true),
            ("plain", false),
            ("perface", false),
            ("reindexed", false),
            ("textured", false),
        ]
    );
    let colors = primitives[0].2.as_ref().unwrap();
    assert_eq!(
        colors,
        &[vec![1., 0., 0.], vec![0., 1., 0.], vec![0., 0., 1.]]
    );
    for (name, _, colors) in &primitives {
        let m = root["materials"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == name.as_str())
            .unwrap();
        let expected = if colors.is_some() {
            assert!(name.ends_with("-vertex-colors"), "{name}");
            [1., 1., 1., 1.]
        } else if name.starts_with("textured") {
            [0.8, 0.8, 0.8, 1.]
        } else {
            [0., 0., 1., 1.]
        };
        assert_eq!(
            m["pbrMetallicRoughness"]["baseColorFactor"],
            serde_json::json!(expected),
            "{name}"
        );
    }
}

#[test]
fn unportable_vertex_components_warn_instead_of_failing() {
    let f = Fixture::new();
    f.file("in.obj", "v 0 0 0 1\nv 2 0 0 0.5\nv 0 2 0 1 2 3 4 5\nv 0 0 2 -1 0.5 255\nvt 0 0 0\nvt 1 0 0.5\nvt 0 1\nvt 1 1\nf 1/1 2/2 3/3\nf 1/1 2/2 4/4\n");
    let result = f.import("in.obj", false);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    for warning in [
        "vertex weights (w) are omitted",
        "unrecognized extra v components are omitted",
        "vertex colors outside [0,1] are clamped",
        "texture coordinates beyond u v are omitted",
    ] {
        assert_eq!(stderr.matches(warning).count(), 1, "{warning}: {stderr}");
    }
    let (root, bin) = f.result();
    let primitive = &root["meshes"][0]["primitives"][0];
    let position = primitive["attributes"]["POSITION"].as_u64().unwrap() as usize;
    // The weight is not applied as a divisor.
    assert_eq!(floats(&root, &bin, position, 3)[1], vec![2., 0., 0.]);
    let uv = primitive["attributes"]["TEXCOORD_0"].as_u64().unwrap() as usize;
    assert_eq!(floats(&root, &bin, uv, 2)[1], vec![1., 1.]);
    // Only the second face has a coloured position, so only it has colours.
    assert!(primitive["attributes"].get("COLOR_0").is_none());
    let colored = &root["meshes"][0]["primitives"][1];
    let color = colored["attributes"]["COLOR_0"].as_u64().unwrap() as usize;
    let colors = floats(&root, &bin, color, 3);
    assert_eq!(colors[1], vec![1., 1., 1.]);
    assert_eq!(colors[2][0], 0.);
    assert_eq!(colors[2][2], 1.);
}
#[test]
fn exit_codes_separate_a_bad_file_from_a_bad_environment() {
    let triangle = "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n";
    let run = |f: &Fixture, args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_mesh2gltf"))
            .args(args)
            .current_dir(&f.0)
            .output()
            .unwrap()
            .status
            .code()
    };
    let f = Fixture::new();
    f.file("good.obj", triangle);
    f.file("bad.obj", "v 0 0 oops\n");
    f.file(
        "texture.obj",
        &format!("mtllib texture.mtl\nvt 0 0\nusemtl m\n{triangle}"),
    );
    f.file("texture.mtl", "newmtl m\nmap_Kd missing.png\n");
    assert_eq!(run(&f, &["good.obj", "-o", "ok.gltf"]), Some(0));
    assert_eq!(run(&f, &["bad.obj", "-o", "out.gltf"]), Some(1));
    assert_eq!(run(&f, &["missing.obj", "-o", "out.gltf"]), Some(1));
    assert_eq!(run(&f, &["texture.obj", "-o", "out.gltf"]), Some(1));
    assert_eq!(run(&f, &["good.obj", "--no-such-option"]), Some(2));
    // A directory the caller chose isn't the file's fault, even though it is "not found" too.
    assert_eq!(run(&f, &["good.obj", "-o", "no/such/out.gltf"]), Some(4));
    assert_eq!(
        run(&f, &["good.obj", "-o", "out.gltf", "--tmp-dir", "no/such"]),
        Some(4)
    );
    assert_eq!(
        run(
            &f,
            &["good.obj", "-o", "no/such/out.gltf", "--tmp-dir", "."]
        ),
        Some(4)
    );
}
#[test]
fn gltf_and_glb_inputs_are_not_supported() {
    let f = Fixture::new();
    for name in ["in.gltf", "in.glb"] {
        f.file(name, "{\"asset\": {\"version\": \"2.0\"}}");
        let result = f.import(name, false);
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert_eq!(result.status.code(), Some(1), "{stderr}");
        let extension = name.rsplit('.').next().unwrap();
        assert!(
            stderr.contains(&format!("Unsupported input format: .{extension}")),
            "{stderr}"
        );
        assert!(!f.0.join("out.gltf").exists());
    }
}
#[test]
fn help_and_a_second_input() {
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_mesh2gltf"))
            .args(args)
            .output()
            .unwrap()
    };
    for args in [[].as_slice(), &["--help"], &["-h"]] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        let help = String::from_utf8_lossy(&out.stdout);
        assert!(
            help.starts_with("Usage: mesh2gltf [OPTIONS] <file.obj|file.ply|file.wrl|file.vrml>"),
            "{help}"
        );
    }
    let f = Fixture::new();
    f.file("in.obj", "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n");
    let input = f.0.join("in.obj");
    let out = run(&[input.to_str().unwrap(), input.to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("Unexpected argument"), "{stderr}");
}

/// A PLY value, written as text or packed in the file's byte order.
#[derive(Clone, Copy)]
enum P {
    U8(u8),
    I32(i32),
    F32(f32),
    F64(f64),
}
/// A PLY file in `format`: `header` holds its element and property lines, `values` its body.
fn ply(format: &str, header: &str, values: &[P]) -> Vec<u8> {
    let mut out = format!("ply\nformat {format} 1.0\n{header}end_header\n").into_bytes();
    for value in values {
        let big = format == "binary_big_endian";
        match (format, *value) {
            ("ascii", P::U8(x)) => out.extend(format!("{x} ").bytes()),
            ("ascii", P::I32(x)) => out.extend(format!("{x} ").bytes()),
            ("ascii", P::F32(x)) => out.extend(format!("{x} ").bytes()),
            ("ascii", P::F64(x)) => out.extend(format!("{x} ").bytes()),
            (_, P::U8(x)) => out.push(x),
            (_, P::I32(x)) if big => out.extend(x.to_be_bytes()),
            (_, P::I32(x)) => out.extend(x.to_le_bytes()),
            (_, P::F32(x)) if big => out.extend(x.to_be_bytes()),
            (_, P::F32(x)) => out.extend(x.to_le_bytes()),
            (_, P::F64(x)) if big => out.extend(x.to_be_bytes()),
            (_, P::F64(x)) => out.extend(x.to_le_bytes()),
        }
    }
    out
}
fn ply_fixture(bytes: &[u8]) -> (Fixture, std::process::Output) {
    let f = Fixture::new();
    fs::write(f.0.join("in.ply"), bytes).unwrap();
    let result = f.import("in.ply", false);
    (f, result)
}
/// Primitives as (material name, vertex count, index count, attribute names).
fn ply_primitives(root: &Value) -> Vec<(String, u64, u64, Vec<String>)> {
    let accessor = |id: &Value| &root["accessors"][id.as_u64().unwrap() as usize];
    root["meshes"][0]["primitives"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            let material = &root["materials"][p["material"].as_u64().unwrap() as usize];
            (
                material["name"].as_str().unwrap().to_owned(),
                accessor(&p["attributes"]["POSITION"])["count"]
                    .as_u64()
                    .unwrap(),
                accessor(&p["indices"])["count"].as_u64().unwrap(),
                p["attributes"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect(),
            )
        })
        .collect()
}
#[test]
fn ply_ascii_and_binary_of_either_byte_order_give_the_same_model() {
    let header = "comment a quad\nelement vertex 4\nproperty float x\nproperty float y\nproperty float z\nproperty float nx\nproperty float ny\nproperty float nz\nproperty uchar red\nproperty uchar green\nproperty uchar blue\nelement face 1\nproperty list uchar int vertex_indices\n";
    let mut values = Vec::new();
    for (x, y) in [(0., 0.), (1., 0.), (1., 1.), (0., 1.)] {
        values.extend([P::F32(x), P::F32(y), P::F32(0.25)]);
        values.extend([P::F32(0.), P::F32(0.), P::F32(1.)]);
        values.extend([P::U8(255), P::U8((x * 255.) as u8), P::U8(0)]);
    }
    values.extend([4, 0, 1, 2, 3].map(P::I32));
    values[36] = P::U8(4);
    let mut outputs = Vec::new();
    for format in ["ascii", "binary_little_endian", "binary_big_endian"] {
        let (f, result) = ply_fixture(&ply(format, header, &values));
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(result.status.success(), "{format}: {stderr}");
        let (root, bin) = f.result();
        assert_eq!(
            ply_primitives(&root),
            [(
                "default-vertex-colors".into(),
                4,
                6,
                vec!["COLOR_0".into(), "NORMAL".into(), "POSITION".into()]
            )],
            "{format}"
        );
        outputs.push(bin);
    }
    assert_eq!(outputs[0], outputs[1]);
    assert_eq!(outputs[0], outputs[2]);
}
#[test]
fn faceless_ply_is_rejected_as_a_point_cloud() {
    let vertices = "element vertex 1\nproperty float x\nproperty float y\nproperty float z\n";
    let values = [P::F32(0.); 3];
    for header in [
        vertices.to_owned(),
        format!("{vertices}element face 0\nproperty list uchar int vertex_indices\n"),
    ] {
        let (f, result) = ply_fixture(&ply("binary_little_endian", &header, &values));
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert_eq!(result.status.code(), Some(1), "{stderr}");
        assert!(
            stderr.contains("in.ply: PLY file has no faces; point clouds are not supported"),
            "{stderr}"
        );
        assert!(!f.0.join("out.gltf").exists());
    }
}
#[test]
fn ply_per_corner_texcoords_share_vertices_and_pick_textures() {
    let f = Fixture::new();
    for name in ["a.png", "b.png"] {
        image::RgbImage::new(1, 1).save(f.0.join(name)).unwrap();
    }
    let header = "comment TextureFile a.png\ncomment TextureFile b.png\nelement vertex 4\nproperty float x\nproperty float y\nproperty float z\nelement face 4\nproperty list uchar int vertex_indices\nproperty list uchar float texcoord\nproperty int texnumber\nproperty uchar red\n";
    let mut values = Vec::new();
    for (x, y) in [(0., 0.), (1., 0.), (1., 1.), (0., 1.)] {
        values.extend([P::F32(x), P::F32(y), P::F32(0.)]);
    }
    let face = |corners: [i32; 3], uvs: &[f32], texnumber| {
        let mut v = vec![P::U8(3)];
        v.extend(corners.map(P::I32));
        v.push(P::U8(uvs.len() as u8));
        v.extend(uvs.iter().map(|t| P::F32(*t)));
        v.extend([P::I32(texnumber), P::U8(9)]);
        v
    };
    // The two faces on `a.png` agree on the UVs of the corners they share.
    values.extend(face([0, 1, 2], &[0., 0., 1., 0., 1., 1.], 0));
    values.extend(face([0, 2, 3], &[0., 0., 1., 1., 0., 1.], 0));
    values.extend(face([1, 2, 3], &[0., 0., 1., 0., 1., 1.], 1));
    // A texcoord list that doesn't match the corners leaves its face untextured.
    values.extend(face([0, 1, 2], &[0., 0., 1., 0.], 0));
    fs::write(
        f.0.join("in.ply"),
        ply("binary_little_endian", header, &values),
    )
    .unwrap();
    let result = f.import("in.ply", false);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert!(
        stderr.contains("unsupported PLY properties omitted: 1, for example face.red"),
        "{stderr}"
    );
    assert!(
        stderr.contains("1 faces whose texcoord list doesn't match"),
        "{stderr}"
    );
    let (root, _) = f.result();
    let uv = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
    assert_eq!(
        ply_primitives(&root),
        [
            (
                "a.png".into(),
                4,
                6,
                uv(&["NORMAL", "POSITION", "TEXCOORD_0"])
            ),
            (
                "b.png".into(),
                3,
                3,
                uv(&["NORMAL", "POSITION", "TEXCOORD_0"])
            ),
            ("a.png-untextured".into(), 3, 3, uv(&["NORMAL", "POSITION"])),
        ]
    );
    // The texture is the whole colour.
    let factors: Vec<_> = root["materials"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["name"].as_str().unwrap(),
                m["pbrMetallicRoughness"]["baseColorFactor"][0]
                    .as_f64()
                    .unwrap(),
            )
        })
        .collect();
    assert_eq!(
        factors,
        [("a.png-untextured", 1.), ("a.png", 1.), ("b.png", 1.)]
    );
    assert_eq!(root["images"].as_array().unwrap().len(), 2);
}
#[test]
fn ply_vertex_texcoords_and_unknown_elements() {
    let header = "element vertex 3\nproperty float x\nproperty float y\nproperty float z\nproperty float s\nproperty float t\nproperty float scalar_Density\nelement face 1\nproperty list uchar int vertex_indices\nelement edge 2\nproperty list uchar int vertex_indices\nproperty uchar red\n";
    let mut values = Vec::new();
    for (x, y) in [(0., 0.), (1., 0.), (0., 1.)] {
        values.extend([x, y, 0., x, y, 7.].map(P::F32));
    }
    values.extend([P::U8(3), P::I32(0), P::I32(1), P::I32(2)]);
    for _ in 0..2 {
        values.extend([P::U8(2), P::I32(0), P::I32(1), P::U8(5)]);
    }
    for format in ["ascii", "binary_little_endian"] {
        let (f, result) = ply_fixture(&ply(format, header, &values));
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(result.status.success(), "{stderr}");
        assert!(
            stderr.contains("unsupported PLY elements omitted: 1, for example edge"),
            "{stderr}"
        );
        assert!(stderr.contains("vertex.scalar_Density"), "{stderr}");
        let (root, bin) = f.result();
        let p = &root["meshes"][0]["primitives"][0];
        let uv = floats(
            &root,
            &bin,
            p["attributes"]["TEXCOORD_0"].as_u64().unwrap() as usize,
            2,
        );
        // glTF's V runs down, as for OBJ.
        assert_eq!(uv, [vec![0., 1.], vec![1., 1.], vec![0., 0.]]);
    }
}
#[test]
fn ply_georeferenced_doubles_keep_their_place() {
    let header = "element vertex 3\nproperty double x\nproperty double y\nproperty double z\nelement face 1\nproperty list uchar uint vertex_indices\n";
    let mut values = Vec::new();
    for (x, y) in [
        (651234.5678, 6862001.234),
        (651234.5718, 6862001.234),
        (651234.5678, 6862001.238),
    ] {
        values.extend([P::F64(x), P::F64(y), P::F64(120.5)]);
    }
    values.extend([P::U8(3), P::I32(0), P::I32(1), P::I32(2)]);
    for format in ["ascii", "binary_big_endian"] {
        let (f, result) = ply_fixture(&ply(format, header, &values));
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let (root, bin) = f.result();
        assert_eq!(
            root["extras"]["mesh2gltf"]["origin"],
            serde_json::json!(["651234.57", "6862001.236", "120.5"])
        );
        let primitive = &root["meshes"][0]["primitives"][0];
        let p = floats(
            &root,
            &bin,
            primitive["attributes"]["POSITION"].as_u64().unwrap() as usize,
            3,
        );
        let expected = [
            [-0.0022, -0.002, 0.],
            [0.0018, -0.002, 0.],
            [-0.0022, 0.002, 0.],
        ];
        for (p, e) in p.iter().zip(expected) {
            for i in 0..3 {
                assert!((p[i] as f64 - e[i]).abs() < 1e-7, "{p:?} vs {e:?}");
            }
        }
    }
}
#[test]
fn ply_that_cannot_be_read_as_declared_is_rejected() {
    let header = "element vertex 3\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty list uchar int vertex_indices\n";
    let mut values = vec![P::F32(0.); 9];
    values.extend([P::U8(3), P::I32(0), P::I32(1), P::I32(3)]);
    let (_, result) = ply_fixture(&ply("ascii", header, &values));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("in.ply: face 0: PLY index or count out of range: 3"),
        "{stderr}"
    );
    let mut bytes = ply("binary_little_endian", header, &values);
    bytes.truncate(bytes.len() - 2);
    let (_, result) = ply_fixture(&bytes);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("PLY data ends before the elements its header declares"),
        "{stderr}"
    );
    // A texture outside the model's folder is refused like any other reference.
    let header =
        format!("comment TextureFile ../outside.png\n{header}property list uchar float texcoord\n");
    let (_, result) = ply_fixture(&ply("ascii", &header, &values));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("Texture ../outside.png: not found, or outside"),
        "{stderr}"
    );
}
