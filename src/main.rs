//! Simplified Anki server for Kindle e-readers
//! Provides a minimal web interface for card review

use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::sync::Mutex;
use std::sync::Arc;

use anki::collection::CollectionBuilder;
use anki::prelude::*;
use anki::scheduler::answering::{CardAnswer, Rating};
use reqwest::Client;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::routing::post;
use axum::Json;
use axum::Router;
use serde::Deserialize;
use serde::Serialize;
use tracing::info;

struct AppState {
    collection: Arc<Mutex<Collection>>,
}

#[derive(Serialize)]
struct SimpleCard {
    id: i64,
    front: String,
    back: String,
}

#[derive(Serialize)]
struct DeckInfo {
    id: i64,
    name: String,
    due_count: u32,
    new_count: u32,
    review_count: u32,
}

#[derive(Deserialize)]
struct AnswerRequest {
    card_id: i64,
    ease: u8, // 1=Again, 2=Hard, 3=Good, 4=Easy
}

async fn get_decks(State(state): State<Arc<AppState>>) -> Result<Json<Vec<DeckInfo>>, StatusCode> {
    let state = Arc::clone(&state);
    let result = tokio::task::spawn_blocking(move || {
        let mut col = state.collection.blocking_lock();

        let deck_names = col.get_all_deck_names(false).map_err(|e| {
            info!("Error getting decks: {:?}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let tree = col.deck_tree(Some(TimestampSecs::now())).map_err(|e| {
            info!("Error getting deck tree: {:?}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let mut decks = Vec::new();
        for (id, name) in deck_names {
            let node = tree.children.iter().find(|n| n.deck_id == id.0);
            let (due, new, review) = if let Some(n) = node {
                (n.learn_count + n.review_count, n.new_count, n.review_count)
            } else {
                (0, 0, 0)
            };

            decks.push(DeckInfo {
                id: id.0,
                name,
                due_count: due,
                new_count: new,
                review_count: review,
            });
        }

        Ok::<_, StatusCode>(Json(decks))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    result
}

async fn get_decks_html(State(state): State<Arc<AppState>>) -> Result<Html<String>, StatusCode> {
    let state = Arc::clone(&state);
    let result = tokio::task::spawn_blocking(move || {
        let mut col = state.collection.blocking_lock();

        let deck_names = col.get_all_deck_names(false).map_err(|e| {
            info!("Error getting decks: {:?}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let tree = col.deck_tree(Some(TimestampSecs::now())).map_err(|e| {
            info!("Error getting deck tree: {:?}", e);
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
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    result
}

async fn select_deck(
    State(state): State<Arc<AppState>>,
    Path(deck_id): Path<i64>,
) -> Result<Html<String>, StatusCode> {
    let state = Arc::clone(&state);
    let result = tokio::task::spawn_blocking(move || {
        let mut col = state.collection.blocking_lock();

        col.set_current_deck(DeckId(deck_id))
            .map_err(|e| {
                info!("Error selecting deck: {:?}", e);
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

        match col.get_next_card() {
            Ok(Some(queued)) => {
                let card = &queued.card;
                let card_id = card.id();

                let rendered = col
                    .render_existing_card(card_id, false, false)
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

                let html = format!(
                    "<div class=\"container\">\
                        <h2>Review</h2>\
                        <div class=\"card-content\">{}</div>\
                        <div class=\"button-row\">\
                            <button hx-get=\"/api/show-answer/{}\" hx-target=\"#card-area\" hx-swap=\"innerHTML\">Show Answer</button>\
                        </div>\
                    </div>",
                    rendered.question(),
                    card_id.0
                );
                Ok(Html(html))
            }
            Ok(None) => {
                let html = "<div class=\"container\">\
                    <h2>All Done!</h2>\
                    <div class=\"card-content\">\
                        <h2>No cards due!</h2>\
                        <p>You're all caught up! Check back later.</p>\
                    </div>\
                </div>".to_string();
                Ok(Html(html))
            }
            Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
        }
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    result
}

async fn sync_with_ankiweb(State(state): State<Arc<AppState>>) -> Result<Html<String>, StatusCode> {
    let username = std::env::var("ANKI_USERNAME").map_err(|_| {
        info!("ANKI_USERNAME not set");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let password = std::env::var("ANKI_PASSWORD").map_err(|_| {
        info!("ANKI_PASSWORD not set");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| {
            info!("Failed to create HTTP client: {:?}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let auth = match anki::sync::login::sync_login(&username, &password, None, client.clone()).await {
        Ok(auth) => auth,
        Err(e) => {
            info!("Login failed: {:?}", e);
            return Err(StatusCode::UNAUTHORIZED);
        }
    };

    let mut col = state.collection.lock().await;

    match col.normal_sync(auth, client).await {
        Ok(_) => {
            info!("Sync completed successfully");
            Ok(Html("<div class='info'>Sync completed successfully</div>".to_string()))
        }
        Err(e) => {
            let err_msg = format!("{:?}", e);
            info!("Sync failed: {}", err_msg);
            Ok(Html(format!("<div class='error'>Sync failed: {}</div>", err_msg)))
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging - explicitly to stdout
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();

    // Get collection path
    let collection_path = std::env::var("ANKI_COLLECTION_PATH").expect("ANKI_COLLECTION_PATH not set");

    info!("Opening collection: {}", collection_path);

    // Open collection
    let col = CollectionBuilder::new(PathBuf::from(&collection_path)).build()?;

    let state = Arc::new(AppState {
        collection: Arc::new(Mutex::new(col)),
    });

    // Build router
    let app = Router::new()
        .route("/", get(serve_html))
        .route("/htmx.min.js", get(serve_htmx))
        .route("/api/next", get(get_next))
        .route("/api/answer", post(answer))
        .route("/api/card", get(load_card_html))
        .route("/api/show-answer/{card_id}", get(show_answer_html))
        .route("/api/answer/{card_id}/{ease}", post(answer_and_next))
        .route("/api/decks", get(get_decks))
        .route("/api/decks/html", get(get_decks_html))
        .route("/api/deck/{deck_id}", post(select_deck))
        .route("/api/sync", post(sync_with_ankiweb))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    println!("\n=====================================");
    println!("Anki Kindle Server");
    println!("=====================================");
    println!("Open in browser: http://localhost:8080");
    println!("=====================================\n");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn serve_html() -> impl IntoResponse {
    Html(include_str!("../web/index.html"))
}

async fn serve_htmx() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/javascript")],
        include_str!("../web/htmx.min.js"),
    )
}

async fn get_next(State(state): State<Arc<AppState>>) -> Result<Json<SimpleCard>, StatusCode> {
    let mut col = state.collection.lock().await;

    match col.get_next_card() {
        Ok(Some(queued)) => {
            // Render card with templates
            let card = &queued.card;
            let card_id = card.id();

            let rendered = col
                .render_existing_card(card_id, false, false)
                .map_err(|e| {
                    info!("Error rendering card: {:?}", e);
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;

            Ok(Json(SimpleCard {
                id: card_id.0,
                front: rendered.question().to_string(),
                back: rendered.answer().to_string(),
            }))
        }
        Ok(None) => {
            // No cards
            Ok(Json(SimpleCard {
                id: 0,
                front: "No cards due!".to_string(),
                back: "You're all done!".to_string(),
            }))
        }
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn answer(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AnswerRequest>,
) -> Result<StatusCode, StatusCode> {
    let mut col = state.collection.lock().await;

    let card_id = CardId(req.card_id);

    info!("Received answer request: card_id={}, ease={}", req.card_id, req.ease);

    // Map ease to rating
    let rating = match req.ease {
        1 => Rating::Again,
        2 => Rating::Hard,
        3 => Rating::Good,
        4 => Rating::Easy,
        _ => {
            info!("Invalid ease: {}", req.ease);
            return Err(StatusCode::BAD_REQUEST);
        }
    };

    // Get scheduling states
    let states = col
        .get_scheduling_states(card_id)
        .map_err(|e| {
            info!("Error getting scheduling states: {:?}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Determine new state based on rating
    let new_state = match rating {
        Rating::Again => states.again,
        Rating::Hard => states.hard,
        Rating::Good => states.good,
        Rating::Easy => states.easy,
    };

    // Answer the card
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

    col.answer_card(&mut answer)
        .map_err(|e| {
            info!("Error answering card: {:?}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    info!("Card {} answered successfully", req.card_id);

    Ok(StatusCode::OK)
}

async fn load_card_html(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut col = state.collection.lock().await;

    match col.get_next_card() {
        Ok(Some(queued)) => {
            let card = &queued.card;
            let card_id = card.id();

            let rendered = col
                .render_existing_card(card_id, false, false)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

            let html = format!(
                r##"<div class="container">
                    <h2>Review</h2>
                    <div class="card-content">{}</div>
                    <div class="button-row">
                        <button hx-get="/api/show-answer/{}" hx-target="#card-area" hx-swap="innerHTML">Show Answer</button>
                    </div>
                </div>"##,
                rendered.question(),
                card_id.0
            );
            Ok(Html(html))
        }
        Ok(None) => {
            let html = r#"<div class="container">
                <h2>All Done!</h2>
                <div class="card-content">
                    <h2>No cards due!</h2>
                    <p>You're all caught up! Check back later.</p>
                </div>
            </div>"#;
            Ok(Html(html.to_string()))
        }
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
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
        r##"<div class="container">
            <h2>Review</h2>
            <div class="card-content">
                {}
                <hr style="margin: 20px 0; border: 2px solid black;">
                {}
            </div>
            <div class="answer-buttons">
                <button hx-post="/api/answer/{}/1" hx-target="#card-area" hx-swap="innerHTML">Again</button>
                <button hx-post="/api/answer/{}/2" hx-target="#card-area" hx-swap="innerHTML">Hard</button>
                <button hx-post="/api/answer/{}/3" hx-target="#card-area" hx-swap="innerHTML">Good</button>
                <button hx-post="/api/answer/{}/4" hx-target="#card-area" hx-swap="innerHTML">Easy</button>
            </div>
        </div>"##,
        rendered.question(),
        rendered.answer(),
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
) -> Result<Html<String>, StatusCode> {
    let mut col = state.collection.lock().await;
    let card_id = CardId(card_id);

    info!("Received answer request: card_id={}, ease={}", card_id.0, ease);

    // Map ease to rating
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

    // Get scheduling states
    let states = col
        .get_scheduling_states(card_id)
        .map_err(|e| {
            info!("Error getting scheduling states: {:?}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Determine new state based on rating
    let new_state = match rating {
        Rating::Again => states.again,
        Rating::Hard => states.hard,
        Rating::Good => states.good,
        Rating::Easy => states.easy,
    };

    // Answer the card
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

    col.answer_card(&mut answer)
        .map_err(|e| {
            info!("Error answering card: {:?}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    info!("Card {} answered successfully", card_id.0);

    // Load next card - get the next card directly here
    drop(col);
    let mut col = state.collection.lock().await;

    match col.get_next_card() {
        Ok(Some(queued)) => {
            let card = &queued.card;
            let card_id = card.id();

            let rendered = col
                .render_existing_card(card_id, false, false)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

            let html = format!(
                r##"<div class="container">
                    <h2>Review</h2>
                    <div class="card-content">{}</div>
                    <div class="button-row">
                        <button hx-get="/api/show-answer/{}" hx-target="#card-area" hx-swap="innerHTML">Show Answer</button>
                    </div>
                </div>"##,
                rendered.question(),
                card_id.0
            );
            Ok(Html(html))
        }
        Ok(None) => {
            let html = r#"<div class="container">
                <h2>All Done!</h2>
                <div class="card-content">
                    <h2>No cards due!</h2>
                    <p>You're all caught up! Check back later.</p>
                </div>
            </div>"#.to_string();
            Ok(Html(html))
        }
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}
