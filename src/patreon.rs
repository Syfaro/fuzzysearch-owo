use actix_web::{get, post, services, web, HttpRequest, HttpResponse, Scope};
use hmac::{Hmac, Mac};
use oauth2::{
    basic::BasicClient, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, RedirectUrl,
    TokenResponse, TokenUrl,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    jobs,
    models::{self, Site},
    Error,
};

fn get_oauth_client(config: &crate::Config) -> Result<BasicClient, Error> {
    let client_id = ClientId::new(config.patreon_client_id.clone());
    let client_secret = ClientSecret::new(config.patreon_client_secret.clone());

    let auth_url = AuthUrl::new("https://www.patreon.com/oauth2/authorize".to_string())
        .map_err(Error::from_displayable)?;
    let token_url = TokenUrl::new("https://www.patreon.com/api/oauth2/token".to_string())
        .map_err(Error::from_displayable)?;

    let redirect_url = RedirectUrl::new(format!("{}/patreon/callback", config.host_url))
        .map_err(Error::from_displayable)?;

    Ok(
        BasicClient::new(client_id, Some(client_secret), auth_url, Some(token_url))
            .set_redirect_uri(redirect_url),
    )
}

pub fn get_authenticated_client(
    config: &crate::Config,
    token: &str,
) -> Result<reqwest::Client, Error> {
    use reqwest::header;

    let mut headers = header::HeaderMap::new();

    let mut auth_value = header::HeaderValue::from_str(&format!("Bearer {}", token))
        .map_err(Error::from_displayable)?;
    auth_value.set_sensitive(true);
    headers.insert(header::AUTHORIZATION, auth_value);

    let client = reqwest::ClientBuilder::default()
        .default_headers(headers)
        .user_agent(&config.user_agent)
        .build()?;

    Ok(client)
}

#[get("/auth")]
async fn auth(
    config: web::Data<crate::Config>,
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    user: models::User,
) -> Result<HttpResponse, Error> {
    let client = get_oauth_client(&config)?;

    let (authorize_url, csrf_state) = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(oauth2::Scope::new("identity".to_string()))
        .add_scope(oauth2::Scope::new("campaigns".to_string()))
        .add_scope(oauth2::Scope::new("campaigns.posts".to_string()))
        .add_scope(oauth2::Scope::new("w:campaigns.webhook".to_string()))
        .url();

    models::AuthState::create(&conn, user.id, csrf_state.secret()).await?;

    Ok(HttpResponse::Found()
        .insert_header(("Location", authorize_url.as_str()))
        .finish())
}

#[derive(Debug, Deserialize)]
struct PatreonOAuthCallback {
    code: String,
    state: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PatreonData<T> {
    pub data: T,
    pub meta: Option<PatreonMeta>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PatreonMeta {
    pub pagination: Option<PatreonPagination>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PatreonPagination {
    pub total: i32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PatreonDataItem<A, R = ()> {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<A>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relationships: Option<R>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PatreonIdentityAttributes {
    full_name: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct PatreonRelationshipsCampaign<A, R = ()> {
    campaign: PatreonData<PatreonDataItem<A, R>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PatreonWebhookDataAttributes {
    paused: bool,
    secret: String,
    triggers: Vec<String>,
    uri: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PatreonPostAttributes {
    pub embed_data: Option<serde_json::Value>,
    pub embed_url: Option<String>,
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    pub title: Option<String>,
    pub url: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct PatreonCredentials {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_after: chrono::DateTime<chrono::Utc>,
}

#[get("/callback")]
async fn callback(
    config: web::Data<crate::Config>,
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    faktory: web::Data<jobs::FaktoryClient>,
    user: models::User,
    query: web::Query<PatreonOAuthCallback>,
) -> Result<HttpResponse, Error> {
    if !models::AuthState::lookup(&conn, user.id, &query.state).await? {
        return Err(Error::Missing);
    }

    let client = get_oauth_client(&config)?;
    let code = AuthorizationCode::new(query.code.clone());

    let token = client
        .exchange_code(code)
        .request_async(oauth2::reqwest::async_http_client)
        .await
        .map_err(|_err| Error::UserError("Could not authenticate with Patreon".into()))?;

    models::AuthState::remove(&conn, user.id, &query.state).await?;

    let client = get_authenticated_client(&config, token.access_token().secret())?;

    let identity: PatreonData<
        PatreonDataItem<PatreonIdentityAttributes, PatreonRelationshipsCampaign<serde_json::Value>>,
    > = client
        .get("https://www.patreon.com/api/oauth2/v2/identity")
        .query(&[("include", "campaign"), ("fields[user]", "full_name")])
        .send()
        .await?
        .json()
        .await?;

    let patreon_campaign_id = identity
        .data
        .relationships
        .as_ref()
        .ok_or_else(|| Error::UserError("No associated campaign".into()))?
        .campaign
        .data
        .id
        .as_str();

    let credentials = PatreonCredentials {
        access_token: token.access_token().secret().to_string(),
        refresh_token: token
            .refresh_token()
            .ok_or(Error::Missing)?
            .secret()
            .to_string(),
        expires_after: chrono::Utc::now()
            + chrono::Duration::from_std(
                token
                    .expires_in()
                    .unwrap_or(std::time::Duration::from_secs(3600)),
            )
            .expect("expires in too large"),
    };

    let saved_data = SavedPatreonData {
        site_id: patreon_campaign_id.to_string(),
        credentials: credentials.clone(),
        campaign: None,
    };

    let linked_account = match models::LinkedAccount::lookup_by_site_id(
        &conn,
        user.id,
        Site::Patreon,
        patreon_campaign_id,
    )
    .await?
    {
        Some(account) => account,
        None => {
            tracing::info!("got new campaign");

            let full_name = identity
                .data
                .attributes
                .map(|attributes| attributes.full_name)
                .unwrap_or_else(|| "Unknown campaign".to_string());

            let account = models::LinkedAccount::create(
                &conn,
                user.id,
                Site::Patreon,
                &full_name,
                Some(serde_json::to_value(saved_data)?),
            )
            .await?;

            faktory
                .enqueue_job(
                    jobs::JobInitiator::user(user.id),
                    jobs::add_account_job(user.id, account.id)?,
                )
                .await?;

            account
        }
    };

    let webhooks: PatreonData<Vec<PatreonDataItem<PatreonWebhookDataAttributes>>> = client
        .get("https://www.patreon.com/api/oauth2/v2/webhooks")
        .query(&[("fields[webhook]", "paused,secret,triggers,uri")])
        .send()
        .await?
        .json()
        .await?;

    if webhooks.data.len() > 1 {
        return Err(Error::UnknownMessage(
            "Too many webhooks were created".into(),
        ));
    }

    let url = format!(
        "{}/patreon/webhook?id={}",
        config.host_url, linked_account.id
    );

    let (webhook_id, webhook_secret) = if let Some(webhook) = webhooks.data.into_iter().next() {
        tracing::info!("patreon had one webhook, checking that data is correct");

        let attributes = webhook.attributes.ok_or(Error::Missing)?;

        if attributes.uri != url {
            tracing::warn!("patreon webhook had incorrect url");

            let resp: serde_json::Value = client
                .patch(format!(
                    "https://www.patreon.com/api/oauth2/v2/webhooks/{}",
                    webhook.id
                ))
                .json(&serde_json::json!({
                    "data": {
                        "id": webhook.id,
                        "type": "webhook",
                        "attributes": {
                            "triggers": ["posts:publish", "posts:update", "posts:delete"],
                            "uri": url,
                            "paused": false,
                        },
                    }
                }))
                .send()
                .await?
                .json()
                .await?;

            tracing::debug!("got patreon webhook patch response: {:?}", resp);
        }

        (webhook.id, attributes.secret)
    } else {
        tracing::info!("patreon had no webhooks, creating");

        create_webhook(&client, &url, patreon_campaign_id).await?
    };

    let campaign = extract_campaign(linked_account.data)?;
    let current_campaign = Campaign {
        webhook_id,
        webhook_secret,
    };

    if campaign.as_ref() != Some(&current_campaign) {
        tracing::warn!("database had outdated webhook information");

        models::LinkedAccount::update_data(
            &conn,
            linked_account.id,
            Some(serde_json::to_value(SavedPatreonData {
                site_id: patreon_campaign_id.to_string(),
                credentials,
                campaign: Some(current_campaign),
            })?),
        )
        .await?;
    }

    Ok(HttpResponse::Found()
        .insert_header((
            "Location",
            format!("/user/account/{}", linked_account.id.to_string()),
        ))
        .finish())
}

async fn create_webhook(
    client: &reqwest::Client,
    url: &str,
    patreon_campaign_id: &str,
) -> Result<(String, String), Error> {
    let webhook: PatreonData<PatreonDataItem<PatreonWebhookDataAttributes>> = client
        .post("https://www.patreon.com/api/oauth2/v2/webhooks")
        .json(&serde_json::json!({
            "data": {
                "type": "webhook",
                "attributes": {
                    "triggers": ["posts:publish", "posts:update", "posts:delete"],
                    "uri": url,
                },
                "relationships": {
                    "campaign": {
                        "data": {
                            "type": "campaign",
                            "id": patreon_campaign_id,
                        }
                    }
                }
            }
        }))
        .send()
        .await?
        .json()
        .await?;

    let attributes = webhook.data.attributes.ok_or(Error::Missing)?;

    Ok((webhook.data.id, attributes.secret))
}

#[derive(Debug, Deserialize)]
struct WebhookPostQuery {
    id: Uuid,
}

#[post("/webhook")]
async fn webhook_post(
    conn: web::Data<sqlx::Pool<sqlx::Postgres>>,
    req: HttpRequest,
    query: web::Query<WebhookPostQuery>,
    body: web::Bytes,
) -> Result<HttpResponse, Error> {
    let account = models::LinkedAccount::lookup_by_id(&conn, query.id)
        .await?
        .ok_or(Error::Missing)?;

    let campaign = extract_campaign(account.data)?.ok_or(Error::Missing)?;

    let headers = req.headers();
    let signature = headers.get("x-patreon-signature").ok_or(Error::Missing)?;
    let signature = hex::decode(&signature.as_bytes())
        .map_err(|_err| Error::UnknownMessage("invalid signature hex".into()))?;

    let mut mac: Hmac<md5::Md5> = Hmac::new_from_slice(campaign.webhook_secret.as_bytes())
        .map_err(Error::from_displayable)?;
    mac.update(&body);

    mac.verify_slice(&signature)
        .map_err(|_err| Error::UnknownMessage("invalid signature".into()))?;

    let data = serde_json::from_slice(&body)?;

    let id = models::PatreonWebhookEvent::log(&conn, account.id, data).await?;
    tracing::info!("inserted patreon webhook event: {}", id);

    Ok(HttpResponse::Ok().body("OK"))
}

#[derive(Deserialize, Serialize)]
pub struct SavedPatreonData {
    pub site_id: String,
    pub credentials: PatreonCredentials,
    pub campaign: Option<Campaign>,
}

#[derive(Deserialize, Serialize, PartialEq)]
pub struct Campaign {
    pub webhook_id: String,
    pub webhook_secret: String,
}

fn extract_campaign(data: Option<serde_json::Value>) -> Result<Option<Campaign>, Error> {
    tracing::trace!("attempting to extract campaign data from data");

    let data = match data {
        Some(data) => data,
        None => return Ok(None),
    };

    let data: Option<SavedPatreonData> = serde_json::from_value(data)?;

    Ok(data.and_then(|data| data.campaign))
}

pub fn service() -> Scope {
    web::scope("/patreon").service(services![auth, callback, webhook_post])
}
