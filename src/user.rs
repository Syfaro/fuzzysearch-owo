use actix_web::{get, post, services, web, Error, HttpResponse, Scope};
use actix_web::{HttpRequest, Responder};
use askama::Template;
use futures::TryStreamExt;
use serde::Deserialize;
use sha2::Digest;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;

use crate::routes::*;
use crate::{jobs, models};

#[derive(Template)]
#[template(path = "user/home.html")]
struct Home {
    user: models::User,

    item_count: i64,
    total_content_size: i64,

    recent_media: Vec<models::OwnedMediaItem>,
    monitored_accounts: Vec<models::LinkedAccount>,
}

#[get("/home")]
async fn home(
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    user: models::User,
) -> Result<HttpResponse, Error> {
    let (item_count, total_content_size) = models::OwnedMediaItem::user_item_count(&conn, user.id)
        .await
        .unwrap();
    let recent_media = models::OwnedMediaItem::recent_media(&conn, user.id)
        .await
        .unwrap();
    let monitored_accounts = models::LinkedAccount::owned_by_user(&conn, user.id)
        .await
        .unwrap();

    let body = Home {
        user,
        item_count,
        total_content_size,
        recent_media,
        monitored_accounts,
    }
    .render()
    .unwrap();

    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[get("/events")]
async fn events(
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    user: models::User,
    req: HttpRequest,
) -> Result<HttpResponse, Error> {
    let events = models::UserEvent::recent_events(&conn, user.id)
        .await
        .unwrap();

    Ok(web::Json(events).respond_to(&req))
}

#[post("/single")]
async fn single(
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    redis: web::Data<redis::aio::ConnectionManager>,
    s3: web::Data<rusoto_s3::S3Client>,
    config: web::Data<crate::Config>,
    faktory: web::Data<fuzzysearch_common::faktory::FaktoryClient>,
    user: models::User,
    mut form: actix_multipart::Multipart,
) -> Result<HttpResponse, Error> {
    while let Ok(Some(mut field)) = form.try_next().await {
        tracing::trace!("checking multipart field: {:?}", field);

        if !matches!(field.content_disposition().get_name(), Some("image")) {
            continue;
        }

        let mut file = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let file = tempfile::tempfile()?;
            Ok(tokio::fs::File::from_std(file))
        })
        .await
        .unwrap()
        .unwrap();

        let mut hasher = sha2::Sha256::default();

        let mut size = 0;
        while let Ok(Some(chunk)) = field.try_next().await {
            if size > 10_000_000 {
                panic!("file too large");
            }

            file.write_all(&chunk).await?;
            hasher.update(&chunk);
            size += chunk.len();
        }

        if size == 0 {
            tracing::info!("empty image, ignoring");

            return Ok(HttpResponse::Found()
                .insert_header(("Location", USER_HOME))
                .finish());
        }

        let sha256_hash: [u8; 32] = hasher.finalize().try_into().unwrap();
        let hash_str = hex::encode(&sha256_hash);

        tracing::info!(size, hash = ?hash_str, "completed uploading file");

        file.seek(std::io::SeekFrom::Start(0)).await?;

        let file = file.into_std().await;
        let (perceptual_hash, im) = tokio::task::spawn_blocking(move || {
            let reader = std::io::BufReader::new(file);
            let reader = image::io::Reader::new(reader)
                .with_guessed_format()
                .unwrap();
            let im = reader.decode().unwrap();

            let hasher = fuzzysearch_common::get_hasher();
            let hash = hasher.hash_image(&im);
            let hash: [u8; 8] = hash.as_bytes().try_into().unwrap();

            (hash, im)
        })
        .await
        .unwrap();

        let id = models::OwnedMediaItem::add_manual_item(
            &conn,
            user.id,
            i64::from_be_bytes(perceptual_hash),
            sha256_hash,
        )
        .await
        .unwrap();

        models::OwnedMediaItem::update_media(&conn, &s3, &config, id, im)
            .await
            .unwrap();

        models::UserEvent::notify(&conn, &redis, user.id, "Uploaded image.")
            .await
            .unwrap();

        faktory
            .enqueue(
                faktory::Job::new(
                    "search_existing_submissions",
                    vec![
                        serde_json::to_value(user.id).unwrap(),
                        serde_json::to_value(id).unwrap(),
                    ],
                )
                .on_queue(crate::jobs::FUZZYSEARCH_OWO_QUEUE),
            )
            .await
            .unwrap();
    }

    Ok(HttpResponse::Found()
        .insert_header(("Location", USER_HOME))
        .finish())
}

#[derive(Template)]
#[template(path = "user/account_link.html")]
struct AccountLink;

#[get("/link")]
async fn account_link_get() -> HttpResponse {
    let body = AccountLink.render().unwrap();
    HttpResponse::Ok().content_type("text/html").body(body)
}

#[derive(Deserialize)]
struct AccountLinkForm {
    site: String,
    username: String,
}

#[post("/link")]
async fn account_link_post(
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    faktory: web::Data<fuzzysearch_common::faktory::FaktoryClient>,
    user: models::User,
    form: web::Form<AccountLinkForm>,
) -> HttpResponse {
    let site: models::SourceSite = form.site.parse().unwrap();

    let account_id = models::LinkedAccount::create(&conn, user.id, site, &form.username, None)
        .await
        .unwrap();

    faktory
        .enqueue(jobs::add_account_job(user.id, account_id))
        .await
        .unwrap();

    HttpResponse::Found()
        .insert_header((
            "Location",
            format!("/user/account/{}", account_id.to_string()),
        ))
        .finish()
}

#[derive(Deserialize)]
struct AccountRemoveForm {
    account_id: Uuid,
}

#[post("/remove")]
async fn account_remove(
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    user: models::User,
    form: web::Form<AccountRemoveForm>,
) -> HttpResponse {
    models::LinkedAccount::remove(&conn, user.id, form.account_id)
        .await
        .unwrap();

    HttpResponse::Found()
        .insert_header(("Location", USER_HOME))
        .finish()
}

#[derive(Template)]
#[template(path = "user/account_view.html")]
struct AccountView {
    account: models::LinkedAccount,

    item_count: i64,
    total_content_size: i64,
}

#[get("/{account_id}")]
async fn account_view(
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    path: web::Path<(Uuid,)>,
    user: models::User,
) -> HttpResponse {
    let account_id: Uuid = path.into_inner().0;
    let account = models::LinkedAccount::lookup_by_id(&conn, account_id, user.id)
        .await
        .unwrap()
        .unwrap();
    let (item_count, total_content_size) = models::LinkedAccount::items(&conn, user.id, account_id)
        .await
        .unwrap();

    let body = AccountView {
        account,
        item_count,
        total_content_size,
    }
    .render()
    .unwrap();
    HttpResponse::Ok().content_type("text/html").body(body)
}

#[derive(Template)]
#[template(path = "user/media_view.html")]
struct MediaView {
    media: models::OwnedMediaItem,
    recent_events: Vec<models::UserEvent>,
}

#[get("/{media_id}")]
async fn media_view(
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    path: web::Path<(Uuid,)>,
    user: models::User,
) -> HttpResponse {
    let media = models::OwnedMediaItem::get_by_id(&conn, path.0, user.id)
        .await
        .unwrap()
        .unwrap();

    let recent_events = models::UserEvent::recent_events_for_media(&conn, user.id, media.id)
        .await
        .unwrap();

    let body = MediaView {
        media,
        recent_events,
    }
    .render()
    .unwrap();
    HttpResponse::Ok().content_type("text/html").body(body)
}

#[derive(Deserialize)]
struct MediaRemoveForm {
    media_id: Uuid,
}

#[post("/remove")]
async fn media_remove(
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    user: models::User,
    form: web::Form<MediaRemoveForm>,
) -> HttpResponse {
    models::OwnedMediaItem::remove(&conn, user.id, form.media_id)
        .await
        .unwrap();

    HttpResponse::Found()
        .insert_header(("Location", USER_HOME))
        .finish()
}

pub fn service() -> Scope {
    web::scope("/user")
        .service(services![home, events, single])
        .service(web::scope("/account").service(services![
            account_link_get,
            account_link_post,
            account_remove,
            account_view
        ]))
        .service(web::scope("/media").service(services![media_view, media_remove]))
}
