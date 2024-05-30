use actix_web::{get, post, services, web, HttpRequest, HttpResponse};
use async_trait::async_trait;
use foxlib::jobs::{FaktoryForge, FaktoryProducer};
use hmac::{Hmac, Mac};
use oauth2::{
    AccessToken, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, RedirectUrl,
    TokenResponse, TokenUrl,
};
use uuid::Uuid;

use crate::jobs::{JobContext, JobInitiatorExt};
use crate::models::{LinkedAccount, Site};
use crate::site::{CollectedSite, SiteFromConfig, SiteServices};
use crate::{jobs, models, AsUrl, Error};

pub struct Patreon {
    auth_url: AuthUrl,
    token_url: TokenUrl,

    client_id: ClientId,
    client_secret: ClientSecret,

    redirect_url: RedirectUrl,
}

impl Patreon {
    const AUTH_URL: &'static str = "https://www.patreon.com/oauth2/authorize";
    const TOKEN_URL: &'static str = "https://www.patreon.com/api/oauth2/token";

    fn new<S: Into<String>>(client_id: S, client_secret: S, base_url: S) -> Result<Self, Error> {
        let client_id = ClientId::new(client_id.into());
        let client_secret = ClientSecret::new(client_secret.into());

        let auth_url = AuthUrl::new(Self::AUTH_URL.to_string()).map_err(Error::from_displayable)?;
        let token_url = TokenUrl::new(Self::TOKEN_URL.into()).map_err(Error::from_displayable)?;

        let redirect_url = RedirectUrl::new(format!("{}/patreon/callback", base_url.into()))
            .map_err(Error::from_displayable)?;

        Ok(Self {
            auth_url,
            token_url,
            client_id,
            client_secret,
            redirect_url,
        })
    }

    fn get_oauth_client(&self) -> oauth2::basic::BasicClient {
        oauth2::basic::BasicClient::new(
            self.client_id.clone(),
            Some(self.client_secret.clone()),
            self.auth_url.clone(),
            Some(self.token_url.clone()),
        )
        .set_redirect_uri(self.redirect_url.clone())
    }

    async fn refresh_credentials(
        &self,
        conn: &sqlx::PgPool,
        redlock: &redlock::RedLock,
        data: &types::SavedPatreonData,
        account_id: Uuid,
    ) -> Result<AccessToken, Error> {
        super::refresh_credentials(
            redlock,
            account_id,
            data,
            || self.get_oauth_client(),
            |item| {
                Some((
                    item.credentials.refresh_token.clone(),
                    item.credentials.refresh_token.clone(),
                    item.credentials.expires_after,
                ))
            },
            || async {
                let account = models::LinkedAccount::lookup_by_id(conn, account_id)
                    .await?
                    .ok_or(Error::Missing)?;

                let data = account.data.ok_or(Error::Missing)?;
                let data: types::SavedPatreonData = serde_json::from_value(data)?;

                Ok(data)
            },
            |data, access_token, refresh_token, expires_after| async move {
                let data = serde_json::to_value(types::SavedPatreonData {
                    credentials: types::PatreonCredentials {
                        access_token,
                        refresh_token,
                        expires_after,
                    },
                    ..data
                })?;

                models::LinkedAccount::update_data(conn, account_id, Some(data)).await?;

                Ok(())
            },
        )
        .await
    }
}

#[async_trait(?Send)]
impl SiteFromConfig for Patreon {
    async fn site_from_config(config: &crate::Config) -> Result<Self, Error> {
        Self::new(
            &config.patreon_client_id,
            &config.patreon_client_secret,
            &config.host_url,
        )
    }
}

#[async_trait(?Send)]
impl CollectedSite for Patreon {
    fn oauth_page(&self) -> Option<&'static str> {
        Some("/patreon/auth")
    }

    fn register_jobs(&self, _forge: &mut FaktoryForge<jobs::JobContext, Error>) {}

    #[tracing::instrument(skip(self, ctx, account), fields(user_id = %account.owner_id, account_id = %account.id))]
    async fn add_account(&self, ctx: &JobContext, account: LinkedAccount) -> Result<(), Error> {
        models::LinkedAccount::update_loading_state(
            &ctx.conn,
            &ctx.redis,
            account.owner_id,
            account.id,
            models::LoadingState::DiscoveringItems,
        )
        .await?;

        let data: types::SavedPatreonData =
            serde_json::from_value(account.data.ok_or(Error::Missing)?)?;

        let token = self
            .refresh_credentials(&ctx.conn, &ctx.redlock, &data, account.id)
            .await?;
        let client = super::get_authenticated_client(&ctx.config, &token)?;

        let posts: types::PatreonData<Vec<types::PatreonDataItem<types::PatreonPostAttributes>>> =
            client
                .get(format!(
                    "https://www.patreon.com/api/oauth2/v2/campaigns/{}/posts",
                    data.site_id
                ))
                .query(&[(
                    "fields[post]",
                    "embed_data,embed_url,published_at,title,url",
                )])
                .send()
                .await?
                .json()
                .await?;

        tracing::info!("got page of posts: {:?}", posts);

        tracing::warn!("setting patreon to complete without loading");

        models::LinkedAccount::update_loading_state(
            &ctx.conn,
            &ctx.redis,
            account.owner_id,
            account.id,
            models::LoadingState::Complete,
        )
        .await?;

        Ok(())
    }
}

impl SiteServices for Patreon {
    fn services() -> Vec<actix_web::Scope> {
        vec![web::scope("/patreon").service(services![auth, callback, webhook_post])]
    }
}

#[get("/auth")]
async fn auth(
    config: web::Data<crate::Config>,
    conn: web::Data<sqlx::PgPool>,
    user: models::User,
) -> Result<HttpResponse, Error> {
    let patreon = Patreon::site_from_config(&config).await?;
    let client = patreon.get_oauth_client();

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

#[get("/callback")]
async fn callback(
    config: web::Data<crate::Config>,
    conn: web::Data<sqlx::PgPool>,
    faktory: web::Data<FaktoryProducer>,
    request: actix_web::HttpRequest,
    user: models::User,
    query: web::Query<types::PatreonOAuthCallback>,
) -> Result<HttpResponse, Error> {
    if !models::AuthState::lookup(&conn, user.id, &query.state).await? {
        return Err(Error::Missing);
    }

    let patreon = Patreon::site_from_config(&config).await?;
    let client = patreon.get_oauth_client();
    let code = AuthorizationCode::new(query.code.clone());

    let token = client
        .exchange_code(code)
        .request_async(oauth2::reqwest::async_http_client)
        .await
        .map_err(|_err| Error::UserError("Could not authenticate with Patreon".into()))?;

    models::AuthState::remove(&conn, user.id, &query.state).await?;

    let client = super::get_authenticated_client(&config, token.access_token())?;

    let identity: types::PatreonData<
        types::PatreonDataItem<
            types::PatreonIdentityAttributes,
            types::PatreonRelationshipsCampaign<serde_json::Value>,
        >,
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

    let credentials = types::PatreonCredentials {
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
                    .unwrap_or_else(|| std::time::Duration::from_secs(3600)),
            )
            .expect("expires in too large"),
    };

    let saved_data = types::SavedPatreonData {
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
                None,
            )
            .await?;

            faktory
                .enqueue_job(
                    jobs::AddAccountJob {
                        user_id: user.id,
                        account_id: account.id,
                    }
                    .initiated_by(jobs::JobInitiator::user(user.id)),
                )
                .await?;

            account
        }
    };

    let webhooks: types::PatreonData<
        Vec<types::PatreonDataItem<types::PatreonWebhookDataAttributes>>,
    > = client
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
    let current_campaign = types::Campaign {
        webhook_id,
        webhook_secret,
    };

    if campaign.as_ref() != Some(&current_campaign) {
        tracing::warn!("database had outdated webhook information");

        models::LinkedAccount::update_data(
            &conn,
            linked_account.id,
            Some(serde_json::to_value(types::SavedPatreonData {
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
            request
                .url_for("user_account", [linked_account.id.as_url()])?
                .as_str(),
        ))
        .finish())
}

#[post("/webhook")]
async fn webhook_post(
    conn: web::Data<sqlx::PgPool>,
    req: HttpRequest,
    query: web::Query<types::WebhookPostQuery>,
    body: web::Bytes,
) -> Result<HttpResponse, Error> {
    let account = models::LinkedAccount::lookup_by_id(&conn, query.id)
        .await?
        .ok_or(Error::Missing)?;

    let campaign = extract_campaign(account.data)?.ok_or(Error::Missing)?;

    let headers = req.headers();
    let signature = headers.get("x-patreon-signature").ok_or(Error::Missing)?;
    let signature = hex::decode(signature.as_bytes())
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

async fn create_webhook(
    client: &reqwest::Client,
    url: &str,
    patreon_campaign_id: &str,
) -> Result<(String, String), Error> {
    let webhook: types::PatreonData<types::PatreonDataItem<types::PatreonWebhookDataAttributes>> =
        client
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

fn extract_campaign(data: Option<serde_json::Value>) -> Result<Option<types::Campaign>, Error> {
    tracing::trace!("attempting to extract campaign data from data");

    let data = match data {
        Some(data) => data,
        None => return Ok(None),
    };

    let data: Option<types::SavedPatreonData> = serde_json::from_value(data)?;

    Ok(data.and_then(|data| data.campaign))
}

mod types {
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    #[derive(Clone, Deserialize, Serialize)]
    pub struct PatreonCredentials {
        pub access_token: String,
        pub refresh_token: String,
        pub expires_after: chrono::DateTime<chrono::Utc>,
    }

    #[derive(Deserialize, Serialize, PartialEq, Eq)]
    pub struct Campaign {
        pub webhook_id: String,
        pub webhook_secret: String,
    }

    #[derive(Deserialize, Serialize)]
    pub struct SavedPatreonData {
        pub site_id: String,
        pub credentials: PatreonCredentials,
        pub campaign: Option<Campaign>,
    }

    #[derive(Debug, Deserialize)]
    pub struct PatreonOAuthCallback {
        pub code: String,
        pub state: String,
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
    pub struct PatreonIdentityAttributes {
        pub full_name: String,
    }

    #[derive(Debug, Deserialize, Serialize)]
    pub struct PatreonRelationshipsCampaign<A, R = ()> {
        pub campaign: PatreonData<PatreonDataItem<A, R>>,
    }

    #[derive(Debug, Deserialize, Serialize)]
    pub struct PatreonWebhookDataAttributes {
        pub paused: bool,
        pub secret: String,
        pub triggers: Vec<String>,
        pub uri: String,
    }

    #[derive(Debug, Deserialize, Serialize)]
    pub struct PatreonPostAttributes {
        pub embed_data: Option<serde_json::Value>,
        pub embed_url: Option<String>,
        pub published_at: Option<chrono::DateTime<chrono::Utc>>,
        pub title: Option<String>,
        pub url: String,
    }

    #[derive(Debug, Deserialize)]
    pub struct WebhookPostQuery {
        pub id: Uuid,
    }
}
