use actix_web::{get, post, services, web, HttpResponse, Scope};
use askama::Template;
use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{jobs, models, Error};

pub fn service() -> Scope {
    web::scope("/admin").service(services![admin, inject_post, subreddit_post])
}

#[derive(Template)]
#[template(path = "admin/index.html")]
struct Admin {
    subreddits: Vec<models::RedditSubreddit>,
}

#[get("/tools")]
async fn admin(conn: web::Data<sqlx::PgPool>, user: models::User) -> Result<HttpResponse, Error> {
    if !user.is_admin {
        return Err(actix_web::error::ErrorUnauthorized("Unauthorized").into());
    }

    let subreddits = models::RedditSubreddit::subreddits(&conn).await?;

    let body = Admin { subreddits }.render()?;

    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

fn generate_id() -> String {
    rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(24)
        .map(char::from)
        .collect()
}

#[derive(Deserialize)]
struct InjectForm {
    content_url: String,
    page_url: String,
    posted_by: String,
}

#[post("/inject")]
async fn inject_post(
    faktory: web::Data<jobs::FaktoryClient>,
    user: models::User,
    form: web::Form<InjectForm>,
) -> Result<HttpResponse, Error> {
    if !user.is_admin {
        return Err(actix_web::error::ErrorUnauthorized("Unauthorized").into());
    }

    let bytes = reqwest::get(&form.content_url).await?.bytes().await?;

    let sha = Sha256::digest(&bytes);
    let im = image::load_from_memory(&bytes)?;

    let hasher = fuzzysearch_common::get_hasher();
    let hash = hasher.hash_image(&im);
    let hash = hash.as_bytes().try_into().expect("hash was wrong length");

    let sub = crate::jobs::IncomingSubmission {
        site: models::Site::InternalTesting,
        site_id: generate_id(),
        posted_by: if !form.posted_by.is_empty() {
            Some(form.posted_by.clone())
        } else {
            None
        },
        sha256: Some(sha.try_into().expect("sha256 was wrong length")),
        perceptual_hash: Some(hash),
        content_url: form.content_url.clone(),
        page_url: if !form.page_url.is_empty() {
            Some(form.page_url.clone())
        } else {
            None
        },
        posted_at: None,
    };

    let job = jobs::new_submission_job(sub)?;

    faktory
        .enqueue_job(jobs::JobInitiator::user(user.id), job)
        .await?;

    Ok(HttpResponse::Found()
        .insert_header(("Location", "/admin/tools"))
        .finish())
}

#[derive(Deserialize)]
struct SubredditForm {
    subreddit_name: String,
}

#[post("/subreddit")]
async fn subreddit_post(
    conn: web::Data<sqlx::PgPool>,
    user: models::User,
    form: web::Form<SubredditForm>,
) -> Result<HttpResponse, Error> {
    if !user.is_admin {
        return Err(actix_web::error::ErrorUnauthorized("Unauthorized").into());
    }

    models::RedditSubreddit::add(&conn, &form.subreddit_name).await?;

    Ok(HttpResponse::Found()
        .insert_header(("Location", "/admin/tools"))
        .finish())
}
