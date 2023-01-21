extern crate config;
extern crate rss;

use megalodon::megalodon::GetTimelineOptionsWithLocal;
use rss::ChannelBuilder;
use rss::ItemBuilder;

use actix_web::{
    error, get,
    http::{header::ContentType, StatusCode},
    web, App, HttpResponse, HttpServer,
};
use derive_more::{Display, Error};

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
        }
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let url = format!("0.0.0.0:{}", "6060");
    println!("Running on: http://{}", url);

    HttpServer::new(|| App::new().service(feed))
        .bind(url)?
        .run()
        .await
}

#[get("/{mastodon_instance}/{access_token}")]
async fn feed(path: web::Path<(String, String)>) -> Result<HttpResponse, UserError> {
    let (mastodon_instance, access_token) = path.into_inner();
    let full_instance_url = format!("https://{}/", mastodon_instance);
    let cloned_instace = full_instance_url.clone();

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
        .body(create_feed(status, cloned_instace).map_err(|_e| UserError::InternalError)?));
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
            .link(post.url.unwrap_or_else(|| String::from("")))
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
        content = format!("\n{}<img src=\"{}\">", content, media.preview_url);
    }

    content
}
