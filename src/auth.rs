use std::future::Future;
use std::pin::Pin;

use actix_session::Session;
use actix_web::{dev::ConnectionInfo, get, post, services, web, FromRequest, HttpResponse, Scope};
use askama::Template;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zxcvbn::zxcvbn;

use crate::{models, routes::*, Error};

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

#[derive(Template)]
#[template(path = "auth/register.html")]
struct Register<'a> {
    error_messages: Option<Vec<&'a str>>,
}

#[get("/register")]
async fn register_get() -> Result<HttpResponse, Error> {
    let body = Register {
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
    conn_info: ConnectionInfo,
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
            error_messages: Some(error_messages),
        }
        .render()?;

        return Ok(HttpResponse::Ok().content_type("text/html").body(body));
    }

    let user_id = models::User::create(&pool, &form.username, &form.password).await?;
    let session_id = models::UserSession::create(
        &pool,
        user_id,
        models::UserSessionSource::registration(conn_info.realip_remote_addr()),
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
    error_message: Option<&'a str>,
}

#[get("/login")]
async fn login_get(session: Session) -> Result<HttpResponse, Error> {
    if session.get_session_token()?.is_some() {
        return Ok(HttpResponse::Found()
            .insert_header(("Location", USER_HOME))
            .finish());
    }

    let body = Login {
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
    conn_info: ConnectionInfo,
    session: Session,
    pool: web::Data<sqlx::Pool<sqlx::Postgres>>,
    form: web::Form<LoginFormData>,
) -> Result<HttpResponse, Error> {
    let user = models::User::lookup_by_login(&pool, &form.username, &form.password).await?;

    if let Some(user) = user {
        let session_id = models::UserSession::create(
            &pool,
            user.id,
            models::UserSessionSource::login(conn_info.realip_remote_addr()),
        )
        .await?;
        session.set_session_token(user.id, session_id)?;

        Ok(HttpResponse::Found()
            .insert_header(("Location", USER_HOME))
            .finish())
    } else {
        let body = Login {
            error_message: Some("Unknown username or password."),
        }
        .render()?;

        Ok(HttpResponse::Ok().content_type("text/html").body(body))
    }
}

#[post("/logout")]
async fn logout(session: Session, conn: web::Data<sqlx::PgPool>) -> Result<HttpResponse, Error> {
    if let Ok(Some(token)) = session.get_session_token() {
        models::UserSession::destroy(&conn, token.session_id, token.user_id).await?;
    }

    session.purge();

    Ok(HttpResponse::Found()
        .insert_header(("Location", AUTH_LOGIN))
        .finish())
}

pub fn service() -> Scope {
    web::scope("/auth").service(services![
        register_get,
        register_post,
        login_get,
        login_post,
        logout
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
