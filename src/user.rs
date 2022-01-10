use std::borrow::Cow;
use std::collections::HashMap;

use actix_web::dev::ConnectionInfo;
use actix_web::{get, post, services, web, HttpResponse, Scope};
use askama::Template;
use futures::TryStreamExt;
use rand::Rng;
use serde::Deserialize;
use sha2::Digest;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;

use crate::{
    auth::FuzzySearchSessionToken,
    jobs,
    models::{self, Site},
    routes::*,
    Error,
};

#[derive(Template)]
#[template(path = "user/index.html")]
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
    let (item_count, total_content_size) =
        models::OwnedMediaItem::user_item_count(&conn, user.id).await?;
    let recent_media = models::OwnedMediaItem::recent_media(&conn, user.id).await?;
    let monitored_accounts = models::LinkedAccount::owned_by_user(&conn, user.id).await?;

    let body = Home {
        user,
        item_count,
        total_content_size,
        recent_media,
        monitored_accounts,
    }
    .render()?;

    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[get("/events")]
async fn events(
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    user: models::User,
) -> Result<web::Json<Vec<models::UserEvent>>, Error> {
    let events = models::UserEvent::recent_events(&conn, user.id).await?;

    Ok(web::Json(events))
}

#[post("/single")]
async fn single(
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    redis: web::Data<redis::aio::ConnectionManager>,
    s3: web::Data<rusoto_s3::S3Client>,
    config: web::Data<crate::Config>,
    faktory: web::Data<jobs::FaktoryClient>,
    user: models::User,
    mut form: actix_multipart::Multipart,
) -> Result<HttpResponse, Error> {
    while let Ok(Some(mut field)) = form.try_next().await {
        tracing::trace!("checking multipart field: {:?}", field);

        if !matches!(field.content_disposition().get_name(), Some("image")) {
            continue;
        }

        let mut file = tokio::task::spawn_blocking(move || -> Result<_, String> {
            let file = tempfile::tempfile().map_err(|err| err.to_string())?;
            Ok(tokio::fs::File::from_std(file))
        })
        .await
        .map_err(Error::from_displayable)?
        .map_err(|err| Error::UnknownMessage(err.into()))?;

        let mut hasher = sha2::Sha256::default();

        let mut size = 0;
        while let Ok(Some(chunk)) = field.try_next().await {
            if size > 10_000_000 {
                return Err(Error::TooLarge(size));
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

        let sha256_hash: [u8; 32] = hasher
            .finalize()
            .try_into()
            .expect("sha256 hash was wrong size");
        let hash_str = hex::encode(&sha256_hash);

        tracing::info!(size, hash = ?hash_str, "received complete file from client");

        file.seek(std::io::SeekFrom::Start(0)).await?;

        let file = file.into_std().await;
        let (perceptual_hash, im) = tokio::task::spawn_blocking(
            move || -> Result<([u8; 8], image::DynamicImage), image::ImageError> {
                let reader = std::io::BufReader::new(file);
                let reader = image::io::Reader::new(reader).with_guessed_format()?;
                let im = reader.decode()?;

                let hasher = fuzzysearch_common::get_hasher();
                let hash = hasher.hash_image(&im);
                let hash: [u8; 8] = hash
                    .as_bytes()
                    .try_into()
                    .expect("perceptual hash returned wrong bytes");

                Ok((hash, im))
            },
        )
        .await
        .map_err(|_err| Error::UnknownMessage("join error".into()))??;

        let id = models::OwnedMediaItem::add_manual_item(
            &conn,
            user.id,
            i64::from_be_bytes(perceptual_hash),
            sha256_hash,
        )
        .await?;

        models::OwnedMediaItem::update_media(&conn, &s3, &config, id, im).await?;

        models::UserEvent::notify(&conn, &redis, user.id, "Uploaded image.").await?;

        faktory
            .enqueue_job(
                jobs::JobInitiator::user(user.id),
                jobs::search_existing_submissions_job(user.id, id)?,
            )
            .await?;
    }

    Ok(HttpResponse::Found()
        .insert_header(("Location", USER_HOME))
        .finish())
}

#[derive(Template)]
#[template(path = "user/account/link.html")]
struct AccountLink;

#[get("/link")]
async fn account_link_get(user: models::User) -> Result<HttpResponse, Error> {
    if !user.has_verified_email() {
        return Err(Error::UserError(
            "You must verify your email address first.".into(),
        ));
    }

    let body = AccountLink.render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[derive(Deserialize)]
struct AccountLinkForm {
    site: models::Site,
    username: Option<String>,
}

#[post("/link")]
async fn account_link_post(
    config: web::Data<crate::Config>,
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    user: models::User,
    form: web::Form<AccountLinkForm>,
) -> Result<HttpResponse, Error> {
    if !user.has_verified_email() {
        return Err(Error::UserError(
            "You must verify your email address first.".into(),
        ));
    }

    if let Ok(Some(collected_site)) = form.site.collected_site(&config).await {
        if let Some(location) = collected_site.oauth_page() {
            return Ok(HttpResponse::Found()
                .insert_header(("Location", location))
                .finish());
        }
    }

    let username = form.username.as_deref().ok_or(Error::Missing)?;

    let token: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(12)
        .map(char::from)
        .collect();

    let data = match form.site {
        Site::FurAffinity => Some(serde_json::json!({ "verification_key": token })),
        _ => None,
    };

    let account = models::LinkedAccount::create(&conn, user.id, form.site, username, data).await?;

    Ok(HttpResponse::Found()
        .insert_header((
            "Location",
            format!("/user/account/{}", account.id.to_string()),
        ))
        .finish())
}

#[derive(Deserialize)]
struct AccountIdForm {
    account_id: Uuid,
}

#[post("/remove")]
async fn account_remove(
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    user: models::User,
    form: web::Form<AccountIdForm>,
) -> Result<HttpResponse, Error> {
    models::LinkedAccount::remove(&conn, user.id, form.account_id).await?;

    Ok(HttpResponse::Found()
        .insert_header(("Location", USER_HOME))
        .finish())
}

#[derive(Template)]
#[template(path = "user/account/view.html")]
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
) -> Result<HttpResponse, Error> {
    let account_id: Uuid = path.into_inner().0;
    let account = models::LinkedAccount::lookup_by_id(&conn, account_id)
        .await?
        .ok_or(Error::Missing)?;
    let (item_count, total_content_size) =
        models::LinkedAccount::items(&conn, user.id, account_id).await?;

    let body = AccountView {
        account,
        item_count,
        total_content_size,
    }
    .render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[post("/verify")]
async fn account_verify(
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    faktory: web::Data<jobs::FaktoryClient>,
    user: models::User,
    form: web::Form<AccountIdForm>,
) -> Result<HttpResponse, Error> {
    let account = models::LinkedAccount::lookup_by_id(&conn, form.account_id)
        .await?
        .ok_or(Error::Missing)?;

    if account.owner_id != user.id {
        return Err(Error::Missing);
    }

    faktory
        .enqueue_job(
            jobs::JobInitiator::user(user.id),
            jobs::verify_account_job(user.id, account.id)?,
        )
        .await?;

    Ok(HttpResponse::Found()
        .insert_header((
            "Location",
            format!("/user/account/{}", account.id.to_string()),
        ))
        .finish())
}

#[derive(Template)]
#[template(path = "user/media/view.html")]
struct MediaView {
    media: models::OwnedMediaItem,
    recent_events: Vec<models::UserEvent>,
}

#[get("/view/{media_id}")]
async fn media_view(
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    path: web::Path<(Uuid,)>,
    user: models::User,
) -> Result<HttpResponse, Error> {
    let media = models::OwnedMediaItem::get_by_id(&conn, path.0, user.id)
        .await?
        .ok_or(Error::Missing)?;

    let recent_events =
        models::UserEvent::recent_events_for_media(&conn, user.id, media.id).await?;

    let body = MediaView {
        media,
        recent_events,
    }
    .render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
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
) -> Result<HttpResponse, Error> {
    models::OwnedMediaItem::remove(&conn, user.id, form.media_id).await?;

    Ok(HttpResponse::Found()
        .insert_header(("Location", USER_HOME))
        .finish())
}

#[derive(Deserialize)]
struct MediaListPage {
    page: u32,
}

pub struct PaginationData {
    pub url: Cow<'static, str>,
    pub page_size: u32,
    pub item_count: u32,

    pub current_page: u32,
}

impl PaginationData {
    pub fn new<U>(url: U, page_size: u32, item_count: u32, current_page: u32) -> Self
    where
        U: Into<Cow<'static, str>>,
    {
        Self {
            url: url.into(),
            page_size,
            item_count,
            current_page,
        }
    }

    pub fn url<P: std::fmt::Display>(&self, page: P) -> String {
        format!("{}?page={}", self.url, page)
    }

    pub fn last_page(&self) -> u32 {
        self.item_count / self.page_size
    }

    pub fn previous_page(&self) -> Option<u32> {
        if self.current_page == 0 {
            None
        } else {
            Some(self.current_page - 1)
        }
    }

    pub fn next_page(&self) -> Option<u32> {
        if (self.current_page + 1) * 25 >= self.item_count {
            None
        } else {
            Some(self.current_page + 1)
        }
    }

    pub fn display_lower(&self) -> u32 {
        std::cmp::max(1, self.current_page.saturating_sub(2))
    }

    pub fn display_upper(&self) -> u32 {
        std::cmp::min(self.last_page(), self.current_page + 3)
    }
}

#[derive(Template)]
#[template(path = "user/media/list.html")]
struct MediaList {
    media: Vec<models::OwnedMediaItem>,
    event_counts: HashMap<Uuid, i64>,
    pagination_data: PaginationData,
}

#[get("/list")]
async fn media_list(
    conn: web::Data<sqlx::PgPool>,
    user: models::User,
    page: Option<web::Query<MediaListPage>>,
) -> Result<HttpResponse, Error> {
    let page = page.map(|page| page.page).unwrap_or(0);

    let media = models::OwnedMediaItem::media_page(&conn, user.id, page).await?;
    let ids: Vec<_> = media.iter().map(|item| item.id).collect();
    let event_counts = models::OwnedMediaItem::event_counts(&conn, &ids).await?;
    let count = models::OwnedMediaItem::count(&conn, user.id).await?;

    let pagination_data = PaginationData::new("/user/media/list", 25, count as u32, page);

    let body = MediaList {
        media,
        event_counts,
        pagination_data,
    }
    .render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[derive(Template)]
#[template(path = "user/email/add.html")]
struct AddEmail {
    error_message: Option<String>,
}

#[get("/add")]
async fn email_add(_user: models::User) -> Result<HttpResponse, Error> {
    let body = AddEmail {
        error_message: None,
    }
    .render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[derive(Deserialize)]
struct AddEmailForm {
    email: lettre::Address,
}

#[post("/add")]
async fn email_add_post(
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    faktory: web::Data<jobs::FaktoryClient>,
    user: models::User,
    form: web::Form<AddEmailForm>,
) -> Result<HttpResponse, Error> {
    let email = form.email.to_string();

    if email.len() > 120 {
        return Err(Error::UserError(
            "Email must be less than 120 characters.".into(),
        ));
    }

    if models::User::email_exists(&conn, &email).await? {
        return Err(Error::UserError("Email is already in use.".into()));
    }

    models::User::set_email(&conn, user.id, &email).await?;

    faktory
        .enqueue_job(
            jobs::JobInitiator::user(user.id),
            jobs::email_verification_job(user.id)?,
        )
        .await?;

    Ok(HttpResponse::Found()
        .insert_header(("Location", USER_HOME))
        .finish())
}

#[derive(Deserialize)]
struct EmailVerifyQuery {
    #[serde(rename = "u")]
    user_id: Uuid,
    #[serde(rename = "v")]
    verifier: Uuid,
}

#[get("/verify")]
async fn verify_email(
    conn_info: ConnectionInfo,
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    query: web::Query<EmailVerifyQuery>,
    user: Option<models::User>,
    session: actix_session::Session,
) -> Result<HttpResponse, Error> {
    if !models::User::verify_email(&conn, query.user_id, query.verifier).await? {
        return Err(Error::Missing);
    };

    if user.is_none() {
        let session_id = models::UserSession::create(
            &conn,
            query.user_id,
            models::UserSessionSource::email_verification(conn_info.realip_remote_addr()),
        )
        .await?;
        session.set_session_token(query.user_id, session_id)?;
    }

    Ok(HttpResponse::Found()
        .insert_header(("Location", USER_HOME))
        .finish())
}

pub fn service() -> Scope {
    web::scope("/user")
        .service(services![home, events, single])
        .service(web::scope("/account").service(services![
            account_link_get,
            account_link_post,
            account_remove,
            account_view,
            account_verify,
        ]))
        .service(web::scope("/media").service(services![media_list, media_view, media_remove]))
        .service(web::scope("/email").service(services![email_add, email_add_post, verify_email]))
}
