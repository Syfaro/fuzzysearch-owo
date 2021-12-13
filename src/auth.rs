use std::future::Future;
use std::pin::Pin;

use actix_session::Session;
use actix_web::{
    error::ErrorUnauthorized, get, post, services, web, FromRequest, HttpResponse, Scope,
};
use askama::Template;
use serde::Deserialize;
use uuid::Uuid;
use zxcvbn::zxcvbn;

use crate::{models, routes::*, Error};

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
    session: Session,
    pool: web::Data<sqlx::Pool<sqlx::Postgres>>,
    form: web::Form<RegisterFormData>,
) -> Result<HttpResponse, Error> {
    let mut error_messages = Vec::with_capacity(1);

    if form.username.len() < 5 {
        error_messages.push("Username must be 5 characters or greater.");
    }

    if models::User::username_exists(&pool, &form.username).await? {
        error_messages.push("Username already in use.");
    }

    if form.password != form.password_confirm {
        error_messages.push("Passwords do not match.");
    }

    let estimate = zxcvbn(&form.password, &[&form.username]).map_err(Error::from_displayable)?;
    if estimate.score() < 3 {
        error_messages.push("Password must be longer or contain more special symbols.");
    }

    if !error_messages.is_empty() {
        let body = Register {
            error_messages: Some(error_messages),
        }
        .render()?;

        return Ok(HttpResponse::Ok().content_type("text/html").body(body));
    }

    let user_id = models::User::create(&pool, &form.username, &form.password).await?;

    session.insert("user-id", user_id)?;

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
    if session.get::<Uuid>("user-id")?.is_some() {
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
    session: Session,
    pool: web::Data<sqlx::Pool<sqlx::Postgres>>,
    form: web::Form<LoginFormData>,
) -> Result<HttpResponse, Error> {
    let user = models::User::lookup_by_login(&pool, &form.username, &form.password).await?;

    if let Some(user) = user {
        session.insert("user-id", user.id)?;

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
async fn logout(session: Session) -> HttpResponse {
    session.purge();

    HttpResponse::Found()
        .insert_header(("Location", AUTH_LOGIN))
        .finish()
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

            let user_id = match session.get::<Uuid>("user-id") {
                Ok(Some(user_id)) => user_id,
                Ok(None) => return Err(ErrorUnauthorized("user not authenticated").into()),
                Err(err) => return Err(err.into()),
            };

            match models::User::lookup_by_id(&db, user_id).await {
                Ok(Some(user)) => Ok(user),
                Ok(None) => {
                    session.purge();
                    Err(ErrorUnauthorized("user not authenticated").into())
                }
                Err(err) => Err(err),
            }
        })
    }
}
