use clap::{Arg, Command};

fn main() -> Result<(), ctlyrics_tools::song_list::SongListError> {
    let matches = Command::new("ctlyrics-generate")
        .about("Generate songs_list.txt from music directory")
        .arg(Arg::new("dir").required(true).help("Music directory path"))
        .get_matches();
    let dir = matches.get_one::<String>("dir").unwrap();

    ctlyrics_tools::song_list::generate_song_list(dir, "songs_list.txt")?;
    println!("Generated songs_list.txt");
    Ok(())
}
