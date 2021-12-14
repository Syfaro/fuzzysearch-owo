use std::borrow::Cow;

use actix_http::{http, StatusCode};
use actix_web::HttpResponseBuilder;
use askama::Template;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unknown error")]
    Unknown,
    #[error("unknown error: {0}")]
    UnknownMessage(Cow<'static, str>),
    #[error("user error: {0}")]
    UserError(Cow<'static, str>),

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("template error: {0}")]
    Template(#[from] askama::Error),
    #[error("redis error: {0}")]
    Redis(#[from] redis::RedisError),
    #[error("faktory error: {0}")]
    Faktory(faktory::FaktoryError),
    #[error("actix error: {0}")]
    Actix(#[from] actix_web::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("s3 error")]
    S3(String),
    #[error("reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("email content error: {0}")]
    EmailContent(#[from] lettre::error::Error),
    #[error("email transport error: {0}")]
    EmailTransport(#[from] lettre::transport::smtp::Error),

    #[error("request too large: {0}")]
    TooLarge(usize),

    #[error("loading error: {0}")]
    LoadingError(String),

    #[error("resource missing")]
    Missing,
}

impl Error {
    fn error_message(&self) -> Cow<'static, str> {
        match self {
            Self::Unknown | Self::UnknownMessage(_) => "An unknown error occured.".into(),
            Self::Template(_) => "Page could not be rendered.".into(),
            Self::Missing => "Resource could not be found.".into(),
            Self::Image(err) => format!("Image could not be handled: {}", err.to_string()).into(),
            Self::TooLarge(_size) => "Request body too large.".into(),
            Self::LoadingError(msg) => format!("Error loading resource: {}", msg).into(),
            Self::UserError(msg) => msg.to_owned(),
            _ => "An internal server error occured.".into(),
        }
    }

    /// If we should retry this error when encountered in a job.
    #[allow(clippy::match_like_matches_macro)]
    pub fn should_retry(&self) -> bool {
        match self {
            Error::Missing => false,
            Error::TooLarge(_) => false,
            Error::EmailContent(_) => false,
            _ => true,
        }
    }
}

impl Error {
    pub fn from_displayable<D: std::fmt::Display>(displayable: D) -> Self {
        let display = displayable.to_string();

        Self::UnknownMessage(display.into())
    }
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage<'a> {
    error_message: &'a str,
    status_line: String,
}

impl actix_web::error::ResponseError for Error {
    fn status_code(&self) -> actix_http::StatusCode {
        match self {
            Self::Actix(err) => err.as_response_error().status_code(),
            Self::Missing => StatusCode::NOT_FOUND,
            Self::Image(_) | Self::UserError(_) => StatusCode::BAD_REQUEST,
            Self::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn error_response(&self) -> actix_web::HttpResponse {
        if let Error::Actix(err) = self {
            return err.as_response_error().error_response();
        }

        let code = self.status_code();

        let status_line = match code.canonical_reason() {
            Some(reason) => format!("Error {}: {}", code.as_str(), reason),
            None => format!("Error {}", code.as_str()),
        };

        let page = ErrorPage {
            error_message: &self.error_message(),
            status_line,
        }
        .render();

        let mut response = HttpResponseBuilder::new(self.status_code());

        match page {
            Ok(page) => response
                .insert_header((http::header::CONTENT_TYPE, "text/html"))
                .body(page),
            _ => response
                .insert_header((http::header::CONTENT_TYPE, "text/plain"))
                .body("could not render error page"),
        }
    }
}
