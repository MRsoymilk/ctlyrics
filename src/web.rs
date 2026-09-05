use crate::i18n::{Locale, detect_browser_locale, format as tr_format, tr};
use crate::mapping::{
    MappingError, MappingStore, SongFile, SongMapping, generate_id, get_mapping_path,
    list_lrc_files, read_lrc_content, scan_music_dir,
};
use askama::Template;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Form, Multipart, Path, Query, State},
    http::HeaderMap,
    http::StatusCode,
    http::header::{ACCEPT_LANGUAGE, COOKIE, SET_COOKIE},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::sync::{Arc, Mutex};
use tower_http::services::ServeDir;

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    locale: &'static str,
    text: WebText,
    client_text_json: String,
    auto_selected: bool,
    en_selected: bool,
    zh_cn_selected: bool,
    music_dir: String,
    has_music_dir: bool,
    songs: Vec<SongRow>,
    search: String,
    lrc_files: Vec<String>,
}

struct WebText {
    page_title: &'static str,
    loading: &'static str,
    drop_overlay: &'static str,
    music_directory: &'static str,
    edit: &'static str,
    not_configured: &'static str,
    configure: &'static str,
    search_placeholder: &'static str,
    song_count: &'static str,
    no_audio_files: &'static str,
    configure_first: &'static str,
    song: &'static str,
    file_path: &'static str,
    lyric_file: &'static str,
    lyric_preview: &'static str,
    actions: &'static str,
    mapped: &'static str,
    unmapped: &'static str,
    no_lyrics: &'static str,
    confirm_unmap: &'static str,
    remove_mapping: &'static str,
    edit_mapping: &'static str,
    add_mapping: &'static str,
    title: &'static str,
    artist: &'static str,
    select_lyric: &'static str,
    drop_lrc: &'static str,
    click_multiple: &'static str,
    cancel: &'static str,
    save_mapping: &'static str,
    settings: &'static str,
    music_directory_path: &'static str,
    supported_formats: &'static str,
    save_settings: &'static str,
    language: &'static str,
    language_auto_label: &'static str,
    language_english: &'static str,
    language_chinese: &'static str,
}

impl WebText {
    fn new(locale: Locale) -> Self {
        Self {
            page_title: tr(locale, "page_title"),
            loading: tr(locale, "loading"),
            drop_overlay: tr(locale, "drop_overlay"),
            music_directory: tr(locale, "music_directory"),
            edit: tr(locale, "edit"),
            not_configured: tr(locale, "not_configured"),
            configure: tr(locale, "configure"),
            search_placeholder: tr(locale, "search_placeholder"),
            song_count: tr(locale, "song_count"),
            no_audio_files: tr(locale, "no_audio_files"),
            configure_first: tr(locale, "configure_first"),
            song: tr(locale, "song"),
            file_path: tr(locale, "file_path"),
            lyric_file: tr(locale, "lyric_file"),
            lyric_preview: tr(locale, "lyric_preview"),
            actions: tr(locale, "actions"),
            mapped: tr(locale, "mapped"),
            unmapped: tr(locale, "unmapped"),
            no_lyrics: tr(locale, "no_lyrics"),
            confirm_unmap: tr(locale, "confirm_unmap"),
            remove_mapping: tr(locale, "remove_mapping"),
            edit_mapping: tr(locale, "edit_mapping"),
            add_mapping: tr(locale, "add_mapping"),
            title: tr(locale, "title"),
            artist: tr(locale, "artist"),
            select_lyric: tr(locale, "select_lyric"),
            drop_lrc: tr(locale, "drop_lrc"),
            click_multiple: tr(locale, "click_multiple"),
            cancel: tr(locale, "cancel"),
            save_mapping: tr(locale, "save_mapping"),
            settings: tr(locale, "settings"),
            music_directory_path: tr(locale, "music_directory_path"),
            supported_formats: tr(locale, "supported_formats"),
            save_settings: tr(locale, "save_settings"),
            language: tr(locale, "language"),
            language_auto_label: tr(locale, "language_auto_label"),
            language_english: tr(locale, "language_english"),
            language_chinese: tr(locale, "language_chinese"),
        }
    }
}

#[derive(Serialize)]
struct ClientText {
    upload_lrc_only: &'static str,
    uploaded_count: &'static str,
    upload_failed: &'static str,
    edit_mapping: &'static str,
    add_mapping: &'static str,
    settings_save_failed: &'static str,
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

#[derive(Deserialize)]
struct LanguageForm {
    language: String,
}

#[derive(Serialize)]
struct UploadResponse {
    files: Vec<String>,
}

type SharedStore = Arc<Mutex<MappingStore>>;

pub fn create_router() -> Router {
    let store = Arc::new(Mutex::new(
        MappingStore::load(&get_mapping_path()).unwrap_or_default(),
    ));

    Router::new()
        .route("/", get(index))
        .route("/settings", post(update_settings))
        .route("/language", post(update_language))
        .route("/map", post(create_mapping))
        .route("/unmap", post(remove_mapping))
        .route("/api/lrc/upload", post(upload_lrc))
        .route("/api/lrc/:filename", get(get_lrc_content))
        .nest_service("/static", ServeDir::new("static"))
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024))
        .with_state(store)
}

async fn index(
    State(store): State<SharedStore>,
    Query(query): Query<SearchQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let (locale, preference) = web_locale(&headers);
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
                let lrc_filename = mapping
                    .as_ref()
                    .map(|m| m.lrc_filename.clone())
                    .unwrap_or_default();
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
        locale: locale.code(),
        text: WebText::new(locale),
        client_text_json: serde_json::to_string(&ClientText {
            upload_lrc_only: tr(locale, "upload_lrc_only"),
            uploaded_count: tr(locale, "uploaded_count"),
            upload_failed: tr(locale, "upload_failed"),
            edit_mapping: tr(locale, "edit_mapping"),
            add_mapping: tr(locale, "add_mapping"),
            settings_save_failed: tr(locale, "settings_save_failed"),
        })
        .unwrap(),
        auto_selected: preference == "auto",
        en_selected: preference == "en",
        zh_cn_selected: preference == "zh-CN",
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
    headers: HeaderMap,
    Form(form): Form<SettingsForm>,
) -> Result<Response, AppError> {
    let (locale, _) = web_locale(&headers);
    let mut store_guard = store.lock().unwrap();
    let path = std::path::Path::new(&form.music_dir);
    if !path.exists() || !path.is_dir() {
        let msg = format!(
            r#"<div class="message error">{}</div>"#,
            tr(locale, "directory_not_found")
        );
        return Ok((StatusCode::BAD_REQUEST, Html(msg)).into_response());
    }
    store_guard.set_music_dir(form.music_dir.clone());
    store_guard.save(&get_mapping_path())?;
    let msg = format!(
        r#"<div class="message success">{}</div>"#,
        tr(locale, "settings_saved")
    );
    Ok(Html(msg).into_response())
}

async fn update_language(Form(form): Form<LanguageForm>) -> Response {
    let preference = match form.language.as_str() {
        "en" => "en",
        "zh-CN" => "zh-CN",
        _ => "auto",
    };
    let mut response = Redirect::to("/").into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        format!("ctlyrics_language={preference}; Path=/; SameSite=Lax; Max-Age=31536000")
            .parse()
            .unwrap(),
    );
    response
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

async fn upload_lrc(headers: HeaderMap, mut multipart: Multipart) -> Result<Response, AppError> {
    let (locale, _) = web_locale(&headers);
    let mut uploaded = Vec::new();
    fs::create_dir_all("lyrics").map_err(MappingError::from)?;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        let Some(filename) = field.file_name() else {
            continue;
        };
        let filename = std::path::Path::new(filename)
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| AppError::BadRequest(tr(locale, "invalid_filename").to_string()))?
            .to_string();

        let is_lrc = std::path::Path::new(&filename)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("lrc"));
        if !is_lrc {
            return Err(AppError::BadRequest(tr_format(
                locale,
                "lrc_only",
                &[("filename", &filename)],
            )));
        }

        let content = field
            .bytes()
            .await
            .map_err(|e| AppError::BadRequest(e.to_string()))?;
        fs::write(std::path::Path::new("lyrics").join(&filename), content)
            .map_err(MappingError::from)?;
        uploaded.push(filename);
    }

    if uploaded.is_empty() {
        return Err(AppError::BadRequest(
            tr(locale, "no_upload_file").to_string(),
        ));
    }

    Ok(Json(UploadResponse { files: uploaded }).into_response())
}

fn web_locale(headers: &HeaderMap) -> (Locale, &'static str) {
    let cookie_preference = headers
        .get(COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                let (name, value) = cookie.trim().split_once('=')?;
                (name == "ctlyrics_language").then_some(value)
            })
        });

    if let Some(locale) = cookie_preference.and_then(Locale::from_code) {
        return (locale, locale.code());
    }

    let accept_language = headers
        .get(ACCEPT_LANGUAGE)
        .and_then(|value| value.to_str().ok());
    (detect_browser_locale(accept_language), "auto")
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
    #[error("Mapping error: {0}")]
    Mapping(#[from] MappingError),
    #[error("Bad request: {0}")]
    BadRequest(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            AppError::Mapping(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            AppError::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
        };
        (status, msg).into_response()
    }
}
