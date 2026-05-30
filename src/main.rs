// Immutable Config
const DOMAIN: &str = "https://trending.oopus.info";
const DOMAIN_API: &str = "https://trend-story-api.oopus.info";

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use rusqlite::{Connection, Result as SqlResult};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use warp::Filter;

// Sitemap is expensive to generate (~7s DB query + XML build).
// RwLock allows concurrent cache reads while serializing generation.
type SitemapCache = Arc<RwLock<Option<(String, Instant)>>>;
const SITEMAP_CACHE_TTL: Duration = Duration::from_secs(86400);

#[derive(Debug, Serialize, Deserialize)]
struct LatestResponse {
    date: Option<String>,
    records: Vec<NewsRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DateResponse {
    date: String,
    date_with_url: String,
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
    serpapi_data_date: Option<String>,
    keywords: Option<String>,
    image: Option<ImageInfo>,
    tag: Vec<String>,
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

async fn get_date(date_param: String) -> Result<impl warp::Reply, warp::Rejection> {
    // Validate date format (must be 8 digits)
    if date_param.len() != 8 || !date_param.chars().all(|c| c.is_numeric()) {
        return Err(warp::reject::custom(InvalidDateFormat));
    }
    
    // Convert yyyymmdd to yyyy-mm-dd
    let formatted_date = format!(
        "{}-{}-{}",
        &date_param[0..4],
        &date_param[4..6],
        &date_param[6..8]
    );
    
    match query_news_by_date(&formatted_date) {
        Ok(response) => {
            if response.records.is_empty() {
                Err(warp::reject::custom(NoDataFound))
            } else {
                Ok(warp::reply::json(&response))
            }
        }
        Err(e) => {
            eprintln!("Database error: {}", e);
            Err(warp::reject::custom(DatabaseError))
        }
    }
}

async fn get_sitemap(cache: SitemapCache) -> Result<impl warp::Reply, warp::Rejection> {
    // Fast path: read lock allows fully concurrent cache hits
    {
        let guard = cache.read().await;
        if let Some((xml, generated_at)) = guard.as_ref() {
            if generated_at.elapsed() < SITEMAP_CACHE_TTL {
                return Ok(warp::reply::with_header(
                    warp::reply::with_header(xml.clone(), "content-type", "application/xml; charset=utf-8"),
                    "cache-control",
                    "public, max-age=86400",
                ));
            }
        }
    } // read lock released

    // Slow path: write lock serializes generation (prevents thundering herd)
    let mut guard = cache.write().await;

    // Double-check: another task may have populated the cache while we
    // waited for the write lock
    if let Some((xml, generated_at)) = guard.as_ref() {
        if generated_at.elapsed() < SITEMAP_CACHE_TTL {
            return Ok(warp::reply::with_header(
                warp::reply::with_header(xml.clone(), "content-type", "application/xml; charset=utf-8"),
                "cache-control",
                "public, max-age=86400",
            ));
        }
    }

    // Generate from DB (write lock held — only one task runs this at a time)
    let new_xml = match tokio::task::spawn_blocking(query_sitemap_data).await {
        Ok(Ok(xml)) => xml,
        Ok(Err(e)) => {
            eprintln!("Sitemap database error: {}", e);
            return Err(warp::reject::custom(DatabaseError));
        }
        Err(e) => {
            eprintln!("Sitemap spawn_blocking error: {}", e);
            return Err(warp::reject::custom(DatabaseError));
        }
    };

    *guard = Some((new_xml.clone(), Instant::now()));

    Ok(warp::reply::with_header(
        warp::reply::with_header(new_xml, "content-type", "application/xml; charset=utf-8"),
        "cache-control",
        "public, max-age=86400",
    ))
}

async fn get_dates() -> Result<impl warp::Reply, warp::Rejection> {
    match query_all_dates() {
        Ok(dates) => Ok(warp::reply::json(&dates)),
        Err(e) => {
            eprintln!("Database error: {}", e);
            Err(warp::reject::custom(DatabaseError))
        }
    }
}

fn query_sitemap_data() -> SqlResult<String> {
    let db_path = "../trends-story/trends_data.db";

    if !Path::new(db_path).exists() {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
            Some("Database file not found".to_string()),
        ));
    }

    let conn = Connection::open(db_path)?;

    // Single query: id + yyyymmdd + lastmod for every article that has content
    let mut stmt = conn.prepare(
        "SELECT id, \
                REPLACE(substr(date, 1, 10), '-', '') AS yyyymmdd, \
                substr(date, 1, 10) AS lastmod \
         FROM main_news_data \
         WHERE news IS NOT NULL AND date IS NOT NULL \
         ORDER BY date ASC, id ASC",
    )?;

    let rows: Vec<(i64, String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;

    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );

    // Home page entry
    xml.push_str(&format!(
        "  <url><loc>{}/</loc><changefreq>daily</changefreq><priority>1.0</priority></url>\n",
        DOMAIN
    ));

    // Collect date entries (deduplicated) and article entries separately
    let mut seen_dates: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut date_xml = String::new();
    let mut article_xml = String::new();

    for (id, yyyymmdd, lastmod) in &rows {
        if seen_dates.insert(yyyymmdd.clone()) {
            date_xml.push_str(&format!(
                "  <url><loc>{}/date/{}</loc><lastmod>{}</lastmod>\
                 <changefreq>yearly</changefreq><priority>0.8</priority></url>\n",
                DOMAIN, yyyymmdd, lastmod
            ));
        }
        article_xml.push_str(&format!(
            "  <url><loc>{}/article/{}?date={}</loc><lastmod>{}</lastmod>\
             <changefreq>yearly</changefreq><priority>0.6</priority></url>\n",
            DOMAIN, id, yyyymmdd, lastmod
        ));
    }

    xml.push_str(&date_xml);
    xml.push_str(&article_xml);
    xml.push_str("</urlset>");

    Ok(xml)
}

fn query_all_dates() -> SqlResult<Vec<DateResponse>> {
    let db_path = "../trends-story/trends_data.db";
    
    if !Path::new(db_path).exists() {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
            Some("Database file not found".to_string())
        ));
    }

    let conn = Connection::open(db_path)?;
    
    // Query unique dates from main_news_data, extract yyyymmdd format, and sort by id
    let mut stmt = conn.prepare(
        "SELECT DISTINCT REPLACE(substr(date, 1, 10), '-', '') as date_formatted \
         FROM main_news_data \
         ORDER BY id ASC"
    )?;

    let date_rows = stmt.query_map([], |row| {
        let date: String = row.get(0)?;
        Ok(date)
    })?;
    
    let mut dates = Vec::new();
    let mut seen = std::collections::HashSet::new();
    
    for row_result in date_rows {
        let date_formatted = row_result?;
        
        // Only add unique dates
        if seen.insert(date_formatted.clone()) {
            let date_with_url = format!(
                "{}/date/{}",
                DOMAIN,
                date_formatted
            );
            
            dates.push(DateResponse {
                date: date_formatted,
                date_with_url,
            });
        }
    }
    
    Ok(dates)
}

fn query_latest_news() -> SqlResult<LatestResponse> {
    let db_path = "../trends-story/trends_data.db";
    
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
            date: None,
            records: vec![],
        }),
    };

    // Query all records from the latest day
    let mut stmt = conn.prepare(
        "SELECT main_news_data.id, main_news_data.news, main_news_data.date, \
         main_news_data.serpapi_id, main_news_data.image_id, \
         serpapi_data.date AS serpapi_data_date \
         FROM main_news_data \
         LEFT JOIN serpapi_data \
         ON main_news_data.serpapi_id = serpapi_data.id \
         WHERE substr(main_news_data.date, 1, 10) = ?1 \
         ORDER BY main_news_data.id ASC"
    )?;

    let news_rows = stmt.query_map([&day_filter], |row| {
        Ok((
            row.get::<_, i64>(0)?,      // id
            row.get::<_, Option<String>>(1)?,  // news
            row.get::<_, Option<String>>(2)?,  // date
            row.get::<_, Option<i64>>(3)?,     // serpapi_id
            row.get::<_, Option<i64>>(4)?,     // image_id
            row.get::<_, Option<String>>(5)?,  // serpapi_data_date
        ))
    })?;
    
    let mut records = Vec::new();
    
    for row_result in news_rows {
        let (id, news, date, serpapi_id, image_id, serpapi_data_date) = row_result?;

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
                    let date_str = tokens[1];
                    // Convert yyyymmdd to yyyy/mm/dd
                    if date_str.len() == 8 {
                        let year = &date_str[0..4];
                        let month = &date_str[4..6];
                        let day = &date_str[6..8];
                        format!("{}/images/{}/{}/{}/{}", DOMAIN_API, year, month, day, fname)
                    } else {
                        // Fallback for unexpected format
                        format!("{}/images/{}/{}", DOMAIN_API, date_str, fname)
                    }
                } else {
                    format!("{}/images/{}", DOMAIN_API, fname)
                }
            });
            Some(ImageInfo { file_name, url })
        } else {
            None
        };

        // Query categories from serpapi_data if serpapi_id exists
        let tag = if let Some(serpapi_id) = serpapi_id {
            let mut cat_stmt = conn.prepare(
                "SELECT categories FROM serpapi_data WHERE id = ?1"
            )?;
            let categories: Option<String> = cat_stmt.query_row([serpapi_id], |row| row.get(0)).unwrap_or(None);
            if let Some(cat_str) = categories {
                if cat_str.trim().is_empty() {
                    Vec::new()
                } else {
                    let mut seen = std::collections::HashSet::new();
                    cat_str.split('|')
                        .filter_map(|token| {
                            let parts: Vec<&str> = token.splitn(2, '-').collect();
                            if parts.len() == 2 {
                                let val = parts[1].trim();
                                if !val.is_empty() && seen.insert(val.to_string()) {
                                    Some(val.to_string())
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<String>>()
                }
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        records.push(NewsRecord {
            id,
            news,
            date,
            serpapi_id,
            image_id,
            serpapi_data_date,
            keywords,
            image,
            tag,
        });
    }
    
    Ok(LatestResponse {
        date: latest_day,
        records,
    })
}

fn query_news_by_date(target_date: &str) -> SqlResult<LatestResponse> {
    let db_path = "../trends-story/trends_data.db";
    
    if !Path::new(db_path).exists() {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
            Some("Database file not found".to_string())
        ));
    }

    let conn = Connection::open(db_path)?;
    
    // Query all records from the specified date
let mut stmt = conn.prepare(
    "SELECT main_news_data.id, main_news_data.news, main_news_data.date, \
     main_news_data.serpapi_id, main_news_data.image_id, \
     serpapi_data.date AS serpapi_data_date \
     FROM main_news_data \
     LEFT JOIN serpapi_data \
     ON main_news_data.serpapi_id = serpapi_data.id \
     WHERE substr(main_news_data.date, 1, 10) = ?1 \
     ORDER BY main_news_data.id ASC"
)?;

    let news_rows = stmt.query_map([target_date], |row| {
        Ok((
            row.get::<_, i64>(0)?,      // id
            row.get::<_, Option<String>>(1)?,  // news
            row.get::<_, Option<String>>(2)?,  // date
            row.get::<_, Option<i64>>(3)?,     // serpapi_id
            row.get::<_, Option<i64>>(4)?,     // image_id
            row.get::<_, Option<String>>(5)?,  // serpapi_data_date
        ))
    })?;
    
    let mut records = Vec::new();
    
    for row_result in news_rows {
        let (id, news, date, serpapi_id, image_id, serpapi_data_date) = row_result?;

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
                    let date_str = tokens[1];
                    // Convert yyyymmdd to yyyy/mm/dd
                    if date_str.len() == 8 {
                        let year = &date_str[0..4];
                        let month = &date_str[4..6];
                        let day = &date_str[6..8];
                        format!("{}/images/{}/{}/{}/{}", DOMAIN_API, year, month, day, fname)
                    } else {
                        // Fallback for unexpected format
                        format!("{}/images/{}/{}", DOMAIN_API, date_str, fname)
                    }
                } else {
                    format!("{}/images/{}", DOMAIN_API, fname)
                }
            });
            Some(ImageInfo { file_name, url })
        } else {
            None
        };

        // Query categories from serpapi_data if serpapi_id exists
        let tag = if let Some(serpapi_id) = serpapi_id {
            let mut cat_stmt = conn.prepare(
                "SELECT categories FROM serpapi_data WHERE id = ?1"
            )?;
            let categories: Option<String> = cat_stmt.query_row([serpapi_id], |row| row.get(0)).unwrap_or(None);
            if let Some(cat_str) = categories {
                if cat_str.trim().is_empty() {
                    Vec::new()
                } else {
                    let mut seen = std::collections::HashSet::new();
                    cat_str.split('|')
                        .filter_map(|token| {
                            let parts: Vec<&str> = token.splitn(2, '-').collect();
                            if parts.len() == 2 {
                                let val = parts[1].trim();
                                if !val.is_empty() && seen.insert(val.to_string()) {
                                    Some(val.to_string())
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<String>>()
                }
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        records.push(NewsRecord {
            id,
            news,
            date,
            serpapi_id,
            image_id,
            serpapi_data_date,
            keywords,
            image,
            tag,
        });
    }
    
    Ok(LatestResponse {
        date: Some(target_date.to_string()),
        records,
    })
}

#[derive(Debug)]
struct DatabaseError;

impl warp::reject::Reject for DatabaseError {}

#[derive(Debug)]
struct InvalidDateFormat;

impl warp::reject::Reject for InvalidDateFormat {}

#[derive(Debug)]
struct NoDataFound;

impl warp::reject::Reject for NoDataFound {}

#[tokio::main]
async fn main() {
    // CORS filter
    let cors = warp::cors()
        .allow_any_origin()
        .allow_headers(vec!["content-type"])
        .allow_methods(vec!["GET", "POST", "DELETE"]);

    // Routes
    let latest = warp::path("latest")
        .and(warp::get())
        .and_then(get_latest);

    let dates = warp::path("dates")
        .and(warp::get())
        .and_then(get_dates);

    let date = warp::path("date")
        .and(warp::path::param::<String>())
        .and(warp::get())
        .and_then(get_date);

    // Serve images from ../trends-story/images via /images route
    let images = warp::path("images")
        .and(warp::fs::dir("../trends-story/images"));

    let sitemap_cache: SitemapCache = Arc::new(RwLock::new(None));
    let sitemap = warp::path("sitemap.xml")
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::any().map(move || sitemap_cache.clone()))
        .and_then(get_sitemap);

    let routes = latest
        .or(dates)
        .or(date)
        .or(images)
        .or(sitemap)
        .with(cors)
        .recover(handle_rejection);

    const PORT: u16 = 3003;
    
    println!("Starting Trend Story API server on http://localhost:{}", PORT);
    println!("Available endpoints:");
    println!("  GET /latest - Get all news records from the latest date with keywords");
    println!("  GET /dates - Get all available dates in yyyymmdd format");
    println!("  GET /date/<yyyymmdd> - Get all news records from a specific date");
    println!("  GET /images/* - Serve images from ../trends-story/images");
    println!("  GET /sitemap.xml - Full sitemap (date pages + article pages)");

    warp::serve(routes)
        .run(([127, 0, 0, 1], PORT))
        .await;
}

async fn handle_rejection(err: warp::Rejection) -> Result<impl warp::Reply, std::convert::Infallible> {
    let code;
    let message;

    if err.is_not_found() {
        code = warp::http::StatusCode::NOT_FOUND;
        message = "Not Found";
    } else if let Some(_) = err.find::<InvalidDateFormat>() {
        code = warp::http::StatusCode::BAD_REQUEST;
        message = "Invalid date format. Expected 8 digits (yyyymmdd)";
    } else if let Some(_) = err.find::<NoDataFound>() {
        code = warp::http::StatusCode::NOT_FOUND;
        message = "No data found for the requested date";
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
