extern crate rss;

use atrium_api::{
    app::bsky::feed::get_timeline::{
        Parameters as GetTimelineParameters, ParametersData as GetTimelineParametersData,
    },
    agent::atp_agent::{store::MemorySessionStore, AtpAgent},
};
use atrium_xrpc_client::reqwest::ReqwestClient;
use chrono::{DateTime, Utc};
use dotenvy::dotenv;
use reqwest::Client;
use rss::{ChannelBuilder, Enclosure, ItemBuilder};
use rss::extension::{Extension, ExtensionMap};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

use actix_web::{
    error, get,
    http::{header::ContentType, StatusCode},
    web, App, HttpResponse, HttpServer,
};
use derive_more::{Display, Error};
use std::env;
use std::io;

#[derive(Debug, Display, Error)]
enum InternalError {
    #[display(fmt = "An internal error occurred. Please try again later.")]
    RSSItemError,
    ChannelError,
}

#[derive(Debug, Display)]
enum UserError {
    #[display(fmt = "An internal error occurred. Please try again later.")]
    InternalError,
    #[display(fmt = "Invalid Mastodon instance format.")]
    InvalidInstance,
    #[display(fmt = "Access token is required.")]
    MissingAccessToken,
    #[display(fmt = "Mastodon API error: {message}")]
    MastodonApiError { message: String },
    #[display(fmt = "Bluesky credentials are required.")]
    MissingBlueskyCredentials,
}

impl error::ResponseError for UserError {
    fn error_response(&self) -> HttpResponse {
        HttpResponse::build(self.status_code())
            .insert_header(ContentType::html())
            .body(self.to_string())
    }

    fn status_code(&self) -> StatusCode {
        match *self {
            UserError::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
            UserError::InvalidInstance
            | UserError::MissingAccessToken
            | UserError::MastodonApiError { .. }
            | UserError::MissingBlueskyCredentials => StatusCode::BAD_REQUEST,
        }
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    dotenv().ok();
    if let Err(error) = bluesky_credentials() {
        eprintln!("{error}");
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Missing Bluesky credentials",
        ));
    }
    let url = format!("0.0.0.0:{}", port_from_env());
    println!("Running on: http://{}", url);

    HttpServer::new(|| App::new().service(feed).service(bluesky_feed))
        .bind(url)?
        .run()
        .await
}

#[get("/{mastodon_instance}/{access_token}")]
async fn feed(path: web::Path<(String, String)>) -> Result<HttpResponse, UserError> {
    let (mastodon_instance, access_token) = path.into_inner();
    let full_instance_url = validate_mastodon_instance(&mastodon_instance)?;
    if access_token.trim().is_empty() {
        return Err(UserError::MissingAccessToken);
    }
    let cloned_instance = full_instance_url.clone();

    let posts = fetch_mastodon_timeline(&full_instance_url, &access_token).await?;

    return Ok(HttpResponse::Ok()
        .content_type("application/rss+xml")
        .body(create_feed(posts, cloned_instance).map_err(|e| {
            eprintln!("Failed to build Mastodon RSS feed: {e}");
            UserError::InternalError
        })?));
}

#[get("/bluesky")]
async fn bluesky_feed() -> Result<HttpResponse, UserError> {
    let (identifier, password) = bluesky_credentials()?;

    let agent = AtpAgent::new(
        ReqwestClient::new("https://bsky.social"),
        MemorySessionStore::default(),
    );
    agent.login(&identifier, &password).await.map_err(|e| {
        eprintln!("Failed to login to Bluesky: {e}");
        UserError::InternalError
    })?;

    let timeline = agent
        .api
        .app
        .bsky
        .feed
        .get_timeline(GetTimelineParameters::from(GetTimelineParametersData {
            algorithm: None,
            cursor: None,
            limit: 40u8.try_into().ok(),
        }))
        .await
        .map_err(|e| {
            eprintln!("Failed to fetch Bluesky timeline: {e}");
            UserError::InternalError
        })?;

    let posts = timeline
        .data
        .feed
        .into_iter()
        .filter_map(bluesky_post_from_feed)
        .collect::<Vec<_>>();

    Ok(HttpResponse::Ok()
        .content_type("application/rss+xml")
        .body(create_bluesky_feed(posts).map_err(|e| {
            eprintln!("Failed to build Bluesky RSS feed: {e}");
            UserError::InternalError
        })?))
}

fn bluesky_credentials() -> Result<(String, String), UserError> {
    let identifier = env::var("BLUESKY_IDENTIFIER").unwrap_or_default();
    let password = env::var("BLUESKY_PASSWORD").unwrap_or_default();
    if identifier.trim().is_empty() || password.trim().is_empty() {
        return Err(UserError::MissingBlueskyCredentials);
    }

    Ok((identifier, password))
}

#[derive(Debug, Clone, Default, Deserialize)]
struct MastodonAccount {
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    username: String,
}

#[derive(Debug, Clone, Deserialize)]
struct MastodonAttachment {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    preview_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct MastodonStatus {
    id: String,
    uri: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    account: MastodonAccount,
    #[serde(default)]
    content: String,
    created_at: String,
    #[serde(default)]
    reblog: Option<Box<MastodonStatus>>,
    #[serde(default)]
    media_attachments: Vec<MastodonAttachment>,
}

async fn fetch_mastodon_timeline(
    instance_url: &str,
    access_token: &str,
) -> Result<Vec<MastodonStatus>, UserError> {
    let url = format!("{}api/v1/timelines/home", instance_url);
    let client = Client::new();
    let response = client
        .get(&url)
        .bearer_auth(access_token)
        .query(&[("limit", "40")])
        .send()
        .await
        .map_err(|e| {
            eprintln!("Failed to fetch Mastodon timeline: {e}");
            UserError::InternalError
        })?;

    let status = response.status();
    let body = response.text().await.map_err(|e| {
        eprintln!("Failed to read Mastodon timeline response: {e}");
        UserError::InternalError
    })?;

    if !status.is_success() {
        let message = mastodon_error_message(&body)
            .unwrap_or_else(|| format!("HTTP {} from Mastodon API.", status.as_u16()));
        eprintln!("Mastodon API returned error status {}: {}", status, message);
        return Err(UserError::MastodonApiError { message });
    }

    match serde_json::from_str::<Vec<MastodonStatus>>(&body) {
        Ok(posts) => Ok(posts),
        Err(err) => {
            if let Some(message) = mastodon_error_message(&body) {
                eprintln!("Mastodon API error payload: {}", message);
                return Err(UserError::MastodonApiError { message });
            }

            let preview = body.chars().take(500).collect::<String>();
            eprintln!("Failed to decode Mastodon timeline: {err}. Body preview: {preview}");
            Err(UserError::MastodonApiError {
                message: "Unexpected response from Mastodon API.".to_string(),
            })
        }
    }
}

fn mastodon_error_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let error = value.get("error").and_then(|val| val.as_str());
    let description = value
        .get("error_description")
        .and_then(|val| val.as_str());
    let message = value.get("message").and_then(|val| val.as_str());

    error
        .or(description)
        .or(message)
        .map(|text| text.to_string())
}

fn create_feed(
    posts: std::vec::Vec<MastodonStatus>,
    mastodon_instance_url: String,
) -> Result<String, InternalError> {
    let mut post_items = Vec::new();

    for post in posts {
        let mut guid = rss::Guid::default();
        guid.set_value(post.id.to_string());
        guid.set_permalink(false);

        let pub_date = mastodon_pub_date(&post.created_at);
        let display_name = if post.account.display_name.trim().is_empty() {
            post.account.username.clone()
        } else {
            post.account.display_name.clone()
        };

        let item = ItemBuilder::default()
            .description(content_for(&post))
            .title(display_name)
            .pub_date(pub_date)
            .link(post.url.clone().unwrap_or_else(|| post.uri.clone()))
            .guid(guid)
            .build()
            .map_err(|e| {
                eprintln!("Failed to build Mastodon RSS item: {e}");
                InternalError::RSSItemError
            })?;

        post_items.push(item);
    }

    let channel = ChannelBuilder::default()
        .items(post_items)
        .link(mastodon_instance_url)
        .title("Mastodon Timeline")
        .description("Mastodon Timeline")
        .build()
        .map_err(|e| {
            eprintln!("Failed to build Mastodon RSS channel: {e}");
            InternalError::ChannelError
        })?;

    channel
        .write_to(::std::io::sink())
        .map_err(|e| {
            eprintln!("Failed to serialize Mastodon RSS channel: {e}");
            InternalError::ChannelError
        })?;
    Ok(channel.to_string())
}

#[derive(Debug, Clone)]
struct BlueskyPost {
    id: String,
    author_handle: String,
    author_display_name: String,
    content: String,
    created_at: Option<DateTime<Utc>>,
    enclosure: Option<Enclosure>,
    media_thumbnail: Option<String>,
}

fn bluesky_post_from_feed(
    feed_item: atrium_api::app::bsky::feed::defs::FeedViewPost,
) -> Option<BlueskyPost> {
    let record_value = serde_json::to_value(&feed_item.post.record).ok()?;
    let embed_value = serde_json::to_value(&feed_item.post.embed).ok();
    let reason_value = serde_json::to_value(&feed_item.reason).ok();
    let content = bluesky_content_from_record(&record_value, embed_value.as_ref(), reason_value.as_ref());
    let enclosure = bluesky_enclosure_from_embed(embed_value.as_ref());
    let media_thumbnail = bluesky_media_thumbnail_from_embed(embed_value.as_ref());
    let indexed_at = feed_item.post.indexed_at.as_str().to_string();
    let created_at =
        bluesky_created_at(&record_value).or_else(|| bluesky_parse_timestamp(&indexed_at));

    Some(BlueskyPost {
        id: feed_item.post.uri.to_string(),
        author_handle: feed_item.post.author.handle.to_string(),
        author_display_name: feed_item
            .post
            .author
            .display_name
            .as_ref()
            .map(|name| name.to_string())
            .unwrap_or_else(|| feed_item.post.author.handle.to_string()),
        content,
        created_at,
        enclosure,
        media_thumbnail,
    })
}

fn bluesky_text_from_record(record: &Value) -> Option<String> {
    record
        .get("text")
        .and_then(|text| text.as_str())
        .map(|text| format!("<p>{}</p>", bluesky_escape_html(text)))
}

fn bluesky_content_from_record(
    record: &Value,
    embed: Option<&Value>,
    reason: Option<&Value>,
) -> String {
    let mut parts = Vec::new();

    if let Some(reposter) = bluesky_repost_by(reason) {
        parts.push(format!("<p><em>Reposted by {}</em></p>", reposter));
    }

    if let Some(text) = bluesky_text_from_record(record) {
        parts.push(text);
    }

    if let Some(embed) = embed {
        parts.extend(bluesky_embed_html(embed));
    }

    parts.join("\n")
}

fn bluesky_embed_html(embed: &Value) -> Vec<String> {
    let embed_type = embed.get("$type").and_then(|value| value.as_str());
    match embed_type {
        Some("app.bsky.embed.images#view") => bluesky_images_html(embed),
        Some("app.bsky.embed.video#view") => bluesky_video_html(embed),
        Some("app.bsky.embed.record#view") => bluesky_quote_html(embed).into_iter().collect(),
        Some("app.bsky.embed.recordWithMedia#view") => {
            let mut parts = Vec::new();
            if let Some(record) = embed.get("record") {
                if let Some(quote) = bluesky_quote_html(record) {
                    parts.push(quote);
                }
            }
            if let Some(media) = embed.get("media") {
                parts.extend(bluesky_embed_html(media));
            }
            parts
        }
        _ => Vec::new(),
    }
}

fn bluesky_enclosure_from_embed(embed: Option<&Value>) -> Option<Enclosure> {
    let embed = embed?;
    let embed_type = embed.get("$type").and_then(|value| value.as_str())?;
    match embed_type {
        "app.bsky.embed.images#view" => {
            let first = embed.get("images").and_then(|value| value.as_array())?.first()?;
            let url = first
                .get("thumb")
                .or_else(|| first.get("fullsize"))
                .and_then(|value| value.as_str())?;
            Some(bluesky_image_enclosure(url))
        }
        "app.bsky.embed.video#view" => {
            let thumbnail = embed.get("thumbnail").and_then(|value| value.as_str())?;
            Some(bluesky_image_enclosure(thumbnail))
        }
        "app.bsky.embed.recordWithMedia#view" => {
            let media = embed.get("media")?;
            bluesky_enclosure_from_embed(Some(media))
        }
        _ => None,
    }
}

fn bluesky_media_thumbnail_from_embed(embed: Option<&Value>) -> Option<String> {
    let embed = embed?;
    let embed_type = embed.get("$type").and_then(|value| value.as_str())?;
    match embed_type {
        "app.bsky.embed.images#view" => {
            let first = embed.get("images").and_then(|value| value.as_array())?.first()?;
            first
                .get("thumb")
                .or_else(|| first.get("fullsize"))
                .and_then(|value| value.as_str())
                .map(|value| value.to_string())
        }
        "app.bsky.embed.video#view" => embed
            .get("thumbnail")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        "app.bsky.embed.recordWithMedia#view" => {
            let media = embed.get("media")?;
            bluesky_media_thumbnail_from_embed(Some(media))
        }
        _ => None,
    }
}

fn bluesky_media_extensions(url: &str) -> ExtensionMap {
    let mut attrs = HashMap::new();
    attrs.insert("url".to_string(), url.to_string());
    attrs.insert("medium".to_string(), "image".to_string());
    attrs.insert("type".to_string(), "image/jpeg".to_string());

    let extension = Extension {
        name: "media:content".to_string(),
        value: None,
        attrs,
        children: HashMap::new(),
    };

    let mut media_map = HashMap::new();
    media_map.insert("media:content".to_string(), vec![extension]);

    let mut extensions = ExtensionMap::default();
    extensions.insert("media".to_string(), media_map);
    extensions
}

fn bluesky_image_enclosure(url: &str) -> Enclosure {
    Enclosure {
        url: url.to_string(),
        length: "0".to_string(),
        mime_type: "image/jpeg".to_string(),
    }
}

fn bluesky_images_html(embed: &Value) -> Vec<String> {
    let images = embed.get("images").and_then(|value| value.as_array());
    images
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let url = item.get("fullsize").and_then(|value| value.as_str())?;
                    let alt = item
                        .get("alt")
                        .and_then(|value| value.as_str())
                        .unwrap_or("");
                    Some(format!(
                        "<p><img src=\"{}\" alt=\"{}\"></p>",
                        url,
                        bluesky_escape_html(alt)
                    ))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn bluesky_video_html(embed: &Value) -> Vec<String> {
    let playlist = embed
        .get("playlist")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if playlist.is_empty() {
        return Vec::new();
    }

    let alt = embed
        .get("alt")
        .and_then(|value| value.as_str())
        .unwrap_or("Video thumbnail");
    let thumbnail = embed
        .get("thumbnail")
        .and_then(|value| value.as_str())
        .unwrap_or("");

    let mut parts = Vec::new();
    if !thumbnail.is_empty() {
        parts.push(format!(
            "<p><a href=\"{}\"><img src=\"{}\" alt=\"{}\"></a></p>",
            playlist,
            thumbnail,
            bluesky_escape_html(alt)
        ));
    }
    parts.push(format!(
        "<p><a href=\"{}\">Video</a></p>",
        playlist
    ));
    parts
}

fn bluesky_quote_html(embed: &Value) -> Option<String> {
    let record = embed.get("record")?;
    let record_type = record.get("$type")?.as_str()?;
    if record_type != "app.bsky.embed.record#viewRecord" {
        return None;
    }

    let author = record.get("author");
    let author_name = author
        .and_then(|value| value.get("displayName"))
        .and_then(|value| value.as_str())
        .filter(|name| !name.trim().is_empty())
        .or_else(|| {
            author
                .and_then(|value| value.get("handle"))
                .and_then(|value| value.as_str())
        })
        .unwrap_or("Unknown");

    let handle = author
        .and_then(|value| value.get("handle"))
        .and_then(|value| value.as_str());
    let text = record
        .get("value")
        .and_then(|value| value.get("text"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let uri = record.get("uri").and_then(|value| value.as_str());
    let link = match (handle, uri) {
        (Some(handle), Some(uri)) => bluesky_post_link(handle, uri),
        _ => String::new(),
    };

    let mut quote = String::new();
    quote.push_str("<blockquote>");
    quote.push_str(&format!(
        "<p><strong>{}</strong></p>",
        bluesky_escape_html(author_name)
    ));
    if !text.trim().is_empty() {
        quote.push_str(&format!("<p>{}</p>", bluesky_escape_html(text)));
    }
    if !link.is_empty() {
        quote.push_str(&format!("<p><a href=\"{}\">View quoted post</a></p>", link));
    }
    quote.push_str("</blockquote>");

    Some(quote)
}

fn bluesky_repost_by(reason: Option<&Value>) -> Option<String> {
    let reason = reason?;
    let reason_type = reason.get("$type").and_then(|value| value.as_str())?;
    if reason_type != "app.bsky.feed.defs#reasonRepost" {
        return None;
    }

    let author = reason.get("by")?;
    let display_name = author
        .get("displayName")
        .and_then(|value| value.as_str())
        .filter(|name| !name.trim().is_empty());
    let handle = author.get("handle").and_then(|value| value.as_str());

    display_name
        .or(handle)
        .map(|value| bluesky_escape_html(value))
}

fn bluesky_escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn bluesky_created_at(record: &Value) -> Option<DateTime<Utc>> {
    record
        .get("createdAt")
        .or_else(|| record.get("created_at"))
        .and_then(|value| value.as_str())
        .and_then(bluesky_parse_timestamp)
}

fn bluesky_parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn bluesky_post_link(handle: &str, uri: &str) -> String {
    let rkey = uri.split('/').last().unwrap_or_default();
    format!("https://bsky.app/profile/{}/post/{}", handle, rkey)
}

fn create_bluesky_feed(posts: Vec<BlueskyPost>) -> Result<String, InternalError> {
    let mut post_items = Vec::new();

    for post in posts {
        let mut guid = rss::Guid::default();
        guid.set_value(post.id.clone());
        guid.set_permalink(false);

        let pub_date = post.created_at.map(|created_at| created_at.to_rfc2822());
        let mut item = ItemBuilder::default()
            .description(post.content)
            .title(post.author_display_name)
            .link(bluesky_post_link(&post.author_handle, &post.id))
            .guid(guid)
            .pub_date(pub_date)
            .enclosure(post.enclosure.clone())
            .build()
            .map_err(|e| {
                eprintln!("Failed to build Bluesky RSS item: {e}");
                InternalError::RSSItemError
            })?;

        if let Some(thumbnail) = post.media_thumbnail.as_deref() {
            item.set_extensions(bluesky_media_extensions(thumbnail));
        }

        post_items.push(item);
    }

    let mut channel = ChannelBuilder::default()
        .items(post_items)
        .link("https://bsky.app")
        .title("Bluesky Timeline")
        .description("Bluesky Timeline")
        .build()
        .map_err(|e| {
            eprintln!("Failed to build Bluesky RSS channel: {e}");
            InternalError::ChannelError
        })?;

    let mut namespaces = HashMap::new();
    namespaces.insert(
        "media".to_string(),
        "http://search.yahoo.com/mrss/".to_string(),
    );
    channel.set_namespaces(namespaces);

    channel
        .write_to(::std::io::sink())
        .map_err(|e| {
            eprintln!("Failed to serialize Bluesky RSS channel: {e}");
            InternalError::ChannelError
        })?;
    Ok(channel.to_string())
}

fn content_for(status: &MastodonStatus) -> String {
    let mut content = format!("<p>{}</p>", status.content);

    if let Some(reblog) = &status.reblog {
        content = format!(
            "{}\n{}:\n<blockquote>{}</blockquote>",
            content,
            reblog.account.display_name,
            content_for(reblog)
        );
    }

    for media in &status.media_attachments {
        if let Some(preview_url) = &media.preview_url {
            let alt_text = media
                .description
                .as_ref()
                .map(|description| format!(" alt=\"{}\"", description))
                .unwrap_or_default();
            content = format!(
                "\n{}<img src=\"{}\"{}>",
                content, preview_url, alt_text
            );
        }
    }

    content
}

fn mastodon_pub_date(value: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.to_rfc2822())
}

fn port_from_env() -> u16 {
    let port = env::var("PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(6060);
    if port == 0 {
        6060
    } else {
        port
    }
}

fn validate_mastodon_instance(instance: &str) -> Result<String, UserError> {
    if instance.is_empty()
        || instance.len() > 253
        || instance.contains('/')
        || instance.contains(':')
        || instance.contains('@')
    {
        return Err(UserError::InvalidInstance);
    }

    let mut has_label = false;
    for label in instance.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(UserError::InvalidInstance);
        }
        has_label = true;
        let bytes = label.as_bytes();
        if bytes.first() == Some(&b'-') || bytes.last() == Some(&b'-') {
            return Err(UserError::InvalidInstance);
        }
        if !label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(UserError::InvalidInstance);
        }
    }

    if !has_label {
        return Err(UserError::InvalidInstance);
    }

    Ok(format!("https://{}/", instance))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn test_account() -> MastodonAccount {
        MastodonAccount {
            display_name: "Display Name".into(),
            username: "user".into(),
        }
    }

    fn test_attachment(preview_url: &str, description: Option<&str>) -> MastodonAttachment {
        MastodonAttachment {
            preview_url: Some(preview_url.into()),
            description: description.map(|value| value.to_string()),
        }
    }

    fn test_status(content: &str) -> MastodonStatus {
        MastodonStatus {
            id: "status-1".into(),
            uri: "https://example.com/status/1".into(),
            url: Some("https://example.com/@user/1".into()),
            account: test_account(),
            content: content.into(),
            created_at: Utc
                .with_ymd_and_hms(2021, 1, 1, 12, 0, 0)
                .unwrap()
                .to_rfc3339(),
            reblog: None,
            media_attachments: Vec::new(),
        }
    }

    #[test]
    fn content_for_adds_attachments_and_reblogs() {
        let mut status = test_status("Hello");
        status.media_attachments = vec![
            test_attachment("https://example.com/img.png", Some("Alt text")),
            test_attachment("https://example.com/img2.png", None),
        ];

        let mut reblog = test_status("Boosted content");
        reblog.account.display_name = "Booster".into();
        status.reblog = Some(Box::new(reblog));

        let content = content_for(&status);
        assert!(content.contains("<p>Hello</p>"));
        assert!(content.contains("Booster:"));
        assert!(content.contains("<blockquote>"));
        assert!(content.contains("img.png\" alt=\"Alt text\""));
        assert!(content.contains("img2.png\""));
    }

    #[test]
    fn create_feed_builds_rss_output() {
        let posts = vec![test_status("Post 1"), test_status("Post 2")];
        let feed_output = create_feed(posts, "https://mastodon.example/".into()).unwrap();
        assert!(feed_output.contains("<rss"));
        assert!(feed_output.contains("<item>"));
        assert!(feed_output.contains("Mastodon Timeline"));
    }

    #[test]
    fn validate_mastodon_instance_accepts_domains() {
        let url = validate_mastodon_instance("mastodon.social").unwrap();
        assert_eq!(url, "https://mastodon.social/");
    }

    #[test]
    fn validate_mastodon_instance_rejects_invalid() {
        assert!(validate_mastodon_instance("https://bad.example").is_err());
        assert!(validate_mastodon_instance("bad/host").is_err());
        assert!(validate_mastodon_instance("").is_err());
        assert!(validate_mastodon_instance("-bad.example").is_err());
    }

    #[test]
    fn bluesky_post_link_builds_expected_url() {
        let link = bluesky_post_link("user.bsky.social", "at://did:plc:123/app.bsky.feed.post/abc");
        assert_eq!(
            link,
            "https://bsky.app/profile/user.bsky.social/post/abc"
        );
    }

    #[test]
    fn bluesky_created_at_parses_record_timestamp() {
        let record = serde_json::json!({
            "createdAt": "2023-01-01T12:34:56Z"
        });
        let parsed = bluesky_created_at(&record).unwrap();
        assert_eq!(parsed.to_rfc3339(), "2023-01-01T12:34:56+00:00");
    }
}
