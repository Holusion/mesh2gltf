//! The node tree of scene formats: nodes with their transforms, meshes reused by several nodes,
//! and the source's axes and unit on one root node. Flat formats (OBJ, PLY, VRML) have no tree:
//! each object is one node, as `gltf` writes it.
use crate::{fixed::BBox, Res};
use serde_json::{json, Value};

/// A 4×4 transform, column-major like glTF's `matrix`.
pub type Matrix = [f64; 16];
pub const IDENTITY: Matrix = [
    1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
];

/// `a · b`: `b` applies first.
pub fn multiply(a: &Matrix, b: &Matrix) -> Matrix {
    std::array::from_fn(|i| {
        let (column, row) = (i / 4, i % 4);
        (0..4).map(|k| a[k * 4 + row] * b[column * 4 + k]).sum()
    })
}
pub fn translation(t: [f64; 3]) -> Matrix {
    let mut m = IDENTITY;
    m[12..15].copy_from_slice(&t);
    m
}
pub fn transform_point(m: &Matrix, p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|row| (0..3).map(|k| m[k * 4 + row] * p[k]).sum::<f64>() + m[12 + row])
}

pub struct Node {
    pub name: String,
    /// Always an earlier node, so that a parent's transform is known before its children's.
    pub parent: Option<u32>,
    /// The local transform, composed in `f64`.
    pub matrix: Matrix,
    pub mesh: Option<Instance>,
}
pub struct Instance {
    /// An object of the face spool: several nodes may instance one, and share its glTF mesh.
    pub object: u32,
    /// Applied to the geometry only, not to the node's children: COLLADA's `bind_shape_matrix`,
    /// FBX's geometric transform. Identity otherwise.
    pub geometry: Matrix,
}
/// The source's axes and unit, which the root node turns into glTF's: Y up, metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Basis {
    /// glTF coordinates from source ones: `out[i] = Σ axes[i][j]·source[j]`. Any signed
    /// permutation, so a left-handed source fits as well as a Y-up or Z-up one.
    pub axes: [[f64; 3]; 3],
    /// Metres per source unit.
    pub unit: f64,
}
impl Basis {
    pub const Y_UP: [[f64; 3]; 3] = [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]];
    /// `(x, y, z)` to `(x, z, −y)`, the rotation `--up-axis z` gives flat formats.
    pub const Z_UP: [[f64; 3]; 3] = [[1., 0., 0.], [0., 0., 1.], [0., -1., 0.]];
    pub fn matrix(&self) -> Matrix {
        let mut m = IDENTITY;
        for (row, axis) in self.axes.iter().enumerate() {
            for (column, a) in axis.iter().enumerate() {
                m[column * 4 + row] = a * self.unit;
            }
        }
        m
    }
}
pub struct Graph {
    /// The root node's name: the source's scene.
    pub name: String,
    pub nodes: Vec<Node>,
    pub basis: Basis,
    /// Each object's bounding box in its own coordinates, for the scene's.
    pub bounds: Vec<BBox>,
}
/// The glTF nodes of a graph, the root first, and the scene's bounds in glTF space.
pub struct Written {
    pub nodes: Vec<Value>,
    pub bounds: Option<([f64; 3], [f64; 3])>,
}
/// Writes `graph` as glTF nodes under one root carrying `basis`. `meshes` gives each object's
/// glTF mesh, `None` for one without triangles. Positions are stored relative to `origin`, in
/// source units, which a child node holding each mesh restores along with the instance's own
/// geometry matrix: `M·G·(p′ + o)`. Nodes whose subtree holds no mesh are dropped. `center` moves
/// the scene's bounding-box center to 0.
pub fn write(
    graph: &Graph,
    basis: &Basis,
    meshes: &[Option<usize>],
    origin: [f64; 3],
    center: bool,
) -> Res<Written> {
    let nodes = &graph.nodes;
    let mesh_of = |n: &Node| n.mesh.as_ref().and_then(|i| meshes[i.object as usize]);
    let mut world = Vec::with_capacity(nodes.len());
    for (i, node) in nodes.iter().enumerate() {
        let parent = match node.parent {
            Some(p) if p as usize >= i => return Err("Scene node listed before its parent".into()),
            Some(p) => world[p as usize],
            None => IDENTITY,
        };
        world.push(multiply(&parent, &node.matrix));
    }
    let mut alive: Vec<bool> = nodes.iter().map(|n| mesh_of(n).is_some()).collect();
    for i in (0..nodes.len()).rev() {
        if let (true, Some(p)) = (alive[i], nodes[i].parent) {
            alive[p as usize] = true;
        }
    }
    let base = basis.matrix();
    let mut bounds: Option<([f64; 3], [f64; 3])> = None;
    for (node, world) in nodes.iter().zip(&world) {
        let (Some(instance), Some(_)) = (&node.mesh, mesh_of(node)) else {
            continue;
        };
        let Some((min, max)) = graph.bounds[instance.object as usize].as_f64() else {
            continue;
        };
        let m = multiply(&base, &multiply(world, &instance.geometry));
        for corner in 0..8 {
            let p = std::array::from_fn(|i| if corner >> i & 1 == 0 { min[i] } else { max[i] });
            let q = transform_point(&m, p);
            let (lo, hi) = bounds.get_or_insert((q, q));
            for i in 0..3 {
                lo[i] = lo[i].min(q[i]);
                hi[i] = hi[i].max(q[i]);
            }
        }
    }
    let root_matrix = match bounds.filter(|_| center) {
        Some((lo, hi)) => multiply(
            &translation(std::array::from_fn(|i| -(lo[i] + hi[i]) / 2.)),
            &base,
        ),
        None => base,
    };
    // Indices first, so that children can be listed: each live node, then its mesh child.
    let restore = translation(origin);
    let mut index = vec![usize::MAX; nodes.len()];
    let mut out = vec![json!({"name": graph.name})];
    let mut children: Vec<Vec<usize>> = vec![Vec::new()];
    for (i, node) in nodes.iter().enumerate() {
        if !alive[i] {
            continue;
        }
        index[i] = out.len();
        let mut value = json!({"name": node.name});
        set_matrix(&mut value, &node.matrix);
        let parent = node.parent.map_or(0, |p| index[p as usize]);
        children[parent].push(index[i]);
        out.push(value);
        children.push(Vec::new());
        if let (Some(instance), Some(mesh)) = (&node.mesh, mesh_of(node)) {
            let local = multiply(&instance.geometry, &restore);
            if local == IDENTITY {
                out[index[i]]["mesh"] = json!(mesh);
            } else {
                let mut child = json!({"name": node.name, "mesh": mesh});
                set_matrix(&mut child, &local);
                children[index[i]].push(out.len());
                out.push(child);
                children.push(Vec::new());
            }
        }
    }
    set_matrix(&mut out[0], &root_matrix);
    for (node, children) in out.iter_mut().zip(children) {
        if !children.is_empty() {
            node["children"] = json!(children);
        }
    }
    Ok(Written { nodes: out, bounds })
}
fn set_matrix(node: &mut Value, m: &Matrix) {
    if *m != IDENTITY {
        node["matrix"] = json!(m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matrices_compose_right_to_left() {
        let t = translation([1., 2., 3.]);
        let z = Basis {
            axes: Basis::Z_UP,
            unit: 2.,
        }
        .matrix();
        // Translate, then rotate Z up into Y up and scale: (1, 2, 3) → (2, 6, −4).
        assert_eq!(transform_point(&multiply(&z, &t), [0.; 3]), [2., 6., -4.]);
        assert_eq!(transform_point(&multiply(&t, &z), [0.; 3]), [1., 2., 3.]);
    }
}
