use std::collections::HashMap;

use actix_web::{get, post, services, web, HttpResponse, Scope};
use askama::Template;
use futures::TryStreamExt;
use rand::Rng;
use serde::Deserialize;
use sha2::Digest;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;

use crate::{
    auth::{self, FuzzySearchSessionToken},
    jobs,
    models::{self, setting, Site},
    routes::*,
    ClientIpAddr, Error,
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
    let user_item_count = models::OwnedMediaItem::user_item_count(&conn, user.id);
    let recent_media = models::OwnedMediaItem::recent_media(&conn, user.id);
    let monitored_accounts = models::LinkedAccount::owned_by_user(&conn, user.id);

    let ((item_count, total_content_size), recent_media, monitored_accounts) =
        futures::try_join!(user_item_count, recent_media, monitored_accounts)?;

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

#[derive(Template)]
#[template(path = "user/settings.html")]
struct Settings<'a> {
    telegram_login: &'a auth::TelegramLoginConfig,

    user: &'a models::User,
    saved: bool,

    email_notifications: setting::EmailNotifications,
    telegram_notifications: setting::TelegramNotifications,
}

#[get("/settings")]
async fn settings_get(
    telegram_login: web::Data<auth::TelegramLoginConfig>,
    conn: web::Data<sqlx::PgPool>,
    user: models::User,
) -> Result<HttpResponse, Error> {
    let email_notifications = models::UserSetting::get(&conn, user.id)
        .await?
        .unwrap_or_default();

    let telegram_notifications = models::UserSetting::get(&conn, user.id)
        .await?
        .unwrap_or_default();

    let body = Settings {
        telegram_login: &telegram_login,

        user: &user,
        saved: false,

        email_notifications,
        telegram_notifications,
    }
    .render()?;

    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[post("/settings")]
async fn settings_post(
    telegram_login: web::Data<auth::TelegramLoginConfig>,
    conn: web::Data<sqlx::PgPool>,
    user: models::User,
    form: web::Form<HashMap<String, String>>,
) -> Result<HttpResponse, Error> {
    tracing::trace!("got settings: {:?}", form);

    let email_notifications = form
        .get("email-notifications")
        .map(|value| setting::EmailNotifications(value == "on"))
        .unwrap_or_else(models::UserSettingItem::off_value);

    let telegram_notifications = form
        .get("telegram-notifications")
        .map(|value| setting::TelegramNotifications(value == "on"))
        .unwrap_or_else(models::UserSettingItem::off_value);

    let display_name = match form.get("display_name") {
        Some(display_name) if !display_name.is_empty() => Some(display_name.as_str()),
        _ => None,
    };

    futures::try_join!(
        models::UserSetting::set(&conn, user.id, &email_notifications),
        models::UserSetting::set(&conn, user.id, &telegram_notifications),
        models::User::update_display_name(&conn, user.id, display_name)
    )?;

    let user = models::User::lookup_by_id(&conn, user.id)
        .await?
        .ok_or(Error::Missing)?;

    let body = Settings {
        telegram_login: &telegram_login,

        user: &user,
        saved: true,

        email_notifications,
        telegram_notifications,
    }
    .render()?;

    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[post("/delete")]
async fn delete(
    conn: web::Data<sqlx::PgPool>,
    session: actix_session::Session,
    user: models::User,
) -> Result<HttpResponse, Error> {
    models::User::delete(&conn, user.id).await?;
    session.purge();

    return Ok(HttpResponse::Found()
        .insert_header(("Location", "/"))
        .finish());
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
            if size > 25_000_000 {
                return Err(Error::TooLarge(size));
            }

            file.write_all(&chunk).await?;
            hasher.update(&chunk);
            size += chunk.len();
        }

        if size == 0 {
            continue;
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
        Site::Weasyl => Some(serde_json::json!({ "verification_key": token })),
        _ => None,
    };

    let account = models::LinkedAccount::create(&conn, user.id, form.site, username, data).await?;

    Ok(HttpResponse::Found()
        .insert_header(("Location", format!("/user/account/{}", account.id)))
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

    if account.owner_id != user.id {
        return Err(Error::Missing);
    }

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
        .insert_header(("Location", format!("/user/account/{}", account.id)))
        .finish())
}

#[derive(Template)]
#[template(path = "user/media/view.html")]
struct MediaView<'a> {
    media: models::OwnedMediaItem,
    similar_image_events: &'a [(chrono::DateTime<chrono::Utc>, models::SimilarImage)],
    other_events: &'a [models::UserEvent],
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

    let (similar_events, other_events): (Vec<_>, Vec<_>) = recent_events
        .into_iter()
        .partition(|event| matches!(event.data, Some(models::UserEventData::SimilarImage(_))));

    let similar_events: Vec<_> = similar_events
        .into_iter()
        .filter_map(|event| match event.data {
            Some(models::UserEventData::SimilarImage(similar)) => Some((event.created_at, similar)),
            _ => None,
        })
        .collect();

    let body = MediaView {
        media,
        similar_image_events: &similar_events,
        other_events: &other_events,
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

#[derive(Debug, Deserialize)]
struct MediaListQuery {
    page: Option<u32>,
    sort: Option<models::MediaListSort>,
}

fn decode_query(query: &str) -> HashMap<&str, &str> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .collect()
}

fn encode_query(params: &HashMap<&str, &str>) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&")
}

pub struct PaginationData<'a> {
    uri: &'a actix_web::http::Uri,
    params: HashMap<&'a str, &'a str>,

    pub page_size: u32,
    pub item_count: u32,

    pub current_page: u32,
}

impl<'a> PaginationData<'a> {
    pub fn new(
        uri: &'a actix_web::http::Uri,
        page_size: u32,
        item_count: u32,
        current_page: u32,
    ) -> Self {
        let params = decode_query(uri.query().unwrap_or_default());

        Self {
            uri,
            params,

            page_size,
            item_count,
            current_page,
        }
    }

    pub fn url<P: std::fmt::Display>(&self, page: P) -> String {
        let page = page.to_string();

        let mut params = self.params.clone();
        params.insert("page", &page);

        format!("{}?{}", self.uri.path(), encode_query(&params))
    }

    pub fn last_page(&self) -> u32 {
        self.item_count.saturating_sub(1) / self.page_size
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
struct MediaList<'a> {
    media: &'a [models::OwnedMediaItem],
    sort: &'a str,
    pagination_data: PaginationData<'a>,
}

#[get("/list")]
async fn media_list(
    request: actix_web::HttpRequest,
    conn: web::Data<sqlx::PgPool>,
    user: models::User,
    query: web::Query<MediaListQuery>,
) -> Result<HttpResponse, Error> {
    let page = query.page.unwrap_or(0);
    let sort = query.sort.unwrap_or(models::MediaListSort::Added);

    let media = models::OwnedMediaItem::media_page(&conn, user.id, page, sort).await?;

    let count = models::OwnedMediaItem::count(&conn, user.id).await?;
    let pagination_data = PaginationData::new(request.uri(), 25, count as u32, page);

    let body = MediaList {
        media: &media,
        sort: sort.name(),
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
    client_ip: ClientIpAddr,
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
            models::UserSessionSource::email_verification(client_ip.ip_addr),
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
        .service(services![
            home,
            settings_get,
            settings_post,
            delete,
            events,
            single
        ])
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
