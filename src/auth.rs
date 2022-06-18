use std::pin::Pin;
use std::{collections::HashMap, future::Future};

use actix_session::Session;
use actix_web::{get, post, services, web, FromRequest, HttpResponse, Scope};
use askama::Template;
use chrono::TimeZone;
use hmac::Hmac;
use hmac::Mac;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zxcvbn::zxcvbn;

use crate::{models, routes::*, ClientIpAddr, Error};

pub trait FuzzySearchSessionToken {
    const TOKEN_NAME: &'static str;

    fn get_session_token(&self) -> Result<Option<SessionToken>, Error>;
    fn set_session_token(&self, user_id: Uuid, session_id: Uuid) -> Result<(), Error>;
}

impl FuzzySearchSessionToken for Session {
    const TOKEN_NAME: &'static str = "user-token";

    fn get_session_token(&self) -> Result<Option<SessionToken>, Error> {
        self.get::<SessionToken>(Self::TOKEN_NAME)
            .map_err(Into::into)
    }

    fn set_session_token(&self, user_id: Uuid, session_id: Uuid) -> Result<(), Error> {
        self.insert(
            Self::TOKEN_NAME,
            SessionToken {
                user_id,
                session_id,
            },
        )
        .map_err(Into::into)
    }
}

#[derive(Clone)]
pub struct TelegramLoginConfig {
    pub bot_username: String,
    pub auth_url: String,
    pub token: String,
}

#[derive(Template)]
#[template(path = "auth/register.html")]
struct Register<'a> {
    telegram_login: &'a TelegramLoginConfig,
    error_messages: Option<Vec<&'a str>>,
}

#[get("/register")]
async fn register_get(
    telegram_login: web::Data<TelegramLoginConfig>,
) -> Result<HttpResponse, Error> {
    let body = Register {
        telegram_login: &telegram_login,
        error_messages: None,
    }
    .render()?;

    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[derive(Deserialize)]
struct RegisterFormData {
    username: String,
    password: String,
    password_confirm: String,
}

#[post("/register")]
async fn register_post(
    telegram_login: web::Data<TelegramLoginConfig>,
    client_ip: ClientIpAddr,
    session: Session,
    pool: web::Data<sqlx::Pool<sqlx::Postgres>>,
    form: web::Form<RegisterFormData>,
) -> Result<HttpResponse, Error> {
    let mut error_messages = Vec::with_capacity(1);

    if form.username.len() < 5 {
        error_messages.push("Username must be 5 characters or greater.");
    }

    if form.username.len() > 24 {
        error_messages.push("Username must be less than 24 characters.");
    }

    if form.username.contains('@') {
        error_messages.push("Username may not contain the '@' symbol.");
    }

    if models::User::username_exists(&pool, &form.username).await? {
        error_messages.push("Username already in use.");
    }

    if form.password != form.password_confirm {
        error_messages.push("Passwords do not match.");
    }

    let estimate = zxcvbn(&form.password, &[&form.username]).map_err(Error::from_displayable)?;
    if estimate.score() < 3 {
        error_messages.push("Password must be longer or contain more symbols.");
    }

    if !error_messages.is_empty() {
        let body = Register {
            telegram_login: &telegram_login,
            error_messages: Some(error_messages),
        }
        .render()?;

        return Ok(HttpResponse::Ok().content_type("text/html").body(body));
    }

    let user_id = models::User::create(&pool, &form.username, &form.password).await?;
    let session_id = models::UserSession::create(
        &pool,
        user_id,
        models::UserSessionSource::Registration,
        client_ip.ip_addr.as_deref(),
    )
    .await?;

    session.set_session_token(user_id, session_id)?;

    Ok(HttpResponse::Found()
        .insert_header(("Location", USER_HOME))
        .finish())
}

#[derive(Template)]
#[template(path = "auth/login.html")]
struct Login<'a> {
    telegram_login: &'a TelegramLoginConfig,
    error_message: Option<&'a str>,
}

#[get("/login")]
async fn login_get(
    telegram_login: web::Data<TelegramLoginConfig>,
    session: Session,
) -> Result<HttpResponse, Error> {
    if session.get_session_token()?.is_some() {
        return Ok(HttpResponse::Found()
            .insert_header(("Location", USER_HOME))
            .finish());
    }

    let body = Login {
        telegram_login: &telegram_login,
        error_message: None,
    }
    .render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[derive(Deserialize)]
struct LoginFormData {
    username: String,
    password: String,
}

#[post("/login")]
async fn login_post(
    telegram_login: web::Data<TelegramLoginConfig>,
    client_ip: ClientIpAddr,
    session: Session,
    pool: web::Data<sqlx::Pool<sqlx::Postgres>>,
    form: web::Form<LoginFormData>,
) -> Result<HttpResponse, Error> {
    let user = models::User::lookup_by_login(&pool, &form.username, &form.password).await?;

    if let Some(user) = user {
        let session_id = models::UserSession::create(
            &pool,
            user.id,
            models::UserSessionSource::Login,
            client_ip.ip_addr.as_deref(),
        )
        .await?;
        session.set_session_token(user.id, session_id)?;

        Ok(HttpResponse::Found()
            .insert_header(("Location", USER_HOME))
            .finish())
    } else {
        let body = Login {
            telegram_login: &telegram_login,
            error_message: Some("Unknown username or password."),
        }
        .render()?;

        Ok(HttpResponse::Ok().content_type("text/html").body(body))
    }
}

#[get("/telegram")]
async fn telegram(
    telegram_login: web::Data<TelegramLoginConfig>,
    pool: web::Data<sqlx::PgPool>,
    client_ip: ClientIpAddr,
    query: web::Query<HashMap<String, String>>,
    session: Session,
    user: Option<models::User>,
) -> Result<HttpResponse, Error> {
    let mut query = query.into_inner();

    let hash = query.remove("hash").ok_or(Error::Missing)?;

    let hash = hex::decode(hash).map_err(actix_web::error::ErrorBadRequest)?;

    let id: i64 = query
        .get("id")
        .ok_or(Error::Missing)?
        .parse()
        .map_err(actix_web::error::ErrorBadRequest)?;

    let first_name = query.get("first_name").ok_or(Error::Missing)?.to_owned();

    let auth_date: i64 = query
        .get("auth_date")
        .ok_or(Error::Missing)?
        .parse()
        .map_err(actix_web::error::ErrorBadRequest)?;

    let auth_date = chrono::Utc.timestamp(auth_date, 0);
    if auth_date + chrono::Duration::minutes(15) < chrono::Utc::now() {
        return Err(actix_web::error::ErrorBadRequest("data too old").into());
    }

    let mut data: Vec<_> = query.into_iter().collect();
    data.sort_by(|(a, _), (b, _)| a.cmp(b));

    let data = data
        .into_iter()
        .map(|(key, value)| format!("{}={}", key, value))
        .collect::<Vec<_>>()
        .join("\n");

    let token = Sha256::digest(&telegram_login.token);

    let mut mac = Hmac::<Sha256>::new_from_slice(&token)
        .expect("hmac could not be constructed with provided token");
    mac.update(data.as_bytes());

    mac.verify_slice(&hash)
        .map_err(actix_web::error::ErrorBadRequest)?;

    let user_id = if let Some(user) = user {
        tracing::info!("user already authenticated, associating telegram account");

        if models::User::lookup_by_telegram_id(&pool, id)
            .await?
            .is_some()
        {
            tracing::warn!("telegram account is already associated to other account");

            return Err(Error::user_error(
                "Telegram account is already registered to another account.",
            ));
        }

        models::User::associate_telegram(&pool, user.id, id, &first_name).await?;

        user.id
    } else if let Some(user) = models::User::lookup_by_telegram_id(&pool, id).await? {
        tracing::info!("known telegram account");

        user.id
    } else {
        tracing::info!("new telegram account, creating account");

        models::User::create_telegram(&pool, id, &first_name).await?
    };

    let session_id = models::UserSession::create(
        &pool,
        user_id,
        models::UserSessionSource::Telegram,
        client_ip.ip_addr.as_deref(),
    )
    .await?;
    session.set_session_token(user_id, session_id)?;

    return Ok(HttpResponse::Found()
        .insert_header(("Location", USER_HOME))
        .finish());
}

#[post("/logout")]
async fn logout(
    session: Session,
    conn: web::Data<sqlx::PgPool>,
    redis: web::Data<redis::aio::ConnectionManager>,
) -> Result<HttpResponse, Error> {
    if let Ok(Some(token)) = session.get_session_token() {
        models::UserSession::destroy(&conn, &redis, token.session_id, token.user_id).await?;
    }

    session.purge();

    Ok(HttpResponse::Found()
        .insert_header(("Location", AUTH_LOGIN))
        .finish())
}

#[derive(Template)]
#[template(path = "auth/sessions.html")]
struct Sessions {
    current_session_id: Uuid,
    sessions: Vec<models::UserSession>,
}

#[get("/sessions")]
async fn sessions(
    session: Session,
    user: models::User,
    conn: web::Data<sqlx::PgPool>,
) -> Result<HttpResponse, Error> {
    let session_token = session
        .get_session_token()?
        .ok_or_else(|| Error::unknown_message("session must exist"))?;

    let sessions = models::UserSession::list(&conn, user.id).await?;

    let body = Sessions {
        current_session_id: session_token.session_id,
        sessions,
    }
    .render()?;
    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[derive(Deserialize)]
struct SessionsRemoveForm {
    session_id: Uuid,
}

#[post("/sessions/remove")]
async fn sessions_remove(
    session: Session,
    user: models::User,
    conn: web::Data<sqlx::PgPool>,
    redis: web::Data<redis::aio::ConnectionManager>,
    form: web::Form<SessionsRemoveForm>,
) -> Result<HttpResponse, Error> {
    let session_token = session
        .get_session_token()?
        .ok_or_else(|| Error::unknown_message("session must exist"))?;

    if session_token.session_id == form.session_id {
        return Err(Error::user_error("You cannot remove the current session."));
    }

    models::UserSession::destroy(&conn, &redis, form.session_id, user.id).await?;

    Ok(HttpResponse::Found()
        .insert_header(("Location", AUTH_SESSIONS))
        .finish())
}

pub fn service() -> Scope {
    web::scope("/auth").service(services![
        register_get,
        register_post,
        login_get,
        login_post,
        telegram,
        logout,
        sessions,
        sessions_remove,
    ])
}

#[derive(Serialize, Deserialize)]
pub struct SessionToken {
    pub user_id: Uuid,
    pub session_id: Uuid,
}

impl FromRequest for models::User {
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self, Self::Error>>>>;

    fn from_request(
        req: &actix_web::HttpRequest,
        payload: &mut actix_http::Payload,
    ) -> Self::Future {
        let session = Session::from_request(req, payload);
        let db = req
            .app_data::<web::Data<sqlx::Pool<sqlx::Postgres>>>()
            .expect("app was missing database connection")
            .clone();

        Box::pin(async move {
            let session = session.await.map_err(Error::from)?;

            let token = match session.get_session_token() {
                Ok(Some(token)) => token,
                Ok(None) => return Err(Error::Unauthorized),
                Err(err) => return Err(err),
            };

            match models::UserSession::check(&db, token.session_id, token.user_id).await {
                Ok(Some(user)) => Ok(user),
                Ok(None) => {
                    session.purge();
                    Err(Error::Unauthorized)
                }
                Err(err) => Err(err),
            }
        })
    }
}
