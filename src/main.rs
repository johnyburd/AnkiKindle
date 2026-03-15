//! Simplified Anki server for Kindle e-readers
//! Provides a minimal web interface for card review

use reqwest::header::CONTENT_TYPE;
use std::env;
use std::error::Error;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::fs;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use anki::collection::CollectionBuilder;
use anki::prelude::*;
use anki::scheduler::answering::{CardAnswer, Rating};
use anki_proto::scheduler::custom_study_request::Value as CustomStudyValue;
use anki_proto::scheduler::CustomStudyRequest;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::routing::post;
use axum::Router;
use reqwest::Client;
use tracing::{debug, info};

use anki::sync::login::SyncAuth;
use std::sync::atomic::{AtomicBool, Ordering};

struct AppState {
    collection: Arc<Mutex<Collection>>,
    media_sync_running: Arc<AtomicBool>,
    cached_auth: Arc<Mutex<Option<SyncAuth>>>,
    media_folder: PathBuf,
}

impl AppState {
    pub async fn get_next_card_html(&self) -> Result<impl IntoResponse, StatusCode> {
        let mut col = self.collection.lock().await;

        match col
            .get_next_card()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        {
            Some(queued) => {
                let card = &queued.card;
                let card_id = card.id();

                let rendered = col
                    .render_existing_card(card_id, false, false)
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

                let html = format!(
                    r##"
                    <div class="card-content">{}</div>
                    <div class="button-row">
                        <button hx-get="/api/show-answer/{}" hx-target="#card-area" hx-swap="innerHTML">Show Answer</button>
                    </div>"##,
                    process_card_html(&rendered.question()),
                    card_id.0
                );
                Ok(Html(html))
            }
            None => Ok(Html(all_done_html())),
        }
    }

    pub async fn set_current_deck(&self, deck_id: i64) -> Result<(), StatusCode> {
        let mut col = self.collection.lock().await;

        col.set_current_deck(DeckId(deck_id)).map_err(|e| {
            info!("Error selecting deck: {e:?}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        Ok(())
    }
}

async fn get_decks_html(State(state): State<Arc<AppState>>) -> Result<Html<String>, StatusCode> {
    let mut col = state.collection.lock().await;
    let deck_names = col.get_all_deck_names(false).map_err(|e| {
        info!("Error getting decks: {e:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let tree = col.deck_tree(Some(TimestampSecs::now())).map_err(|e| {
        info!("Error getting deck tree: {e:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let mut decks: Vec<(i64, String, u32, u32, u32)> = deck_names
        .into_iter()
        .map(|(id, name)| {
            let node = tree.children.iter().find(|n| n.deck_id == id.0);
            let (due, new, review) = if let Some(n) = node {
                (n.learn_count + n.review_count, n.new_count, n.review_count)
            } else {
                (0, 0, 0)
            };
            (id.0, name, due, new, review)
        })
        .collect();

    decks.sort_by(|a, b| {
        let a_total = a.2 + a.3 + a.4;
        let b_total = b.2 + b.3 + b.4;
        b_total.cmp(&a_total).then(a.1.cmp(&b.1))
    });

    let mut html = String::new();
    for (id, name, due, new, review) in decks {
        let deck_html = format!(
                "<div class=\"deck-item\" hx-post=\"/api/deck/{}\" hx-target=\"#card-area\" hx-swap=\"innerHTML\" hx-push-url=\"false\">\
                    <div class=\"deck-name\">{}</div>\
                    <div class=\"deck-counts\">{} new, {} due, {} review</div>\
                </div>",
                id, name, new, due, review
            );
        html.push_str(&deck_html);
    }

    if html.is_empty() {
        html = "<div class='info'>No decks found!</div>".to_string();
    }

    Ok::<_, StatusCode>(Html(html))
}

async fn select_deck(
    State(state): State<Arc<AppState>>,
    Path(deck_id): Path<i64>,
) -> Result<impl IntoResponse, StatusCode> {
    let state = Arc::clone(&state);
    state.set_current_deck(deck_id).await?;
    state.get_next_card_html().await
}

fn all_done_html() -> String {
    r##"<div class="container">
        <h2>All Done!</h2>
        <div class="card-content">
            <h2>No cards due!</h2>
            <p>You're all caught up! Check back later.</p>
        </div>
        <div class="button-row">
            <button hx-post="/api/custom-study/new/5" hx-target="#card-area" hx-swap="innerHTML">Study 5 Extra Cards</button>
        </div>
    </div>"##.to_string()
}

fn process_card_html(html: &str) -> String {
    let type_re = regex::Regex::new(r"\[\[type:[^\]]+\]\]").unwrap();
    let html = type_re.replace_all(html, "");

    let sound_re = regex::Regex::new(r"\[sound:[^\]]+\]").unwrap();
    sound_re
        .replace_all(&html, "<span class='no-audio'>🔇</span>")
        .into_owned()
}

async fn custom_study_new(
    State(state): State<Arc<AppState>>,
    Path(count): Path<i32>,
) -> Result<impl IntoResponse, StatusCode> {
    let state = Arc::clone(&state);

    let mut col: tokio::sync::MutexGuard<'_, Collection> = state.collection.lock().await;
    let deck = col.get_current_deck().map_err(|e| {
        info!("Error getting current deck: {e:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let deck_id = deck.id;

    col.custom_study(CustomStudyRequest {
        deck_id: deck_id.0,
        value: Some(CustomStudyValue::NewLimitDelta(count)),
    })
    .map_err(|e| {
        info!("Error extending new limit: {e:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    state.get_next_card_html().await
}

async fn sync_with_ankiweb(State(state): State<Arc<AppState>>) -> Result<Html<String>, StatusCode> {
    let username = env::var("ANKI_USERNAME").map_err(|_| {
        info!("ANKI_USERNAME not set");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let password = env::var("ANKI_PASSWORD").map_err(|_| {
        info!("ANKI_PASSWORD not set");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let client = Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| {
            info!("Failed to create HTTP client: {e:?}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let auth = {
        let mut cached = state.cached_auth.lock().await;
        if let Some(auth) = cached.clone() {
            debug!("Reusing cached auth token");
            auth
        } else {
            info!("Logging in to AnkiWeb");
            match anki::sync::login::sync_login(&username, &password, None, client.clone()).await {
                Ok(auth) => {
                    *cached = Some(auth.clone());
                    auth
                }
                Err(e) => {
                    info!("Login failed: {e:?}");
                    return Err(StatusCode::UNAUTHORIZED);
                }
            }
        }
    };

    info!("Collection sync started");
    let mut col = state.collection.lock().await;

    let sync_result = col.normal_sync(auth.clone(), client.clone()).await;

    match sync_result {
        Err(e) => {
            let err_msg = format!("{e:?}");
            info!("Collection sync failed: {}", err_msg);
            *state.cached_auth.lock().await = None;
            return Ok(Html(format!(
                "<div class='error'>Sync failed: {}</div>",
                err_msg
            )));
        }
        Ok(_) => {
            info!("Collection sync completed successfully");
        }
    }

    let progress = col.new_progress_handler();
    let media_manager = col.media().map_err(|e| {
        info!("Failed to create media mgr: {e:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    drop(col);

    if state
        .media_sync_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        let state_clone = Arc::clone(&state);
        let client_clone = client.clone();
        let auth_clone = auth.clone();

        tokio::spawn(async move {
            info!("Media sync started in background");

            let result = media_manager
                .sync_media(progress, auth_clone, client_clone, None)
                .await;
            info!("Media sync result {result:?}");

            state_clone
                .media_sync_running
                .store(false, Ordering::SeqCst);
        });

        Ok(Html(
            "<div class='info'>Collection sync complete. Media sync running in background.</div>"
                .to_string(),
        ))
    } else {
        info!("Media sync already running, skipping");
        Ok(Html("<div class='info'>Collection sync complete. Media sync already in progress, skipped.</div>".to_string()))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();
    let bind_addr: SocketAddr = env::var("BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
        .parse()
        .expect("BIND_ADDR not valid");

    let collection_path = env::var("ANKI_COLLECTION_PATH").expect("ANKI_COLLECTION_PATH not set");
    let collection_path = PathBuf::from(&collection_path);

    let collection_dir = collection_path
        .parent()
        .expect("ANKI_COLLECTION_PATH has no parent directory");
    let media_folder = collection_dir.join("collection.media");
    let media_db = collection_dir.join("collection.media.db2");

    info!("Opening collection: {collection_path:?} {media_folder:?} {media_db:?}");

    let col = CollectionBuilder::new(collection_path)
        .set_media_paths(&media_folder, &media_db)
        .build()?;
    let state = Arc::new(AppState {
        collection: Arc::new(Mutex::new(col)),
        media_sync_running: Arc::new(AtomicBool::new(false)),
        cached_auth: Arc::new(Mutex::new(None)),
        media_folder,
    });

    let app = Router::new()
        .route("/", get(serve_html))
        .route("/htmx.min.js", get(serve_htmx))
        .route("/jquery.min.js", get(serve_jquery))
        .route("/api/card", get(load_card_html))
        .route("/api/show-answer/{card_id}", get(show_answer_html))
        .route("/api/custom-study/new/{count}", post(custom_study_new))
        .route("/api/answer/{card_id}/{ease}", post(answer_and_next))
        .route("/api/decks/html", get(get_decks_html))
        .route("/api/deck/{deck_id}", post(select_deck))
        .route("/api/sync", post(sync_with_ankiweb))
        .route("/{filename}", get(serve_media))
        .with_state(state);

    info!("Listening on: http://{bind_addr}");

    let listener = TcpListener::bind(bind_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn serve_media(
    State(state): State<Arc<AppState>>,
    Path(filename): Path<String>,
) -> impl IntoResponse {
    let path = state.media_folder.join(&filename);

    match fs::read(&path).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(&path)
                .first_or_octet_stream()
                .to_string();
            ([(CONTENT_TYPE, mime)], bytes).into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn serve_html() -> impl IntoResponse {
    Html(include_str!("../web/index.html"))
}

async fn serve_htmx() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "application/javascript")],
        include_str!("../web/htmx.min.js"),
    )
}

async fn serve_jquery() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "application/javascript")],
        include_str!("../web/jquery.min.js"),
    )
}

async fn load_card_html(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    state.get_next_card_html().await
}

async fn show_answer_html(
    State(state): State<Arc<AppState>>,
    Path(card_id): Path<i64>,
) -> Result<Html<String>, StatusCode> {
    let mut col = state.collection.lock().await;
    let card_id = CardId(card_id);

    let rendered = col
        .render_existing_card(card_id, false, false)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let html = format!(
        r##"
            <div class="card-content">
                {}
            </div>
            <div class="answer-buttons">
                <button hx-post="/api/answer/{}/1" hx-target="#card-area" hx-swap="innerHTML">Again</button>
                <button hx-post="/api/answer/{}/2" hx-target="#card-area" hx-swap="innerHTML">Hard</button>
                <button hx-post="/api/answer/{}/3" hx-target="#card-area" hx-swap="innerHTML">Good</button>
                <button hx-post="/api/answer/{}/4" hx-target="#card-area" hx-swap="innerHTML">Easy</button>
            </div>"##,
        process_card_html(&rendered.answer()),
        card_id.0,
        card_id.0,
        card_id.0,
        card_id.0
    );
    Ok(Html(html))
}

async fn answer_and_next(
    State(state): State<Arc<AppState>>,
    Path((card_id, ease)): Path<(i64, u8)>,
) -> Result<impl IntoResponse, StatusCode> {
    let mut col = state.collection.lock().await;
    let card_id = CardId(card_id);

    info!(
        "Received answer request: card_id={}, ease={}",
        card_id.0, ease
    );

    let rating = match ease {
        1 => Rating::Again,
        2 => Rating::Hard,
        3 => Rating::Good,
        4 => Rating::Easy,
        _ => {
            info!("Invalid ease: {}", ease);
            return Err(StatusCode::BAD_REQUEST);
        }
    };

    let states = col.get_scheduling_states(card_id).map_err(|e| {
        info!("Error getting scheduling states: {e:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let new_state = match rating {
        Rating::Again => states.again,
        Rating::Hard => states.hard,
        Rating::Good => states.good,
        Rating::Easy => states.easy,
    };

    let mut answer = CardAnswer {
        card_id,
        current_state: states.current,
        new_state,
        rating,
        answered_at: TimestampMillis::now(),
        milliseconds_taken: 0,
        custom_data: None,
        from_queue: true,
    };

    col.answer_card(&mut answer).map_err(|e| {
        info!("Error answering card: {e:?}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    info!("Card {} answered successfully", card_id.0);

    drop(col);

    state.get_next_card_html().await
}
