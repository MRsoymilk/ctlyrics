use ctlyrics::mapping::{generate_id, scan_music_dir};
use std::fs;
use std::path::PathBuf;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ctlyrics-test-{}", generate_id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn generated_ids_are_unique() {
    assert_ne!(generate_id(), generate_id());
}

#[test]
fn scans_supported_music_files_from_a_temporary_directory() {
    let directory = TestDirectory::new();
    let nested = directory.0.join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(directory.0.join("Beta - Artist.FLAC"), []).unwrap();
    fs::write(nested.join("Alpha.mp3"), []).unwrap();
    fs::write(directory.0.join("ignored.txt"), []).unwrap();

    let songs = scan_music_dir(directory.0.to_str().unwrap()).unwrap();

    assert_eq!(songs.len(), 2);
    assert_eq!(songs[0].title, "Alpha");
    assert_eq!(songs[0].artist, "");
    assert_eq!(songs[1].title, "Beta");
    assert_eq!(songs[1].artist, "Artist");
    assert_eq!(songs[1].ext, "FLAC");
}
