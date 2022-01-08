# fuzzysearch-owo

FuzzySearch OwO builds off the technologies (and database) of [FuzzySearch] to
detect reposts of artwork. Users can manually upload artwork to their collection
or it can automatically import content from linked websites.

[fuzzysearch]: https://github.com/Syfaro/fuzzysearch

## Features

It searches for similar images on:

- F-list (identifying images using the character gallery page)
- Reddit (from a static list of subreddits)
- FurAffinity, Weasyl, e621, and Twitter (data provided by FuzzySearch webhooks)

It can import user content from:

- FurAffinity (and can be extended to other data sent in from FuzzySearch)
- DeviantArt

It will only import user content after the user has either signed into the
service (via OAuth, etc.) or some other verification of their profile.

Patreon importing is a wanted feature, and most of the code already exists, but
the Patreon API does not return any images nor attachments making it impossible.

## Deployment

This software has no privileged access to other services, making it possible to
run yourself. However, no support is provided for this.

It currently runs as two separate services, one to serve the website and the
other to perform background tasks. These can be scaled as needed. The worker
services can run different queues, one for internal requests and another for
outgoing requests. This can be set with the `--faktory-queues` option. Requests
to related projects (e.g. FuzzySearch) are not considered external.

A `docker-compose.yml` file has been provided with the essentials to get things
up and running.

### Dependencies

It has a few software dependencies to run:

- PostgreSQL with the [bktree] extension as the primary backing datastore
- Redis to send events about user actions
- [Faktory] for managing background tasks
- [faktory-cron] to run scheduled jobs (provided in jobs.yaml)

It also requires credentials or app tokens for the following sites:

- DeviantArt (OAuth client)
- F-list (username and password)
- FurAffinity (authenticated cookies)
- FuzzySearch (API key)
- Patreon (OAuth client, not currently used)
- Reddit (username, password, and OAuth client)
- S3-like (endpoint, region, bucket, access, and secret key)
- SMTP (host, username, and password)

[bktree]: https://github.com/fake-name/pg-spgist_hamming
[faktory]: https://github.com/contribsys/faktory
[faktory-cron]: https://github.com/Syfaro/faktory-cron

### Database Migrations

Migrations can either be applied manually with [sqlx-cli] or by running a
service with the `--run-migrations` flag. This operation is safe to run many
times and should not block when already up.

[sqlx-cli]: https://crates.io/crates/sqlx-cli

### Configuration

All required and optional configuration options can be seen when the binary is
run with the `--help` flag.

### Monitoring and Observability

It exposes Prometheus metrics on the host provided in the environment variable
`METRICS_HOST` at `/metrics`.

Logs are sent to a Jaeger agent available at `JAEGER_COLLECTOR`.
