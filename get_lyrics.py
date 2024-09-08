import os
import requests
from bs4 import BeautifulSoup

def search_song(song_name):
    search_url = f'https://www.sq0527.cn/search?ac={song_name}'
    response = requests.get(search_url)
    if response.status_code != 200:
        print(f"request failed: {response.status_code}")
        return None
    soup = BeautifulSoup(response.text, 'html.parser')
    search_results = soup.select('ul.mul li a')
    if not search_results:
        print(f"can't find {song_name}")
        return None
    return search_results

def choose_search_result(search_results, limit=15, by_self=False):
    limited_results = search_results[:limit]
    for index, result in enumerate(limited_results, 1):
        print(f"{index}. {result.text.strip()}")
    if by_self:
        choice = int(input("your choice: ")) - 1
        if 0 <= choice < len(limited_results):
            return limited_results[choice]['href']
        else:
            print("invalid choice!")
            return None
    else:
        return limited_results[0]['href']

def get_lyrics(lyrics_url):
    response = requests.get(lyrics_url)
    if response.status_code != 200:
        print(f"request failed, error code: {response.status_code}")
        return None
    soup = BeautifulSoup(response.text, 'html.parser')
    lyrics = soup.select_one('textarea.layui-textarea')
    return lyrics.text if lyrics else None

def save_lyrics(lyrics, filename, save_directory="./lyrics"):
    if not os.path.exists(save_directory):
        os.makedirs(save_directory)
    file_path = os.path.join(save_directory, filename)
    with open(file_path, 'w', encoding='utf-8') as file:
        file.write(lyrics)
    print(f"lyrics saved to {file_path}")

def get_songs_from_file(file_path):
    songs = []
    with open(file_path, 'r', encoding='utf-8') as file:
        for line in file:
            line = line.strip()
            if ' - ' in line:
                song_name = line.split(' - ', 1)[0]
            else:
                song_name = line
            songs.append((song_name, line))
    return songs

def log_error(file, message):
    with open(file, 'a', encoding='utf-8') as file:
        file.write(f"{message}\n")

if __name__ == "__main__":
    songs_list = "songs_list.txt"
    lyrics_dir = "./lyrics"
    is_self = False
    res_limit = 15
    songs = get_songs_from_file(songs_list)
    total_songs = len(songs)
    index = 1
    error_log = "error.txt"
    for song_name, original_filename in songs:
        os.system('clear')
        print(f"\n({index}/{total_songs}) searching: {song_name}")
        index += 1
        search_results = search_song(song_name)
        is_success = True
        error_msg = song_name
        if search_results:
            selected_result = choose_search_result(search_results, res_limit, is_self)
            if selected_result:
                lyrics_url = f'https://www.sq0527.cn{selected_result}'
                lyrics = get_lyrics(lyrics_url)
                if lyrics:
                    save_lyrics(lyrics, f"{original_filename}.lrc", lyrics_dir)
                else:
                    print("lyrics not found")
                    is_success = False
                    error_msg += "|lyrics not found"
            else:
                print("selected result not found")
                is_success = False
                error_msg += "|selected result not found"
        else:
            print("search results not found")
            is_success = False
            error_msg += "|search results not found"
        if not is_success:
            log_error(error_log, error_msg)
