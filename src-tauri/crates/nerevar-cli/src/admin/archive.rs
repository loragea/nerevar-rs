//! The package name an upload defaults to when `--name` is not given.
//!
//! `PUT /admin/packages/{name}` names the directory the package will take in
//! the host's `data/`, and the archive's own file name is nearly always what
//! the co-admin means: `Better Bodies.zip` becomes `Better Bodies`. The name
//! is checked against the host's own rule
//! (`nerevar_core::admin::validate_staged_package_name`) here rather than in
//! flight, so a bad one fails before the upload starts rather than after it
//! finishes.

use std::path::Path;

use nerevar_core::admin::validate_staged_package_name;

/// Compressed-tar suffixes, which `Path::file_stem` gets wrong: the stem of
/// `Better Bodies.tar.gz` is `Better Bodies.tar`. Matched case-insensitively.
const COMPOUND_SUFFIXES: &[&str] = &[".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"];

/// The package name to upload `archive` under: its file name with the archive
/// extension removed, validated as a package name.
pub fn package_name_from_archive(archive: &Path) -> Result<String, String> {
    let file_name = archive
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "Cannot read a package name out of {} — pass --name.",
                archive.display()
            )
        })?;

    let lowercase = file_name.to_ascii_lowercase();
    let stem = COMPOUND_SUFFIXES
        .iter()
        .find(|suffix| lowercase.ends_with(*suffix))
        .map(|suffix| &file_name[..file_name.len() - suffix.len()])
        .unwrap_or_else(|| match file_name.rsplit_once('.') {
            // A leading dot is the whole name of a dotfile, not an extension.
            Some((stem, _)) if !stem.is_empty() => stem,
            _ => file_name,
        });

    validate_staged_package_name(stem).map_err(|reason| {
        format!(
            "\"{stem}\", from {}, is not a usable package name: {reason}. Pass --name.",
            archive.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zip_gives_up_its_stem() {
        assert_eq!(
            package_name_from_archive(Path::new("Better Bodies.zip")).unwrap(),
            "Better Bodies"
        );
        assert_eq!(
            package_name_from_archive(Path::new("/home/ada/mods/Better Bodies.ZIP")).unwrap(),
            "Better Bodies"
        );
    }

    #[test]
    fn a_compound_tar_suffix_comes_off_whole() {
        for name in [
            "Tamriel Rebuilt.tar.gz",
            "Tamriel Rebuilt.TAR.GZ",
            "Tamriel Rebuilt.tar.xz",
            "Tamriel Rebuilt.tar.bz2",
            "Tamriel Rebuilt.tar.zst",
        ] {
            assert_eq!(
                package_name_from_archive(Path::new(name)).unwrap(),
                "Tamriel Rebuilt",
                "for {name}"
            );
        }
    }

    #[test]
    fn a_name_with_dots_in_it_keeps_all_but_the_last() {
        assert_eq!(
            package_name_from_archive(Path::new("Morrowind Rebirth 6.2.zip")).unwrap(),
            "Morrowind Rebirth 6.2"
        );
    }

    #[test]
    fn an_extensionless_archive_is_its_own_name() {
        assert_eq!(
            package_name_from_archive(Path::new("/tmp/BetterBodies")).unwrap(),
            "BetterBodies"
        );
    }

    #[test]
    fn a_name_the_host_would_refuse_fails_here_instead_of_after_the_upload() {
        // `..` and the reserved directory names are what the host rejects;
        // the CLI must not start streaming a file only to be told so.
        for name in ["/tmp/data.zip", "/tmp/.nerevar.zip", "/tmp/tes3mp.zip"] {
            let error = package_name_from_archive(Path::new(name)).unwrap_err();
            assert!(error.contains("--name"), "for {name}: {error}");
        }
    }
}
