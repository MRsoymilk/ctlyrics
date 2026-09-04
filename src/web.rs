use crate::mapping::{MappingError, MappingStore, SongFile, SongMapping, generate_id, get_mapping_path, list_lrc_files, read_lrc_content, scan_music_dir};
use askama::Template;
use axum::{
    extract::{Form, Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tower_http::services::ServeDir;

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    music_dir: String,
    has_music_dir: bool,
    songs: Vec<SongRow>,
    search: String,
    lrc_files: Vec<String>,
}



#[derive(Debug, Clone, Serialize)]
struct SongRow {
    song: SongFile,
    mapping: Option<SongMapping>,
    lrc_preview: String,
    lrc_filename: String,
    has_lrc_filename: bool,
    has_lrc_preview: bool,
}

#[derive(Deserialize)]
struct SearchQuery {
    q: Option<String>,
}

#[derive(Deserialize)]
struct SettingsForm {
    music_dir: String,
}

#[derive(Deserialize)]
struct MappingForm {
    song_path: String,
    title: String,
    artist: String,
    lrc_filename: String,
}

#[derive(Deserialize)]
struct UnmapForm {
    song_path: String,
}

type SharedStore = Arc<Mutex<MappingStore>>;

pub fn create_router() -> Router {
    let store = Arc::new(Mutex::new(
        MappingStore::load(&get_mapping_path()).unwrap_or_default(),
    ));

    Router::new()
        .route("/", get(index))
        .route("/settings", post(update_settings))
        .route("/map", post(create_mapping))
        .route("/unmap", post(remove_mapping))
        .route("/api/lrc/:filename", get(get_lrc_content))
        .nest_service("/static", ServeDir::new("static"))
        .with_state(store)
}

async fn index(
    State(store): State<SharedStore>,
    Query(query): Query<SearchQuery>,
) -> impl IntoResponse {
    let store_guard = store.lock().unwrap();
    let music_dir_opt = store_guard.music_dir().map(|s| s.to_string());
    let music_dir_str = music_dir_opt.clone().unwrap_or_default();
    let has_music_dir = music_dir_opt.is_some();
    let search = query.q.unwrap_or_default().to_lowercase();

    let (songs, lrc_files) = if let Some(ref dir) = music_dir_opt {
        let songs = scan_music_dir(dir).unwrap_or_default();
        let lrc_files = list_lrc_files().unwrap_or_default();

        let rows: Vec<SongRow> = songs
            .into_iter()
            .filter(|s| {
                search.is_empty()
                    || s.title.to_lowercase().contains(&search)
                    || s.artist.to_lowercase().contains(&search)
                    || s.path.to_lowercase().contains(&search)
            })
            .map(|song| {
                let mapping = store_guard.get_by_music_path(&song.path).cloned();
                let lrc_filename = mapping.as_ref().map(|m| m.lrc_filename.clone()).unwrap_or_default();
                let lrc_preview = mapping
                    .as_ref()
                    .and_then(|m| read_lrc_content(&m.lrc_filename).ok())
                    .map(|content| {
                        let lines: Vec<&str> = content.lines().take(5).collect();
                        lines.join("\n")
                    })
                    .unwrap_or_default();
                let has_lrc_filename = !lrc_filename.is_empty();
                let has_lrc_preview = !lrc_preview.is_empty();
                SongRow {
                    song,
                    mapping,
                    lrc_preview,
                    lrc_filename,
                    has_lrc_filename,
                    has_lrc_preview,
                }
            })
            .collect();
        (rows, lrc_files)
    } else {
        (Vec::new(), Vec::new())
    };

    let template = IndexTemplate {
        music_dir: music_dir_str,
        has_music_dir,
        songs,
        search,
        lrc_files,
    };
    Html(template.render().unwrap())
}

async fn update_settings(
    State(store): State<SharedStore>,
    Form(form): Form<SettingsForm>,
) -> Result<impl IntoResponse, AppError> {
    let mut store_guard = store.lock().unwrap();
    let path = std::path::Path::new(&form.music_dir);
    if !path.exists() || !path.is_dir() {
        let msg = r#"<div class="message error">目录不存在</div>"#;
        return Ok(Html(msg));
    }
    store_guard.set_music_dir(form.music_dir.clone());
    store_guard.save(&get_mapping_path())?;
    let msg = r#"<div class="message success">保存成功</div>"#;
    Ok(Html(msg))
}

async fn create_mapping(
    State(store): State<SharedStore>,
    Form(form): Form<MappingForm>,
) -> Result<Redirect, AppError> {
    let mut store_guard = store.lock().unwrap();
    let mapping = SongMapping {
        id: generate_id(),
        title: form.title,
        artist: form.artist,
        music_path: form.song_path,
        lrc_filename: form.lrc_filename,
    };
    store_guard.insert(mapping);
    store_guard.save(&get_mapping_path())?;
    Ok(Redirect::to("/"))
}

async fn remove_mapping(
    State(store): State<SharedStore>,
    Form(form): Form<UnmapForm>,
) -> Result<Redirect, AppError> {
    let mut store_guard = store.lock().unwrap();
    if let Some(mapping) = store_guard.get_by_music_path(&form.song_path) {
        let id = mapping.id.clone();
        store_guard.remove(&id);
        store_guard.save(&get_mapping_path())?;
    }
    Ok(Redirect::to("/"))
}

async fn get_lrc_content(
    State(_store): State<SharedStore>,
    Path(filename): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let content = read_lrc_content(&filename)?;
    Ok(Html(content))
}

#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error("Not found")]
    NotFound,
    #[error("Mapping error: {0}")]
    Mapping(#[from] MappingError),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "Not found".to_string()),
            AppError::Mapping(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
        (status, msg).into_response()
    }
}