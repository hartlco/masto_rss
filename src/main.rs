extern crate rss;

use atrium_api::{
    app::bsky::feed::get_timeline::{
        Parameters as GetTimelineParameters, ParametersData as GetTimelineParametersData,
    },
    client::AtpServiceClient,
};
use atrium_xrpc::{
    HttpClient, XrpcClient,
    http::{Request, Response},
    types::AuthorizationToken,
};
use atrium_xrpc_client::reqwest::ReqwestClient;
use chrono::{DateTime, Utc};
use megalodon::megalodon::GetTimelineOptionsWithLocal;
use rss::{ChannelBuilder, ItemBuilder};
use serde_json::Value;

use actix_web::{
    error, get,
    http::{header::ContentType, StatusCode},
    web, App, HttpResponse, HttpServer,
};
use derive_more::{Display, Error};
use std::env;

#[derive(Debug, Display, Error)]
enum InternalError {
    #[display(fmt = "An internal error occurred. Please try again later.")]
    RSSItemError,
    ChannelError,
}

#[derive(Debug, Display, Error)]
enum UserError {
    #[display(fmt = "An internal error occurred. Please try again later.")]
    InternalError,
    #[display(fmt = "Invalid Mastodon instance format.")]
    InvalidInstance,
    #[display(fmt = "Access token is required.")]
    MissingAccessToken,
    #[display(fmt = "Bluesky access token is required.")]
    MissingBlueskyAccessToken,
}

#[derive(Clone)]
struct TokenClient {
    inner: ReqwestClient,
    token: String,
}

impl TokenClient {
    fn new(base_uri: impl AsRef<str>, token: String) -> Self {
        Self { inner: ReqwestClient::new(base_uri), token }
    }
}

impl HttpClient for TokenClient {
    async fn send_http(
        &self,
        request: Request<Vec<u8>>,
    ) -> Result<Response<Vec<u8>>, Box<dyn std::error::Error + Send + Sync + 'static>> {
        self.inner.send_http(request).await
    }
}

impl XrpcClient for TokenClient {
    fn base_uri(&self) -> String {
        self.inner.base_uri()
    }

    async fn authorization_token(&self, _is_refresh: bool) -> Option<AuthorizationToken> {
        Some(AuthorizationToken::Bearer(self.token.clone()))
    }
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
            | UserError::MissingBlueskyAccessToken => StatusCode::BAD_REQUEST,
        }
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
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

    let client = megalodon::generator(
        megalodon::SNS::Mastodon,
        full_instance_url,
        Some(access_token),
        None,
    );

    let options: GetTimelineOptionsWithLocal = GetTimelineOptionsWithLocal {
        only_media: None,
        limit: Some(40),
        max_id: None,
        since_id: None,
        min_id: None,
        local: None,
    };
    let res = client
        .get_home_timeline(Some(&options))
        .await
        .map_err(|_e| UserError::InternalError)?;
    let status = res.json();

    return Ok(HttpResponse::Ok()
        .content_type("application/rss+xml")
        .body(create_feed(status, cloned_instance).map_err(|_e| UserError::InternalError)?));
}

#[get("/bluesky/{access_token}")]
async fn bluesky_feed(path: web::Path<String>) -> Result<HttpResponse, UserError> {
    let access_token = path.into_inner();
    if access_token.trim().is_empty() {
        return Err(UserError::MissingBlueskyAccessToken);
    }

    let client = AtpServiceClient::new(TokenClient::new("https://bsky.social", access_token));

    let timeline = client
        .service
        .app
        .bsky
        .feed
        .get_timeline(GetTimelineParameters::from(GetTimelineParametersData {
            algorithm: None,
            cursor: None,
            limit: 40u8.try_into().ok(),
        }))
        .await
        .map_err(|_e| UserError::InternalError)?;

    let posts = timeline
        .data
        .feed
        .into_iter()
        .filter_map(bluesky_post_from_feed)
        .collect::<Vec<_>>();

    Ok(HttpResponse::Ok()
        .content_type("application/rss+xml")
        .body(create_bluesky_feed(posts).map_err(|_e| UserError::InternalError)?))
}

fn create_feed(
    posts: std::vec::Vec<megalodon::entities::Status>,
    mastodon_instance_url: String,
) -> Result<String, InternalError> {
    let mut post_items = Vec::new();

    for post in posts {
        let mut guid = rss::Guid::default();
        guid.set_value(post.id.to_string());
        guid.set_permalink(false);

        let pub_date = post.created_at.to_rfc2822();

        let item = ItemBuilder::default()
            .description(content_for(&post))
            .title(post.account.display_name)
            .pub_date(pub_date)
            .link(post.url.clone().unwrap_or_else(|| post.uri.clone()))
            .guid(guid)
            .build()
            .map_err(|_e| InternalError::RSSItemError)?;

        post_items.push(item);
    }

    let channel = ChannelBuilder::default()
        .items(post_items)
        .link(mastodon_instance_url)
        .title("Mastodon Timeline")
        .description("Mastodon Timeline")
        .build()
        .map_err(|_e| InternalError::ChannelError)?;

    channel
        .write_to(::std::io::sink())
        .map_err(|_e| InternalError::ChannelError)?;
    Ok(channel.to_string())
}

#[derive(Debug, Clone)]
struct BlueskyPost {
    id: String,
    author_handle: String,
    author_display_name: String,
    content: String,
    created_at: Option<DateTime<Utc>>,
}

fn bluesky_post_from_feed(
    feed_item: atrium_api::app::bsky::feed::defs::FeedViewPost,
) -> Option<BlueskyPost> {
    let record_value = serde_json::to_value(&feed_item.post.record).ok()?;
    let content = bluesky_text_from_record(&record_value).unwrap_or_default();
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
    })
}

fn bluesky_text_from_record(record: &Value) -> Option<String> {
    record.get("text").and_then(|text| text.as_str()).map(|text| {
        format!(
            "<p>{}</p>",
            text.replace('<', "&lt;").replace('>', "&gt;")
        )
    })
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
        let item = ItemBuilder::default()
            .description(post.content)
            .title(post.author_display_name)
            .link(bluesky_post_link(&post.author_handle, &post.id))
            .guid(guid)
            .pub_date(pub_date)
            .build()
            .map_err(|_e| InternalError::RSSItemError)?;

        post_items.push(item);
    }

    let channel = ChannelBuilder::default()
        .items(post_items)
        .link("https://bsky.app")
        .title("Bluesky Timeline")
        .description("Bluesky Timeline")
        .build()
        .map_err(|_e| InternalError::ChannelError)?;

    channel
        .write_to(::std::io::sink())
        .map_err(|_e| InternalError::ChannelError)?;
    Ok(channel.to_string())
}

fn content_for(status: &megalodon::entities::Status) -> String {
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
        let alt_text = media
            .description
            .as_ref()
            .map(|description| format!(" alt=\"{}\"", description))
            .unwrap_or_default();
        content = format!(
            "\n{}<img src=\"{}\"{}>",
            content, media.preview_url, alt_text
        );
    }

    content
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
    use megalodon::entities::{attachment::AttachmentType, Account, Attachment, Status, StatusVisibility};

    fn test_account() -> Account {
        Account {
            id: "1".into(),
            username: "user".into(),
            acct: "user".into(),
            display_name: "Display Name".into(),
            locked: false,
            created_at: Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap(),
            followers_count: 0,
            following_count: 0,
            statuses_count: 0,
            note: String::new(),
            url: "https://example.com/@user".into(),
            avatar: String::new(),
            avatar_static: String::new(),
            header: String::new(),
            header_static: String::new(),
            emojis: Vec::new(),
            moved: None,
            fields: None,
            bot: None,
            source: None,
        }
    }

    fn test_attachment(preview_url: &str, description: Option<&str>) -> Attachment {
        Attachment {
            id: "att-1".into(),
            r#type: AttachmentType::Image,
            url: preview_url.into(),
            remote_url: None,
            preview_url: preview_url.into(),
            text_url: None,
            meta: None,
            description: description.map(|value| value.to_string()),
            blurhash: None,
        }
    }

    fn test_status(content: &str) -> Status {
        Status {
            id: "status-1".into(),
            uri: "https://example.com/status/1".into(),
            url: Some("https://example.com/@user/1".into()),
            account: test_account(),
            in_reply_to_id: None,
            in_reply_to_account_id: None,
            reblog: None,
            content: content.into(),
            plain_content: None,
            created_at: Utc.with_ymd_and_hms(2021, 1, 1, 12, 0, 0).unwrap(),
            emojis: Vec::new(),
            replies_count: 0,
            reblogs_count: 0,
            favourites_count: 0,
            reblogged: None,
            favourited: None,
            muted: None,
            sensitive: false,
            spoiler_text: String::new(),
            visibility: StatusVisibility::Public,
            media_attachments: Vec::new(),
            mentions: Vec::new(),
            tags: Vec::new(),
            card: None,
            poll: None,
            application: None,
            language: None,
            pinned: None,
            emoji_reactions: None,
            quote: false,
            bookmarked: None,
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
