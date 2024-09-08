import os

def get_songs_from_directory(directory):
    valid_extensions = ('.mp3', '.flac', '.wav')
    songs = []

    for filename in os.listdir(directory):
        if not filename.endswith(valid_extensions):
            continue
        try:
            if '-' in filename:
                song_name, song_artist = filename.split('-', 1)
                song_name = song_name.strip()
                song_artist = (song_artist.rsplit('.', 1)[0]).strip()
            else:
                song_name = (filename.rsplit('.', 1)[0]).strip()
                song_artist = ""
            songs.append((song_name, song_artist))
        except Exception as e:
            print(f"Error processing {filename}: {e}")
            continue
    return songs

def save_songs_to_file(songs, file_path):
    with open(file_path, 'w', encoding='utf-8') as file:
        for song_name, song_artist in songs:
            if(song_artist == ""):
                file.write(f"{song_name}\n")
            else:
                file.write(f"{song_name} - {song_artist}\n")

if __name__ == "__main__":
    music_dir = "/home/_warehouse/music/"
    output_file = "songs_list.txt"
    songs = get_songs_from_directory(music_dir)
    save_songs_to_file(songs, output_file)
