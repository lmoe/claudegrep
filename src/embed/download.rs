//! Library files taken from wheels on `PyPI` and kept in the cache, one directory per set of files.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use zip::ZipArchive;
use zip::result::ZipError;

const PYPI: &str = "https://files.pythonhosted.org/packages";

#[derive(Debug)]
pub struct Wheel {
    /// Where `PyPI` serves the wheel: a directory named after the BLAKE2b-256 hash of the file, then the file's name.
    pub path: &'static str,
    /// The wheel's SHA-256, as `PyPI` lists it.
    pub sha256: &'static str,
    /// The files to take: where the wheel holds each one, and the name it is cached under.
    pub files: &'static [(&'static str, &'static str)],
}

impl Wheel {
    fn url(&self) -> String {
        format!("{PYPI}/{}", self.path)
    }
}

/// The directory `dir` with the files of `wheels` in it, downloaded when it does not exist yet. `what` names the download
/// in the line that announces it.
pub fn cached(dir: &Path, what: &str, wheels: &[Wheel]) -> Result<PathBuf> {
    if !dir.is_dir() {
        eprintln!("claudegrep: downloading {what}, once …");
        fill(dir, wheels, download)?;
    }
    Ok(dir.to_path_buf())
}

/// Fills `dir` with the files of `wheels`, each fetched by `fetch`. The directory appears whole: it is built under a name
/// of its own next to `dir` and then renamed, so a run that is interrupted leaves none, and of runs that race the first
/// one's directory stays.
fn fill(dir: &Path, wheels: &[Wheel], fetch: impl Fn(&Wheel, &Path) -> Result<File>) -> Result<()> {
    let parent = dir.parent().context("the cache directory has a parent")?;
    fs::create_dir_all(parent)?;
    let partial = tempfile::Builder::new().prefix(".partial").tempdir_in(parent)?;
    for wheel in wheels {
        let archive = partial.path().join(".wheel");
        unpack(fetch(wheel, &archive)?, wheel, partial.path())?;
        fs::remove_file(&archive)?;
    }
    let partial = partial.keep();
    match fs::rename(&partial, dir) {
        Ok(()) => Ok(()),
        Err(error) => {
            fs::remove_dir_all(&partial)?;
            if dir.is_dir() { Ok(()) } else { Err(error).with_context(|| format!("creating {}", dir.display())) }
        }
    }
}

/// Downloads `wheel` into the file `to` and checks it against its SHA-256. The download reads the proxy from the
/// environment (`HTTPS_PROXY`), as the model's does.
fn download(wheel: &Wheel, to: &Path) -> Result<File> {
    let url = wheel.url();
    let agent = ureq::builder().try_proxy_from_env(true).build();
    let response = agent.get(&url).call().with_context(|| format!("downloading {url}"))?;
    let bar = progress_bar(response.header("Content-Length").and_then(|length| length.parse().ok()));
    let mut reader = response.into_reader();
    let mut file = File::options().read(true).write(true).create_new(true).open(to)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let read = reader.read(&mut buffer).with_context(|| format!("downloading {url}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])?;
        bar.inc(read as u64);
    }
    bar.finish_and_clear();
    check(&hasher.finalize(), wheel.sha256).with_context(|| format!("checking {url}"))?;
    file.rewind()?;
    Ok(file)
}

fn progress_bar(bytes: Option<u64>) -> ProgressBar {
    let Some(bytes) = bytes else {
        return ProgressBar::hidden();
    };
    let style = ProgressStyle::with_template("{msg} [{bar:30}] {bytes}/{total_bytes}, {eta} left").expect("valid template");
    ProgressBar::new(bytes).with_style(style.progress_chars("=> ")).with_message("claudegrep: downloading")
}

/// Fails unless `digest` is the SHA-256 `expected`, given in lowercase hex.
fn check(digest: &[u8], expected: &str) -> Result<()> {
    let actual = digest.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    ensure!(actual == expected, "the download has the SHA-256 {actual}, expected {expected}");
    Ok(())
}

fn unpack(archive: File, wheel: &Wheel, into: &Path) -> Result<()> {
    let mut archive = ZipArchive::new(archive)?;
    for (wanted, name) in wheel.files {
        let mut entry = match archive.by_name(wanted) {
            Ok(entry) => entry,
            Err(ZipError::FileNotFound) => bail!("{wanted} not found in {}", wheel.url()),
            Err(error) => return Err(error.into()),
        };
        io::copy(&mut entry, &mut File::create(into.join(name))?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use zip::write::SimpleFileOptions;

    use super::*;

    const WHEEL: Wheel = Wheel {
        path: "aa/bb/cc/library-1.0-py3-none-any.whl",
        sha256: "",
        files: &[("package/lib/library.so.1", "library.so"), ("package/lib/helper.so", "helper.so")],
    };

    fn archive(files: &[(&str, &[u8])], to: &Path) -> Result<File> {
        let mut writer = zip::ZipWriter::new(File::options().read(true).write(true).create_new(true).open(to)?);
        for (path, content) in files {
            writer.start_file(*path, SimpleFileOptions::default())?;
            writer.write_all(content)?;
        }
        Ok(writer.finish()?)
    }

    fn whole(_: &Wheel, to: &Path) -> Result<File> {
        archive(
            &[("package/README", b"readme"), ("package/lib/library.so.1", b"the library"), ("package/lib/helper.so", b"its helper")],
            to,
        )
    }

    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> =
            fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn the_wanted_files_are_cached_under_their_names_and_nothing_else_stays_behind() {
        let cache = tempfile::TempDir::new().unwrap();
        let dir = cache.path().join("libraries").join("1.0");
        fill(&dir, &[WHEEL], whole).unwrap();
        assert_eq!(file_names(&dir), ["helper.so", "library.so"]);
        assert_eq!(fs::read(dir.join("library.so")).unwrap(), b"the library");
        assert_eq!(file_names(dir.parent().unwrap()), ["1.0"]);
    }

    #[test]
    fn the_files_of_several_wheels_share_the_directory() {
        const OTHER: Wheel = Wheel { path: "dd/ee/ff/other.whl", sha256: "", files: &[("other/lib/other.so.2", "other.so.2")] };
        let cache = tempfile::TempDir::new().unwrap();
        let dir = cache.path().join("1.0");
        let fetch = |wheel: &Wheel, to: &Path| {
            if wheel.path == OTHER.path { archive(&[("other/lib/other.so.2", b"another")], to) } else { whole(wheel, to) }
        };
        fill(&dir, &[WHEEL, OTHER], fetch).unwrap();
        assert_eq!(file_names(&dir), ["helper.so", "library.so", "other.so.2"]);
    }

    #[test]
    fn a_wheel_without_a_wanted_file_is_an_error_that_names_it_and_leaves_nothing_behind() {
        let cache = tempfile::TempDir::new().unwrap();
        let dir = cache.path().join("1.0");
        let error = fill(&dir, &[WHEEL], |_, to| archive(&[("package/lib/library.so.1", b"the library")], to)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "package/lib/helper.so not found in https://files.pythonhosted.org/packages/aa/bb/cc/library-1.0-py3-none-any.whl"
        );
        assert_eq!(file_names(cache.path()), Vec::<String>::new());
    }

    #[test]
    fn a_download_that_fails_or_is_no_archive_leaves_nothing_behind() {
        let cache = tempfile::TempDir::new().unwrap();
        let dir = cache.path().join("1.0");
        assert!(fill(&dir, &[WHEEL], |_, _| bail!("offline")).is_err());
        let broken = |_: &Wheel, to: &Path| {
            fs::write(to, b"not an archive")?;
            Ok(File::open(to)?)
        };
        assert!(fill(&dir, &[WHEEL], broken).is_err());
        assert_eq!(file_names(cache.path()), Vec::<String>::new());
    }

    #[test]
    fn runs_racing_on_one_directory_all_succeed() {
        let cache = tempfile::TempDir::new().unwrap();
        let dir = cache.path().join("1.0");
        std::thread::scope(|scope| {
            let runs: Vec<_> = (0..16).map(|_| scope.spawn(|| fill(&dir, &[WHEEL], whole))).collect();
            for run in runs {
                run.join().unwrap().unwrap();
            }
        });
        assert_eq!(file_names(&dir), ["helper.so", "library.so"]);
        assert_eq!(file_names(cache.path()), ["1.0"]);
    }

    #[test]
    fn an_existing_directory_is_used_without_downloading() {
        let cache = tempfile::TempDir::new().unwrap();
        assert_eq!(cached(cache.path(), "nothing", &[WHEEL]).unwrap(), cache.path());
    }

    #[test]
    fn a_download_with_the_expected_hash_passes() {
        check(&Sha256::digest(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad").unwrap();
    }

    #[test]
    fn a_download_with_another_hash_is_an_error_that_names_both() {
        let expected = "e2dc1993aa91d665faf2b17772e4e29a2999821e110c0e3d17e2b1c00d0e7f48";
        assert_eq!(
            check(&Sha256::digest(b"abc"), expected).unwrap_err().to_string(),
            format!("the download has the SHA-256 ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad, expected {expected}")
        );
    }
}
