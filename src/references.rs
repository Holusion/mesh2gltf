//! The one way to follow a file reference written inside a model.
use crate::{
    diagnostics::{safe, Tally},
    Res,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
    rc::Rc,
};
/// One answer for a missing file and a forbidden one, so a reference can't probe what exists
/// outside the model's folder.
#[derive(Debug)]
pub struct Refused;
impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("not found, or outside the model's folder")
    }
}
impl Error for Refused {}
/// A file found by name that another reference, meaning a different path, also leads to: which
/// file each one meant is a guess.
#[derive(Debug)]
pub struct Ambiguous {
    name: String,
    other: String,
}
impl fmt::Display for Ambiguous {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "found only by name, as {}, which {} also leads to; the files can't be told apart",
            self.name, self.other
        )
    }
}
impl Error for Ambiguous {}
/// A file a reference has reached: the path it meant, as the model wrote it, and whether it was
/// found by name rather than at that path.
struct Reach {
    meant: PathBuf,
    written: String,
    by_name: bool,
}
#[derive(Clone)]
pub struct References {
    /// The canonical directory references must stay in. `None` follows them anywhere.
    root: Option<PathBuf>,
    /// Whether a reference that leads nowhere is looked for by name beside the referencing file.
    by_name: bool,
    /// Shared by clones, so that every reference of one import is checked against the others.
    reached: Rc<RefCell<HashMap<PathBuf, Reach>>>,
    /// Reported once every clone is gone, which `run` arranges after the model is read.
    found_by_name: Rc<RefCell<Tally>>,
    /// For `--inspect`: where each reference would be looked for, in place of following it.
    listed: Option<Rc<RefCell<Vec<Vec<String>>>>>,
}
impl References {
    /// What the model in `dir` may reach, given whether the user has vouched for it, and whether
    /// a reference leading nowhere may be looked for by name.
    pub fn for_input(dir: &Path, trusted: bool, by_name: bool) -> io::Result<Self> {
        let references = if trusted {
            Self::unrestricted()
        } else {
            Self::confined(dir)?
        };
        Ok(Self {
            by_name,
            ..references
        })
    }
    /// Records every reference instead of following it, for `listed`. Nothing is looked up, so
    /// the files referred to need not exist.
    pub fn listing(self) -> Self {
        Self {
            listed: Some(Rc::default()),
            ..self
        }
    }
    /// For each reference made so far, the paths it is looked for at, in the order the import
    /// tries them, relative to the folder of the file that makes it, with forward slashes and `.`
    /// and `..` applied. The reference is met by the first of these that exists.
    pub fn listed(&self) -> Vec<Vec<String>> {
        self.listed
            .as_ref()
            .map(|listed| listed.borrow().clone())
            .unwrap_or_default()
    }
    /// For input the user hasn't vouched for: only regular files beneath `dir` can be reached.
    pub fn confined(dir: &Path) -> io::Result<Self> {
        Ok(Self::new(Some(fs::canonicalize(non_empty(dir))?)))
    }
    /// For `--trusted`, and for mesh2gltf reading back intermediates it wrote from input it had
    /// already confined. References in these may point anywhere, and open errors are the real ones.
    pub fn unrestricted() -> Self {
        Self::new(None)
    }
    fn new(root: Option<PathBuf>) -> Self {
        Self {
            root,
            by_name: true,
            reached: Rc::default(),
            found_by_name: Rc::new(RefCell::new(Tally::new(
                "references found by name beside the file that makes them, not at their written path",
            ))),
            listed: None,
        }
    }
    /// Where `reference`, written in a file in `base`, leads. A URI must be decoded first.
    ///
    /// A reference that leads nowhere is looked for by its last name beside the referencing file:
    /// old exporters write the absolute path a file had on the author's machine, and uploads or
    /// archives often lose the folders a relative path names. This is obj2gltf's fallback
    /// (`loadMtl.js`, `loadObj.js`: "Attempting to read the texture file from within the obj
    /// directory instead"), with one difference: a file found so, which another reference meaning
    /// a different path also reaches, is refused as `Ambiguous` rather than shared.
    pub fn resolve(&self, base: &Path, reference: &str) -> Res<PathBuf> {
        let base = non_empty(base);
        let written = reference.replace('\\', "/");
        if let Some(listed) = &self.listed {
            // The referencing file's folder stands for the root, as it does for the input.
            let (at_written, name) = self.lookups(&written, 0);
            let mut tries: Vec<String> = at_written
                .then(|| tidied(&written))
                .flatten()
                .into_iter()
                .collect();
            tries.extend(name.map(str::to_owned).filter(|name| !tries.contains(name)));
            let first = tries.first().map(|path| base.join(path)).ok_or(Refused)?;
            listed.borrow_mut().push(tries);
            return Ok(first);
        }
        let Some(root) = &self.root else {
            let (_, name) = self.lookups(&written, 0);
            // Windows reads the path itself, and `\\\\?\\` only means what it says with backslashes.
            let file = base.join(if cfg!(windows) { reference } else { &written });
            let beside = name
                .map(|name| base.join(name))
                .filter(|beside| !file.exists() && beside.exists());
            let by_name = beside.is_some();
            let found = beside.unwrap_or(file);
            // A path that doesn't exist fails when opened, with the real error.
            if let Ok(file) = fs::canonicalize(&found) {
                self.reach(file, meant(base, &written), reference, by_name)?;
            }
            return Ok(found);
        };
        let base = fs::canonicalize(base).map_err(|_| Refused)?;
        let depth = base.strip_prefix(root).map_err(|_| Refused)?;
        // Decided from the text alone, so the path of a refused reference is never looked up.
        let (at_written, name) = self.lookups(&written, depth.components().count());
        let at_written_path = if at_written {
            regular_file_inside(root, &base.join(&written))
        } else {
            Err(Refused)
        };
        let meant = meant(&base, &written);
        match (at_written_path, name) {
            (Ok(file), _) => self.reach(file, meant, reference, false),
            // Only the name goes any further, so this reaches nothing a bare name couldn't.
            (Err(_), Some(name)) => {
                let file = regular_file_inside(root, &base.join(name))?;
                self.reach(file, meant, reference, true)
            }
            (Err(refused), None) => Err(refused.into()),
        }
    }
    /// Where `written`, with forward slashes, in a file `depth` folders below the root, is looked
    /// for, from its text alone: whether at its written path, and the name to look for beside the
    /// referencing file, if any.
    fn lookups<'a>(&self, written: &'a str, depth: usize) -> (bool, Option<&'a str>) {
        // A URL is no path at all, so it has no name to look for.
        let url = has_prefix(written) && name_of_absolute(written).is_none();
        let name = last_name(written).filter(|_| self.by_name && !url);
        let at_written = self.root.is_none()
            || (name_of_absolute(written).is_none()
                && looks_relative(written)
                && !climbs_out(written, depth));
        (at_written, name)
    }
    /// Records that `written`, meaning `meant`, leads to `file`, and returns `file`, unless a
    /// reference meaning another path leads there too and either was found by name.
    fn reach(&self, file: PathBuf, meant: PathBuf, written: &str, by_name: bool) -> Res<PathBuf> {
        let mut reached = self.reached.borrow_mut();
        if let Some(other) = reached.get(&file) {
            if other.meant != meant && (by_name || other.by_name) {
                let (found, also) = if by_name {
                    (written, other.written.as_str())
                } else {
                    (other.written.as_str(), written)
                };
                let found = found.replace('\\', "/");
                let name = safe(last_name(&found).unwrap_or_default());
                let other = safe(also);
                return Err(Ambiguous { name, other }.into());
            }
        } else {
            let written = written.to_owned();
            reached.insert(
                file.clone(),
                Reach {
                    meant,
                    written,
                    by_name,
                },
            );
        }
        if by_name {
            self.found_by_name.borrow_mut().add(written);
        }
        Ok(file)
    }
}
/// The path a reference means, worked out from its text alone: `.` and `..` are applied to
/// `base` without looking anything up, so spellings of one path agree, whatever exists.
fn meant(base: &Path, written: &str) -> PathBuf {
    let mut path = base.to_path_buf();
    for part in written.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                path.pop();
            }
            _ => path.push(part),
        }
    }
    path
}
/// `written` as `listed` reports it: `.` and `..` applied as `meant` does, relative to the
/// referencing file's folder, or as written if it is absolute. `None` if it names no file.
fn tidied(written: &str) -> Option<String> {
    if name_of_absolute(written).is_some() || has_prefix(written) {
        return Some(written.to_owned());
    }
    let mut parts = Vec::new();
    for part in written.split('/') {
        match part {
            "" | "." => {}
            // Only `--trusted` lets a reference climb out, so the climb is kept as written.
            ".." if parts.last().is_some_and(|last| *last != "..") => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    let path = parts.join("/");
    (!matches!(parts.last(), None | Some(&".."))).then_some(path)
}
fn non_empty(dir: &Path) -> &Path {
    if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    }
}
/// The path part of an absolute reference, `C:\\x` or `/x`: what the file was called on the
/// author's machine.
fn name_of_absolute(written: &str) -> Option<&str> {
    let drive = written.split_once(':').filter(|(prefix, _)| {
        prefix.len() == 1 && prefix.starts_with(|c: char| c.is_ascii_alphabetic())
    });
    match drive {
        Some((_, path)) => Some(path),
        None if written.starts_with('/') => Some(written),
        None => None,
    }
}
/// The file name to look for beside the referencing file: the last part of the path, if it can
/// only name a file there.
fn last_name(written: &str) -> Option<&str> {
    let path = name_of_absolute(written).unwrap_or(written);
    let name = path.rsplit('/').next()?;
    let usable = !matches!(name, "" | "." | "..")
        && !name.contains('\0')
        && !(cfg!(windows) && name.contains(':'));
    usable.then_some(name)
}
/// A relative path: no root, drive letter, URI scheme or NUL byte.
fn looks_relative(written: &str) -> bool {
    // Windows also reads a colon as an alternate data stream, or as a drive.
    let colon = cfg!(windows) && written.contains(':');
    !(written.is_empty()
        || written.contains('\0')
        || written.starts_with('/')
        || colon
        || has_prefix(written))
}
/// `C:`, `http:` and the like. A drive letter is the one-letter case of a scheme.
fn has_prefix(written: &str) -> bool {
    written.split_once(':').is_some_and(|(prefix, _)| {
        prefix.starts_with(|c: char| c.is_ascii_alphabetic())
            && prefix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}
/// Whether the text reaches above the directory `depth` levels below the root. This reads the text
/// only: symlinks are for `regular_file_inside` to catch.
fn climbs_out(written: &str, mut depth: usize) -> bool {
    for part in written.split('/') {
        match part {
            "" | "." => {}
            ".." if depth == 0 => return true,
            ".." => depth -= 1,
            _ => depth += 1,
        }
    }
    false
}
/// The kernel's view of the path, which is what catches `link-to-outside/../file`. The type check
/// comes before any open, because opening a FIFO would block.
fn regular_file_inside(root: &Path, candidate: &Path) -> Result<PathBuf, Refused> {
    let file = fs::canonicalize(candidate).map_err(|_| Refused)?;
    let regular = fs::metadata(&file).is_ok_and(|m| m.is_file());
    if regular && file.starts_with(root) {
        Ok(file)
    } else {
        Err(Refused)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    /// `job` holds the model, with `tex/a.png` and `m.bin`. `outside/s.txt` sits next to it.
    struct Fixture(PathBuf);
    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "mesh2gltf-references-{name}-{}",
                std::process::id()
            ));
            fs::create_dir_all(dir.join("job/tex")).unwrap();
            fs::create_dir_all(dir.join("outside")).unwrap();
            fs::write(dir.join("job/tex/a.png"), "a").unwrap();
            fs::write(dir.join("job/m.bin"), "m").unwrap();
            fs::write(dir.join("outside/s.txt"), "s").unwrap();
            Self(dir)
        }
        fn job(&self) -> PathBuf {
            self.0.join("job")
        }
        fn found(&self, base: &str, reference: &str) -> Option<PathBuf> {
            References::confined(&self.job())
                .unwrap()
                .resolve(&self.job().join(base), reference)
                .ok()
        }
        /// As `--exact-paths` finds it: only at its written path.
        fn exact(&self, base: &str, reference: &str) -> Option<PathBuf> {
            References::for_input(&self.job(), false, false)
                .unwrap()
                .resolve(&self.job().join(base), reference)
                .ok()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn references_inside_are_found() {
        let f = Fixture::new("inside");
        let a = fs::canonicalize(f.job().join("tex/a.png")).unwrap();
        let m = fs::canonicalize(f.job().join("m.bin")).unwrap();
        assert_eq!(f.found("", "tex/a.png").unwrap(), a);
        assert_eq!(f.found("", "tex\\a.png").unwrap(), a);
        assert_eq!(f.found("", "./tex/../m.bin").unwrap(), m);
        assert_eq!(f.found("tex", "a.png").unwrap(), a);
        assert_eq!(f.found("tex", "../m.bin").unwrap(), m);
    }
    #[test]
    fn references_leaving_are_refused() {
        let f = Fixture::new("leaving");
        for reference in [
            "../outside/s.txt",
            "tex/../../outside/s.txt",
            "..\\outside\\s.txt",
            "..",
            "tex/../..",
        ] {
            assert_eq!(f.found("", reference), None, "{reference}");
        }
        assert_eq!(f.found("tex", "../../outside/s.txt"), None);
    }
    #[test]
    fn an_absolute_path_is_looked_for_by_name_beside_the_referencing_file() {
        let f = Fixture::new("beside");
        let m = fs::canonicalize(f.job().join("m.bin")).unwrap();
        let a = fs::canonicalize(f.job().join("tex/a.png")).unwrap();
        for reference in [
            "C:\\Users\\bob\\m.bin",
            "C:/Users/bob/m.bin",
            "C:m.bin",
            "/home/bob/m.bin",
            "\\\\server\\share\\m.bin",
            m.to_str().unwrap(),
        ] {
            assert_eq!(f.found("", reference).as_ref(), Some(&m), "{reference}");
        }
        assert_eq!(f.found("tex", "C:\\Users\\bob\\a.png").unwrap(), a);
        // Beside the referencing file only, not in the folders around it.
        assert_eq!(f.found("tex", "C:\\Users\\bob\\m.bin"), None);
        assert_eq!(f.found("", "C:\\Users\\bob\\a.png"), None);
        // Not a name at all, or not a file.
        for reference in ["/", "C:\\", "C:", "/x/..", "/x/.", "/tex", "C:\\x\\a\0b"] {
            assert_eq!(f.found("", reference), None, "{reference:?}");
        }
    }
    #[test]
    fn a_relative_path_leading_nowhere_is_looked_for_by_name() {
        let f = Fixture::new("relative");
        let m = fs::canonicalize(f.job().join("m.bin")).unwrap();
        let a = fs::canonicalize(f.job().join("tex/a.png")).unwrap();
        fs::write(f.job().join("tex/m.bin"), "in tex").unwrap();
        let in_tex = fs::canonicalize(f.job().join("tex/m.bin")).unwrap();
        assert_eq!(f.found("", "sub/dir/m.bin").unwrap(), m);
        assert_eq!(f.found("", "sub\\dir\\m.bin").unwrap(), m);
        assert_eq!(f.found("tex", "textures/a.png").unwrap(), a);
        // Leaving the folder, only the name is looked up, inside it.
        assert_eq!(f.found("", "../outside/m.bin").unwrap(), m);
        assert_eq!(f.found("", "../outside/s.txt"), None);
        // The written path wins wherever it leads.
        assert_eq!(f.found("", "tex/m.bin").unwrap(), in_tex);
        // Beside the referencing file only, not in the folders around it.
        assert_eq!(f.found("tex", "sub/m.bin").unwrap(), in_tex);
        assert_eq!(f.found("", "sub/a.png"), None);
        // A URL is no path.
        for reference in ["http://x/m.bin", "file:///x/m.bin"] {
            assert_eq!(f.found("", reference), None, "{reference}");
        }
    }
    #[test]
    fn exact_paths_are_only_looked_for_where_written() {
        let f = Fixture::new("exact");
        assert!(f.exact("", "tex/a.png").is_some());
        assert!(f.exact("tex", "../m.bin").is_some());
        for reference in ["sub/dir/m.bin", "C:\\Users\\bob\\m.bin", "/home/bob/m.bin"] {
            assert_eq!(f.exact("", reference), None, "{reference}");
        }
        let job = f.job();
        let trusted = References::for_input(&job, true, false).unwrap();
        let absolute = Path::new("/no/such/folder/m.bin");
        let found = trusted.resolve(&job, absolute.to_str().unwrap()).unwrap();
        // Joined, because on Windows a path from the root takes the drive of what it's joined to.
        assert_eq!(found, job.join(absolute));
    }
    #[test]
    fn references_meeting_on_a_file_found_by_name_are_ambiguous() {
        let f = Fixture::new("ambiguous");
        let job = f.job();
        let ambiguous = |references: &[&str]| {
            let refs = References::confined(&job).unwrap();
            let (last, first) = references.split_last().unwrap();
            for reference in first {
                refs.resolve(&job, reference).unwrap();
            }
            // A clone, as `obj` makes for libraries, shares what was reached.
            match refs.clone().resolve(&job, last) {
                Ok(_) => None,
                Err(e) => {
                    assert!(e.is::<Ambiguous>(), "{e}");
                    Some(e.to_string())
                }
            }
        };
        let message = ambiguous(&["a/m.bin", "b/m.bin"]).unwrap();
        assert_eq!(message, "found only by name, as m.bin, which a/m.bin also leads to; the files can't be told apart");
        let message = ambiguous(&["old/m.bin", "m.bin"]).unwrap();
        assert!(message.contains("as m.bin, which m.bin also"), "{message}");
        assert!(ambiguous(&["m.bin", "old/m.bin"]).is_some());
        assert!(ambiguous(&["C:\\a\\m.bin", "C:\\b\\m.bin"]).is_some());
        // One path, however it is spelled, and paths found where written, are not.
        assert_eq!(ambiguous(&["a/m.bin", "a\\m.bin", "./a/x/../m.bin"]), None);
        assert_eq!(ambiguous(&["m.bin", "tex/../m.bin"]), None);
        assert_eq!(ambiguous(&["tex/a.png", "m.bin"]), None);
        let refs = References::unrestricted();
        refs.resolve(&job, "a/m.bin").unwrap();
        assert!(refs.resolve(&job, "b/m.bin").unwrap_err().is::<Ambiguous>());
    }
    #[cfg(unix)]
    #[test]
    fn the_lookup_by_name_is_held_to_the_same_checks() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new("lookup");
        symlink(f.0.join("outside/s.txt"), f.job().join("s.txt")).unwrap();
        assert_eq!(f.found("", "C:\\Users\\bob\\s.txt"), None);
    }
    #[test]
    fn absolute_and_remote_references_are_refused() {
        let f = Fixture::new("absolute");
        for reference in [
            "/etc/passwd",
            "C:\\Windows\\win.ini",
            "C:/Windows/win.ini",
            "C:win.ini",
            "\\\\server\\share\\a.png",
            "file:///etc/passwd",
            "http://x/y.png",
        ] {
            assert_eq!(f.found("", reference), None, "{reference}");
        }
    }
    #[test]
    fn what_is_not_a_file_is_refused() {
        let f = Fixture::new("files");
        for reference in ["", ".", "tex", "tex/", "missing.png", "m.bin/x", "a\0b"] {
            assert_eq!(f.found("", reference), None, "{reference:?}");
        }
    }
    #[test]
    fn a_colon_is_only_a_prefix_at_the_start() {
        assert!(has_prefix("C:\\x"));
        assert!(has_prefix("http://x"));
        assert!(!has_prefix("12:30.png"));
        assert!(!has_prefix("tex/a:b.png"));
        assert!(!has_prefix("a.png"));
    }
    #[cfg(unix)]
    #[test]
    fn symlinks_are_judged_by_where_they_lead() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new("symlinks");
        symlink(f.job().join("tex/a.png"), f.job().join("inner-link")).unwrap();
        symlink(f.0.join("outside/s.txt"), f.job().join("file-link")).unwrap();
        symlink(f.0.join("outside"), f.job().join("dir-link")).unwrap();
        symlink("/etc", f.job().join("etc-link")).unwrap();
        symlink("loop", f.job().join("loop")).unwrap();
        assert!(f.found("", "inner-link").is_some());
        // `dir-link/../m.bin` reads as `m.bin`, but the kernel takes it to `outside/../m.bin`.
        fs::write(f.0.join("m.bin"), "next to the job").unwrap();
        for reference in [
            "file-link",
            "dir-link/s.txt",
            "dir-link/../m.bin",
            "etc-link/passwd",
            "loop",
        ] {
            assert_eq!(f.exact("", reference), None, "{reference}");
            // By name, only what the folder holds: here `m.bin`, never the one next to it.
            let found = f.found("", reference);
            let inside = fs::canonicalize(f.job().join("m.bin")).unwrap();
            assert!(found.is_none_or(|found| found == inside), "{reference}");
        }
    }
    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_rather_than_opened() {
        let f = Fixture::new("fifo");
        let path = std::ffi::CString::new(f.job().join("fifo").to_str().unwrap()).unwrap();
        // SAFETY: `path` is a valid C string for the duration of the call.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert_eq!(f.found("", "fifo"), None);
    }
    #[test]
    fn unrestricted_urls_are_not_looked_for_by_name() {
        let f = Fixture::new("url");
        let found = References::unrestricted()
            .resolve(&f.job(), "http://x/m.bin")
            .unwrap();
        assert_eq!(found, f.job().join("http://x/m.bin"));
    }
    #[test]
    fn unrestricted_references_go_anywhere() {
        let refs = References::unrestricted();
        let path = refs.resolve(Path::new("base"), "../x\\y.png").unwrap();
        assert_eq!(path, Path::new("base/../x/y.png"));
    }
    #[test]
    fn listing_reports_where_each_reference_is_looked_for_without_looking() {
        let f = Fixture::new("listing");
        let listed = |trusted: bool, by_name: bool, reference: &str| {
            let refs = References::for_input(&f.job(), trusted, by_name)
                .unwrap()
                .listing();
            let found = refs.resolve(&f.job(), reference).ok();
            let listed = refs.listed();
            assert_eq!(found.is_some(), !listed.is_empty(), "{reference}");
            listed.into_iter().next().unwrap_or_default()
        };
        let tries = |reference| listed(false, true, reference);
        assert_eq!(tries("missing.png"), ["missing.png"]);
        assert_eq!(tries("textures/a.png"), ["textures/a.png", "a.png"]);
        assert_eq!(tries(".\\tex\\..\\a.png"), ["a.png"]);
        assert_eq!(tries("C:\\Users\\bob\\a.png"), ["a.png"]);
        assert_eq!(tries("/home/bob/a.png"), ["a.png"]);
        assert_eq!(tries("../outside/s.txt"), ["s.txt"]);
        for nowhere in ["http://x/a.png", "", "tex/..", "C:\\"] {
            assert!(tries(nowhere).is_empty(), "{nowhere:?}");
        }
        assert_eq!(listed(false, false, "textures/a.png"), ["textures/a.png"]);
        assert!(listed(false, false, "/home/bob/a.png").is_empty());
        let trusted = |reference| listed(true, true, reference);
        assert_eq!(trusted("/home/bob/a.png"), ["/home/bob/a.png", "a.png"]);
        assert_eq!(trusted("../outside/s.txt"), ["../outside/s.txt", "s.txt"]);
        // Several references meeting on one name aren't for the listing to judge.
        let refs = References::confined(&f.job()).unwrap().listing();
        refs.resolve(&f.job(), "a/m.bin").unwrap();
        refs.resolve(&f.job(), "b/m.bin").unwrap();
        assert_eq!(refs.listed().len(), 2);
    }
    #[test]
    fn unrestricted_absolute_paths_prefer_what_they_name() {
        let f = Fixture::new("unrestricted");
        let refs = References::unrestricted();
        let job = f.job();
        let outside = fs::canonicalize(f.0.join("outside/s.txt")).unwrap();
        fs::write(job.join("s.txt"), "beside").unwrap();
        let found = refs.resolve(&job, outside.to_str().unwrap()).unwrap();
        assert_eq!(found, outside);
        let found = refs.resolve(&job, "/no/such/folder/s.txt").unwrap();
        assert_eq!(found, job.join("s.txt"));
        // Nothing beside it either: the real path stays, so that the real error is reported.
        let absolute = Path::new("/no/such/folder/other.txt");
        let found = refs.resolve(&job, absolute.to_str().unwrap()).unwrap();
        // Joined, because on Windows a path from the root takes the drive of what it's joined to.
        assert_eq!(found, job.join(absolute));
    }
}
