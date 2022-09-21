use std::collections::HashMap;

use actix_web::{get, post, services, web, HttpResponse, Scope};
use askama::Template;
use foxlib::jobs::{FaktoryProducer, JobQueue};
use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{
    jobs::{JobInitiator, JobInitiatorExt, NewSubmissionJob},
    models, Error, WrappedTemplate,
};

pub fn service() -> Scope {
    web::scope("/admin").service(services![
        admin,
        inject_post,
        job_manual,
        subreddit_add,
        subreddit_state,
        flist_abort
    ])
}

#[derive(Template)]
#[template(path = "admin/index.html")]
struct Admin {
    subreddits: Vec<models::RedditSubreddit>,
    recent_flist_runs: Vec<models::FListImportRun>,
}

#[get("/tools")]
async fn admin(
    request: actix_web::HttpRequest,
    conn: web::Data<sqlx::PgPool>,
    user: models::User,
) -> Result<HttpResponse, Error> {
    if !user.is_admin {
        return Err(actix_web::error::ErrorUnauthorized("Unauthorized").into());
    }

    let subreddits = models::RedditSubreddit::subreddits(&conn);
    let recent_flist_runs = models::FListImportRun::recent_runs(&conn);

    let (subreddits, recent_flist_runs) = futures::try_join!(subreddits, recent_flist_runs)?;

    let body = Admin {
        subreddits,
        recent_flist_runs,
    }
    .wrap(&request, Some(&user))
    .await
    .render()?;

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
    faktory: web::Data<FaktoryProducer>,
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

    faktory
        .enqueue_job(NewSubmissionJob(sub).initiated_by(JobInitiator::user(user.id)))
        .await?;

    Ok(HttpResponse::Found()
        .insert_header(("Location", "/admin/tools"))
        .finish())
}

#[derive(Deserialize)]
struct SubredditForm {
    subreddit_name: String,
}

#[post("/subreddit/add")]
async fn subreddit_add(
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

#[post("/subreddit/state")]
async fn subreddit_state(
    conn: web::Data<sqlx::PgPool>,
    user: models::User,
    form: web::Form<SubredditForm>,
) -> Result<HttpResponse, Error> {
    if !user.is_admin {
        return Err(actix_web::error::ErrorUnauthorized("Unauthorized").into());
    }

    let subreddit = match models::RedditSubreddit::get_by_name(&conn, &form.subreddit_name).await? {
        Some(subreddit) => subreddit,
        _ => return Err(actix_web::error::ErrorNotFound("Unknown subreddit").into()),
    };

    let new_state = !subreddit.disabled;

    models::RedditSubreddit::update_state(&conn, &form.subreddit_name, new_state).await?;

    Ok(HttpResponse::Found()
        .insert_header(("Location", "/admin/tools"))
        .finish())
}

#[derive(Deserialize)]
struct FListImportRunForm {
    import_run_id: uuid::Uuid,
}

#[post("/flist/abort")]
async fn flist_abort(
    conn: web::Data<sqlx::PgPool>,
    user: models::User,
    form: web::Form<FListImportRunForm>,
) -> Result<HttpResponse, Error> {
    if !user.is_admin {
        return Err(actix_web::error::ErrorUnauthorized("Unauthorized").into());
    }

    models::FListImportRun::abort_run(&conn, form.import_run_id).await?;

    Ok(HttpResponse::Found()
        .insert_header(("Location", "/admin/tools"))
        .finish())
}

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ManualJobType {
    Single,
    EachAccount,
    EachUser,
}

#[derive(Deserialize)]
struct ManualJobForm {
    job_type: ManualJobType,
    name: String,
    data: String,
}

#[post("/job/manual")]
async fn job_manual(
    conn: web::Data<sqlx::PgPool>,
    faktory: web::Data<FaktoryProducer>,
    user: models::User,
    form: web::Form<ManualJobForm>,
) -> Result<HttpResponse, Error> {
    if !user.is_admin {
        return Err(actix_web::error::ErrorUnauthorized("Unauthorized").into());
    }

    let data = if form.data.is_empty() {
        None
    } else {
        Some(serde_json::from_str(&form.data)?)
    };

    let mut job_payloads: Vec<Option<serde_json::Value>> = Vec::new();

    if form.job_type == ManualJobType::Single {
        job_payloads.push(data);
    } else {
        let field_name = match form.job_type {
            ManualJobType::EachAccount => "account_id",
            ManualJobType::EachUser => "user_id",
            ManualJobType::Single => unreachable!(),
        };

        let ids = match form.job_type {
            ManualJobType::EachAccount => {
                sqlx::query_file_scalar!("queries/admin/account_ids.sql")
                    .fetch_all(&**conn)
                    .await?
            }
            ManualJobType::EachUser => {
                sqlx::query_file_scalar!("queries/admin/user_ids.sql")
                    .fetch_all(&**conn)
                    .await?
            }
            ManualJobType::Single => unreachable!(),
        };

        for id in ids {
            let payload = data.clone().map_or_else(
                || {
                    let mut map = serde_json::Map::new();
                    map.insert(field_name.to_string(), id.to_string().into());

                    serde_json::Value::Object(map)
                },
                |mut value| {
                    value[field_name] = id.to_string().into();
                    value
                },
            );

            job_payloads.push(Some(payload));
        }
    }

    let custom: HashMap<String, serde_json::Value> = [(
        "initiator".to_string(),
        serde_json::to_value(JobInitiator::user(user.id))?,
    )]
    .into_iter()
    .collect();

    for payload in job_payloads {
        let args = match payload {
            Some(payload) => vec![payload],
            None => vec![],
        };

        let mut job = foxlib::jobs::FaktoryJob::new(form.name.clone(), args);
        job.queue = crate::jobs::Queue::Core.queue_name().to_string();
        job.custom = custom.clone();

        faktory.enqueue_existing_job(job).await?;
    }

    Ok(HttpResponse::Found()
        .insert_header(("Location", "/admin/tools"))
        .finish())
}
