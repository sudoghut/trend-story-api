// Immutable Config
const DOMAIN: &str = "https://trend-story-api.oopus.info";
const SYNC_INTERVAL_MINUTES: u64 = 20; // User-configurable

use std::path::Path;
use rusqlite::{Connection, Result as SqlResult};
use serde::{Deserialize, Serialize};
use warp::Filter;

#[derive(Debug, Serialize, Deserialize)]
struct LatestResponse {
    latest_date: Option<String>,
    records: Vec<NewsRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ImageInfo {
    file_name: Option<String>,
    url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct NewsRecord {
    id: i64,
    news: Option<String>,
    date: Option<String>,
    serpapi_id: Option<i64>,
    image_id: Option<i64>,
    keywords: Option<String>,
    image: Option<ImageInfo>,
}

async fn get_latest() -> Result<impl warp::Reply, warp::Rejection> {
    match query_latest_news() {
        Ok(response) => Ok(warp::reply::json(&response)),
        Err(e) => {
            eprintln!("Database error: {}", e);
            Err(warp::reject::custom(DatabaseError))
        }
    }
}

fn query_latest_news() -> SqlResult<LatestResponse> {
    let db_path = "trends-story/trends_data.db";
    
    if !Path::new(db_path).exists() {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
            Some("Database file not found".to_string())
        ));
    }

    let conn = Connection::open(db_path)?;
    
    // Find the latest day (yyyy-mm-dd) from the date column
    let latest_day: Option<String> = conn.query_row(
        "SELECT substr(date, 1, 10) as day FROM main_news_data ORDER BY date DESC LIMIT 1",
        [],
        |row| row.get(0)
    ).ok();

    // If no day found, return empty response
    let day_filter = match &latest_day {
        Some(day) => day.clone(),
        None => return Ok(LatestResponse {
            latest_date: None,
            records: vec![],
        }),
    };

    // Query all records from the latest day
    let mut stmt = conn.prepare(
        "SELECT id, news, date, serpapi_id, image_id \
         FROM main_news_data \
         WHERE substr(date, 1, 10) = ?1 \
         ORDER BY id ASC"
    )?;

    let news_rows = stmt.query_map([&day_filter], |row| {
        Ok((
            row.get::<_, i64>(0)?,      // id
            row.get::<_, Option<String>>(1)?,  // news
            row.get::<_, Option<String>>(2)?,  // date
            row.get::<_, Option<i64>>(3)?,     // serpapi_id
            row.get::<_, Option<i64>>(4)?,     // image_id
        ))
    })?;
    
    let mut records = Vec::new();
    
    for row_result in news_rows {
        let (id, news, date, serpapi_id, image_id) = row_result?;

        // Query keywords from serpapi_data if serpapi_id exists
        let keywords = if let Some(serpapi_id) = serpapi_id {
            let mut keyword_stmt = conn.prepare(
                "SELECT query FROM serpapi_data WHERE id = ?1"
            )?;
            keyword_stmt.query_row([serpapi_id], |row| {
                let query: Option<String> = row.get(0)?;
                Ok(query)
            }).unwrap_or(None)
        } else {
            None
        };

        // Query image file_name from image_data if image_id exists
        let image = if let Some(image_id) = image_id {
            let mut image_stmt = conn.prepare(
                "SELECT file_name FROM image_data WHERE id = ?1"
            )?;
            let file_name: Option<String> = image_stmt.query_row([image_id], |row| row.get(0)).unwrap_or(None);
            let url = file_name.as_ref().map(|fname| {
                let tokens: Vec<&str> = fname.split('_').collect();
                if tokens.len() > 1 {
                    format!("{}/images/{}/{}", DOMAIN, tokens[1], fname)
                } else {
                    format!("{}/images/{}", DOMAIN, fname)
                }
            });
            Some(ImageInfo { file_name, url })
        } else {
            None
        };

        records.push(NewsRecord {
            id,
            news,
            date,
            serpapi_id,
            image_id,
            keywords,
            image,
        });
    }
    
    Ok(LatestResponse {
        latest_date: latest_day,
        records,
    })
}

#[derive(Debug)]
struct DatabaseError;

impl warp::reject::Reject for DatabaseError {}

#[tokio::main]
async fn main() {
    // Start periodic git sync task
    tokio::spawn(async move {
        use std::process::Command;
        use std::time::Duration;
        loop {
            // If repo doesn't exist, clone; else, pull
            let repo_path = "./trends-story";
            if !std::path::Path::new(repo_path).exists() {
                let _ = Command::new("git")
                    .args(["clone", "https://github.com/sudoghut/trends-story"])
                    .status();
            } else {
                let _ = Command::new("git")
                    .args(["-C", repo_path, "pull"])
                    .status();
            }
            tokio::time::sleep(Duration::from_secs(SYNC_INTERVAL_MINUTES * 60)).await;
        }
    });
    // CORS filter
    let cors = warp::cors()
        .allow_any_origin()
        .allow_headers(vec!["content-type"])
        .allow_methods(vec!["GET", "POST", "DELETE"]);

    // Routes
    let latest = warp::path("latest")
        .and(warp::get())
        .and_then(get_latest);

    // Serve images from ./trends-story/images via /images route
    let images = warp::path("images")
        .and(warp::fs::dir("trends-story/images"));

    let routes = latest
        .or(images)
        .with(cors)
        .recover(handle_rejection);

    println!("Starting Trend Story API server on http://localhost:3003");
    println!("Available endpoints:");
    println!("  GET /latest - Get all news records from the latest date with keywords");
    println!("  GET /images/* - Serve images from trends-story/images");

    warp::serve(routes)
        .run(([127, 0, 0, 1], 3003))
        .await;
}

async fn handle_rejection(err: warp::Rejection) -> Result<impl warp::Reply, std::convert::Infallible> {
    let code;
    let message;

    if err.is_not_found() {
        code = warp::http::StatusCode::NOT_FOUND;
        message = "Not Found";
    } else if let Some(_) = err.find::<DatabaseError>() {
        code = warp::http::StatusCode::INTERNAL_SERVER_ERROR;
        message = "Database Error";
    } else {
        eprintln!("unhandled rejection: {:?}", err);
        code = warp::http::StatusCode::INTERNAL_SERVER_ERROR;
        message = "Internal Server Error";
    }

    let json = warp::reply::json(&serde_json::json!({
        "error": message,
        "code": code.as_u16()
    }));

    Ok(warp::reply::with_status(json, code))
}
