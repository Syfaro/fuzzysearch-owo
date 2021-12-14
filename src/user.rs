use actix_web::{get, post, services, web, HttpResponse, Scope};
use actix_web::{HttpRequest, Responder};
use askama::Template;
use futures::TryStreamExt;
use serde::Deserialize;
use sha2::Digest;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;

use crate::{jobs, models, routes::*, Error};

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
    req: HttpRequest,
) -> Result<HttpResponse, Error> {
    let events = models::UserEvent::recent_events(&conn, user.id).await?;

    Ok(web::Json(events).respond_to(&req))
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
            .await
            .map_err(Error::from_displayable)?;
    }

    Ok(HttpResponse::Found()
        .insert_header(("Location", USER_HOME))
        .finish())
}

#[derive(Template)]
#[template(path = "user/account_link.html")]
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
    username: String,
}

#[post("/link")]
async fn account_link_post(
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    faktory: web::Data<jobs::FaktoryClient>,
    user: models::User,
    form: web::Form<AccountLinkForm>,
) -> Result<HttpResponse, Error> {
    if !user.has_verified_email() {
        return Err(Error::UserError(
            "You must verify your email address first.".into(),
        ));
    }

    let account_id =
        models::LinkedAccount::create(&conn, user.id, form.site, &form.username, None).await?;

    faktory
        .enqueue_job(
            jobs::JobInitiator::user(user.id),
            jobs::add_account_job(user.id, account_id)?,
        )
        .await
        .map_err(Error::from_displayable)?;

    Ok(HttpResponse::Found()
        .insert_header((
            "Location",
            format!("/user/account/{}", account_id.to_string()),
        ))
        .finish())
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
) -> Result<HttpResponse, Error> {
    models::LinkedAccount::remove(&conn, user.id, form.account_id).await?;

    Ok(HttpResponse::Found()
        .insert_header(("Location", USER_HOME))
        .finish())
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
) -> Result<HttpResponse, Error> {
    let account_id: Uuid = path.into_inner().0;
    let account = models::LinkedAccount::lookup_by_id(&conn, account_id, user.id)
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

#[derive(Template)]
#[template(path = "user/add_email.html")]
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

#[derive(askama::Template)]
#[template(path = "email/verify.txt")]
struct EmailVerify<'a> {
    username: &'a str,
    link: &'a str,
}

#[post("/add")]
async fn email_add_post(
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    config: web::Data<crate::Config>,
    user: models::User,
    form: web::Form<AddEmailForm>,
) -> Result<HttpResponse, Error> {
    let body = EmailVerify {
        username: "Syfaro",
        link: &format!(
            "{}/user/email/verify?u={}&v={}",
            config.host_url,
            user.id,
            user.email_verifier.unwrap_or_else(Uuid::new_v4)
        ),
    }
    .render()?;

    models::User::set_email(&conn, user.id, &form.email.to_string()).await?;

    let email = lettre::Message::builder()
        .from(config.smtp_from.clone())
        .reply_to(config.smtp_reply_to.clone())
        .to(lettre::message::Mailbox::new(
            Some(user.username),
            form.email.clone(),
        ))
        .subject("Verify your FuzzySearch OwO account email address")
        .body(body)?;

    let creds = lettre::transport::smtp::authentication::Credentials::new(
        config.smtp_username.clone(),
        config.smtp_password.clone(),
    );

    let mailer: lettre::AsyncSmtpTransport<lettre::Tokio1Executor> =
        lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::relay(&config.smtp_host)
            .unwrap()
            .credentials(creds)
            .build();

    lettre::AsyncTransport::send(&mailer, email).await.unwrap();

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
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    query: web::Query<EmailVerifyQuery>,
    user: Option<models::User>,
    session: actix_session::Session,
) -> Result<HttpResponse, Error> {
    if !models::User::verify_email(&conn, query.user_id, query.verifier).await? {
        return Err(Error::Missing);
    };

    if user.is_none() {
        session.insert("user-id", query.user_id)?;
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
            account_view
        ]))
        .service(web::scope("/media").service(services![media_view, media_remove]))
        .service(web::scope("/email").service(services![email_add, email_add_post, verify_email]))
}
