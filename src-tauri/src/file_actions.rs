use rfd::FileDialog;

pub fn open_directory_picker() -> Result<String, String> {
    match FileDialog::new().pick_folder() {
        Some(path) => Ok(path.to_string_lossy().to_string()),
        None => Err("No directory selected".to_string()),
    }
}

pub fn open_csv_file_picker() -> Result<String, String> {
    match FileDialog::new()
        .add_filter("CSV Files", &["csv"])
        .set_file_name("modlist.csv")
        .pick_file()
    {
        Some(file) => Ok(file.as_path().to_string_lossy().to_string()),
        None => Err("No file selected".to_string()),
    }
}

pub fn open_esm_file_picker() -> Result<String, String> {
    match FileDialog::new()
        .add_filter("ESM Files", &["esm"])
        .set_file_name("Morrowind.esm")
        .pick_file()
    {
        Some(file) => Ok(file.as_path().to_string_lossy().to_string()),
        None => Err("No file selected".to_string()),
    }
}

pub fn open_directory(path: String) -> Result<(), String> {
    match opener::open(&path) {
        Ok(_) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Picks the directory holding an already-unpacked TES3MP runtime — an
/// extracted official release, or a fork build the user maintains.
pub fn open_runtime_directory_picker() -> Result<String, String> {
    match FileDialog::new()
        .set_title("Select a TES3MP install directory")
        .pick_folder()
    {
        Some(path) => Ok(path.to_string_lossy().to_string()),
        None => Err("No directory selected".to_string()),
    }
}

/// Picks a TES3MP release archive on disk. The filter lists the formats
/// `runtime::acquire` can extract; the backend re-checks the file's magic
/// bytes, so a renamed archive still works.
pub fn open_runtime_archive_picker() -> Result<String, String> {
    match FileDialog::new()
        .set_title("Select a TES3MP release archive")
        .add_filter("TES3MP release archives", &["zip", "tar.gz", "tgz"])
        .pick_file()
    {
        Some(file) => Ok(file.as_path().to_string_lossy().to_string()),
        None => Err("No file selected".to_string()),
    }
}
