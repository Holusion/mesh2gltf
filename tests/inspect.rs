//! `--inspect`, which reports the files a model refers to without converting it.
use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
static SEQ: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "mesh2gltf-inspect-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn file(&self, name: &str, body: &str) {
        fs::write(self.0.join(name), body).unwrap();
    }
    fn run(&self, args: &[&str]) -> Output {
        let out = Command::new(env!("CARGO_BIN_EXE_mesh2gltf"))
            .args(args)
            .current_dir(&self.0)
            .output()
            .unwrap();
        // Nothing is written beside the input, whatever happens.
        let written = fs::read_dir(&self.0).unwrap().any(|e| {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            name.starts_with("mesh2gltf-") || name.ends_with(".gltf") || name.ends_with(".bin")
        });
        assert!(!written, "{args:?}");
        out
    }
    /// What `--inspect` reports for `name`, which it must accept.
    fn requires(&self, name: &str, extra: &[&str]) -> Value {
        let out = self.run(&[&["--inspect", name], extra].concat());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{name}: {stderr}");
        let report: Value = serde_json::from_slice(&out.stdout).unwrap();
        report["requires"].clone()
    }
    /// The exit code and stderr of an inspection that must fail.
    fn refused(&self, args: &[&str]) -> (i32, String) {
        let out = self.run(args);
        assert!(out.stdout.is_empty(), "{args:?}");
        let code = out.status.code().unwrap();
        (code, String::from_utf8_lossy(&out.stderr).into_owned())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
const TRIANGLE: &str = "v 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nusemtl m\nf 1/1 2/1 3/1\n";
#[test]
fn an_obj_requires_the_libraries_its_first_mtllib_names() {
    let f = Fixture::new();
    // The files named need not exist.
    f.file(
        "one.obj",
        &format!("# exported\nMTLLIB my model.mtl\n{TRIANGLE}"),
    );
    assert_eq!(f.requires("one.obj", &[]), json!([["my model.mtl"]]));
    f.file("two.obj", "mtllib a.mtl \\\n  materials/b.mtl\n");
    let both = json!([["a.mtl"], ["materials/b.mtl", "b.mtl"]]);
    assert_eq!(f.requires("two.obj", &[]), both);
    f.file("none.obj", TRIANGLE);
    assert_eq!(f.requires("none.obj", &[]), json!([]));
    // Reading stops there: what follows is never looked at.
    let after = format!("{}\nmtllib other.mtl\n", "x".repeat(2 << 20));
    f.file("stops.obj", &format!("mtllib a.mtl\n{after}"));
    assert_eq!(f.requires("stops.obj", &[]), json!([["a.mtl"]]));
}
#[test]
fn an_mtl_requires_every_texture_it_names_once() {
    let f = Fixture::new();
    f.file(
        "in.mtl",
        "newmtl a\nmap_Kd -s 2 2 -bm 1 textures/diffuse.jpg\n\
         newmtl b\nmap_kd C:\\Users\\bob\\wall.png # old path\n\
         newmtl c\nmap_Kd \"./tex/../diffuse.jpg\"\n\
         newmtl d\nmap_Kd textures/diffuse.jpg\nnewmtl plain\nKd 1 0 0\nmap_Bump bump.png\n",
    );
    let requires = json!([
        ["textures/diffuse.jpg", "diffuse.jpg"],
        ["wall.png"],
        ["diffuse.jpg"]
    ]);
    assert_eq!(f.requires("in.mtl", &[]), requires);
    let exact = json!([["textures/diffuse.jpg"], ["diffuse.jpg"]]);
    f.file(
        "exact.mtl",
        "newmtl a\nmap_Kd textures/diffuse.jpg\nnewmtl b\nmap_Kd diffuse.jpg\n",
    );
    assert_eq!(f.requires("exact.mtl", &["--exact-paths"]), exact);
}
#[test]
fn a_ply_requires_its_textures_only_with_texture_coordinates() {
    let f = Fixture::new();
    let header = |uv: &str| {
        format!(
            "ply\nformat binary_little_endian 1.0\ncomment TextureFile my tex.png\n\
             element vertex 3\nproperty float x\nproperty float y\nproperty float z\n{uv}\
             element face 1\nproperty list uchar int vertex_indices\nend_header\n"
        )
    };
    // The header is all that's read, so the body may be missing.
    f.file("uv.ply", &header("property float s\nproperty float t\n"));
    assert_eq!(f.requires("uv.ply", &[]), json!([["my tex.png"]]));
    f.file("plain.ply", &header(""));
    assert_eq!(f.requires("plain.ply", &[]), json!([]));
    f.file(
        "points.ply",
        &header("").replace("element face 1", "element face 0"),
    );
    let (code, stderr) = f.refused(&["--inspect", "points.ply"]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("point clouds are not supported"),
        "{stderr}"
    );
}
#[test]
fn what_the_conversion_would_reject_fails_the_inspection() {
    let f = Fixture::new();
    let refused = "not found, or outside the model's folder";
    for (name, body, message) in [
        ("url.mtl", "newmtl a\nmap_Kd http://x/y.png\n", refused),
        (
            "option.mtl",
            "newmtl a\nmap_Kd -zz 1 t.png\n",
            "Unsupported map_Kd option -zz",
        ),
        ("value.mtl", "newmtl a\nKd 1 0\n", "three RGB"),
        ("url.obj", "mtllib https://x/y.mtl\n", refused),
        (
            "scene.wrl",
            "#VRML V2.0 utf8\n",
            "--inspect reads .obj, .mtl or .ply",
        ),
    ] {
        f.file(name, body);
        let (code, stderr) = f.refused(&["--inspect", name]);
        assert_eq!(code, 1, "{name}: {stderr}");
        assert!(stderr.contains(message), "{name}: {stderr}");
    }
}
#[test]
fn inspecting_takes_no_conversion_option() {
    let f = Fixture::new();
    f.file("in.obj", TRIANGLE);
    for option in [&["-o", "out.gltf"][..], &["--center"], &["--images=copy"]] {
        let (code, stderr) = f.refused(&[&["--inspect", "in.obj"], option].concat());
        assert_eq!(code, 2, "{option:?}: {stderr}");
        assert!(stderr.contains("--inspect converts nothing"), "{stderr}");
    }
}
#[test]
fn an_upload_meeting_every_requirement_converts() {
    let f = Fixture::new();
    f.file("in.obj", &format!("mtllib materials/in.mtl\n{TRIANGLE}"));
    f.file(
        "in.mtl",
        "newmtl m\nmap_Kd C:\\scans\\textures\\diffuse.png\n",
    );
    let required = |name| f.requires(name, &[]);
    // Each file is inspected alone, and only the last path of each requirement is uploaded.
    let names: Vec<_> = [required("in.obj"), required("in.mtl")]
        .iter()
        .flat_map(|r| r.as_array().unwrap().clone())
        .map(|tries| tries.as_array().unwrap().last().unwrap().clone())
        .collect();
    assert_eq!(names, ["in.mtl", "diffuse.png"]);
    image::RgbImage::new(2, 2)
        .save(f.0.join("diffuse.png"))
        .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_mesh2gltf"))
        .arg(f.0.join("in.obj"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let root: Value = serde_json::from_slice(&fs::read(f.0.join("in.gltf")).unwrap()).unwrap();
    assert_eq!(root["images"][0]["uri"], "diffuse.png");
}
