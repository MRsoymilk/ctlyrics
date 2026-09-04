use clap::Command;

fn main() -> Result<(), ctlyrics_tools::downloader::DownloaderError> {
    Command::new("ctlyrics-download")
        .about("Download lyrics from songs_list.txt")
        .get_matches();

    ctlyrics_tools::downloader::download_lyrics()
}
