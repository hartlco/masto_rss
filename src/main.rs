extern crate rss;
extern crate config;

use megalodon::megalodon::GetTimelineOptionsWithLocal;
use rss::ChannelBuilder;
use rss::ItemBuilder;

use actix_web::{get, web, App, HttpResponse, HttpServer};

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let url = format!("0.0.0.0:{}", "6060");
    println!("Running on: http://{}", url);

    HttpServer::new(|| {
        App::new()
            .service(feed)
    })
    .bind(url)?
    .run()
    .await
}

#[get("/{mastodon_instance}/{access_token}")]
async fn feed(path: web::Path<(String, String)>) -> HttpResponse {
    let (mastodon_instance, access_token) = path.into_inner();
    let full_instance_url = format!("https://{}/", mastodon_instance);
    let cloned_instace = full_instance_url.clone();

    let client = megalodon::generator(
        megalodon::SNS::Mastodon,
        String::from(full_instance_url),
        Some(access_token),
        None,
    );
    let res = client.get_home_timeline(Option::None).await.unwrap();
    let status = res.json();

    return HttpResponse::Ok()
    .content_type("application/rss+xml")
    .body(create_feed(status, cloned_instace));
}

fn create_feed(
    posts: std::vec::Vec<megalodon::entities::Status>,
    mastodon_instance_url: String,
    // user_name: String
) -> String {
    let mut post_items = Vec::new();

    for post in posts {
        let mut guid = rss::Guid::default();
        guid.set_value(post.id.to_string());
        guid.set_permalink(false);

        let pub_date = post.created_at.to_rfc2822();
        
        let item =  ItemBuilder::default()
        .description(content_for(&post))
        .title(post.account.display_name)
        .pub_date(pub_date)
        .link(post.url.unwrap_or(String::from("")))
        .guid(guid)
        .build()
        .unwrap();

        post_items.push(item);
    }

    let channel = ChannelBuilder::default()
    // .title(format!("Mastodon Homefeed: {}", user_name))
    .items(post_items)
    .link(mastodon_instance_url)
    // TODO: Get user name from mastodon instance
    .title("Mastodon Timeline")
    .description("Mastodon Timeline")
    .build()
    .unwrap();

    channel.write_to(::std::io::sink()).unwrap();
    let string = channel.to_string();
    return string;
}

fn content_for(status: &megalodon::entities::Status) -> String {
    let mut content = format!("<p>{}</p>", status.content).to_string();

    if let Some(reblog) = &status.reblog {
        content = format!("{}\n{}:\n<blockquote>{}</blockquote>", content, reblog.account.display_name, content_for(reblog));
    }

    for media in &status.media_attachments {
        content = format!("\n{}<img src=\"{}\">", content, media.preview_url);
    }

    return content;
}